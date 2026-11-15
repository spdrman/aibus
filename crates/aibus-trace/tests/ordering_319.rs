//! #319: a trace orders by its uuid v7 event ids, not by an integer counter. Replay, torn-tail
//! repair and idempotent append all key on the id, two events in one millisecond keep their order,
//! and a store written by the schema-1 format (integer `sequence`) is refused with a clear message.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, SystemTime};

use aibus_core::Redactor;
use aibus_trace::{
    AttemptRef, Batch, EventKind, Origin, PayloadPolicy, StoreError, StoreLimits, TraceEvent,
    TraceStore, Visibility,
};
use serde_json::Value;
use uuid::Uuid;

const MS: u64 = 1_800_000_000_000;

fn v7(ms: u64, n: u64) -> Uuid {
    let mut tail = [0u8; 10];
    tail[2..].copy_from_slice(&n.to_be_bytes());
    uuid::Builder::from_unix_timestamp_millis(ms, &tail).into_uuid()
}

fn ev(id: Uuid, kind: EventKind) -> TraceEvent {
    let attempt = AttemptRef {
        workflow_run_id: Uuid::from_u128(1),
        attempt_id: Uuid::from_u128(2),
        parent_attempt_id: None,
        provider_extension: "org.aibus.test".into(),
    };
    TraceEvent::new(
        &attempt,
        id,
        kind,
        Origin::CliStream,
        Visibility::Observed,
        SystemTime::UNIX_EPOCH + Duration::from_millis(MS),
        "test/1",
    )
}

fn one(e: TraceEvent) -> Batch {
    Batch {
        events: vec![e],
        payload: None,
    }
}

fn open_result(dir: &Path) -> Result<TraceStore, StoreError> {
    TraceStore::open(
        dir,
        Redactor::new(),
        PayloadPolicy::MetadataOnly,
        StoreLimits::default(),
    )
    .map(|(s, _)| s)
}

fn open(dir: &Path) -> TraceStore {
    open_result(dir).unwrap()
}

fn lines(dir: &Path) -> Vec<Value> {
    fs::read_to_string(dir.join("events.jsonl"))
        .unwrap()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn ids(dir: &Path) -> Vec<Uuid> {
    lines(dir)
        .iter()
        .map(|v| v["event_id"].as_str().unwrap().parse().unwrap())
        .collect()
}

fn append_bytes(dir: &Path, bytes: &[u8]) {
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(dir.join("events.jsonl"))
        .unwrap();
    f.write_all(bytes).unwrap();
}

#[test]
fn the_envelope_carries_no_integer_sequence_319() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    let mut s = open(&dir);
    s.append(one(ev(v7(MS, 1), EventKind::AttemptStarted)))
        .unwrap();
    drop(s);
    let l = lines(&dir);
    assert_eq!(l[0]["schema_version"], 2);
    assert!(l[0].get("sequence").is_none(), "{}", l[0]);
}

#[test]
fn events_the_store_writes_itself_carry_a_uuid_v7_id_319() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    let mut s = open(&dir);
    s.append(one(ev(v7(MS, 1), EventKind::AttemptStarted)))
        .unwrap();
    drop(s);
    append_bytes(&dir, b"{\"torn");
    drop(open(&dir));
    let all = ids(&dir);
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|i| i.get_version_num() == 7), "{all:?}");
}

#[test]
fn the_store_refuses_an_event_id_that_is_not_a_uuid_v7_319() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = open(&tmp.path().join("run"));
    let e = ev(Uuid::new_v8([1; 16]), EventKind::AttemptStarted);
    let err = s.append(one(e)).unwrap_err().to_string();
    assert!(err.contains("uuid v7"), "{err}");
}

#[test]
fn read_orders_by_event_id_whatever_the_file_order_319() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    fs::create_dir_all(&dir).unwrap();
    let mut text = String::new();
    for n in [3u64, 1, 2] {
        let mut v = serde_json::to_value(ev(v7(MS + n, n), EventKind::MessageChunk)).unwrap();
        v["schema_version"] = 2.into();
        v.as_object_mut().unwrap().remove("sequence");
        text.push_str(&format!("{v}\n"));
    }
    fs::write(dir.join("events.jsonl"), text).unwrap();
    let (events, report) = TraceStore::read(&dir).unwrap();
    assert_eq!(report.unsupported_schema, 0);
    let got: Vec<Uuid> = events.iter().map(|e| e.event_id).collect();
    assert_eq!(got, vec![v7(MS + 1, 1), v7(MS + 2, 2), v7(MS + 3, 3)]);
}

#[test]
fn two_events_written_in_one_millisecond_keep_their_order_319() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    let mut s = open(&dir);
    s.append(one(ev(v7(MS, 5), EventKind::AttemptStarted)))
        .unwrap();
    drop(s);
    // A corrupt line and a torn tail: reopening writes two warnings of its own, back to back.
    append_bytes(&dir, b"not json\n{\"torn");
    drop(open(&dir));
    let file_order = ids(&dir);
    assert_eq!(file_order.len(), 3);
    let mut sorted = file_order.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted, file_order,
        "ids ascend in the order they were written"
    );
    let reasons: Vec<String> = TraceStore::read(&dir)
        .unwrap()
        .0
        .iter()
        .skip(1)
        .map(|e| e.data["reason"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(reasons, ["torn_tail_recovered", "corrupt_lines"]);
}

#[test]
fn an_event_after_a_torn_tail_sorts_after_everything_before_it_319() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    let mut s = open(&dir);
    // Written "in the future" relative to the wall clock: the next ids must still sort after it.
    s.append(one(ev(
        v7(u64::from(u32::MAX) * 1000, 1),
        EventKind::AttemptStarted,
    )))
    .unwrap();
    drop(s);
    append_bytes(&dir, b"{\"torn");
    drop(open(&dir));
    let all = ids(&dir);
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|i| i.get_version_num() == 7), "{all:?}");
    assert!(all[0] < all[1], "{all:?}");
}

#[test]
fn appending_the_same_event_id_twice_writes_it_once_319() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    let e = ev(v7(MS, 1), EventKind::AttemptStarted);
    let mut s = open(&dir);
    assert_eq!(s.append(one(e.clone())).unwrap().len(), 1);
    assert!(s.append(one(e.clone())).unwrap().is_empty());
    drop(s);
    // Across a restart too: the ids come back from the file.
    let mut s = open(&dir);
    assert!(s.append(one(e)).unwrap().is_empty());
    drop(s);
    let l = lines(&dir);
    assert_eq!(l.len(), 1);
    assert_eq!(l[0]["schema_version"], 2);
}

#[test]
fn a_schema_1_store_is_refused_with_a_message_that_says_why_319() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    fs::create_dir_all(&dir).unwrap();
    let mut v = serde_json::to_value(ev(Uuid::new_v8([2; 16]), EventKind::AttemptStarted)).unwrap();
    v["schema_version"] = 1.into();
    v["sequence"] = 1.into();
    fs::write(dir.join("events.jsonl"), format!("{v}\n")).unwrap();
    let err = open_result(&dir)
        .err()
        .expect("a schema 1 store is refused")
        .to_string();
    assert!(err.contains("schema 1") && err.contains("uuid v7"), "{err}");
    // Nothing was appended to it.
    assert_eq!(lines(&dir).len(), 1);
    // A reader skips the line and says so.
    let (events, report) = TraceStore::read(&dir).unwrap();
    assert!(events.is_empty());
    assert_eq!(report.unsupported_schema, 1);
}
