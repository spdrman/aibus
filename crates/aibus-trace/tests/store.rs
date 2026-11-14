//! The append-only store: sequence, restart, crash recovery, the writer lock, idempotence,
//! redaction before anything reaches the disk, private files and the size limits.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, SystemTime};

use aibus_core::ids::AccountName;
use aibus_core::Redactor;
use aibus_trace::{
    derived_id, AttemptRef, Batch, EventKind, Origin, PayloadPolicy, StoreError, StoreLimits,
    TraceEvent, TraceStore, Visibility,
};
use serde_json::json;
use uuid::Uuid;

fn attempt() -> AttemptRef {
    AttemptRef {
        workflow_run_id: Uuid::from_u128(1),
        attempt_id: Uuid::from_u128(2),
        parent_attempt_id: None,
        provider_extension: "org.aibus.test".into(),
    }
}

fn ev(n: u64, kind: EventKind) -> TraceEvent {
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000 + n);
    TraceEvent::new(
        &attempt(),
        derived_id(&[&n.to_le_bytes()]),
        kind,
        Origin::CliStream,
        Visibility::Observed,
        at,
        "test/1",
    )
}

fn one(e: TraceEvent) -> Batch {
    Batch {
        events: vec![e],
        payload: None,
    }
}

fn open(dir: &Path) -> TraceStore {
    TraceStore::open(
        dir,
        Redactor::new(),
        PayloadPolicy::MetadataOnly,
        StoreLimits::default(),
    )
    .unwrap()
    .0
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

#[test]
fn appends_get_monotonic_sequences_and_continue_after_a_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    {
        let mut s = open(&dir);
        for n in 0..3 {
            let written = s.append(one(ev(n, EventKind::MessageChunk))).unwrap();
            assert_eq!(written[0].sequence, n + 1);
        }
    }
    // An aibus restart: the lock is gone with the process, the sequence goes on.
    let mut s = open(&dir);
    assert_eq!(s.next_sequence(), 4);
    s.append(one(ev(9, EventKind::AgentStopped))).unwrap();
    let (events, report) = TraceStore::read(&dir).unwrap();
    assert_eq!(
        events.iter().map(|e| e.sequence).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    assert_eq!(report, Default::default());
}

#[test]
fn a_torn_tail_is_moved_aside_and_the_next_append_follows_the_last_whole_line() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    {
        let mut s = open(&dir);
        s.append(one(ev(0, EventKind::AttemptStarted))).unwrap();
        s.append(one(ev(1, EventKind::ToolStarted))).unwrap();
    }
    // A crash in the middle of the third write.
    let half = r#"{"schema_version":1,"event_id":"0000"#;
    fs::OpenOptions::new()
        .append(true)
        .open(dir.join("events.jsonl"))
        .unwrap()
        .write_all(half.as_bytes())
        .unwrap();

    let (mut s, recovery) = TraceStore::open(
        &dir,
        Redactor::new(),
        PayloadPolicy::MetadataOnly,
        StoreLimits::default(),
    )
    .unwrap();
    assert_eq!(recovery.torn_bytes, half.len() as u64);
    let torn: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("events.jsonl.torn-"))
        .collect();
    assert_eq!(torn.len(), 1, "the torn bytes are kept as evidence");
    s.append(one(ev(2, EventKind::ToolCompleted))).unwrap();
    let (events, report) = TraceStore::read(&dir).unwrap();
    assert_eq!(report.corrupt_lines, 0);
    let kinds: Vec<_> = events.iter().map(|e| e.kind).collect();
    // The repair itself is in the trace, between the two halves.
    assert_eq!(
        kinds,
        vec![
            EventKind::AttemptStarted,
            EventKind::ToolStarted,
            EventKind::TraceWarning,
            EventKind::ToolCompleted
        ]
    );
    assert_eq!(events[2].data["reason"], "torn_tail_recovered");
    assert_eq!(
        events.iter().map(|e| e.sequence).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
}

/// The child half of `a_writer_killed_mid_append_leaves_only_whole_events`: appends until killed.
#[test]
fn crash_child_appends_forever() {
    let Some(dir) = std::env::var_os("AIBUS_TRACE_CRASH_DIR") else {
        return;
    };
    let mut s = open(Path::new(&dir));
    let pad = "x".repeat(4000);
    for n in 0.. {
        let mut e = ev(n, EventKind::MessageChunk);
        e.data = json!({ "pad": pad });
        s.append(one(e)).unwrap();
    }
}

