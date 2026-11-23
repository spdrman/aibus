//! The Phase 1 reference suite (#285): ten repo-review cases that run locally and offline, against
//! recorded Claude Code streams from the spike (#281) or small stand-in CLIs, and produce a versioned
//! scorecard and a redacted example trace. `AIBUS_BLESS=1` rewrites the committed scorecard and
//! trace; otherwise what this run produces has to match them. Nothing here starts the real CLI or
//! needs a network: the optional live mode (`live_reference`, ignored unless asked for) is the only
//! test that does, and CI never asks.
//!
//! The ten cases are the ten kinds #285 lists: native delegation, failed tool, timeout, cancelled
//! run, restart, overlapping output, malformed event, missing usage, approval and trace replay.
//! `fixtures/claude-code/reference-285/corpus.json` says, per case, whether its stream is a real
//! recording, a real recording with one stated change, or made by a stand-in.
#![cfg(unix)]

mod common;
#[path = "../../aibus-supervisor/tests/common/mod.rs"]
mod sup;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aibus_core::Redactor;
use aibus_supervisor::claude::EXTENSION_ID;
use aibus_supervisor::{cli, supervise, Ending, Launch, Limits, Normalizer, Outcome};
use aibus_trace::view::{self, Recorded};
use aibus_trace::{
    project, AttemptRef, AttemptStatus, DataQuality, EventKind, Origin, PayloadPolicy, StoreLimits,
    ToolOutcome, TraceEvent, TraceStore, Visibility,
};
use serde_json::{json, Value};
use sup::{load, Feed};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Bumped when a column of the scorecard is added, removed or changes meaning.
const SCORECARD_VERSION: &str = "1.0.0";
/// Bumped when a case is added or its stream changes.
const CORPUS_VERSION: u32 = 1;

