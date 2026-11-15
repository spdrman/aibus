//! The process host against stand-in CLIs (small shell scripts launched by absolute path, the way
//! the real one is): exit versus the first `result`, cancel and kill, signals, the bounded
//! environment, the prompt on stdin, stderr, and hostile output (huge lines, invalid UTF-8, streams
//! that never end, too much output). No test here runs the real Claude Code CLI.
#![cfg(unix)]

mod common;

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aibus_core::ids::AccountName;
use aibus_core::Redactor;
use aibus_supervisor::{cli, supervise, Ending, Launch, Limits, Normalizer};
use aibus_trace::{
    project, AttemptStatus, EventKind, PayloadPolicy, StoreLimits, TraceEvent, TraceStore,
};
use tokio_util::sync::CancellationToken;

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

    /// A stand-in CLI: `body` is a POSIX shell script.
    fn cli(&self, body: &str) -> PathBuf {
        let p = self.root.join("bin/fake-cli");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn launch(&self, exe: PathBuf, limits: Limits) -> Launch {
        Launch {
            exe,
            args: vec!["-p".into(), "--output-format".into(), "stream-json".into()],
            cwd: self.root.join("work"),
            env: cli::bounded_env(Vec::<(OsString, OsString)>::new(), &[], &[]).unwrap(),
            stdin: b"the prompt".to_vec(),
            limits,
        }
    }

    async fn run(
        &self,
        launch: Launch,
        policy: PayloadPolicy,
        redactor: Redactor,
        cancel: CancellationToken,
    ) -> (aibus_supervisor::Outcome, Vec<TraceEvent>) {
        let dir = self.root.join("trace");
        let (mut store, _) =
            TraceStore::open(&dir, redactor, policy, StoreLimits::default()).unwrap();
        let mut norm = Normalizer::new(common::attempt());
        // A regression that makes the host hang fails in seconds instead of at the job timeout.
        let outcome = tokio::time::timeout(
            Duration::from_secs(30),
            supervise(launch, &mut norm, &mut store, cancel),
        )
        .await
        .expect("supervise did not return within 30 s")
        .unwrap();
        drop(store);
        (outcome, TraceStore::read(&dir).unwrap().0)
    }

    async fn plain(
        &self,
        body: &str,
        limits: Limits,
    ) -> (aibus_supervisor::Outcome, Vec<TraceEvent>) {
        let exe = self.cli(body);
        self.run(
            self.launch(exe, limits),
            PayloadPolicy::MetadataOnly,
            Redactor::new(),
            CancellationToken::new(),
        )
        .await
    }

    fn trace_dir(&self) -> PathBuf {
        self.root.join("trace")
    }
}

fn fixture(name: &str) -> String {
    common::spike_dir()
        .join(format!("{name}.stdout.jsonl"))
        .display()
        .to_string()
}

fn last(events: &[TraceEvent]) -> &TraceEvent {
    events.last().unwrap()
}

fn grep_tree(dir: &Path, needle: &str) -> Vec<String> {
    let mut hits = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            hits.extend(grep_tree(&p, needle));
        } else if String::from_utf8_lossy(&fs::read(&p).unwrap()).contains(needle) {
            hits.push(p.display().to_string());
        }
    }
    hits
}

#[tokio::test]
async fn a_stand_in_replaying_the_foreground_run_ends_completed_with_the_full_graph() {
    let rig = Rig::new();
    let (out, events) = rig
        .plain(
            &format!("cat '{}'", fixture("11-foreground")),
            Limits::default(),
        )
        .await;
    assert_eq!(
        (out.ending, out.status),
        (Ending::Exited(0), AttemptStatus::Completed)
    );
    assert_eq!(events.first().unwrap().kind, EventKind::AttemptStarted);
    assert_eq!(last(&events).kind, EventKind::AttemptCompleted);
    let p = project(&events);
    assert_eq!((p.agents.len(), p.edges.len(), p.tools.len()), (3, 2, 2));
}

#[tokio::test]
async fn the_attempt_ends_at_exit_not_at_the_first_result() {
    let rig = Rig::new();
    // Three `result`s, then the process lingers, then prints one more line and exits.
    let body = format!(
        "cat '{}'\nsleep 1\necho '{{\"type\":\"system\",\"subtype\":\"status\",\"session_id\":\"late\"}}'\nexit 0",
        fixture("07-deleg")
    );
    let started = Instant::now();
    let (out, events) = rig.plain(&body, Limits::default()).await;
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "the host waited for the process"
    );
    assert_eq!(out.status, AttemptStatus::Completed);
    let end = events
        .iter()
        .position(|e| e.kind == EventKind::AttemptCompleted)
        .unwrap();
    assert_eq!(end, events.len() - 1);
    assert_eq!(last(&events).data["results_seen"], 3);
}

#[tokio::test]
async fn cancel_asks_politely_first_and_the_attempt_is_cancelled() {
    let rig = Rig::new();
    let marker = rig.root.join("got-term");
    let body = format!(
        "trap 'touch {}; exit 143' TERM\nhead -n 3 '{}'\nwhile :; do sleep 0.05; done",
        marker.display(),
        fixture("11-foreground")
    );
    let exe = rig.cli(&body);
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        c.cancel();
    });
    let (out, events) = rig
        .run(
            rig.launch(exe, Limits::default()),
            PayloadPolicy::MetadataOnly,
            Redactor::new(),
            cancel,
        )
        .await;
    assert_eq!(
        (out.ending, out.status),
        (Ending::Cancelled, AttemptStatus::Cancelled)
    );
    assert!(marker.exists(), "SIGTERM came before any kill");
    assert_eq!(last(&events).kind, EventKind::AttemptFailed);
    assert_eq!(last(&events).data["status"], "cancelled");
}

