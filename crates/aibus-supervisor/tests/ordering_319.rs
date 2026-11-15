//! #319: the normaliser's events carry uuid v7 ids that order the trace, and every recorded
//! fixture trace is in the schema-2 format (no integer sequence, ids ascending in file order).

mod common;

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use aibus_supervisor::Normalizer;
use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

#[test]
fn events_normalised_in_one_millisecond_keep_their_order_319() {
    let run = load("06-basic");
    let at = SystemTime::UNIX_EPOCH + Duration::from_millis(1_800_000_000_000);
    let mut n = Normalizer::new(attempt());
    let mut events = n.start(at, json!({})).events;
    for l in &run.lines {
        events.extend(n.stream_line(l, at).events);
    }
    events.extend(n.finish(run.ending, run.wall, at).events);
    assert!(events.len() > 5);
    assert!(events.iter().all(|e| e.event_id.get_version_num() == 7));
    for w in events.windows(2) {
        assert!(
            w[0].event_id < w[1].event_id,
            "{:?} then {:?}",
            w[0].kind,
            w[1].kind
        );
    }
}

#[test]
fn events_normalised_with_a_clock_that_steps_back_still_ascend_319() {
    let run = load("06-basic");
    let mut n = Normalizer::new(attempt());
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_millis(1_800_000_100_000);
    let mut events = n.start(t0, json!({})).events;
    for (i, l) in run.lines.iter().enumerate() {
        let at = t0 - Duration::from_millis(i as u64);
        events.extend(n.stream_line(l, at).events);
    }
    events.extend(
        n.finish(run.ending, run.wall, t0 - Duration::from_secs(1))
            .events,
    );
    assert!(events.len() > 5);
    for w in events.windows(2) {
        assert!(w[0].event_id < w[1].event_id);
    }
}

#[test]
fn the_recorded_fixture_traces_are_schema_2_with_ascending_v7_ids_319() {
    let root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/claude-code/traces-284");
    let mut checked = 0;
    for entry in fs::read_dir(&root).unwrap().flatten() {
        let file = entry.path().join("events.jsonl");
        let Ok(text) = fs::read_to_string(&file) else {
            continue;
        };
        let mut last: Option<Uuid> = None;
        for line in text.lines() {
            let v: Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["schema_version"], 2, "{}", file.display());
            assert!(v.get("sequence").is_none(), "{}", file.display());
            let id: Uuid = v["event_id"].as_str().unwrap().parse().unwrap();
            assert_eq!(id.get_version_num(), 7, "{}", file.display());
            assert!(
                last.is_none_or(|l| l < id),
                "{} out of order",
                file.display()
            );
            last = Some(id);
        }
        checked += 1;
    }
    assert!(checked >= 17, "only {checked} fixture traces found");
}