#[test]
fn a_writer_killed_mid_append_leaves_only_whole_events() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "crash_child_appends_forever",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env("AIBUS_TRACE_CRASH_DIR", &dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let file = dir.join("events.jsonl");
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while fs::metadata(&file).map(|m| m.len()).unwrap_or(0) < 400_000 {
        assert!(
            std::time::Instant::now() < deadline,
            "the child never wrote"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    child.kill().unwrap(); // SIGKILL: no destructor, no flush
    child.wait().unwrap();

    let (mut s, recovery) = TraceStore::open(
        &dir,
        Redactor::new(),
        PayloadPolicy::MetadataOnly,
        StoreLimits::default(),
    )
    .unwrap();
    assert_eq!(recovery.corrupt_lines, 0);
    let next = s.next_sequence();
    s.append(one(ev(u64::MAX, EventKind::AttemptFailed)))
        .unwrap();
    let (events, report) = TraceStore::read(&dir).unwrap();
    assert_eq!(report.corrupt_lines + report.torn_bytes, 0);
    let seqs: Vec<u64> = events.iter().map(|e| e.sequence).collect();
    assert_eq!(
        seqs,
        (1..=seqs.len() as u64).collect::<Vec<_>>(),
        "no hole, no repeat"
    );
    assert!(seqs.len() as u64 >= next - 1);
}

#[test]
fn a_second_writer_is_refused_while_the_first_holds_the_run() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    let _first = open(&dir);
    let second = TraceStore::open(
        &dir,
        Redactor::new(),
        PayloadPolicy::MetadataOnly,
        StoreLimits::default(),
    );
    assert!(matches!(second, Err(StoreError::Locked(_))));
    // Readers never need the lock.
    assert!(TraceStore::read(&dir).is_ok());
}

#[test]
fn the_same_event_id_is_written_once_even_across_a_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    {
        let mut s = open(&dir);
        assert_eq!(
            s.append(one(ev(7, EventKind::ToolStarted))).unwrap().len(),
            1
        );
        assert_eq!(
            s.append(one(ev(7, EventKind::ToolStarted))).unwrap().len(),
            0
        );
    }
    let mut s = open(&dir);
    assert_eq!(
        s.append(one(ev(7, EventKind::ToolStarted))).unwrap().len(),
        0
    );
    assert_eq!(TraceStore::read(&dir).unwrap().0.len(), 1);
}

#[test]
fn payloads_and_data_are_redacted_before_they_reach_the_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    let redactor = Redactor::new();
    let registered = "registered-canary-0123456789";
    redactor.register(&AccountName("work".into()), registered);
    let shaped = format!("sk-ant-api03-{}", "A".repeat(40));
    let (mut s, _) = TraceStore::open(
        &dir,
        redactor,
        PayloadPolicy::Retain { max_bytes: 1 << 20 },
        StoreLimits::default(),
    )
    .unwrap();
    let mut e = ev(1, EventKind::ToolCompleted);
    e.data = json!({ "tool": format!("Read {registered}") });
    let written = s
        .append(Batch {
            events: vec![e],
            payload: Some(
                json!({ "content": [registered, shaped.as_str()], "nested": { "k": shaped } }),
            ),
        })
        .unwrap();
    let r = written[0]
        .payload_ref
        .clone()
        .expect("a payload ref under Retain");
    assert!(r.starts_with("local://objects/"), "{r}");
    assert!(
        grep_tree(&dir, registered).is_empty(),
        "registered secret on disk"
    );
    assert!(
        grep_tree(&dir, &shaped).is_empty(),
        "key-shaped secret on disk"
    );
    assert!(
        !grep_tree(&dir, "[redacted:work]").is_empty(),
        "the redaction marker is what is stored"
    );
}

#[test]
fn metadata_only_writes_no_payload_object() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    let mut s = open(&dir);
    let written = s
        .append(Batch {
            events: vec![ev(1, EventKind::MessageChunk)],
            payload: Some(json!({ "text": "PAYLOAD-TEXT" })),
        })
        .unwrap();
    assert_eq!(written[0].payload_ref, None);
    assert!(grep_tree(&dir, "PAYLOAD-TEXT").is_empty());
}

#[test]
fn a_payload_past_the_retention_budget_is_not_kept_and_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    let (mut s, _) = TraceStore::open(
        &dir,
        Redactor::new(),
        PayloadPolicy::Retain { max_bytes: 64 },
        StoreLimits::default(),
    )
    .unwrap();
    let big = "B".repeat(1000);
    let written = s
        .append(Batch {
            events: vec![ev(1, EventKind::MessageChunk)],
            payload: Some(json!({ "text": big })),
        })
        .unwrap();
    assert_eq!(written[0].payload_ref, None);
    let (events, _) = TraceStore::read(&dir).unwrap();
    assert!(events.iter().any(
        |e| e.kind == EventKind::GapDetected && e.data["reason"] == "payload_budget_exhausted"
    ));
    assert!(grep_tree(&dir, &big).is_empty());
}