#[tokio::test]
async fn a_cli_that_ignores_term_is_killed_with_its_whole_process_group() {
    let rig = Rig::new();
    let pidfile = rig.root.join("grandchild.pid");
    let body = format!(
        "trap '' TERM\nsleep 300 &\necho $! > {}\nwhile :; do sleep 0.05; done",
        pidfile.display()
    );
    let exe = rig.cli(&body);
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        c.cancel();
    });
    let limits = Limits {
        grace: Duration::from_millis(300),
        ..Limits::default()
    };
    let started = Instant::now();
    let (out, _) = rig
        .run(
            rig.launch(exe, limits),
            PayloadPolicy::MetadataOnly,
            Redactor::new(),
            cancel,
        )
        .await;
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(out.status, AttemptStatus::Cancelled);
    let pid = fs::read_to_string(&pidfile).unwrap();
    let alive = std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .status()
        .unwrap()
        .success();
    assert!(!alive, "the grandchild {pid} outlived the kill");
}

#[tokio::test]
async fn a_cli_killed_by_someone_else_is_interrupted_never_completed() {
    let rig = Rig::new();
    let (out, events) = rig
        .plain(
            &format!("cat '{}'\nkill -9 $$", fixture("10-killed")),
            Limits::default(),
        )
        .await;
    assert_eq!(
        (out.ending, out.status),
        (Ending::Signalled(9), AttemptStatus::Interrupted)
    );
    assert!(!events.iter().any(|e| e.kind == EventKind::AttemptCompleted));
    assert_eq!(last(&events).data["status"], "interrupted");
}

#[tokio::test]
async fn a_nonzero_exit_is_failed() {
    let rig = Rig::new();
    let (out, events) = rig
        .plain(
            &format!("cat '{}'\nexit 3", fixture("06-basic")),
            Limits::default(),
        )
        .await;
    assert_eq!(
        (out.ending, out.status),
        (Ending::Exited(3), AttemptStatus::Failed)
    );
    assert_eq!(last(&events).data["exit_code"], 3);
}

#[tokio::test]
async fn wall_time_interrupts_a_stream_that_never_ends() {
    let rig = Rig::new();
    let limits = Limits {
        wall_time: Duration::from_millis(800),
        grace: Duration::from_millis(200),
        lines_per_second: 5_000,
        ..Limits::default()
    };
    let started = Instant::now();
    let (out, events) = rig
        .plain(
            r#"yes '{"type":"system","subtype":"status","session_id":"s"}'"#,
            limits,
        )
        .await;
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(
        (out.ending, out.status),
        (Ending::WallTime, AttemptStatus::Interrupted)
    );
    assert!(
        events.len() < 50,
        "known, unprojected lines leave no event each: {}",
        events.len()
    );
}

#[tokio::test]
async fn output_past_the_byte_limit_stops_the_attempt() {
    let rig = Rig::new();
    let limits = Limits {
        max_stdout_bytes: 256 * 1024,
        lines_per_second: 1_000_000,
        ..Limits::default()
    };
    let (out, events) = rig
        .plain(
            r#"yes '{"type":"system","subtype":"status","session_id":"s"}'"#,
            limits,
        )
        .await;
    assert_eq!(
        (out.ending, out.status),
        (Ending::OutputLimit, AttemptStatus::Interrupted)
    );
    assert!(events
        .iter()
        .any(|e| e.kind == EventKind::GapDetected && e.data["reason"] == "output_limit"));
}

#[tokio::test]
async fn a_huge_line_without_newline_is_a_gap_and_the_next_line_still_counts() {
    let rig = Rig::new();
    let limits = Limits {
        max_line_bytes: 64 * 1024,
        ..Limits::default()
    };
    let body = format!(
        "head -c 20000000 /dev/zero | tr '\\000' 'a'\nprintf '\\n'\ncat '{}'",
        fixture("06-basic")
    );
    let (out, events) = rig.plain(&body, limits).await;
    assert_eq!(out.status, AttemptStatus::Completed);
    let gap = events
        .iter()
        .find(|e| e.kind == EventKind::GapDetected && e.data["reason"] == "line_too_long")
        .expect("a gap");
    assert_eq!(gap.data["bytes"], 20_000_000);
    assert!(
        fs::metadata(rig.trace_dir().join("events.jsonl"))
            .unwrap()
            .len()
            < 1_000_000,
        "the line itself is nowhere in the trace"
    );
}

#[tokio::test]
async fn invalid_utf8_and_broken_json_are_gaps_and_do_not_end_the_attempt() {
    let rig = Rig::new();
    let body = format!(
        "printf '\\377\\376{{\"type\":\"x\"}}\\n'\nprintf '{{\"type\":\\n'\ncat '{}'",
        fixture("06-basic")
    );
    let (out, events) = rig.plain(&body, Limits::default()).await;
    assert_eq!(out.status, AttemptStatus::Completed);
    let reasons: Vec<_> = events
        .iter()
        .filter(|e| e.kind == EventKind::GapDetected)
        .map(|e| e.data["reason"].as_str().unwrap().to_owned())
        .collect();
    assert!(reasons.contains(&"invalid_utf8".to_string()), "{reasons:?}");
    assert!(
        reasons.contains(&"malformed_json".to_string()),
        "{reasons:?}"
    );
}

