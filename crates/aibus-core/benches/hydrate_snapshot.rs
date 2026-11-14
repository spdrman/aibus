//! Hydrating the reference snapshot (10 section 9): the snapshot of the 03 section 11 session
//! (100k transcript lines behind handles, 8 runs, 2,000 log rows) into a fresh `App`, which is
//! what `session resume` does after it has read the file. Gated against the baseline for the
//! runner class; the budget is a generous ceiling, since the spec gives no number.

use std::time::Instant;

use aibus_core::state::migrate::{chain, version_of};
use aibus_core::App;
use aibus_graph::mini::MiniGraph;
use aibus_graph::{Graph, Snapshot};

#[path = "shared/fixture.rs"]
mod fixture;
#[path = "../../../benches/gate.rs"]
mod gate;

const BUDGET_US: f64 = 100_000.0;
const SAMPLES: usize = 100;
const ROUNDS: usize = 3;

fn main() {
    let mut gate = gate::Gate::new();
    let (app, _) = fixture::big_session(MiniGraph::new);
    let snap = app.graph.snapshot();
    let json = serde_json::to_string(&snap).unwrap();
    println!(
        "reference snapshot: {} inputs, {} bytes of JSON",
        snap.inputs.len(),
        json.len()
    );

    let hydrate = gate::Gate::best_of(ROUNDS, || {
        (0..SAMPLES)
            .map(|_| {
                let snap = snap.clone();
                let mut fresh = App::new(MiniGraph::new(), || 0);
                let start = Instant::now();
                fresh.hydrate(snap, &[]).unwrap();
                start.elapsed().as_nanos() as f64 / 1000.0
            })
            .collect()
    });
    // The whole resume path after the bytes are read: parse, migrate, hydrate.
    let resume = gate::Gate::best_of(ROUNDS, || {
        (0..SAMPLES)
            .map(|_| {
                let mut fresh = App::new(MiniGraph::new(), || 0);
                let start = Instant::now();
                let snap: Snapshot = serde_json::from_str(&json).unwrap();
                let snap = chain(version_of(&snap), snap).unwrap();
                fresh.hydrate(snap, &[]).unwrap();
                start.elapsed().as_nanos() as f64 / 1000.0
            })
            .collect()
    });
    gate.judge("hydrate_snapshot-mini-hydrate", &hydrate, BUDGET_US);
    gate.judge(
        "hydrate_snapshot-mini-parse-and-hydrate",
        &resume,
        BUDGET_US,
    );
    gate.finish();
}
