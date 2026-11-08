#![cfg(feature = "causl-rs")]
//! `CauslGraph` against the A1.2 conformance suite. The suite file is the one `aibus-graph` runs
//! on `MiniGraph`; each case is its own test so a failure names the case.

#[path = "../../aibus-graph/tests/conformance/mod.rs"]
#[allow(dead_code)]
mod conformance;

use aibus_graph_causl::CauslGraph;

macro_rules! cases {
    ($($name:ident),* $(,)?) => {
        $(#[test] fn $name() { conformance::$name(CauslGraph::new); })*
    };
}

cases!(
    determinism,
    glitch_freedom,
    atomic_rollback,
    time,
    subscriber_order,
    dynamic_dependencies,
    equality_cutoff,
    retention,
    time_travel,
    layering,
    explain_and_metadata,
    simulate,
    hydrate_round_trip,
);
