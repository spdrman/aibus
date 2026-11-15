//! #328: the id generator is seeded from the store, a planted maximum id does not wrap to time 0,
//! the counter-full branch keeps order, and "the same line twice" means exactly what the spec says.

use std::fs;
use std::io::Write;
use std::time::{Duration, SystemTime};

use aibus_core::Redactor;
use aibus_trace::view::Recorded;
use aibus_trace::{
    AttemptRef, Batch, EventIds, EventKind, Origin, PayloadPolicy, StoreLimits, TraceEvent,
    TraceStore, Visibility,
};
use uuid::Uuid;

const MS: u64 = 1_800_000_000_000;
const MAX_MS: u64 = 0xffff_ffff_ffff;

fn at(ms: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_millis(ms)
}

fn v7(ms: u64, counter: u16) -> Uuid {
    let mut b = [0u8; 16];
    b[..6].copy_from_slice(&ms.to_be_bytes()[2..]);
    b[6] = 0x70 | (counter >> 8) as u8;
    b[7] = counter as u8;
    b[8] = 0x80;
    Uuid::from_bytes(b)
}

fn time_of(id: Uuid) -> u64 {
    id.as_bytes()[..6]
        .iter()
        .fold(0, |a, x| a << 8 | u64::from(*x))
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
        at(MS),
        "test/1",
    )
}

fn batch(events: Vec<TraceEvent>) -> Batch {
    Batch {
        events,
        payload: None,
    }
}

fn open(dir: &std::path::Path, limits: StoreLimits) -> TraceStore {
    TraceStore::open(dir, Redactor::new(), PayloadPolicy::MetadataOnly, limits)
        .unwrap()
        .0
}

#[test]
fn a_store_holding_a_future_dated_id_seeds_the_generator_328() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    let future = v7(MS + 60_000, 7);
    let mut s = open(&dir, StoreLimits::default());
    s.append(batch(vec![ev(future, EventKind::AttemptStarted)]))
        .unwrap();
    drop(s);
    let mut ids = open(&dir, StoreLimits::default()).ids();
    // A late writer: its clock is a minute behind the id on disk.
    let next = ids.next(at(MS), &[b"late"]);
    assert!(next > future, "{next} sorts before {future}");
}

#[test]
fn a_late_writers_events_never_sort_before_earlier_ones_328() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    let mut ids = EventIds::default();
    let mut s = open(&dir, StoreLimits::default());
    let first: Vec<TraceEvent> = (0..3u8)
        .map(|i| ev(ids.next(at(MS + 5_000), &[&[i]]), EventKind::MessageChunk))
        .collect();
    s.append(batch(first)).unwrap();
    drop(s);
    let mut ids = open(&dir, StoreLimits::default()).ids();
    let late = ev(ids.next(at(MS), &[b"x"]), EventKind::MessageChunk);
    let late_id = late.event_id;
    let mut s = open(&dir, StoreLimits::default());
    s.append(batch(vec![late])).unwrap();
    drop(s);
    let (events, _) = TraceStore::read(&dir).unwrap();
    assert_eq!(
        events.last().unwrap().event_id,
        late_id,
        "the late writer sorts last"
    );
}

#[test]
fn a_planted_maximum_id_does_not_wrap_the_next_id_to_time_zero_328() {
    // Decision: clamp. The time saturates at 2^48 - 1 and the counter stays full, so ids keep
    // their order by their hash tail, never by a restart from time 0.
    let planted = Uuid::from_bytes([
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f, 0xff, 0xbf, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff,
    ]);
    let mut ids = EventIds::after(planted);
    for i in 0..3u8 {
        let next = ids.next(at(MS), &[&[i]]);
        assert_eq!(time_of(next), MAX_MS, "{next} wrapped");
        assert_eq!(next.get_version_num(), 7);
    }
}

#[test]
fn a_store_whose_last_id_is_the_maximum_writes_its_own_events_last_328() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    let mut s = open(&dir, StoreLimits::default());
    s.append(batch(vec![ev(
        v7(MAX_MS, 0x0fff),
        EventKind::AttemptStarted,
    )]))
    .unwrap();
    drop(s);
    fs::OpenOptions::new()
        .append(true)
        .open(dir.join("events.jsonl"))
        .unwrap()
        .write_all(b"{\"torn")
        .unwrap();
    drop(open(&dir, StoreLimits::default()));
    let (events, _) = TraceStore::read(&dir).unwrap();
    let warning = events
        .iter()
        .find(|e| e.kind == EventKind::TraceWarning)
        .unwrap();
    assert_eq!(
        time_of(warning.event_id),
        MAX_MS,
        "the warning wrapped to time 0"
    );
}