#[tokio::test]
async fn the_child_environment_is_exactly_the_bounded_one() {
    let rig = Rig::new();
    let out_file = rig.root.join("env.txt");
    let exe = rig.cli(&format!("env > {}", out_file.display()));
    let parent: Vec<(OsString, OsString)> = [
        ("HOME", "/home/u"),
        ("CLAUDE_CONFIG_DIR", "/home/u/.claude-b"),
        ("CLAUDECODE", "1"),
        ("CLAUDE_CODE_ENTRYPOINT", "cli"),
        (
            "ANTHROPIC_API_KEY",
            "sk-ant-api03-PARENTKEYPARENTKEYPARENTKEY",
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect();
    let mut launch = rig.launch(exe, Limits::default());
    launch.env = cli::bounded_env(parent, &[], &[]).unwrap();
    rig.run(
        launch,
        PayloadPolicy::MetadataOnly,
        Redactor::new(),
        CancellationToken::new(),
    )
    .await;
    let env = fs::read_to_string(&out_file).unwrap();
    let names: Vec<&str> = env
        .lines()
        .filter_map(|l| l.split('=').next())
        .filter(|n| !["PWD", "SHLVL", "_", "OLDPWD"].contains(n))
        .collect();
    let mut names = names;
    names.sort();
    assert_eq!(names, ["CLAUDE_CONFIG_DIR", "HOME", "PATH"], "{env}");
    assert!(!env.contains("PARENTKEY") && !env.contains("CLAUDECODE"));
}

#[tokio::test]
async fn the_prompt_goes_to_stdin_and_never_to_argv() {
    let rig = Rig::new();
    let (stdin_file, argv_file) = (rig.root.join("stdin.txt"), rig.root.join("argv.txt"));
    let exe = rig.cli(&format!(
        "cat > {}\necho \"$@\" > {}",
        stdin_file.display(),
        argv_file.display()
    ));
    let mut launch = rig.launch(exe, Limits::default());
    launch.stdin = b"secret plan: rename everything".to_vec();
    rig.run(
        launch,
        PayloadPolicy::MetadataOnly,
        Redactor::new(),
        CancellationToken::new(),
    )
    .await;
    assert_eq!(
        fs::read_to_string(stdin_file).unwrap(),
        "secret plan: rename everything"
    );
    assert!(!fs::read_to_string(argv_file)
        .unwrap()
        .contains("secret plan"));
}

#[tokio::test]
async fn a_login_prompt_on_stderr_is_auth_required_and_its_text_is_never_stored() {
    let rig = Rig::new();
    let body = "echo 'Not logged in. Please run /login or paste your API key: ' >&2\necho 'warning: something mundane' >&2\nexit 1";
    let exe = rig.cli(body);
    // Even with payload retention on, the prompt's text stays out.
    let (out, events) = rig
        .run(
            rig.launch(exe, Limits::default()),
            PayloadPolicy::Retain { max_bytes: 1 << 20 },
            Redactor::new(),
            CancellationToken::new(),
        )
        .await;
    assert_eq!(out.status, AttemptStatus::Failed);
    assert!(events.iter().any(|e| e.kind == EventKind::AuthRequired));
    assert!(grep_tree(&rig.trace_dir(), "paste your API key").is_empty());
    assert!(grep_tree(&rig.trace_dir(), "Not logged in").is_empty());
    let warn = events
        .iter()
        .find(|e| e.kind == EventKind::TraceWarning && e.data["channel"] == "stderr")
        .expect("the mundane line is a warning");
    assert!(
        warn.payload_ref.is_some(),
        "and its text is a retained payload"
    );
}

#[tokio::test]
async fn an_auth_error_result_is_auth_required_without_its_text() {
    let rig = Rig::new();
    let body = r#"echo '{"type":"result","subtype":"success","is_error":true,"result":"Invalid API key · Please run /login","session_id":"s","num_turns":0}'"#;
    let exe = rig.cli(body);
    let (_, events) = rig
        .run(
            rig.launch(exe, Limits::default()),
            PayloadPolicy::Retain { max_bytes: 1 << 20 },
            Redactor::new(),
            CancellationToken::new(),
        )
        .await;
    assert!(events.iter().any(|e| e.kind == EventKind::AuthRequired));
    assert!(grep_tree(&rig.trace_dir(), "Invalid API key").is_empty());
    assert_eq!(last(&events).data["status"], "failed");
}

#[tokio::test]
async fn a_relative_executable_or_a_missing_directory_is_refused_before_anything_runs() {
    let rig = Rig::new();
    let exe = rig.cli("touch ran");
    let dir = rig.trace_dir();
    let (mut store, _) = TraceStore::open(
        &dir,
        Redactor::new(),
        PayloadPolicy::MetadataOnly,
        StoreLimits::default(),
    )
    .unwrap();
    let mut norm = Normalizer::new(common::attempt());
    let mut relative = rig.launch(PathBuf::from("bin/fake-cli"), Limits::default());
    relative.cwd = rig.root.clone();
    assert!(matches!(
        supervise(relative, &mut norm, &mut store, CancellationToken::new()).await,
        Err(aibus_supervisor::SupervisorError::Refused(_))
    ));
    let mut nowhere = rig.launch(exe, Limits::default());
    nowhere.cwd = rig.root.join("missing");
    assert!(matches!(
        supervise(nowhere, &mut norm, &mut store, CancellationToken::new()).await,
        Err(aibus_supervisor::SupervisorError::Refused(_))
    ));
    assert!(!rig.root.join("ran").exists() && !rig.root.join("work/ran").exists());
}

#[tokio::test]
async fn retained_payloads_from_a_live_run_are_redacted_on_disk() {
    let rig = Rig::new();
    let canary = "sk-ant-api03-LIVECANARYLIVECANARYLIVECANARY";
    let registered = "registered-live-canary-42";
    let line = format!(
        r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{canary} and {registered}"}}]}},"session_id":"s","parent_tool_use_id":null}}"#
    );
    let exe = rig.cli(&format!("echo '{line}'\ncat '{}'", fixture("06-basic")));
    let redactor = Redactor::new();
    redactor.register(&AccountName("work".into()), registered);
    rig.run(
        rig.launch(exe, Limits::default()),
        PayloadPolicy::Retain { max_bytes: 1 << 20 },
        redactor,
        CancellationToken::new(),
    )
    .await;
    assert!(grep_tree(&rig.trace_dir(), canary).is_empty());
    assert!(grep_tree(&rig.trace_dir(), registered).is_empty());
}

fn alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .status()
        .unwrap()
        .success()
}

#[tokio::test]
async fn a_descendant_that_closed_its_pipes_is_killed_when_the_cli_exits_normally() {
    let rig = Rig::new();
    let pidfile = rig.root.join("sleeper.pid");
    let body = format!(
        "sleep 300 </dev/null >/dev/null 2>&1 &\necho $! > {}\nexit 0",
        pidfile.display()
    );
    let (out, _) = rig.plain(&body, Limits::default()).await;
    assert_eq!(out.ending, Ending::Exited(0));
    let pid = fs::read_to_string(&pidfile).unwrap();
    // The kill is a signal, so give the reaping a moment.
    let until = Instant::now() + Duration::from_secs(5);
    while alive(&pid) && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!alive(&pid), "the sleeper {pid} outlived the attempt");
}

