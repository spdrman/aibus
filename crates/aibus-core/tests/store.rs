mod common;

use std::sync::Arc;

use aibus_core::state::store::*;
use aibus_graph::{Graph, Value};
use common::*;

fn sample(n: usize) -> String {
    // Multi-byte characters and newlines, so sealing has to find boundaries.
    (0..n)
        .map(|i| format!("line {i} caf\u{e9} \u{2603}\n"))
        .collect()
}

#[test]
fn the_tail_seals_at_64_kib_keeping_about_32_kib_back() {
    let s = Store::new();
    let id = s.new_transcript();
    let (b0, t0) = s.handles(id);

    assert!(!s.append(id, &"a".repeat(60 * 1024)));
    let (b1, t1) = s.handles(id);
    assert_eq!(b1.version, b0.version, "no block sealed yet");
    assert_ne!(
        t1.version, t0.version,
        "the tail handle moves on every append"
    );

    assert!(
        s.append(id, &"b".repeat(8 * 1024)),
        "68 KiB crosses the line"
    );
    let (b2, _) = s.handles(id);
    assert_ne!(b2.version, b1.version);
    let blocks = s.blocks(id);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].text.len(), SEAL_BYTES);
    assert_eq!(s.tail(id).len(), 68 * 1024 - SEAL_BYTES);
    assert!(s.tail(id).len() <= TAIL_MAX);
}

#[test]
fn sealed_blocks_are_immutable_and_nothing_is_lost() {
    let s = Store::new();
    let id = s.new_transcript();
    let all = sample(20_000);
    let mut first: Option<Arc<Block>> = None;
    for chunk in all.as_bytes().chunks(5000) {
        // Chunks can split a character; feed whole characters only.
        let text = String::from_utf8_lossy(chunk).into_owned();
        s.append(id, &text);
        if first.is_none() {
            first = s.blocks(id).first().cloned();
        }
    }
    let first = first.expect("something sealed");
    assert!(
        Arc::ptr_eq(&first, &s.blocks(id)[0]),
        "an early block is the same allocation"
    );
    assert!(s.tail(id).len() <= TAIL_MAX);
    for b in s.blocks(id) {
        assert!(b.text.len() <= TAIL_MAX);
    }
    // blocks then tail is the stream, up to the lossy chunk edges (a replacement char each).
    let joined: String = s
        .blocks(id)
        .iter()
        .map(|b| b.text.to_string())
        .collect::<String>()
        + &s.tail(id);
    assert!(joined.starts_with("line 0 caf"));
    assert!(joined.len() >= all.len() - 64);
}

#[test]
fn one_huge_append_seals_repeatedly() {
    let s = Store::new();
    let id = s.new_transcript();
    s.append(id, &"x".repeat(300 * 1024));
    assert!(s.tail(id).len() <= TAIL_MAX);
    assert!(s.blocks(id).len() >= 7);
}

#[test]
fn the_log_is_a_ring_of_2000_rows() {
    let s = Store::new();
    for i in 0..2500 {
        s.log_push(Value::I64(i));
    }
    assert_eq!(s.log_len(), LOG_RING);
    let rows = s.log_rows(3);
    assert_eq!(
        rows,
        vec![Value::I64(2497), Value::I64(2498), Value::I64(2499)]
    );
    assert_eq!(s.log_rows(10_000).first(), Some(&Value::I64(500)));
    let v = s.log_handle().version;
    s.log_push(Value::Null);
    assert_ne!(s.log_handle().version, v);
}

#[test]
fn evict_finished_keeps_only_the_summary_head() {
    let s = Store::new();
    let id = s.new_transcript();
    s.append(id, &sample(10_000));
    assert!(s.content_bytes() > 100 * 1024);
    let before = s.handles(id);
    s.evict_finished(id);
    let after = s.handles(id);
    assert_ne!(before.0.version, after.0.version);
    assert_ne!(before.1.version, after.1.version);
    assert!(s.blocks(id).is_empty());
    assert!(s.tail(id).is_empty());
    assert!(s.head(id).len() <= SUMMARY_HEAD);
    assert!(s.head(id).starts_with("line 0 caf"));
    assert!(s.content_bytes() <= SUMMARY_HEAD);
}

#[test]
fn wrapped_lines_are_cached_per_block_and_width() {
    let s = Store::new();
    let id = s.new_transcript();
    s.append(id, &sample(8_000));
    let b = s.blocks(id)[0].clone();
    let a1 = s.wrapped(&b, 40);
    let a2 = s.wrapped(&b, 40);
    assert!(Arc::ptr_eq(&a1, &a2), "second call is a cache hit");
    let w = s.wrapped(&b, 80);
    assert!(!Arc::ptr_eq(&a1, &w));
    assert!(w.len() <= a1.len());
    let (sealed, tail) = s.rows(id, 80);
    assert!(sealed >= w.len());
    assert!(tail > 0);
}

#[test]
fn the_snapshot_does_not_grow_with_transcript_content() {
    let size = |text: String| {
        let mut a = app();
        a.step(started(R())).unwrap();
        for chunk in text.as_bytes().chunks(4096) {
            a.step(text_stream(R(), &String::from_utf8_lossy(chunk)))
                .unwrap();
        }
        (
            serde_json::to_string(&a.graph.snapshot()).unwrap().len(),
            a.store.content_bytes(),
        )
    };
    let (small, small_content) = size("hello\n".repeat(10));
    let (big, big_content) = size(sample(150_000));
    assert!(
        big_content > 2_000_000,
        "the big session really holds megabytes"
    );
    assert!(small_content < 1000);
    assert!(
        big < small + 300,
        "snapshot grew from {small} to {big} bytes while content grew from {small_content} to {big_content}"
    );
    assert!(big < 20_000);
}
