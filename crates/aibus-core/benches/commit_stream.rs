//! A1.3 spike: the cost of one `Stream` message through `App::step` (reduce, one commit with
//! recompute, effects) on the 03 section 11 reference session: 100k transcript lines open in
//! the transcript view, 8 runs streaming, 2,000 log rows.
//!
//! Prints p50 / p99 / max in microseconds. Exits non-zero over budget only when
//! `AIBUS_BENCH_GATE=1`, so a miss is still recorded.

use std::time::Instant;

use aibus_core::state::derive::transcript_rows;
use aibus_core::state::run_key;
use aibus_core::*;
use aibus_graph::mini::MiniGraph;
use aibus_graph::{Graph, Value};
use aibus_provider::StreamChunk;

const BUDGET_US: f64 = 4000.0;
const SAMPLES: usize = 5000;

fn line(i: usize) -> String {
    format!("line {i:06} the quick brown fox jumps over the lazy dog and keeps going a while\n")
}

fn pct(sorted: &[f64], p: usize) -> f64 {
    sorted[(sorted.len() * p / 100).min(sorted.len() - 1)]
}

fn main() {
    let mut app = App::new(MiniGraph::new(), || 1_700_000_000_000);
    let runs: Vec<RunId> = (0..8).map(|_| RunId::new()).collect();
    for r in &runs {
        app.step(Msg::RunStarted {
            run: *r,
            agent: "worker".parse().unwrap(),
            role: Role::Worker,
            parent: None,
            model: ModelId("m".into()),
            account: AccountName("a".into()),
        })
        .unwrap();
    }
    let stream = |run: RunId, text: String| Msg::Stream {
        run,
        chunks: vec![
            StreamChunk::TextDelta(text),
            StreamChunk::Usage(Usage {
                output_tokens: 3,
                ..Usage::default()
            }),
        ],
    };

    // 100k lines into run 0 (about 7 MB), in 4 KiB frames.
    let mut chunk = String::new();
    for i in 0..100_000 {
        chunk.push_str(&line(i));
        if chunk.len() >= 4096 {
            app.step(stream(runs[0], std::mem::take(&mut chunk)))
                .unwrap();
        }
    }
    app.step(stream(runs[0], chunk)).unwrap();
    for r in &runs[1..] {
        app.step(stream(*r, line(0).repeat(500))).unwrap();
    }
    for i in 0..2000 {
        app.step(Msg::IoError {
            context: format!("ctx {i}"),
            error: "e".into(),
        })
        .unwrap();
    }

    // The deriveds a widget set would add: the open transcript, a card per run, the status bar.
    let width = 100;
    let open = runs[0].to_string();
    let key = format!("view:transcript:{open}:lines");
    let t = app.graph.derived(
        &key.as_str().into(),
        transcript_rows(app.store.clone(), open, width),
    );
    for r in &runs {
        let id = r.to_string();
        app.graph.derived(
            &format!("view:card:{id}").as_str().into(),
            Box::new(move |t| {
                let st = t
                    .get_key(&run_key(&id, "status"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let us = t
                    .get_key(&run_key(&id, "usage"))
                    .cloned()
                    .unwrap_or(Value::Null);
                Value::List(vec![st, us].into())
            }),
        );
    }
    app.graph.derived(
        &"view:status_bar".into(),
        Box::new(|t| {
            t.get_key("model:orchestrator:counts")
                .cloned()
                .unwrap_or(Value::Null)
        }),
    );
    // One warm-up commit fills the wrapped-line cache for every sealed block.
    app.step(stream(runs[0], line(1))).unwrap();
    let rows = app.graph.read(t).clone();

    let mut samples = Vec::with_capacity(SAMPLES);
    for i in 0..SAMPLES {
        let msg = stream(runs[i % 8], line(i));
        let start = Instant::now();
        app.step(msg).unwrap();
        samples.push(start.elapsed().as_nanos() as f64 / 1000.0);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let (p50, p99, max) = (
        pct(&samples, 50),
        pct(&samples, 99),
        samples[samples.len() - 1],
    );
    // Worst case: every message goes to the run whose transcript is open, so each commit
    // recomputes the transcript derived.
    let mut open_only = Vec::with_capacity(SAMPLES);
    for i in 0..SAMPLES {
        let msg = stream(runs[0], line(i));
        let start = Instant::now();
        app.step(msg).unwrap();
        open_only.push(start.elapsed().as_nanos() as f64 / 1000.0);
    }
    open_only.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "stream_commit_open_run_only: p50 {:.0} us, p99 {:.0} us, max {:.0} us",
        pct(&open_only, 50),
        pct(&open_only, 99),
        open_only[open_only.len() - 1]
    );
    let p99 = p99.max(pct(&open_only, 99));
    let snap = serde_json::to_string(&app.graph.snapshot()).unwrap().len();
    println!("transcript rows at width {width}: {rows:?}");
    println!(
        "store content {} KiB, snapshot {} bytes, graph time {}",
        app.store.content_bytes() / 1024,
        snap,
        app.graph.now()
    );
    println!("stream_commit_100k_lines_8_runs_2000_log: p50 {p50:.0} us, p99 {p99:.0} us, max {max:.0} us (budget {BUDGET_US:.0} us p99)");

    // The alternative the spec writes (tail as a `Str` node): one commit of a 48 KiB string.
    let mut g = MiniGraph::new();
    let tail = g.input(&"alt:tail".into(), Value::Null);
    let mut alt = Vec::with_capacity(2000);
    for i in 0..2000 {
        let text: String = "x".repeat(48 * 1024 + i % 7);
        let start = Instant::now();
        g.commit("alt", &[(tail, Value::Str(text.as_str().into()))])
            .unwrap();
        alt.push(start.elapsed().as_nanos() as f64 / 1000.0);
    }
    alt.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "alt_tail_as_str_48KiB: p50 {:.0} us, p99 {:.0} us; retained over 4096 commits about {} MiB",
        pct(&alt, 50),
        pct(&alt, 99),
        4096 * 48 / 1024
    );

    if p99 > BUDGET_US {
        println!("WARNING: p99 over the {BUDGET_US:.0} us budget");
        if std::env::var("AIBUS_BENCH_GATE").as_deref() == Ok("1") {
            std::process::exit(1);
        }
    }
}