#[tokio::test]
async fn a_store_error_mid_run_still_ends_the_cli_and_its_children() {
    let rig = Rig::new();
    let pidfile = rig.root.join("child.pid");
    let body = format!(
        "sleep 300 &\necho $! > {}\necho '{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"hi\"}}]}},\"session_id\":\"s\",\"parent_tool_use_id\":null}}'\nwait",
        pidfile.display()
    );
    let exe = rig.cli(&body);
    let (mut store, _) = TraceStore::open(
        &rig.trace_dir(),
        Redactor::new(),
        PayloadPolicy::Retain { max_bytes: 1 << 20 },
        StoreLimits::default(),
    )
    .unwrap();
    // A file where the payload directory must go: the first retained payload fails the append.
    fs::write(rig.trace_dir().join("objects"), b"x").unwrap();
    let mut norm = Normalizer::new(common::attempt());
    let r = tokio::time::timeout(
        Duration::from_secs(30),
        supervise(
            rig.launch(exe, Limits::default()),
            &mut norm,
            &mut store,
            CancellationToken::new(),
        ),
    )
    .await
    .expect("supervise did not return within 30 s");
    assert!(r.is_err(), "the store error should surface");
    let pid = fs::read_to_string(&pidfile).unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    while alive(&pid) && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!alive(&pid), "the child {pid} was orphaned");
}

// ---- #324: output still queued when the CLI exits is read, or a gap says it was not ----

/// A stand-in that writes `n` distinct stderr lines at once and exits. The host reads them at
/// `lines_per_second`, so most are still queued in the pipe when the CLI is gone.
fn flood_body(n: u32) -> String {
    format!("i=0; while [ $i -lt {n} ]; do echo \"queued line $i\" >&2; i=$((i+1)); done")
}

fn drain_limits(lines_per_second: u32, stall_ms: u64, max_ms: u64) -> Limits {
    Limits {
        lines_per_second,
        drain_stall: Duration::from_millis(stall_ms),
        drain_max: Duration::from_millis(max_ms),
        ..Limits::default()
    }
}

fn on_channel<'a>(events: &'a [TraceEvent], kind: EventKind, ch: &str) -> Vec<&'a TraceEvent> {
    events
        .iter()
        .filter(|e| e.kind == kind && e.data["channel"] == ch)
        .collect()
}

#[tokio::test]
async fn issue_324_output_queued_at_exit_is_read_to_the_end() {
    let rig = Rig::new();
    let started = Instant::now();
    let (out, events) = rig
        .plain(&flood_body(600), drain_limits(200, 1_500, 20_000))
        .await;
    assert_eq!(out.ending, Ending::Exited(0));
    let read = on_channel(&events, EventKind::TraceWarning, "stderr").len();
    assert_eq!(read, 600, "only {read} of 600 queued lines were read");
    assert!(started.elapsed() < Duration::from_secs(15));
}

#[tokio::test]
async fn issue_324_a_flood_after_exit_hits_the_bound_and_says_so() {
    let rig = Rig::new();
    let started = Instant::now();
    let (out, events) = rig
        .plain(&flood_body(3_000), drain_limits(20, 400, 1_000))
        .await;
    assert_eq!(out.ending, Ending::Exited(0));
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "the drain after exit has to end at its bound, took {:?}",
        started.elapsed()
    );
    let read = on_channel(&events, EventKind::TraceWarning, "stderr").len();
    assert!(read < 3_000, "the bound was never reached ({read} read)");
    let gaps: Vec<_> = events
        .iter()
        .filter(|e| e.kind == EventKind::GapDetected && e.data["reason"] == "unread_after_exit")
        .collect();
    assert_eq!(gaps.len(), 1, "one gap says output was left unread");
    let d = &gaps[0].data;
    assert_eq!(d["bound"], "drain_max", "{d}");
    assert!(d["lines"].as_u64().unwrap() > 0, "lines were counted: {d}");
    assert!(d["bytes"].as_u64().unwrap() > 0, "bytes were counted: {d}");
}

