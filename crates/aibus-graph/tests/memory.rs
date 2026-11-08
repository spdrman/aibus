//! 4,096 retained commits that carry handle values stay under 4 MiB (spec 03, section 9).
//!
//! This is the only test in the file because it counts live heap bytes with a global allocator,
//! and a second test running beside it would pollute the count.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use aibus_graph::mini::MiniGraph;
use aibus_graph::{Graph, StoreHandle, Value, COMMIT_HISTORY_CAP};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards every call to `System` unchanged and only adds bookkeeping.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        LIVE.fetch_add(l.size(), Ordering::Relaxed);
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        LIVE.fetch_add(new, Ordering::Relaxed);
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        System.realloc(p, l, new)
    }
}

#[global_allocator]
static A: Counting = Counting;

fn handle(id: u64, version: u32) -> Value {
    Value::Handle(StoreHandle {
        store: "transcript".into(),
        id,
        version,
    })
}

#[test]
fn retained_history_with_handles_fits_in_4_mib() {
    const HANDLES: u64 = 8;
    let mut g = MiniGraph::new();
    let inputs: Vec<_> = (0..HANDLES)
        .map(|i| g.input(&format!("model:run:{i}:blocks").into(), handle(i, 0)))
        .collect();
    // One derived per handle (a view over it), so each commit changes 8 inputs and 8 deriveds.
    for (i, n) in inputs.iter().enumerate() {
        let n = *n;
        g.derived(
            &format!("view:run:{i}").into(),
            Box::new(move |t| t.get(n).clone()),
        );
    }
    let before = LIVE.load(Ordering::Relaxed);
    let measure = |g: &mut MiniGraph, from: u32, to: u32| {
        for v in from..to {
            let writes: Vec<_> = inputs
                .iter()
                .enumerate()
                .map(|(i, n)| (*n, handle(i as u64, v)))
                .collect();
            g.commit("stream", &writes).unwrap();
        }
        LIVE.load(Ordering::Relaxed) - before
    };
    let full = measure(&mut g, 1, COMMIT_HISTORY_CAP as u32 + 1);
    assert_eq!(g.commit_log().len(), COMMIT_HISTORY_CAP);
    let churn = measure(
        &mut g,
        COMMIT_HISTORY_CAP as u32 + 1,
        3 * COMMIT_HISTORY_CAP as u32,
    );
    println!("4096 retained commits (8 handle inputs + 8 deriveds each): {full} bytes; after {} more: {churn} bytes", 2 * COMMIT_HISTORY_CAP);
    assert!(full < 4 << 20, "{full} bytes after filling the window");
    assert!(churn < 4 << 20, "{churn} bytes with eviction running");
}
