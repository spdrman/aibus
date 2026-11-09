#![cfg(feature = "causl-rs")]
//! Every recorded stream under `tests/replay/` through both engines (spec 03 section 3, 10
//! section 5): the recorded snapshot hash and every `view:` node on `causl`, then byte-identical
//! snapshots, the same graph time and the same commit intents in the same order against `mini`.
//! `docs/BUILD.md` says how to run it.

use aibus_graph::mini::MiniGraph;
use aibus_graph_causl::CauslGraph;
use aibus_testkit::replay::{assert_replays, assert_same_on_both, fixtures};

#[test]
fn every_fixture_replays_identically_on_both_engines() {
    let all = fixtures();
    assert!(all.len() >= 20, "{} fixtures", all.len());
    for fx in &all {
        assert_replays(fx, CauslGraph::new);
        assert_same_on_both(fx, MiniGraph::new, CauslGraph::new);
    }
}

#[test]
fn the_reference_session_leaves_the_same_snapshot_on_both_engines() {
    let (mini, _) = aibus_testkit::big_session(MiniGraph::new);
    let (causl, _) = aibus_testkit::big_session(CauslGraph::new);
    assert_eq!(
        aibus_core::replay::snapshot_hash(&mini.graph),
        aibus_core::replay::snapshot_hash(&causl.graph)
    );
}