#[tokio::test]
async fn issue_324_a_cli_that_exits_mid_line_still_has_its_last_line_read() {
    let rig = Rig::new();
    let body = format!("{}\nprintf 'cut off by exit' >&2", flood_body(500));
    let (out, events) = rig.plain(&body, drain_limits(200, 1_500, 20_000)).await;
    assert_eq!(out.ending, Ending::Exited(0));
    let warnings = on_channel(&events, EventKind::TraceWarning, "stderr");
    assert_eq!(warnings.len(), 501, "{} of 501 lines read", warnings.len());
}

#[tokio::test]
async fn issue_324_overlapping_output_on_both_pipes_at_exit_is_all_read() {
    let rig = Rig::new();
    let body =
        "i=0; while [ $i -lt 300 ]; do echo \"err $i\" >&2; echo \"not json $i\"; i=$((i+1)); done";
    let (_, events) = rig.plain(body, drain_limits(200, 1_500, 20_000)).await;
    let err = on_channel(&events, EventKind::TraceWarning, "stderr").len();
    let out = on_channel(&events, EventKind::GapDetected, "stdout").len();
    assert_eq!((err, out), (300, 300), "stderr and stdout lines read");
}

#[tokio::test]
async fn issue_324_a_descendant_holding_the_pipes_is_still_cut_off_quickly() {
    let rig = Rig::new();
    let started = Instant::now();
    let (out, events) = rig
        .plain(
            "sleep 30 &\necho hello >&2",
            drain_limits(2_000, 500, 5_000),
        )
        .await;
    assert_eq!(out.ending, Ending::Exited(0));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(
        !events
            .iter()
            .any(|e| e.data["reason"] == "unread_after_exit"),
        "nothing was queued, so there is nothing to report"
    );
}

const DRIP: &str = "( while :; do echo drip >&2; sleep 0.2; done ) &";

fn unread_gaps(events: &[TraceEvent]) -> Vec<&TraceEvent> {
    events
        .iter()
        .filter(|e| e.kind == EventKind::GapDetected && e.data["reason"] == "unread_after_exit")
        .collect()
}

#[tokio::test]
async fn issue_324_a_slow_drip_after_exit_ends_at_drain_max_with_a_gap() {
    let rig = Rig::new();
    let started = Instant::now();
    let (out, events) = rig.plain(DRIP, drain_limits(2_000, 500, 2_000)).await;
    assert_eq!(out.ending, Ending::Exited(0));
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(1_900),
        "ended early: {took:?}"
    );
    assert!(took < Duration::from_secs(6), "ended late: {took:?}");
    let gaps = unread_gaps(&events);
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].data["bound"], "drain_max");
}

#[tokio::test]
async fn issue_324_cancel_during_the_drain_ends_it_as_cancelled() {
    let rig = Rig::new();
    let exe = rig.cli(DRIP);
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        c.cancel();
    });
    let started = Instant::now();
    let (out, _) = rig
        .run(
            rig.launch(exe, drain_limits(2_000, 5_000, 20_000)),
            PayloadPolicy::MetadataOnly,
            Redactor::new(),
            cancel,
        )
        .await;
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(out.ending, Ending::Cancelled);
    assert_eq!(out.status, AttemptStatus::Cancelled);
}

#[tokio::test]
async fn issue_324_cancel_during_the_drain_records_what_was_left_unread() {
    let rig = Rig::new();
    let pidfile = rig.root.join("cli.pid");
    let exe = rig.cli(&format!(
        "echo $$ > {}\n{}",
        pidfile.display(),
        flood_body(3_000)
    ));
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    // Cancel only once the CLI process is gone, so the cancel lands in the drain.
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let Ok(pid) = fs::read_to_string(&pidfile) else {
                continue;
            };
            let gone = !std::process::Command::new("kill")
                .args(["-0", pid.trim()])
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success();
            if gone {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        c.cancel();
    });
    let started = Instant::now();
    let (out, events) = rig
        .run(
            rig.launch(exe, drain_limits(20, 5_000, 20_000)),
            PayloadPolicy::MetadataOnly,
            Redactor::new(),
            cancel,
        )
        .await;
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(out.ending, Ending::Cancelled);
    let gaps = unread_gaps(&events);
    assert_eq!(gaps.len(), 1, "pending lines are a gap");
    assert!(gaps[0].data["lines"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn issue_324_the_wall_deadline_during_the_drain_ends_it_as_wall_time() {
    let rig = Rig::new();
    let limits = Limits {
        wall_time: Duration::from_secs(1),
        ..drain_limits(2_000, 5_000, 20_000)
    };
    let started = Instant::now();
    let (out, _) = rig.plain(DRIP, limits).await;
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(out.ending, Ending::WallTime);
}

/// A CLI that stops on SIGTERM while a descendant that ignores it keeps writing: the drain after
/// the exit has to end at the grace period, not at `drain_max`.
const STOPPABLE_WITH_DRIP: &str =
    "( trap '' TERM; while :; do echo drip >&2; sleep 0.2; done ) &\n\
     echo xxxxxxxxxxxxxxxxxxxxxxxx\ntrap 'exit 0' TERM\nwhile :; do sleep 0.1; done";

fn stopped_limits() -> Limits {
    Limits {
        grace: Duration::from_millis(700),
        max_stdout_bytes: 10_000,
        ..drain_limits(2_000, 10_000, 20_000)
    }
}

async fn stopped_before_exit(
    limits: Limits,
    cancel_at: Option<u64>,
) -> (Ending, Duration, Vec<String>) {
    let rig = Rig::new();
    let exe = rig.cli(STOPPABLE_WITH_DRIP);
    let cancel = CancellationToken::new();
    if let Some(ms) = cancel_at {
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            c.cancel();
        });
    }
    let started = Instant::now();
    let (out, events) = rig
        .run(
            rig.launch(exe, limits),
            PayloadPolicy::MetadataOnly,
            Redactor::new(),
            cancel,
        )
        .await;
    let bounds = unread_gaps(&events)
        .iter()
        .map(|g| g.data["bound"].as_str().unwrap_or_default().to_string())
        .collect();
    (out.ending, started.elapsed(), bounds)
}

