//! A seeded fuzz run over the normaliser, on every push: random bytes, mutated spike lines,
//! random JSON in the shapes the parser looks at, and a flood of unique ids. Nothing may panic,
//! every event stays small, and the normaliser's state stays under its cap.

mod common;

use std::time::{Duration, SystemTime};

use aibus_core::Redactor;
use aibus_supervisor::{Ending, Normalizer};
use aibus_trace::{Batch, EventKind, PayloadPolicy, StoreLimits, TraceStore};
use serde_json::{json, Value};

/// xorshift64*: no dependency, and the same inputs on every machine.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn pick<'a, T>(&mut self, v: &'a [T]) -> &'a T {
        &v[self.below(v.len())]
    }
}

const TYPES: [&str; 8] = [
    "system",
    "assistant",
    "user",
    "result",
    "stream_event",
    "rate_limit_event",
    "nonsense",
    "",
];
const SUBTYPES: [&str; 10] = [
    "init",
    "task_started",
    "task_updated",
    "task_notification",
    "permission_denied",
    "hook_started",
    "hook_response",
    "status",
    "task_progress",
    "x",
];
const HOOKS: [&str; 9] = [
    "SessionStart",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "SubagentStart",
    "SubagentStop",
    "Stop",
    "SessionEnd",
    "Mystery",
];

fn random_value(r: &mut Rng, depth: u32) -> Value {
    match (r.below(9), depth) {
        (0, _) => Value::Null,
        (1, _) => json!(r.below(2) == 1),
        (2, _) => json!(r.next() as i64),
        (3, _) => json!(f64::from_bits(r.next()))
            .as_f64()
            .map_or(json!(1.5), |f| json!(f)),
        (4, _) => json!("x".repeat(r.below(300))),
        (5, _) => json!(*r.pick(&[
            "toolu_A",
            "toolu_B",
            "a1",
            "a2",
            "Agent",
            "Task",
            "Read",
            "completed",
            "failed",
            ""
        ])),
        (6, d) if d < 4 => Value::Array((0..r.below(5)).map(|_| random_value(r, d + 1)).collect()),
        (7, d) if d < 4 => {
            let keys = [
                "type",
                "id",
                "name",
                "tool_use_id",
                "agent_id",
                "content",
                "input",
                "status",
                "usage",
                "patch",
                "is_error",
                "text",
                "message",
            ];
            Value::Object(
                (0..r.below(6))
                    .map(|_| ((*r.pick(&keys)).to_owned(), random_value(r, d + 1)))
                    .collect(),
            )
        }
        _ => json!(r.below(1000)),
    }
}

/// A line shaped like what the parser reads, with random contents.
fn shaped(r: &mut Rng) -> Vec<u8> {
    let ids = ["toolu_A", "toolu_B", "toolu_C", "a1", "a2", "sess"];
    let mut v = json!({
        "type": r.pick(&TYPES),
        "subtype": r.pick(&SUBTYPES),
        "session_id": r.pick(&ids),
        "agent_id": r.pick(&ids),
        "parent_tool_use_id": r.pick(&ids),
        "tool_use_id": r.pick(&ids),
        "task_id": r.pick(&ids),
        "hook_event_name": r.pick(&HOOKS),
        "tool_name": r.pick(&["Agent", "Task", "Read", "Write"]),
        "message": { "content": [
            { "type": r.pick(&["text", "tool_use", "tool_result", "thinking", "weird"]), "id": r.pick(&ids), "tool_use_id": r.pick(&ids), "name": r.pick(&["Agent", "Task", "Read"]), "is_error": r.below(2) == 1, "text": "t" },
        ]},
        "patch": { "status": r.pick(&["completed", "failed", "killed", "running"]), "end_time": r.next() },
        "usage": { "input_tokens": r.below(1 << 20), "total_tokens": random_value(r, 3) },
        "total_cost_usd": random_value(r, 3),
        "is_error": r.below(2) == 1,
        "result": r.pick(&["ok", "Please run /login", "Invalid API key"]),
        "tool_response": { "agentId": r.pick(&ids) },
    });
    // Knock random fields out or swap their type.
    if let Value::Object(map) = &mut v {
        let keys: Vec<String> = map.keys().cloned().collect();
        for _ in 0..r.below(6) {
            let k = r.pick(&keys).clone();
            if r.below(2) == 0 {
                map.remove(&k);
            } else {
                map.insert(k, random_value(r, 0));
            }
        }
    }
    serde_json::to_vec(&v).unwrap()
}