const ALL_KINDS: [EventKind; 21] = [
    EventKind::WorkflowStarted,
    EventKind::StepScheduled,
    EventKind::RouteDecided,
    EventKind::ApprovalRequested,
    EventKind::ApprovalResolved,
    EventKind::AttemptStarted,
    EventKind::AttemptCompleted,
    EventKind::AttemptFailed,
    EventKind::AgentStarted,
    EventKind::AgentStopped,
    EventKind::TaskDelegated,
    EventKind::MessageChunk,
    EventKind::ToolStarted,
    EventKind::ToolCompleted,
    EventKind::ToolFailed,
    EventKind::CheckpointWritten,
    EventKind::ArtifactCreated,
    EventKind::UsageReported,
    EventKind::AuthRequired,
    EventKind::GapDetected,
    EventKind::TraceWarning,
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn reference_dir() -> PathBuf {
    repo().join("fixtures/claude-code/reference-285")
}

fn wire(v: impl serde::Serialize) -> String {
    match serde_json::to_value(v) {
        Ok(Value::String(s)) => s,
        other => format!("{other:?}"),
    }
}

fn bless() -> bool {
    std::env::var_os("AIBUS_BLESS").is_some()
}

/// A named check: a failing one stops the case and names itself, a passing one lands in the
/// scorecard so the card says what was actually checked.
#[derive(Default)]
struct Checks(Vec<String>);

impl Checks {
    fn ok(&mut self, name: &str, holds: bool) {
        assert!(holds, "check failed: {name}");
        self.0.push(name.to_owned());
    }
}

fn attempt_ref(n: u128) -> AttemptRef {
    AttemptRef {
        workflow_run_id: Uuid::from_u128(0x285_0001),
        attempt_id: Uuid::from_u128(0x285_1000 + n),
        parent_attempt_id: None,
        provider_extension: EXTENSION_ID.into(),
    }
}

fn levels(events: &[TraceEvent]) -> BTreeMap<String, u64> {
    let mut m = BTreeMap::new();
    for e in events {
        *m.entry(wire(e.visibility)).or_insert(0) += 1;
    }
    m
}

fn of_kind(events: &[TraceEvent], kind: EventKind) -> Vec<&TraceEvent> {
    events.iter().filter(|e| e.kind == kind).collect()
}

/// What a projection counts, without evidence levels: equal for the same run however it arrived.
fn facts(events: &[TraceEvent]) -> (BTreeSet<String>, BTreeSet<String>, BTreeSet<String>) {
    let p = project(events);
    (
        p.agents
            .iter()
            .map(|a| format!("{} {:?}", a.agent_id, a.role))
            .collect(),
        p.tools
            .iter()
            .map(|t| format!("{} {:?} {:?}", t.tool_use_id, t.agent_id, t.name))
            .collect(),
        p.edges
            .iter()
            .map(|e| format!("{:?} -> {} via {:?}", e.parent, e.child, e.via_tool_use_id))
            .collect(),
    )
}

fn spike_stream(name: &str) -> PathBuf {
    sup::spike_dir().join(format!("{name}.stdout.jsonl"))
}

fn stream_values(name: &str) -> Vec<Value> {
    fs::read_to_string(spike_stream(name))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// A prepared stream written next to the case's trace, one JSON value per line.
fn write_stream(dir: &Path, name: &str, lines: &[Value]) -> PathBuf {
    let p = dir.join(name);
    let mut s = String::new();
    for l in lines {
        s.push_str(&serde_json::to_string(l).unwrap());
        s.push('\n');
    }
    fs::write(&p, s).unwrap();
    p
}

/// A scratch directory with a work directory in it and a stand-in CLI runner.
struct Rig {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Rig {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("work")).unwrap();
        Self { _tmp: tmp, root }
    }

    fn trace(&self) -> PathBuf {
        self.root.join("trace")
    }

    fn open(&self, policy: PayloadPolicy) -> (TraceStore, aibus_trace::Recovery) {
        TraceStore::open(
            &self.trace(),
            Redactor::new(),
            policy,
            StoreLimits::default(),
        )
        .unwrap()
    }

    /// One attempt of the stand-in `script` (under `stand-ins/`), launched the way the real CLI is:
    /// an absolute executable (`/bin/sh`), an argv vector, the bounded environment plus `env`.
    async fn attempt(
        &self,
        script: &str,
        env: &[(&str, &str)],
        limits: Limits,
        cancel: CancellationToken,
        who: AttemptRef,
        store: &mut TraceStore,
    ) -> Outcome {
        let launch = Launch {
            exe: "/bin/sh".into(),
            args: vec![reference_dir()
                .join("stand-ins")
                .join(script)
                .into_os_string()],
            cwd: self.root.join("work"),
            env: cli::bounded_env(
                Vec::<(std::ffi::OsString, std::ffi::OsString)>::new(),
                &[],
                env,
            )
            .unwrap(),
            stdin: b"review this pull request".to_vec(),
            limits,
        };
        let mut norm = Normalizer::new(who);
        // A regression that hangs the host fails here in seconds, not at the job timeout.
        tokio::time::timeout(
            Duration::from_secs(30),
            supervise(launch, &mut norm, store, cancel),
        )
        .await
        .expect("supervise did not return within 30 s")
        .unwrap()
    }

    async fn once(
        &self,
        script: &str,
        stream: &Path,
        limits: Limits,
    ) -> (Outcome, Vec<TraceEvent>) {
        self.once_with(script, stream, &[], limits).await
    }

    async fn once_with(
        &self,
        script: &str,
        stream: &Path,
        env: &[(&str, &str)],
        limits: Limits,
    ) -> (Outcome, Vec<TraceEvent>) {
        let (mut store, _) = self.open(PayloadPolicy::MetadataOnly);
        let stream = stream.display().to_string();
        let mut vars = vec![("STREAM", stream.as_str())];
        vars.extend_from_slice(env);
        let out = self
            .attempt(
                script,
                &vars,
                limits,
                CancellationToken::new(),
                attempt_ref(1),
                &mut store,
            )
            .await;
        drop(store);
        (out, TraceStore::read(&self.trace()).unwrap().0)
    }
}

fn last(events: &[TraceEvent]) -> &TraceEvent {
    events.last().unwrap()
}

/// The case's row: what it is, how it ended and the checks it passed.
fn row(
    id: &str,
    kind: &str,
    source: &str,
    ending: &str,
    status: &str,
    events: Option<&[TraceEvent]>,
    checks: Checks,
) -> Value {
    json!({
        "id": id,
        "kind": kind,
        "stream": source,
        "result": "pass",
        "ending": ending,
        "attempt_status": status,
        "events": events.map(<[TraceEvent]>::len),
        "evidence_levels": events.map(levels),
        "checks": checks.0,
    })
}

// ---------------------------------------------------------------- the ten cases

/// 1. A lead hands two read-only lookups to two native subagents; every edge has source events.
fn assert_native_delegation(events: &[TraceEvent], c: &mut Checks) {
    let p = project(events);
    c.ok("one lead and two native children", p.agents.len() == 3);
    c.ok("two delegation edges", p.edges.len() == 2);
    let lead = p
        .agents
        .iter()
        .find(|a| a.role.as_deref() == Some("lead"))
        .map(|a| a.agent_id.clone());
    c.ok("a lead was observed", lead.is_some());
    c.ok(
        "both edges point from the lead, via a native tool_use id",
        p.edges
            .iter()
            .all(|e| e.parent == lead && e.via_tool_use_id.is_some()),
    );
    c.ok(
        "both edges are observed and carry source events",
        p.edges
            .iter()
            .all(|e| e.level == Visibility::Observed && !e.sources.is_empty()),
    );
    c.ok(
        "each child ran a tool that completed",
        p.tools.len() == 2 && p.tools.iter().all(|t| t.outcome == ToolOutcome::Completed),
    );
    let ended = of_kind(events, EventKind::AttemptCompleted);
    c.ok("the attempt completed", ended.len() == 1);
}

async fn case_01_native_delegation() -> Value {
    let rig = Rig::new();
    let (out, events) = rig
        .once(
            "replay.sh",
            &spike_stream("11-foreground"),
            Limits::default(),
        )
        .await;
    let mut c = Checks::default();
    c.ok(
        "supervised run exits 0 and is completed",
        (out.ending, out.status) == (Ending::Exited(0), AttemptStatus::Completed),
    );
    assert_native_delegation(&events, &mut c);

    // The same run with its hooks: a second, independent channel backs every edge.
    let with_hooks = sup::replay("11-foreground", Feed::StreamThenHooks);
    let p = project(&with_hooks);
    c.ok(
        "hooks cross-check both edges (a second channel, same ids)",
        p.edges.len() == 2 && p.edges.iter().all(|e| e.cross_checked),
    );
    c.ok(
        "hook events corroborate and are not double counted",
        with_hooks
            .iter()
            .any(|e| e.origin == Origin::Hook && e.corroborates.is_some())
            && p.tools.len() == 2,
    );
    c.ok(
        "the stream alone and stream plus hooks give the same graph",
        facts(&events) == facts(&with_hooks),
    );
    row(
        "01-native-delegation",
        "native_delegation",
        "recorded_real",
        "exited(0)",
        "completed",
        Some(&events),
        c,
    )
}

/// 2. A tool result comes back with `is_error`: the tool is failed, the attempt is not.
async fn case_02_failed_tool() -> Value {
    let rig = Rig::new();
    let mut lines = stream_values("11-foreground");
    // The one change to the recording: the last child tool result is marked `is_error`.
    let at = lines
        .iter()
        .rposition(|v| {
            v["type"] == "user"
                && v["parent_tool_use_id"].is_string()
                && v["message"]["content"][0]["type"] == "tool_result"
        })
        .expect("a child tool result");
    lines[at]["message"]["content"][0]["is_error"] = json!(true);
    let stream = write_stream(&rig.root, "failed-tool.jsonl", &lines);
    let (out, events) = rig.once("replay.sh", &stream, Limits::default()).await;
    let mut c = Checks::default();
    let p = project(&events);
    c.ok(
        "the attempt still completes (the CLI exited 0)",
        (out.ending, out.status) == (Ending::Exited(0), AttemptStatus::Completed),
    );
    c.ok(
        "exactly one tool failed and one completed",
        p.tools
            .iter()
            .filter(|t| t.outcome == ToolOutcome::Failed)
            .count()
            == 1
            && p.tools
                .iter()
                .filter(|t| t.outcome == ToolOutcome::Completed)
                .count()
                == 1,
    );
    let failed = of_kind(&events, EventKind::ToolFailed);
    c.ok(
        "the failure is an observed native event on the child, not inferred",
        failed.len() == 1
            && failed[0].visibility == Visibility::Observed
            && failed[0].agent_id.is_some()
            && failed[0].native.tool_use_id.is_some(),
    );
    c.ok(
        "a failed tool does not disturb the delegation graph",
        p.agents.len() == 3 && p.edges.len() == 2,
    );
    row(
        "02-failed-tool",
        "failed_tool",
        "recorded_real_with_one_change(is_error on one child tool result)",
        "exited(0)",
        "completed",
        Some(&events),
        c,
    )
}

/// 3. The host's wall-time limit stops a CLI that never finishes; the trace says interrupted.
async fn case_03_timeout() -> Value {
    let rig = Rig::new();
    let limits = Limits {
        wall_time: Duration::from_millis(1_200),
        grace: Duration::from_millis(300),
        ..Limits::default()
    };
    let started = Instant::now();
    let (out, events) = rig
        .once("timeout.sh", &spike_stream("11-foreground"), limits)
        .await;
    let mut c = Checks::default();
    c.ok(
        "stopped by the wall-time limit, within seconds",
        out.ending == Ending::WallTime && started.elapsed() < Duration::from_secs(15),
    );
    c.ok("the attempt is interrupted, never completed", {
        out.status == AttemptStatus::Interrupted
            && !events.iter().any(|e| e.kind == EventKind::AttemptCompleted)
            && last(&events).kind == EventKind::AttemptFailed
            && last(&events).data["status"] == "interrupted"
    });
    let p = project(&events);
    c.ok(
        "what happened before the limit is still in the graph",
        !p.agents.is_empty() && p.attempts[0].status == Some(AttemptStatus::Interrupted),
    );
    row(
        "03-timeout",
        "timeout",
        "recorded_real_prefix_plus_stand_in(never finishes)",
        "wall_time",
        "interrupted",
        None,
        c,
    )
}

/// 4. The user cancels: SIGTERM first, then the attempt is cancelled (not interrupted, not failed).
async fn case_04_cancelled_run() -> Value {
    let rig = Rig::new();
    let marker = rig.root.join("got-term");
    let (mut store, _) = rig.open(PayloadPolicy::MetadataOnly);
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(700)).await;
        c2.cancel();
    });
    let out = rig
        .attempt(
            "cancel.sh",
            &[
                (
                    "STREAM",
                    &spike_stream("11-foreground").display().to_string(),
                ),
                ("MARKER", &marker.display().to_string()),
            ],
            Limits::default(),
            cancel,
            attempt_ref(1),
            &mut store,
        )
        .await;
    drop(store);
    let events = TraceStore::read(&rig.trace()).unwrap().0;
    let mut c = Checks::default();
    c.ok(
        "the host reports a user cancel",
        (out.ending, out.status) == (Ending::Cancelled, AttemptStatus::Cancelled),
    );
    c.ok(
        "the CLI was asked to stop (SIGTERM) before any kill",
        marker.exists(),
    );
    c.ok(
        "the trace ends attempt_failed with status cancelled",
        last(&events).kind == EventKind::AttemptFailed
            && last(&events).data["status"] == "cancelled",
    );
    c.ok(
        "a cancelled attempt is not marked completed or interrupted",
        !events.iter().any(|e| e.kind == EventKind::AttemptCompleted)
            && project(&events).attempts[0].status == Some(AttemptStatus::Cancelled),
    );
    row(
        "04-cancelled-run",
        "cancelled_run",
        "recorded_real_prefix_plus_stand_in(waits for cancel)",
        "cancelled",
        "cancelled",
        None,
        c,
    )
}