#[tokio::test]
async fn issue_324_cancel_before_exit_drains_only_for_the_grace_period() {
    let (ending, took, gaps) = stopped_before_exit(stopped_limits(), Some(400)).await;
    assert_eq!(ending, Ending::Cancelled);
    assert!(took < Duration::from_secs(4), "{took:?}");
    assert_eq!(gaps, ["grace"], "the grace cap ended the drain (#329)");
}

#[tokio::test]
async fn issue_324_wall_time_before_exit_drains_only_for_the_grace_period() {
    let limits = Limits {
        wall_time: Duration::from_millis(400),
        ..stopped_limits()
    };
    let (ending, took, gaps) = stopped_before_exit(limits, None).await;
    assert_eq!(ending, Ending::WallTime);
    assert!(took < Duration::from_secs(4), "{took:?}");
    assert_eq!(gaps, ["grace"], "the grace cap ended the drain (#329)");
}

#[tokio::test]
async fn issue_324_output_limit_before_exit_drains_only_for_the_grace_period() {
    let limits = Limits {
        max_stdout_bytes: 10,
        ..stopped_limits()
    };
    let (ending, took, gaps) = stopped_before_exit(limits, None).await;
    assert_eq!(ending, Ending::OutputLimit);
    assert!(took < Duration::from_secs(4), "{took:?}");
    assert_eq!(gaps, ["grace"], "the grace cap ended the drain (#329)");
}

// ---- #327: stderr past its byte cap is a gap, not a silent drop ----

#[tokio::test]
async fn issue_327_a_stderr_flood_past_the_cap_records_a_gap_with_the_dropped_bytes() {
    let rig = Rig::new();
    let limits = Limits {
        max_stderr_bytes: 2_000,
        ..drain_limits(2_000, 1_000, 10_000)
    };
    let (out, events) = rig.plain(&flood_body(600), limits).await;
    assert_eq!(out.ending, Ending::Exited(0));
    let gaps: Vec<_> = on_channel(&events, EventKind::GapDetected, "stderr")
        .into_iter()
        .filter(|e| e.data["reason"] == "stderr_limit")
        .collect();
    assert_eq!(gaps.len(), 1, "one stderr gap says output was dropped");
    let dropped = gaps[0].data["bytes"].as_u64().unwrap();
    // 600 lines of at least 13 bytes each; the head up to the 2000-byte cap is kept, the rest is
    // dropped, so kept + dropped is everything written.
    assert!(
        dropped > 5_000,
        "the dropped byte count is recorded: {dropped}"
    );
    let kept = on_channel(&events, EventKind::TraceWarning, "stderr").len();
    assert!(
        (1..600).contains(&kept),
        "the head up to the cap is kept, the rest is not ({kept} kept)"
    );
}

#[tokio::test]
async fn issue_327_stderr_under_the_cap_records_no_gap() {
    let rig = Rig::new();
    let limits = Limits {
        max_stderr_bytes: 1_000_000,
        ..drain_limits(2_000, 1_000, 10_000)
    };
    let (_, events) = rig.plain(&flood_body(50), limits).await;
    assert!(on_channel(&events, EventKind::GapDetected, "stderr").is_empty());
}

fn stderr_gaps(events: &[TraceEvent]) -> Vec<&TraceEvent> {
    on_channel(events, EventKind::GapDetected, "stderr")
        .into_iter()
        .filter(|e| e.data["reason"] == "stderr_limit")
        .collect()
}

fn stderr_cap(bytes: u64) -> Limits {
    Limits {
        max_stderr_bytes: bytes,
        ..drain_limits(2_000, 1_000, 10_000)
    }
}

/// `n` bytes of `a` and no newline.
fn raw(n: u32) -> String {
    format!("head -c {n} /dev/zero | tr '\\0' a >&2")
}

#[tokio::test]
async fn issue_327_exactly_at_the_cap_is_no_gap_and_one_past_it_is_one_byte() {
    let rig = Rig::new();
    let body = format!("{}; echo >&2", raw(999));
    let (_, events) = rig.plain(&body, stderr_cap(1_000)).await;
    assert!(stderr_gaps(&events).is_empty(), "1000 bytes at a 1000 cap");
    assert_eq!(
        on_channel(&events, EventKind::TraceWarning, "stderr").len(),
        1
    );

    let body = format!("{}; echo >&2", raw(1_000));
    let (_, events) = rig.plain(&body, stderr_cap(1_000)).await;
    let gaps = stderr_gaps(&events);
    assert_eq!(gaps.len(), 1, "1001 bytes at a 1000 cap");
    assert_eq!(gaps[0].data["bytes"], 1, "only the byte past the cap");
}