#[cfg(unix)]
#[test]
fn every_file_and_directory_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    let (mut s, _) = TraceStore::open(
        &dir,
        Redactor::new(),
        PayloadPolicy::Retain { max_bytes: 1 << 20 },
        StoreLimits::default(),
    )
    .unwrap();
    s.append(Batch {
        events: vec![ev(1, EventKind::MessageChunk)],
        payload: Some(json!({ "t": 1 })),
    })
    .unwrap();
    let mut stack = vec![dir.clone()];
    while let Some(p) = stack.pop() {
        let mode = fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        if p.is_dir() {
            assert_eq!(mode, 0o700, "{}", p.display());
            stack.extend(fs::read_dir(&p).unwrap().map(|e| e.unwrap().path()));
        } else {
            assert_eq!(mode, 0o600, "{}", p.display());
        }
    }
}

#[test]
fn past_the_soft_limit_lifecycle_events_still_land_and_chunks_become_one_gap() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    let limits = StoreLimits {
        soft_max_bytes: 4_000,
        ..StoreLimits::default()
    };
    let (mut s, _) =
        TraceStore::open(&dir, Redactor::new(), PayloadPolicy::MetadataOnly, limits).unwrap();
    for n in 0..100 {
        s.append(one(ev(n, EventKind::MessageChunk))).unwrap();
    }
    s.append(one(ev(1000, EventKind::ToolStarted))).unwrap();
    s.append(one(ev(1001, EventKind::AttemptCompleted)))
        .unwrap();
    let (events, _) = TraceStore::read(&dir).unwrap();
    let gaps = events
        .iter()
        .filter(|e| e.kind == EventKind::GapDetected)
        .count();
    assert_eq!(gaps, 1, "one gap, not one per dropped chunk");
    assert!(events.iter().any(|e| e.kind == EventKind::ToolStarted));
    assert_eq!(events.last().unwrap().kind, EventKind::AttemptCompleted);
    assert!(
        events
            .iter()
            .filter(|e| e.kind == EventKind::MessageChunk)
            .count()
            < 100
    );
}

#[test]
fn an_oversized_event_is_cut_to_its_envelope_with_a_note() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    let limits = StoreLimits {
        max_event_bytes: 2_000,
        ..StoreLimits::default()
    };
    let (mut s, _) =
        TraceStore::open(&dir, Redactor::new(), PayloadPolicy::MetadataOnly, limits).unwrap();
    let mut e = ev(1, EventKind::ToolStarted);
    e.data = json!({ "tool": "T".repeat(50_000) });
    let written = s.append(one(e)).unwrap();
    assert_eq!(written[0].data["truncated"], true);
    let line = fs::read_to_string(dir.join("events.jsonl")).unwrap();
    assert!(line.len() <= 2_001, "{}", line.len());
}

#[test]
fn a_corrupt_middle_line_is_counted_and_readers_skip_it() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    {
        let mut s = open(&dir);
        s.append(one(ev(0, EventKind::AttemptStarted))).unwrap();
    }
    fs::OpenOptions::new()
        .append(true)
        .open(dir.join("events.jsonl"))
        .unwrap()
        .write_all(b"not an event\n")
        .unwrap();
    let (mut s, recovery) = TraceStore::open(
        &dir,
        Redactor::new(),
        PayloadPolicy::MetadataOnly,
        StoreLimits::default(),
    )
    .unwrap();
    assert_eq!(recovery.corrupt_lines, 1);
    s.append(one(ev(1, EventKind::AttemptCompleted))).unwrap();
    let (events, report) = TraceStore::read(&dir).unwrap();
    assert_eq!(report.corrupt_lines, 1);
    assert_eq!(events.first().unwrap().kind, EventKind::AttemptStarted);
    assert_eq!(events.last().unwrap().kind, EventKind::AttemptCompleted);
    assert!(events
        .iter()
        .any(|e| e.kind == EventKind::TraceWarning && e.data["reason"] == "corrupt_lines"));
}

#[test]
fn a_line_of_another_schema_version_is_not_read_as_an_event() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    {
        let mut s = open(&dir);
        s.append(one(ev(0, EventKind::AttemptStarted))).unwrap();
    }
    let mut v = serde_json::to_value(ev(5, EventKind::AttemptCompleted)).unwrap();
    v["schema_version"] = json!(2);
    v["sequence"] = json!(2);
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(dir.join("events.jsonl"))
        .unwrap();
    writeln!(f, "{v}").unwrap();
    let (events, report) = TraceStore::read(&dir).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(report.unsupported_schema, 1);
}