/// 5. The CLI dies mid-tool, aibus dies mid-write, aibus starts again: the store repairs itself, the
///    first attempt stays interrupted with its open tool flagged, the retry is a new attempt, and
///    nothing is resumed.
async fn case_05_restart() -> Value {
    let rig = Rig::new();
    let mut lines = stream_values("11-foreground");
    // Cut the recording right after a child's `Read` starts: the tool never reports back.
    let cut = lines
        .iter()
        .position(|v| {
            v["type"] == "assistant"
                && v["parent_tool_use_id"].is_string()
                && v["message"]["content"][0]["name"] == "Read"
        })
        .expect("a child tool_use");
    lines.truncate(cut + 1);
    let stream = write_stream(&rig.root, "cut-mid-tool.jsonl", &lines);
    let stream_s = stream.display().to_string();

    // Attempt 1: the CLI process is killed by someone else.
    let (mut store, rec) = rig.open(PayloadPolicy::MetadataOnly);
    let mut c = Checks::default();
    c.ok("a fresh store has nothing to repair", rec.torn_bytes == 0);
    let one = rig
        .attempt(
            "killed.sh",
            &[("STREAM", &stream_s)],
            Limits::default(),
            CancellationToken::new(),
            attempt_ref(1),
            &mut store,
        )
        .await;
    drop(store);
    c.ok(
        "attempt 1 is interrupted by the signal",
        (one.ending, one.status) == (Ending::Signalled(9), AttemptStatus::Interrupted),
    );

    // aibus itself dies in the middle of the next write: half an event, no newline.
    {
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(rig.trace().join("events.jsonl"))
            .unwrap();
        f.write_all(br#"{"schema_version":2,"event_id":"0000"#)
            .unwrap();
    }

    // aibus starts again: it opens the same run directory and attempts again.
    let (mut store, rec) = rig.open(PayloadPolicy::MetadataOnly);
    c.ok(
        "the torn tail is cut off and kept aside",
        rec.torn_bytes > 0
            && fs::read_dir(rig.trace()).unwrap().flatten().any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("events.jsonl.torn-")
            }),
    );
    let two = rig
        .attempt(
            "replay.sh",
            &[("STREAM", &spike_stream("06-basic").display().to_string())],
            Limits::default(),
            CancellationToken::new(),
            attempt_ref(2),
            &mut store,
        )
        .await;
    drop(store);
    c.ok(
        "the retry is a second attempt that completes",
        (two.ending, two.status) == (Ending::Exited(0), AttemptStatus::Completed),
    );

    let (events, report) = TraceStore::read(&rig.trace()).unwrap();
    let p = project(&events);
    c.ok(
        "the repaired store reads clean",
        report.torn_bytes == 0 && report.corrupt_lines == 0,
    );
    let file_ids: Vec<Uuid> = fs::read_to_string(rig.trace().join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|l| {
            serde_json::from_str::<Value>(l).unwrap()["event_id"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap()
        })
        .collect();
    c.ok(
        "event ids are unique uuid v7s and increase in file order across the restart",
        file_ids.len() == events.len()
            && file_ids.iter().all(|i| i.get_version_num() == 7)
            && file_ids.windows(2).all(|w| w[0] < w[1]),
    );
    c.ok(
        "the repair is itself in the trace as a warning",
        events.iter().any(|e| {
            e.kind == EventKind::TraceWarning && e.data["reason"] == "torn_tail_recovered"
        }),
    );
    let ids: Vec<Uuid> = p.attempts.iter().map(|a| a.attempt_id).collect();
    c.ok(
        "two attempts, two ids, both still inspectable",
        ids.len() == 2 && ids[0] != ids[1],
    );
    let a1 = p
        .attempts
        .iter()
        .find(|a| a.attempt_id == attempt_ref(1).attempt_id)
        .unwrap();
    let a2 = p
        .attempts
        .iter()
        .find(|a| a.attempt_id == attempt_ref(2).attempt_id)
        .unwrap();
    c.ok(
        "attempt 1 stays interrupted and is flagged requires_reconciliation (a tool never reported back)",
        a1.status == Some(AttemptStatus::Interrupted)
            && a1.requires_reconciliation
            && p.tools.iter().any(|t| t.attempt_id == a1.attempt_id && t.outcome == ToolOutcome::Unknown),
    );
    c.ok(
        "attempt 2 is untouched by attempt 1",
        a2.status == Some(AttemptStatus::Completed) && !a2.requires_reconciliation,
    );
    c.ok(
        "no event claims a vendor session was resumed or restored",
        !events.iter().any(|e| {
            let d = e.data.to_string().to_lowercase();
            d.contains("resum") || d.contains("restor")
        }),
    );
    row(
        "05-restart",
        "restart",
        "recorded_real_prefix_plus_stand_in(killed)",
        "signalled(9) then exited(0)",
        "interrupted then completed",
        Some(&events),
        c,
    )
}