#[test]
fn a_full_counter_moves_to_the_next_millisecond_and_keeps_order_328() {
    let mut ids = EventIds::after(v7(MS, 0x0ffe));
    let a = ids.next(at(MS), &[b"a"]);
    let b = ids.next(at(MS), &[b"b"]);
    let c = ids.next(at(MS), &[b"c"]);
    assert_eq!(time_of(a), MS);
    assert_eq!(time_of(b), MS + 1, "full counter moves on");
    assert_eq!(time_of(c), MS + 1);
    assert!(a < b && b < c, "{a} {b} {c}");
    // And a long burst in one millisecond ascends strictly across the boundary.
    let mut ids = EventIds::default();
    let burst: Vec<Uuid> = (0..10_000u32)
        .map(|i| ids.next(at(MS), &[&i.to_le_bytes()]))
        .collect();
    assert!(burst.windows(2).all(|w| w[0] < w[1]));
    assert!(time_of(*burst.last().unwrap()) > MS);
}

#[test]
fn the_same_line_gives_the_same_id_only_from_a_fresh_generator_328() {
    let parts: [&[u8]; 2] = [b"attempt", b"raw line"];
    // A fresh generator on the same input: the same id (so a re-normalised file is the same file).
    assert_eq!(
        EventIds::default().next(at(MS), &parts),
        EventIds::default().next(at(MS), &parts)
    );
    // One generator fed the same line twice: two ids.
    let mut g = EventIds::default();
    assert_ne!(g.next(at(MS), &parts), g.next(at(MS), &parts));
    // A seeded generator: not the same id as a fresh one.
    let mut seeded = EventIds::after(v7(MS + 1, 0));
    assert_ne!(
        seeded.next(at(MS), &parts),
        EventIds::default().next(at(MS), &parts)
    );
}

#[test]
fn appending_an_id_twice_is_idempotent_whichever_generator_made_it_328() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    let id = EventIds::default().next(at(MS), &[b"line"]);
    let mut s = open(&dir, StoreLimits::default());
    assert_eq!(
        s.append(batch(vec![ev(id, EventKind::MessageChunk)]))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        s.append(batch(vec![ev(id, EventKind::MessageChunk)]))
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn a_storage_gap_sorts_after_the_batch_that_caused_it_328() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    let limits = StoreLimits {
        soft_max_bytes: 0,
        ..StoreLimits::default()
    };
    let mut s = open(&dir, limits);
    let started = v7(MS, 1);
    s.append(batch(vec![
        ev(started, EventKind::AttemptStarted),
        ev(v7(MS, 2), EventKind::MessageChunk),
    ]))
    .unwrap();
    drop(s);
    let (events, _) = TraceStore::read(&dir).unwrap();
    let gap = events
        .iter()
        .position(|e| e.kind == EventKind::GapDetected)
        .unwrap();
    assert_eq!(
        gap,
        events.len() - 1,
        "the gap is last: {:?}",
        events.iter().map(|e| e.kind).collect::<Vec<_>>()
    );
    assert!(events[gap].event_id > started);
}

#[test]
fn a_recorded_view_orders_out_of_order_lines_by_id_328() {
    let mut text = String::new();
    for n in [3u16, 1, 2] {
        let mut v = serde_json::to_value(ev(v7(MS, n), EventKind::MessageChunk)).unwrap();
        v["schema_version"] = 2.into();
        text.push_str(&format!("{v}\n"));
    }
    let rec = Recorded::from_bytes(std::path::Path::new("run"), text.as_bytes());
    let got: Vec<Uuid> = rec.events.iter().map(|e| e.event_id).collect();
    assert_eq!(got, [v7(MS, 1), v7(MS, 2), v7(MS, 3)]);
    assert_eq!(
        rec.events.iter().map(|e| e.position).collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert!(rec.raw[0].starts_with(b"{") && rec.raw[0] != rec.raw[2]);
}
