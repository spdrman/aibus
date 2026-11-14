//! The cost of one `Stream` message through `App::step` (reduce, one commit with recompute,
//! effects) on the 03 section 11 reference session: 100k transcript lines open in the
//! transcript view, 8 runs streaming, 2,000 log rows. A gate (10 section 9):
//!
//! * a stream commit to the run whose transcript is open: under 2 ms p99 (08 section 2),
//! * any commit, the eight runs in turn: under 4 ms p99 (03 section 11),
//! * neither may regress over 20 % against `benches/baselines/<runner class>/`.
//!
//! `AIBUS_BENCH_GATE=1` makes a miss fail, see `benches/gate.rs` for the rest. `run` is generic
//! over the backend: `aibus-graph-causl/benches/commit_stream.rs` includes this file and calls it
//! with `causl`. The includer declares the `gate` and `fixture` modules this uses.

use std::time::Instant;

use aibus_core::state::derive::transcript_rows;
use aibus_core::state::run_key;
use aibus_graph::{Graph, Value};

use crate::fixture::{self, line, stream};
use crate::gate::{Gate, Stats};

const OPEN_BUDGET_US: f64 = 2000.0;
const ANY_BUDGET_US: f64 = 4000.0;
const SAMPLES: usize = 5000;
const ROUNDS: usize = 3;

/// Returns the open-transcript and the any-run statistics, so an includer can set two backends
/// side by side.
pub fn run<G: Graph>(label: &str, mk: fn() -> G) -> (Stats, Stats) {
    println!("backend: {label}");
    let mut gate = Gate::new();
    let (mut app, runs) = fixture::big_session(mk);

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

    // Every message goes to the run whose transcript is open, so each commit recomputes the
    // transcript derived: the worst case, and the 2 ms budget.
    let mut n = 0;
    let open_only = Gate::best_of(ROUNDS, || {
        (0..SAMPLES)
            .map(|_| {
                n += 1;
                let msg = stream(runs[0], line(n));
                let start = Instant::now();
                app.step(msg).unwrap();
                start.elapsed().as_nanos() as f64 / 1000.0
            })
            .collect()
    });
    // The eight runs in turn: seven of eight commits leave the transcript alone.
    let any = Gate::best_of(ROUNDS, || {
        (0..SAMPLES)
            .map(|i| {
                n += 1;
                let msg = stream(runs[i % 8], line(n));
                let start = Instant::now();
                app.step(msg).unwrap();
                start.elapsed().as_nanos() as f64 / 1000.0
            })
            .collect()
    });
    let snap = serde_json::to_string(&app.graph.snapshot()).unwrap().len();
    println!("transcript rows at width {width}: {rows:?}");
    println!(
        "store content {} KiB, snapshot {} bytes, graph time {}",
        app.store.content_bytes() / 1024,
        snap,
        app.graph.now()
    );
    gate.judge(
        &format!("commit_stream-{label}-open"),
        &open_only,
        OPEN_BUDGET_US,
    );
    gate.judge(&format!("commit_stream-{label}-any"), &any, ANY_BUDGET_US);
    println!(
        "stream_commit_100k_lines_8_runs_2000_log: p50 {:.0} us, p99 {:.0} us, max {:.0} us (budget {ANY_BUDGET_US:.0} us p99)",
        any.p50,
        any.p99.max(open_only.p99),
        any.max
    );
    gate.finish();
    (open_only, any)
}