/// 6. Two children's output interleaves on stdout while stderr floods at the same time.
async fn case_06_overlapping_output() -> Value {
    let rig = Rig::new();
    let (out, events) = rig
        .once_with(
            "overlap.sh",
            &spike_stream("07-deleg"),
            &[("NOISE", "20")],
            Limits::default(),
        )
        .await;
    let mut c = Checks::default();
    c.ok(
        "completed despite concurrent stderr",
        (out.ending, out.status) == (Ending::Exited(0), AttemptStatus::Completed),
    );
    let seq: Vec<&str> = events
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                EventKind::MessageChunk | EventKind::ToolStarted | EventKind::ToolCompleted
            )
        })
        .filter_map(|e| e.agent_id.as_deref())
        .collect();
    let interleaved = (0..seq.len()).any(|i| {
        (i + 2..seq.len()).any(|k| seq[i] == seq[k] && seq[i + 1..k].iter().any(|m| *m != seq[i]))
    });
    c.ok("the recording really interleaves two agents", interleaved);
    c.ok(
        "the graph is the same as the offline replay of the same stream",
        facts(&events) == facts(&sup::replay("07-deleg", Feed::Stream)),
    );
    c.ok(
        "nothing was dropped to keep up (no gap on stdout)",
        !events
            .iter()
            .any(|e| e.kind == EventKind::GapDetected && e.data["channel"] == "stdout"),
    );
    c.ok(
        "stderr text never reaches the trace",
        !fs::read(rig.trace().join("events.jsonl"))
            .unwrap()
            .windows(14)
            .any(|w| w == b"stand-in noise"),
    );
    c.ok(
        "several result lines still end the attempt only at exit",
        of_kind(&events, EventKind::AttemptCompleted).len() == 1
            && last(&events).data["results_seen"].as_u64() == Some(3),
    );
    row(
        "06-overlapping-output",
        "overlapping_output",
        "recorded_real_plus_stand_in(20 stderr lines in parallel)",
        "exited(0)",
        "completed",
        None,
        c,
    )
}

/// A CLI that exits normally with output still queued in its pipes. Every line the host reads is an
/// fsync'd append, so it gets through a few hundred lines a second; two seconds after the CLI exits
/// the host kills the group and stops reading, and what is left is dropped with no gap event and no
/// warning: the attempt reads as complete. Here 8,000 distinct stderr lines (about 46 KB, so they all fit in the pipe) sit in the pipe when the
/// stand-in exits, then the recording is printed. Either every line must be in the trace or a gap
/// must say some were not read (#324, fixed in #326). The drain limits are short so the test stays quick.
#[tokio::test]
async fn defect_a_backlog_after_exit_is_not_dropped_silently() {
    let rig = Rig::new();
    let (out, events) = rig
        .once_with(
            "flood.sh",
            &spike_stream("07-deleg"),
            &[],
            Limits {
                drain_stall: Duration::from_secs(1),
                drain_max: Duration::from_secs(3),
                ..Limits::default()
            },
        )
        .await;
    assert_eq!(out.ending, Ending::Exited(0));
    let read = of_kind(&events, EventKind::TraceWarning)
        .iter()
        .filter(|e| e.data["channel"] == "stderr")
        .count();
    let said_so = events.iter().any(|e| {
        e.kind == EventKind::GapDetected
            && ["drop", "unread", "backlog", "drain"]
                .iter()
                .any(|w| e.data["reason"].as_str().is_some_and(|r| r.contains(w)))
    });
    assert!(
        read == 8_000 || said_so,
        "only {read} of 8000 stderr lines were read and nothing says the rest were dropped"
    );
}

/// 7. Broken JSON, invalid UTF-8, an unknown event type and a cut-off last line.
async fn case_07_malformed_event() -> Value {
    let rig = Rig::new();
    let (out, events) = rig
        .once("malformed.sh", &spike_stream("06-basic"), Limits::default())
        .await;
    let mut c = Checks::default();
    c.ok(
        "the attempt is not ended by bad lines",
        (out.ending, out.status) == (Ending::Exited(0), AttemptStatus::Completed),
    );
    let gaps = of_kind(&events, EventKind::GapDetected);
    c.ok(
        "each damaged line is a labelled gap, unavailable not guessed",
        gaps.len() >= 3 && gaps.iter().all(|g| g.visibility == Visibility::Unavailable),
    );
    c.ok(
        "an event type aibus has never seen is kept as a warning naming it",
        events.iter().any(|e| {
            e.kind == EventKind::TraceWarning && e.data.to_string().contains("future_event_kind")
        }),
    );
    c.ok(
        "the damage did not change the graph",
        facts(&events) == facts(&sup::replay("06-basic", Feed::Stream)),
    );
    c.ok(
        "a line the CLI printed twice is counted once (duplicate detection)",
        last(&events).data["duplicates"].as_u64() >= Some(1),
    );
    c.ok(
        "the broken text is not copied into the trace",
        !fs::read_to_string(rig.trace().join("events.jsonl"))
            .unwrap()
            .contains("not json at all"),
    );
    row(
        "07-malformed-event",
        "malformed_event",
        "recorded_real_plus_stand_in(damaged lines)",
        "exited(0)",
        "completed",
        Some(&events),
        c,
    )
}

/// 8. The CLI reports no usage and no cost: they stay unknown, in every view.
async fn case_08_missing_usage() -> Value {
    let rig = Rig::new();
    let mut lines = stream_values("11-foreground");
    // The one change to the recording: every usage, cost and duration the CLI reported is removed.
    for v in &mut lines {
        let Some(o) = v.as_object_mut() else { continue };
        for k in [
            "usage",
            "total_cost_usd",
            "modelUsage",
            "model_usage_since_last_result",
            "duration_ms",
            "duration_api_ms",
            "num_turns",
        ] {
            o.remove(k);
        }
    }
    let stream = write_stream(&rig.root, "no-usage.jsonl", &lines);
    let (out, events) = rig.once("replay.sh", &stream, Limits::default()).await;
    let mut c = Checks::default();
    c.ok(
        "the run completes without usage",
        (out.ending, out.status) == (Ending::Exited(0), AttemptStatus::Completed),
    );
    let usage = of_kind(&events, EventKind::UsageReported);
    let metrics: Vec<Value> = usage
        .iter()
        .flat_map(|e| e.data["metrics"].as_array().cloned().unwrap_or_default())
        .collect();
    c.ok(
        "a usage event is still written",
        !usage.is_empty() && !metrics.is_empty(),
    );
    c.ok(
        "every metric is unknown with no value, none invented",
        metrics
            .iter()
            .all(|m| m["quality"] == "unknown" && m["value"].is_null()),
    );
    let p = project(&events);
    c.ok(
        "the projection carries no known number",
        p.attempts
            .iter()
            .flat_map(|a| a.usage.iter())
            .all(|m| m.value.is_none() && m.quality == DataQuality::Unknown),
    );
    let rec = Recorded::load(&rig.trace()).unwrap();
    let fleet = view::render_fleet(&[view::RunRow::of("no-usage", &rec)]);
    c.ok(
        "the fleet row says unknown, not zero",
        fleet.contains("unknown usd (unknown)"),
    );
    let screen = view::graph(&rec, "no-usage");
    c.ok(
        "the inspector says usage was not reported",
        screen.contains("not reported") || screen.contains("unknown"),
    );
    row(
        "08-missing-usage",
        "missing_usage",
        "recorded_real_with_one_change(usage, cost and durations removed)",
        "exited(0)",
        "completed",
        Some(&events),
        c,
    )
}