fn mutate(r: &mut Rng, line: &[u8]) -> Vec<u8> {
    let mut l = line.to_vec();
    match r.below(6) {
        0 if !l.is_empty() => {
            for _ in 0..1 + r.below(8) {
                let i = r.below(l.len());
                l[i] = r.next() as u8;
            }
        }
        1 => l.truncate(r.below(l.len() + 1)),
        2 if !l.is_empty() => {
            let i = r.below(l.len());
            l.splice(i..i, std::iter::repeat_n(b'[', r.below(400)));
        }
        3 => l.extend_from_slice(&l.clone()),
        4 if !l.is_empty() => {
            let i = r.below(l.len());
            l.insert(i, 0xff);
        }
        _ => {}
    }
    l
}

fn check(batch: &Batch, at: &str) {
    assert!(
        batch.events.len() <= 1100,
        "{at}: {} events from one line",
        batch.events.len()
    );
    for e in &batch.events {
        let size = serde_json::to_vec(e).unwrap().len();
        assert!(size <= 16 * 1024, "{at}: an event of {size} bytes");
    }
}

#[test]
fn forty_thousand_hostile_lines_never_panic_and_stay_bounded() {
    let mut corpus: Vec<Vec<u8>> = Vec::new();
    for name in common::RUNS {
        let run = common::load(name);
        corpus.extend(run.lines);
        corpus.extend(run.hooks);
    }
    let mut r = Rng(0x282_5EED_u64);
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let tmp = tempfile::tempdir().unwrap();
    let (mut store, _) = TraceStore::open(
        &tmp.path().join("run"),
        Redactor::new(),
        PayloadPolicy::Retain { max_bytes: 4 << 20 },
        StoreLimits::default(),
    )
    .unwrap();
    let mut inputs = 0u64;
    for round in 0..40 {
        let mut n = Normalizer::with_ids(common::attempt(), store.ids());
        n.start(at, json!({}));
        for i in 0..1_000 {
            let line = match r.below(4) {
                0 => (0..r.below(512)).map(|_| r.next() as u8).collect(),
                1 => {
                    let base = r.pick(&corpus).clone();
                    mutate(&mut r, &base)
                }
                2 => shaped(&mut r),
                _ => r.pick(&corpus).clone(),
            };
            let batch = if r.below(3) == 0 {
                n.hook_line(&line, at)
            } else {
                n.stream_line(&line, at)
            };
            check(&batch, &format!("round {round} line {i}"));
            if i % 50 == 0 {
                store.append(batch).unwrap();
            }
            inputs += 1;
        }
        for l in [
            b"\xff\xfe".as_slice(),
            b"",
            b"{",
            b"null",
            b"[]",
            b"\"str\"",
        ] {
            check(&n.stream_line(l, at), "edge");
            check(&n.stderr_line(l, at), "stderr");
            inputs += 2;
        }
        let ending = *r.pick(&[
            Ending::Exited(0),
            Ending::Exited(1),
            Ending::Signalled(9),
            Ending::Cancelled,
            Ending::WallTime,
            Ending::OutputLimit,
        ]);
        let end = n.finish(ending, Duration::from_secs(1), at);
        check(&end, "finish");
        assert_eq!(
            end.events
                .iter()
                .filter(|e| matches!(
                    e.kind,
                    EventKind::AttemptCompleted | EventKind::AttemptFailed
                ))
                .count(),
            1
        );
        store.append(end).unwrap();
    }
    assert!(inputs >= 30_000, "{inputs}");
}

#[test]
fn a_flood_of_unique_ids_hits_the_cap_once_and_memory_stays_flat() {
    let mut n = Normalizer::new(common::attempt());
    let at = SystemTime::UNIX_EPOCH;
    let mut gaps = 0;
    let mut peak = 0;
    for i in 0..100_000u32 {
        let line = format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"toolu_{i}","name":"Read","input":{{}}}}]}},"session_id":"s","agent_id":"agent_{i}","parent_tool_use_id":"toolu_p{i}"}}"#
        );
        let b = n.stream_line(line.as_bytes(), at);
        gaps += b
            .events
            .iter()
            .filter(|e| e.kind == EventKind::GapDetected && e.data["reason"] == "tracking_limit")
            .count();
        peak = peak.max(n.tracked());
    }
    assert_eq!(gaps, 1, "one degraded-visibility gap, not one per line");
    assert!(peak <= 70_000, "tracked {peak}");
}