#[tokio::test]
async fn issue_327_a_drop_spread_over_two_reads_is_counted_in_full() {
    let rig = Rig::new();
    let body = format!("{}; sleep 0.4; {}; echo >&2", raw(600), raw(600));
    let (_, events) = rig.plain(&body, stderr_cap(1_000)).await;
    let gaps = stderr_gaps(&events);
    assert_eq!(gaps.len(), 1);
    // 1201 bytes written, 1000 kept: 201 dropped across the second read and the newline.
    assert_eq!(gaps[0].data["bytes"], 201, "{}", gaps[0].data);
}

#[tokio::test]
async fn issue_327_a_drain_cut_off_while_a_descendant_holds_stderr_still_says_so() {
    let rig = Rig::new();
    let body = format!("( {}; sleep 30 ) &", raw(3_000));
    let started = Instant::now();
    let (_, events) = rig.plain(&body, stderr_cap(1_000)).await;
    assert!(started.elapsed() < Duration::from_secs(10));
    let gaps = stderr_gaps(&events);
    assert_eq!(
        gaps.len(),
        1,
        "the pipe never closed, the gap is still written"
    );
    assert_eq!(gaps[0].data["bytes"], 2_000, "{}", gaps[0].data);
}

#[tokio::test]
async fn issue_327_output_limit_stderr_cap_and_grace_cut_all_leave_their_gap() {
    let limits = Limits {
        max_stdout_bytes: 10,
        max_stderr_bytes: 10,
        ..stopped_limits()
    };
    let rig = Rig::new();
    let exe = rig.cli(STOPPABLE_WITH_DRIP);
    let (out, events) = rig
        .run(
            rig.launch(exe, limits),
            PayloadPolicy::MetadataOnly,
            Redactor::new(),
            CancellationToken::new(),
        )
        .await;
    assert_eq!(out.ending, Ending::OutputLimit);
    assert!(events.iter().any(|e| e.data["reason"] == "output_limit"));
    assert_eq!(unread_gaps(&events).len(), 1);
    assert_eq!(stderr_gaps(&events).len(), 1);
}

#[tokio::test]
async fn issue_329_grace_equal_to_drain_max_is_labelled_drain_max() {
    // Equal bounds: the long limit and the grace period are the same instant, and the label stays
    // `drain_max` (D-324); `grace` is only for a drain the grace period shortened.
    let limits = Limits {
        drain_max: Duration::from_millis(700),
        ..stopped_limits()
    };
    let (ending, _, bounds) = stopped_before_exit(limits, Some(400)).await;
    assert_eq!(ending, Ending::Cancelled);
    assert_eq!(bounds, ["drain_max"]);
}

/// The CLI floods stderr and exits at once; the rate limit holds the host back, and a cancel
/// arrives `cancel_ms` after the CLI has finished writing (it leaves a marker as its last act, and
/// the cancel waits for it, so a loaded host cannot cancel a CLI that is still running). Returns
/// how the run ended, the `bound` of each unread gap and how long the run lasted after the cancel.
async fn cancel_after_exit(
    lines_per_second: u32,
    cancel_ms: u64,
) -> (Ending, Vec<String>, Duration) {
    let rig = Rig::new();
    let marker = rig.root.join("flooded");
    let cancel = CancellationToken::new();
    let cancelled_at = std::sync::Arc::new(std::sync::OnceLock::new());
    let (c, at, m) = (cancel.clone(), cancelled_at.clone(), marker.clone());
    tokio::spawn(async move {
        while !m.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tokio::time::sleep(Duration::from_millis(cancel_ms)).await;
        let _ = at.set(Instant::now());
        c.cancel();
    });
    // `yes | head` writes the 36 KB in a blink, then the marker, then the shell exits.
    let exe = rig.cli(&format!(
        "yes 'queued line' | head -n 3000 >&2; : > {}",
        marker.display()
    ));
    let (out, events) = rig
        .run(
            rig.launch(exe, drain_limits(lines_per_second, 5_000, 20_000)),
            PayloadPolicy::MetadataOnly,
            Redactor::new(),
            cancel,
        )
        .await;
    let after_cancel = cancelled_at.get().map_or(Duration::ZERO, Instant::elapsed);
    let bounds = unread_gaps(&events)
        .iter()
        .map(|g| g.data["bound"].as_str().unwrap_or("?").to_owned())
        .collect();
    (out.ending, bounds, after_cancel)
}

#[tokio::test]
async fn issue_329_a_drain_ended_by_a_cancel_after_exit_is_labelled_drain_stall() {
    let (ending, bounds, _) = cancel_after_exit(20, 500).await;
    assert_eq!(ending, Ending::Cancelled);
    assert_eq!(bounds, ["drain_stall"]);
}

/// #336: the label must not depend on which ready branch `select!` picks, so the same scenario
/// run 50 times (ten at a time) gives the same label every time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn issue_336_the_cancel_after_exit_label_is_the_same_in_fifty_runs() {
    let mut labels = std::collections::BTreeMap::<String, u32>::new();
    for _ in 0..5 {
        let mut batch = tokio::task::JoinSet::new();
        for _ in 0..10 {
            batch.spawn(cancel_after_exit(20, 500));
        }
        while let Some(done) = batch.join_next().await {
            let (ending, bounds, _) = done.unwrap();
            assert_eq!(ending, Ending::Cancelled);
            *labels.entry(bounds.join(",")).or_default() += 1;
        }
    }
    assert_eq!(
        labels,
        std::collections::BTreeMap::from([("drain_stall".to_owned(), 50)]),
        "labels over 50 runs"
    );
}

