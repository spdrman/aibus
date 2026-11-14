//! Replaying the spike's captured runs (#281) through the normaliser, with the arrival times the
//! harness recorded, so every event is stamped exactly as a live run would have been.
#![allow(dead_code)]

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use aibus_core::Redactor;
use aibus_supervisor::claude::EXTENSION_ID;
use aibus_supervisor::{Ending, Normalizer};
use aibus_trace::{AttemptRef, Batch, PayloadPolicy, StoreLimits, TraceEvent, TraceStore};
use uuid::Uuid;

pub fn spike_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/claude-code/spike-281")
}

pub fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/claude-code/normalized-282")
}

/// The runs with a stream.
pub const RUNS: [&str; 9] = [
    "06-basic",
    "07-deleg",
    "08-control",
    "09-denied",
    "10-killed",
    "11-foreground",
    "12-teams-probe",
    "13-teams-control",
    "14-teams-attempt",
];

pub fn attempt() -> AttemptRef {
    AttemptRef {
        workflow_run_id: Uuid::from_u128(0x282_0001),
        attempt_id: Uuid::from_u128(0x282_0002),
        parent_attempt_id: None,
        provider_extension: EXTENSION_ID.into(),
    }
}

/// A captured run: stdout lines with their arrival offsets, hook payloads, and how it ended.
pub struct Run {
    pub lines: Vec<Vec<u8>>,
    pub offsets_ms: Vec<u64>,
    pub hooks: Vec<Vec<u8>>,
    pub started: SystemTime,
    pub wall: Duration,
    pub ending: Ending,
}

fn meta(text: &str, key: &str) -> String {
    text.lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("no {key} in meta"))
        .to_owned()
}

pub fn load(name: &str) -> Run {
    let dir = spike_dir();
    let raw = fs::read(dir.join(format!("{name}.stdout.jsonl"))).unwrap();
    let lines: Vec<Vec<u8>> = raw
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(<[u8]>::to_vec)
        .collect();
    let tsv = fs::read_to_string(dir.join(format!("{name}.observed.tsv"))).unwrap();
    let offsets_ms: Vec<u64> = tsv
        .lines()
        .skip(1)
        .map(|l| l.split('\t').nth(1).unwrap().parse().unwrap())
        .collect();
    assert_eq!(
        offsets_ms.len(),
        lines.len(),
        "{name}: one arrival time per line"
    );
    let hooks = fs::read(dir.join(format!("{name}.hooks.jsonl")))
        .map(|h| {
            h.split(|b| *b == b'\n')
                .filter(|l| !l.is_empty())
                .map(<[u8]>::to_vec)
                .collect()
        })
        .unwrap_or_default();
    let m = fs::read_to_string(dir.join(format!("{name}.meta.txt"))).unwrap();
    let started = SystemTime::UNIX_EPOCH
        + Duration::from_millis(meta(&m, "started_unix_ms").parse().unwrap());
    let wall = Duration::from_millis(meta(&m, "wall_ms").parse().unwrap());
    let ending = match meta(&m, "exit").as_str() {
        "Some(0)" => Ending::Exited(0),
        "None" => Ending::WallTime,
        other => panic!("{name}: exit {other}"),
    };
    Run {
        lines,
        offsets_ms,
        hooks,
        started,
        wall,
        ending,
    }
}

/// Which channels to feed, and in what order.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Feed {
    Stream,
    StreamThenHooks,
    HooksThenStream,
}

/// Every batch the normaliser produced for `run`, in order, plus the normaliser.
pub fn normalise(run: &Run, feed: Feed) -> (Vec<Batch>, Normalizer) {
    let mut n = Normalizer::new(attempt());
    let mut out = vec![n.start(run.started, serde_json::json!({ "cli_version": "2.1.311" }))];
    let hook_at = run.started + run.wall;
    let hooks = |n: &mut Normalizer, out: &mut Vec<Batch>| {
        for h in &run.hooks {
            out.push(n.hook_line(h, hook_at));
        }
    };
    if feed == Feed::HooksThenStream {
        hooks(&mut n, &mut out);
    }
    for (line, ms) in run.lines.iter().zip(&run.offsets_ms) {
        out.push(n.stream_line(line, run.started + Duration::from_millis(*ms)));
    }
    if feed == Feed::StreamThenHooks {
        hooks(&mut n, &mut out);
    }
    out.push(n.finish(run.ending, run.wall, run.started + run.wall));
    (out, n)
}

/// The batches through a fresh store and back: the events exactly as they are on disk.
pub fn stored(batches: Vec<Batch>) -> Vec<TraceEvent> {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    let (mut store, _) = TraceStore::open(
        &dir,
        Redactor::new(),
        PayloadPolicy::MetadataOnly,
        StoreLimits::default(),
    )
    .unwrap();
    for b in batches {
        store.append(b).unwrap();
    }
    drop(store);
    let (events, report) = TraceStore::read(&dir).unwrap();
    assert_eq!(report, Default::default());
    events
}

pub fn replay(name: &str, feed: Feed) -> Vec<TraceEvent> {
    stored(normalise(&load(name), feed).0)
}