/// 9. Approval. Phase 1 has no approval surface of its own: the CLI is launched with
///    `--permission-prompts none` so it denies what would prompt, the workflow plan lists which steps
///    are held for approval, and nothing is ever approved on the user's behalf.
async fn case_09_approval() -> Value {
    let mut c = Checks::default();

    // a. The real denial the spike recorded (run 09): a Write was attempted, nobody could answer.
    let events = sup::replay("09-denied", Feed::StreamThenHooks);
    let failed = of_kind(&events, EventKind::ToolFailed);
    c.ok(
        "a denied tool is an observed tool_failed with reason permission_denied",
        failed.iter().any(|e| {
            e.visibility == Visibility::Observed && e.data["reason"] == "permission_denied"
        }),
    );
    c.ok(
        "no approval event is invented for the denial (no approval_requested or approval_resolved)",
        !events.iter().any(|e| {
            matches!(
                e.kind,
                EventKind::ApprovalRequested | EventKind::ApprovalResolved
            )
        }),
    );
    c.ok(
        "the attempt is not marked as approved or failed because of the denial",
        last(&events).kind == EventKind::AttemptCompleted,
    );

    // b. The argv aibus builds can never ask the CLI to skip permissions.
    let spike = sup::spike_dir();
    let probe = cli::probe(
        &fs::read_to_string(spike.join("01-version.txt")).unwrap(),
        &fs::read_to_string(spike.join("02-help.txt")).unwrap(),
    )
    .unwrap();
    let argv: Vec<String> = cli::argv(
        &probe,
        &cli::Request {
            model: None,
            setting_sources: "user".into(),
            max_budget_usd: None,
            include_hook_events: true,
            forward_subagent_text: true,
            agents_json: Some("{}".into()),
        },
    )
    .unwrap()
    .into_iter()
    .map(|a| a.to_string_lossy().into_owned())
    .collect();
    let joined = argv.join(" ");
    c.ok(
        "argv always carries --permission-prompts none",
        joined.contains("--permission-prompts none"),
    );
    c.ok(
        "argv never carries a bypass or auto-accept flag or mode",
        !joined.contains("dangerously")
            && !joined.contains("bypassPermissions")
            && !joined.contains("acceptEdits")
            && !joined.contains("--allowedTools"),
    );

    // c. The compiled plan holds the lead step for approval and says the scheduler is not here yet.
    let home = aibus_testkit::xdg_temp();
    let dir = home.path().join("proj/.aibus/workflows");
    fs::create_dir_all(&dir).unwrap();
    fs::copy(
        repo().join("fixtures/workflows/graph/repo-review.toml"),
        dir.join("repo-review.toml"),
    )
    .unwrap();
    let o = common::aibus(
        home.path(),
        &["--json", "workflow", "plan", "repo-review", "--dry-run"],
    );
    assert_eq!(o.status.code(), Some(0), "{}", common::stderr(&o));
    let plan: Value = serde_json::from_slice(&o.stdout).unwrap();
    let held = plan["approvals"]["steps"]["lead"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    c.ok(
        "the plan holds the lead step for network_write, git_merge and destructive_tool approval",
        ["network_write", "git_merge", "destructive_tool"]
            .iter()
            .all(|k| held.iter().any(|h| h == k)),
    );
    c.ok(
        "the plan says it cannot run yet (plan-only until the scheduler), and starts nothing",
        plan["runnable_now"] == false,
    );
    c.ok(
        "the claude runtime is listed experimental, not available",
        plan["runtimes"]["cli.claude"]["status"] == "experimental",
    );
    row(
        "09-approval",
        "approval",
        "recorded_real(09-denied)+compiled_plan; interactive approval is unsupported in Phase 1",
        "exited(0)",
        "completed",
        Some(&events),
        c,
    )
}

/// 10. A saved trace replays offline: no provider, no process, byte for byte the same every time.
async fn case_10_trace_replay() -> Value {
    let traces = repo().join("fixtures/claude-code/traces-284");
    let views = repo().join("fixtures/claude-code/views-284");
    let mut c = Checks::default();
    let home = aibus_testkit::xdg_temp();
    let replay = |extra: &[&str]| {
        let mut cmd = common::command(home.path());
        // No PATH at all and no provider config: whatever this prints, it printed without a CLI.
        cmd.env("PATH", "")
            .env("AIBUS_CONFIG", home.path().join("none.toml"));
        cmd.args([
            "runs",
            "replay",
            "11-foreground.hooks",
            "--offline",
            "--root",
        ])
        .arg(&traces)
        .args(extra);
        cmd.output().unwrap()
    };
    let one = replay(&[]);
    let two = replay(&[]);
    c.ok("replay exits 0 with no CLI on PATH", one.status.success());
    c.ok(
        "replay is byte for byte repeatable",
        one.stdout == two.stdout,
    );
    let text = String::from_utf8(one.stdout.clone()).unwrap();
    c.ok(
        "replay equals the committed golden",
        text == fs::read_to_string(views.join("11-foreground.hooks.replay.txt")).unwrap(),
    );
    c.ok(
        "replay says it is the recording, not a new run",
        text.contains("offline (no provider, no process; the recorded run, not a new one)"),
    );
    let raw = replay(&["--events"]);
    c.ok(
        "--events prints the stored lines byte for byte",
        raw.stdout == fs::read(traces.join("11-foreground.hooks/events.jsonl")).unwrap(),
    );
    let rec = Recorded::load(&traces.join("11-foreground.hooks")).unwrap();
    c.ok(
        "the replayed graph has the lead, two children and their tools",
        {
            let p = project(&rec.events);
            p.agents.len() == 3 && p.edges.len() == 2 && p.tools.len() == 2
        },
    );
    let mut rerun = common::command(home.path());
    rerun.env("PATH", "");
    let rerun = rerun
        .args([
            "runs",
            "rerun",
            "11-foreground.hooks",
            "--isolated",
            "--root",
        ])
        .arg(&traces)
        .output()
        .unwrap();
    let err = common::stderr(&rerun);
    c.ok(
        "a rerun is refused, says its outcome may differ, and does not start anything",
        !rerun.status.success() && err.contains("may differ") && err.contains("not available yet"),
    );
    let lower = text.to_lowercase();
    c.ok(
        "the replay makes no claim of re-execution, determinism or a restored vendor session",
        !lower.contains("deterministic")
            && !lower.contains("restored")
            && !lower.contains("resumed"),
    );
    row(
        "10-trace-replay",
        "trace_replay",
        "recorded_real(11-foreground.hooks, stored by the real TraceStore)",
        "n/a",
        "n/a",
        Some(&rec.events),
        c,
    )
}

const CASES: [&str; 10] = [
    "01-native-delegation",
    "02-failed-tool",
    "03-timeout",
    "04-cancelled-run",
    "05-restart",
    "06-overlapping-output",
    "07-malformed-event",
    "08-missing-usage",
    "09-approval",
    "10-trace-replay",
];

async fn run_case(id: &str) -> Value {
    match id {
        "01-native-delegation" => case_01_native_delegation().await,
        "02-failed-tool" => case_02_failed_tool().await,
        "03-timeout" => case_03_timeout().await,
        "04-cancelled-run" => case_04_cancelled_run().await,
        "05-restart" => case_05_restart().await,
        "06-overlapping-output" => case_06_overlapping_output().await,
        "07-malformed-event" => case_07_malformed_event().await,
        "08-missing-usage" => case_08_missing_usage().await,
        "09-approval" => case_09_approval().await,
        "10-trace-replay" => case_10_trace_replay().await,
        other => panic!("no case {other}"),
    }
}

#[tokio::test]
async fn case_native_delegation() {
    run_case("01-native-delegation").await;
}
#[tokio::test]
async fn case_failed_tool() {
    run_case("02-failed-tool").await;
}
#[tokio::test]
async fn case_timeout() {
    run_case("03-timeout").await;
}
#[tokio::test]
async fn case_cancelled_run() {
    run_case("04-cancelled-run").await;
}
#[tokio::test]
async fn case_restart() {
    run_case("05-restart").await;
}
#[tokio::test]
async fn case_overlapping_output() {
    run_case("06-overlapping-output").await;
}
#[tokio::test]
async fn case_malformed_event() {
    run_case("07-malformed-event").await;
}
#[tokio::test]
async fn case_missing_usage() {
    run_case("08-missing-usage").await;
}
#[tokio::test]
async fn case_approval() {
    run_case("09-approval").await;
}
#[tokio::test]
async fn case_trace_replay() {
    run_case("10-trace-replay").await;
}

// ---------------------------------------------------------------- the scorecard

/// The ten spike runs that have a stream, each fed with its hooks when it has any.
fn recorded_runs() -> Vec<(&'static str, Vec<TraceEvent>)> {
    sup::RUNS
        .iter()
        .map(|name| {
            let feed = if load(name).hooks.is_empty() {
                Feed::Stream
            } else {
                Feed::StreamThenHooks
            };
            (*name, sup::replay(name, feed))
        })
        .collect()
}

fn metric(m: &aibus_trace::Metric) -> Value {
    json!({ "value": m.value, "unit": m.unit, "quality": m.quality, "source": m.source })
}

/// Event-fidelity coverage over every recorded run: which of the spec's 21 kinds the pipeline
/// produced, at which evidence levels, and how every delegation edge is backed.
fn fidelity(runs: &[(&str, Vec<TraceEvent>)]) -> Value {
    let mut by_kind: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let (mut edges, mut observed, mut cross) = (0u64, 0u64, 0u64);
    let mut by_origin: BTreeMap<String, u64> = BTreeMap::new();
    for (_, events) in runs {
        for e in events {
            *by_kind
                .entry(wire(e.kind))
                .or_default()
                .entry(wire(e.visibility))
                .or_insert(0) += 1;
            *by_origin.entry(wire(e.origin)).or_insert(0) += 1;
        }
        for e in &project(events).edges {
            edges += 1;
            observed += u64::from(e.level == Visibility::Observed);
            cross += u64::from(e.cross_checked);
        }
    }
    let produced: Vec<String> = by_kind.keys().cloned().collect();
    let missing: Vec<String> = ALL_KINDS
        .iter()
        .map(wire)
        .filter(|k| !by_kind.contains_key(k))
        .collect();
    json!({
        "spec_kinds": ALL_KINDS.len(),
        "kinds_produced": produced.len(),
        "kinds_not_produced": missing,
        "not_produced_because": "workflow_started, step_scheduled, route_decided, approval_requested, approval_resolved and checkpoint_written belong to the scheduler (#288, #289); artifact_created to the worktree work (#290); no recorded run needed a login (auth_required) or produced a trace_warning; the cases that damage a stream or write stderr produce the warnings",
        "events_by_kind_and_level": by_kind,
        "events_by_origin": by_origin,
        "delegation_edges": { "total": edges, "observed": observed, "cross_checked_by_hooks": cross },
        "stream_event_types_without_a_trace_event": "hook_started, hook_response, task_progress, task_updated and rate_limit_event are counted by the normaliser and carry no trace event of their own",
    })
}

/// Latency and resource use of the recorded spike runs: one run each, on the owner's Mac on
/// 2026-11-14. Not a benchmark: n=1, so no median or p95, and the host is recorded with the data.
fn recorded_latency_and_usage(runs: &[(&str, Vec<TraceEvent>)]) -> (Value, Value) {
    let mut lat = Vec::new();
    let mut usage = Vec::new();
    for (name, events) in runs {
        let r = load(name);
        let p = project(events);
        let metrics: Vec<Value> = p
            .attempts
            .iter()
            .flat_map(|a| a.usage.iter())
            .map(|m| json!({ "name": m.name, "m": metric(m) }))
            .collect();
        let reported_ms = p
            .attempts
            .iter()
            .flat_map(|a| a.usage.iter())
            .find(|m| m.name == "duration_ms")
            .and_then(|m| m.value);
        lat.push(json!({
            "run": name,
            "ending": wire(sup_status(events)),
            "wall_ms": { "value": r.wall.as_millis(), "unit": "ms", "quality": "derived",
                         "source": "spike harness wall clock, spawn to exit or kill" },
            "cli_reported_duration_ms": { "value": reported_ms, "unit": "ms",
                         "quality": if reported_ms.is_some() { "reported" } else { "unknown" },
                         "source": "result.duration_ms of the last result" },
            "samples": 1,
        }));
        usage.push(json!({ "run": name, "attempt_usage": metrics }));
    }
    (
        json!({
            "benchmark": false,
            "host": "owner MacBook (macOS, arm64), the machine #281 ran on",
            "recorded": "2026-11-14, #281",
            "cli_version": "2.1.311",
            "samples_per_run": 1,
            "median_and_p95": "withheld: one sample per run does not support them",
            "runs": lat,
            "supervisor_overhead": { "quality": "unknown", "source": "not measured in this issue; a timing needs the bench queue on Mercury and is not a required gate" },
        }),
        json!({
            "reported_by_the_cli": usage,
            "measured_by_aibus": {
                "cli_processes_per_attempt": { "value": 1, "unit": "processes", "quality": "derived",
                    "source": "spike runs 08 and 15: ps samples showed one claude process for the lead and both children" },
                "events_per_run": "see cases[].events and the fleet view",
                "cpu_and_memory_of_the_cli": { "value": null, "unit": null, "quality": "unknown",
                    "source": "the host does not sample the child's CPU or memory; nothing is claimed" },
            },
            "cost_note": "cost_usd is the CLI's list-price estimate (costBasis list), quality estimated, never a bill; billed cost is unknown",
        }),
    )
}

fn sup_status(events: &[TraceEvent]) -> AttemptStatus {
    project(events)
        .attempts
        .last()
        .and_then(|a| a.status)
        .unwrap_or(AttemptStatus::Interrupted)
}

fn corpus_manifest_ok() {
    let m: Value =
        serde_json::from_slice(&fs::read(reference_dir().join("corpus.json")).unwrap()).unwrap();
    assert_eq!(m["corpus_version"], CORPUS_VERSION);
    let ids: Vec<&str> = m["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids, CASES,
        "corpus.json lists exactly the ten cases, in order"
    );
    for c in m["cases"].as_array().unwrap() {
        if let Some(s) = c["stand_in"].as_str() {
            assert!(reference_dir().join(s).is_file(), "stand-in {s} exists");
        }
    }
}

fn scorecard_value(cases: Vec<Value>) -> Value {
    let runs = recorded_runs();
    let (latency, resources) = recorded_latency_and_usage(&runs);
    json!({
        "scorecard_version": SCORECARD_VERSION,
        "suite": "aibus-reference-285",
        "corpus": { "version": CORPUS_VERSION, "cases": CASES.len(),
                    "manifest": "fixtures/claude-code/reference-285/corpus.json",
                    "pilot_workflow": "fixtures/workflows/graph/repo-review.toml" },
        "mode": "offline (recorded streams and stand-in CLIs; no provider, no network)",
        "cli": { "name": "Claude Code", "recorded_version": cli::TESTED_VERSION.to_string(),
                 "minimum_supported": cli::MIN_VERSION.to_string(),
                 "recorded_on": "2026-11-14" },
        "summary": { "passed": cases.iter().filter(|c| c["result"] == "pass").count(),
                     "of": CASES.len() },
        "cases": cases,
        "event_fidelity": fidelity(&runs),
        "latency": latency,
        "resource_use": resources,
        "claims_not_made": [
            "no deterministic re-execution: a rerun starts a new attempt and may differ",
            "no resume of a vendor session: native children die with their process",
            "no model-level cost: the CLI reports aggregate usage only",
            "no interactive approval through aibus: --permission-prompts host is untested",
        ],
    })
}

fn render(v: &Value) -> String {
    let mut s = serde_json::to_string_pretty(v).unwrap();
    s.push('\n');
    s
}

/// All ten cases, then the scorecard they add up to, which has to be the committed one.
#[tokio::test]
async fn the_reference_suite_produces_the_versioned_scorecard() {
    corpus_manifest_ok();
    let mut cases = Vec::new();
    for id in CASES {
        cases.push(run_case(id).await);
    }
    let card = render(&scorecard_value(cases));
    let path = reference_dir().join("scorecard.json");
    if bless() {
        fs::write(&path, &card).unwrap();
    } else {
        let want = fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            card, want,
            "the scorecard differs from the committed one (AIBUS_BLESS=1 rewrites it)"
        );
    }
    if let Some(dir) = std::env::var_os("AIBUS_REFERENCE_OUT") {
        fs::create_dir_all(&dir).unwrap();
        fs::write(Path::new(&dir).join("scorecard.json"), &card).unwrap();
    }
    // The scorecard itself must not make a claim the issue forbids.
    let lower = card.to_lowercase();
    for banned in [
        "fully deterministic",
        "deterministic replay of the model",
        "resumed the session",
    ] {
        assert!(!lower.contains(banned), "{banned}");
    }
}