/// #336: an exit that has already happened is acted on before the cancel, and a rate-limit wait
/// does not hold either back. The wait is a second long here; the cancel comes at 100 ms and the
/// run must end near it, not after the wait (a fast-failing bound, far below the 20 s drain max).
/// The time is counted from the cancel, so a slow start cannot fail it.
#[tokio::test]
async fn issue_336_a_rate_limit_wait_does_not_hold_back_the_cancel_after_exit() {
    let (ending, bounds, took) = cancel_after_exit(1, 100).await;
    assert_eq!(ending, Ending::Cancelled);
    assert_eq!(bounds, ["drain_stall"]);
    assert!(
        took < Duration::from_millis(800),
        "the rate-limit wait held the run for {took:?} after the cancel"
    );
}

/// #336: the rate limit still holds after a quiet stretch. Five lines, a pause longer than the
/// window, then thirty-five more at ten lines a second: the window that expired unused is
/// reset by the next line over the limit, and the rest take three more seconds. (A lower bound
/// only, so a slow machine cannot flake it.)
#[tokio::test]
async fn issue_336_the_rate_limit_still_holds_after_a_quiet_window() {
    let rig = Rig::new();
    let body = "for i in 1 2 3 4 5; do echo a$i >&2; done; sleep 1.3; i=0; \
                while [ $i -lt 35 ]; do echo b$i >&2; i=$((i+1)); done";
    let limits = Limits {
        wall_time: Duration::from_secs(20),
        ..drain_limits(10, 5_000, 20_000)
    };
    let started = Instant::now();
    let (out, _) = rig.plain(body, limits).await;
    assert_eq!(out.ending, Ending::Exited(0));
    assert!(
        started.elapsed() >= Duration::from_millis(3_900),
        "the limit was off after the quiet window: {:?}",
        started.elapsed()
    );
}

/// #336: reading resumes about a second after a throttle, not at the deadline. Twenty-five lines at
/// ten a second take two waits; the wall time is 10 s so a wait that ran to the deadline ends the
/// run as `WallTime` and fails here instead of hanging.
#[tokio::test]
async fn issue_336_reading_resumes_a_second_after_a_throttle() {
    let rig = Rig::new();
    let limits = Limits {
        wall_time: Duration::from_secs(10),
        ..drain_limits(10, 5_000, 20_000)
    };
    let started = Instant::now();
    let (out, _) = rig.plain(&flood_body(25), limits).await;
    assert_eq!(out.ending, Ending::Exited(0));
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(1_900),
        "no throttle at all: {took:?}"
    );
    assert!(took < Duration::from_secs(6), "resumed late: {took:?}");
}

#[tokio::test]
async fn issue_327_stdout_exactly_at_its_cap_is_not_an_output_limit() {
    let rig = Rig::new();
    let limits = Limits {
        max_stdout_bytes: 100,
        ..drain_limits(2_000, 1_000, 10_000)
    };
    // 99 bytes and the newline: exactly the cap, which is allowed.
    let body = "head -c 99 /dev/zero | tr '\\\\0' a";
    let body = format!("{body}; echo");
    let (out, events) = rig.plain(&body, limits).await;
    assert_eq!(out.ending, Ending::Exited(0));
    assert!(!events.iter().any(|e| e.data["reason"] == "output_limit"));
}

#[tokio::test]
async fn issue_329_an_unstopped_drain_runs_to_drain_max_not_to_the_grace_period() {
    let rig = Rig::new();
    let limits = Limits {
        grace: Duration::from_millis(300),
        ..drain_limits(20, 5_000, 1_000)
    };
    let started = Instant::now();
    let (out, events) = rig.plain(&flood_body(3_000), limits).await;
    assert_eq!(out.ending, Ending::Exited(0));
    // Lines keep arriving until drain_max, so the drain cannot end before it (a lower bound only,
    // so a slow machine cannot flake it).
    assert!(
        started.elapsed() >= Duration::from_millis(950),
        "ended after {:?}",
        started.elapsed()
    );
    assert_eq!(unread_gaps(&events)[0].data["bound"], "drain_max");
}

/// #328: `supervise` seeds the normaliser from the run, so a writer whose clock is behind the
/// run's last id (here a year ahead) still sorts after it, whichever constructor it came from.
#[tokio::test]
async fn supervise_seeds_a_new_normaliser_from_the_runs_last_id_328() {
    use aibus_trace::{Batch, Origin, Visibility};
    let rig = Rig::new();
    let dir = rig.trace_dir();
    let planted = uuid::Builder::from_unix_timestamp_millis(
        (std::time::SystemTime::now() + Duration::from_secs(365 * 86_400))
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
        &[0; 10],
    )
    .into_uuid();
    {
        let (mut store, _) = TraceStore::open(
            &dir,
            Redactor::new(),
            PayloadPolicy::MetadataOnly,
            StoreLimits::default(),
        )
        .unwrap();
        let e = TraceEvent::new(
            &common::attempt(),
            planted,
            EventKind::TraceWarning,
            Origin::Derived,
            Visibility::Observed,
            std::time::SystemTime::now(),
            "test/1",
        );
        store
            .append(Batch {
                events: vec![e],
                payload: None,
            })
            .unwrap();
    }
    let (_, events) = rig.plain("echo hi", Limits::default()).await;
    assert_eq!(events[0].event_id, planted);
    assert!(events.len() > 2);
    assert!(events.windows(2).all(|w| w[0].event_id < w[1].event_id));
}
