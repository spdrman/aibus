//! Soft micro-bench (A1.2): 300 nodes, one input write that reaches 10 deriveds, time per commit.
//! Prints p50 / p99 / mean in microseconds and a warning if p50 is over the 50 us budget.

use std::hint::black_box;
use std::time::Instant;

use aibus_graph::mini::MiniGraph;
use aibus_graph::{Graph, Value};

fn main() {
    let mut g = MiniGraph::new();
    let hot = g.input(&"model:hot".into(), Value::I64(0));
    // A chain of 10 deriveds hangs off `hot`: all ten are affected by one write.
    let mut prev = hot;
    for i in 0..10 {
        let from = prev;
        prev = g.derived(
            &format!("view:chain:{i}").into(),
            Box::new(move |t| match t.get(from) {
                Value::I64(n) => Value::I64(n + 1),
                _ => Value::Null,
            }),
        );
    }
    // The other 289 nodes (49 inputs, 240 deriveds) are not touched by the write.
    let cold: Vec<_> = (0..49)
        .map(|i| g.input(&format!("model:cold:{i}").into(), Value::I64(i)))
        .collect();
    for i in 0..240 {
        let (a, b) = (cold[i % 49], cold[(i * 7 + 1) % 49]);
        g.derived(
            &format!("view:cold:{i}").into(),
            Box::new(move |t| match (t.get(a), t.get(b)) {
                (Value::I64(x), Value::I64(y)) => Value::I64(x + y),
                _ => Value::Null,
            }),
        );
    }

    let mut samples = Vec::with_capacity(20_000);
    for i in 0..20_000i64 {
        let writes = [(hot, Value::I64(i + 1))];
        let start = Instant::now();
        black_box(g.commit("bench", black_box(&writes)).unwrap());
        samples.push(start.elapsed().as_nanos() as f64 / 1000.0);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let (p50, p99) = (
        samples[samples.len() / 2],
        samples[samples.len() * 99 / 100],
    );
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    println!("recompute_300_nodes_10_affected: p50 {p50:.1} us, p99 {p99:.1} us, mean {mean:.1} us (budget 50 us, soft)");
    if p50 > 50.0 {
        println!("WARNING: over the soft budget");
    }
}