fn files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push((
                    p.strip_prefix(dir).unwrap().to_string_lossy().into_owned(),
                    fs::read(&p).unwrap(),
                ));
            }
        }
    }
    out.sort();
    out
}

/// The example trace: the native-delegation run with payloads kept, through the real store and its
/// redactor, then checked for anything that looks like an identity or a secret.
#[test]
fn the_example_trace_is_the_redacted_native_delegation_run() {
    let tmp = tempfile::tempdir().unwrap();
    let got = tmp.path().join("run");
    let (batches, _) = sup::normalise(&load("11-foreground"), Feed::StreamThenHooks);
    let (mut store, _) = TraceStore::open(
        &got,
        Redactor::new(),
        PayloadPolicy::Retain { max_bytes: 1 << 20 },
        StoreLimits::default(),
    )
    .unwrap();
    for b in batches {
        store.append(b).unwrap();
    }
    drop(store);
    let want = reference_dir().join("example-trace");
    if bless() {
        let _ = fs::remove_dir_all(&want);
        fs::create_dir_all(want.parent().unwrap()).unwrap();
        fs::create_dir_all(&want).unwrap();
        for (rel, bytes) in files(&got) {
            let to = want.join(&rel);
            fs::create_dir_all(to.parent().unwrap()).unwrap();
            fs::write(to, bytes).unwrap();
        }
    }
    assert!(
        files(&got) == files(&want),
        "the example trace differs from what the store writes (AIBUS_BLESS=1 rewrites it)"
    );

    let identity = regex::Regex::new(
        r"(?i)([A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,})|(sk-[a-z0-9_-]{8,})|(bearer\s+[a-z0-9._-]{8,})|(/Users/[^o/][^/]*/)|(org-[a-z0-9]{6,})|(eyJ[a-z0-9_-]{10,}\.)",
    )
    .unwrap();
    for (rel, bytes) in files(&want) {
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !identity.is_match(&text),
            "{rel} holds an identity or a secret"
        );
    }
    let (events, report) = TraceStore::read(&want).unwrap();
    assert_eq!(report, Default::default());
    assert!(events
        .iter()
        .all(|e| e.source.redaction_version == aibus_trace::REDACTION_VERSION));
    assert!(
        files(&want)
            .iter()
            .any(|(rel, _)| rel.starts_with("objects/")),
        "payloads are kept, so there is something to redact"
    );
}

// ---------------------------------------------------------------- the optional live mode

/// Live mode: runs the native-delegation case against the CLIs that are installed and signed in on
/// this machine, and writes what it saw to `AIBUS_REFERENCE_OUT`. It is ignored by default and
/// needs `AIBUS_REFERENCE_LIVE=1`; CI never sets it. It never reads a credential: `auth status` is
/// not run, and the CLI uses its own sign-in through the bounded environment.
///
/// `AIBUS_REFERENCE_LIVE_CLIS` (default `claude`) lists CLIs to look at. Only Claude Code has an
/// adapter in Phase 1, so for every other name live mode records its `--version` and says it was
/// not run through the supervisor.
#[tokio::test]
#[ignore = "live: starts the real CLI; set AIBUS_REFERENCE_LIVE=1 and run with --ignored"]
async fn live_reference() {
    if std::env::var_os("AIBUS_REFERENCE_LIVE").is_none() {
        eprintln!("AIBUS_REFERENCE_LIVE is not set; live mode skipped");
        return;
    }
    let out_dir = PathBuf::from(
        std::env::var_os("AIBUS_REFERENCE_OUT")
            .expect("AIBUS_REFERENCE_OUT names the output directory"),
    );
    fs::create_dir_all(&out_dir).unwrap();
    let wanted = std::env::var("AIBUS_REFERENCE_LIVE_CLIS").unwrap_or_else(|_| "claude".into());
    let parent: Vec<(std::ffi::OsString, std::ffi::OsString)> = std::env::vars_os().collect();
    let env = cli::bounded_env(parent.clone(), &[], &[]).unwrap();
    let dirs: Vec<PathBuf> =
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
    let mut report = Vec::new();

    for name in wanted.split(',').map(str::trim).filter(|n| !n.is_empty()) {
        if name != "claude" {
            // Discovery and the probe are Claude Code's; for any other CLI the version string is
            // all live mode records.
            let exe = dirs.iter().map(|d| d.join(name)).find(|p| p.is_file());
            let version = match &exe {
                Some(p) => cli::capture(p, &["--version"], &env, Duration::from_secs(10), 4096)
                    .await
                    .ok()
                    .map(|b| String::from_utf8_lossy(&b).trim().to_owned()),
                None => None,
            };
            report.push(json!({ "cli": name,
                "status": if exe.is_some() { "installed_not_run" } else { "not_on_path" },
                "version": version,
                "why": "no extension for this CLI until #287; live mode records the version only" }));
            continue;
        }
        let found = cli::discover(&dirs, name, &env).await;
        let Some(chosen) = found.chosen.map(|i| &found.candidates[i]) else {
            report.push(json!({ "cli": name, "status": "not_found_or_unsafe",
                "candidates": found.candidates.iter().map(|c| json!({"path": c.path, "verdict": format!("{:?}", c.verdict)})).collect::<Vec<_>>() }));
            continue;
        };
        let exe = chosen
            .resolved
            .clone()
            .unwrap_or_else(|| chosen.path.clone());

        let version_out = cli::capture(&exe, &["--version"], &env, Duration::from_secs(10), 4096)
            .await
            .unwrap();
        let help_out = cli::capture(&exe, &["--help"], &env, Duration::from_secs(10), 1 << 20)
            .await
            .unwrap();
        let probe = match cli::probe(
            &String::from_utf8_lossy(&version_out),
            &String::from_utf8_lossy(&help_out),
        ) {
            Ok(p) => p,
            Err(e) => {
                report
                    .push(json!({ "cli": name, "status": "probe_refused", "why": e.to_string() }));
                continue;
            }
        };
        let agents = fs::read_to_string(sup::spike_dir().join("inputs/agents.json")).unwrap();
        let argv = cli::argv(
            &probe,
            &cli::Request {
                model: probe.flags.contains("--model").then(|| "haiku".into()),
                setting_sources: "project".into(),
                max_budget_usd: probe
                    .flags
                    .contains("--max-budget-usd")
                    .then(|| "0.30".into()),
                include_hook_events: probe.flags.contains("--include-hook-events"),
                forward_subagent_text: probe.flags.contains("--forward-subagent-text"),
                agents_json: Some(agents),
            },
        )
        .unwrap();

        // A scratch repository the lead reviews: two small files, one subagent for each.
        let work = out_dir.join("live-work");
        fs::create_dir_all(&work).unwrap();
        fs::write(work.join("alpha.txt"), "ALPHA-7731\n").unwrap();
        fs::write(work.join("beta.txt"), "BETA-4410\n").unwrap();
        let prompt = "Use the Agent tool to launch the reader-a subagent and the reader-b subagent, both in the same message, each with run_in_background set to false so you wait for their results. Do not read any file yourself. When both have returned, reply with exactly one line: the text reader-a returned, then \" + \", then the text reader-b returned.";
        let trace_dir = out_dir.join("live-trace");
        let _ = fs::remove_dir_all(&trace_dir);
        let (mut store, _) = TraceStore::open(
            &trace_dir,
            Redactor::new(),
            PayloadPolicy::MetadataOnly,
            StoreLimits::default(),
        )
        .unwrap();
        let mut run_env = parent.clone();
        run_env.retain(|(k, _)| k != "PATH");
        let launch = Launch {
            exe: exe.clone(),
            args: argv,
            cwd: work.canonicalize().unwrap(),
            env: cli::bounded_env(run_env, &[], &[]).unwrap(),
            stdin: prompt.as_bytes().to_vec(),
            limits: Limits {
                wall_time: Duration::from_secs(180),
                ..Limits::default()
            },
        };
        let mut norm = Normalizer::new(attempt_ref(9));
        let started = Instant::now();
        let outcome = supervise(launch, &mut norm, &mut store, CancellationToken::new())
            .await
            .unwrap();
        let wall = started.elapsed();
        drop(store);
        let events = TraceStore::read(&trace_dir).unwrap().0;
        let mut c = Checks::default();
        let verdict = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_native_delegation(&events, &mut c);
        }));
        let p = project(&events);
        report.push(json!({
            "cli": name,
            "status": if verdict.is_ok() { "pass" } else { "fail" },
            "version": probe.version.to_string(),
            "tested_version": probe.tested,
            "ending": format!("{:?}", outcome.ending),
            "attempt_status": wire(outcome.status),
            "checks_passed": c.0,
            "wall_ms": { "value": wall.as_millis(), "unit": "ms", "quality": "derived",
                         "source": "tokio Instant around supervise, this run, n=1, on the host in this row (not a benchmark)" },
            "events": events.len(),
            "evidence_levels": levels(&events),
            "edges": p.edges.iter().map(|e| json!({"level": wire(e.level), "cross_checked": e.cross_checked})).collect::<Vec<_>>(),
            "usage": p.attempts.iter().flat_map(|a| a.usage.iter()).map(metric).collect::<Vec<_>>(),
        }));
    }
    fs::write(
        out_dir.join("live-report.json"),
        render(&json!({ "live": report })),
    )
    .unwrap();
    eprintln!("{}", render(&json!({ "live": report })));
    assert!(
        report.iter().all(|r| r["status"] != "fail"),
        "a live case failed; see live-report.json"
    );
}
