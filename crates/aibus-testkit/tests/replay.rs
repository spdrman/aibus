//! 10 section 5: every fixture under `tests/replay/` replays to the recorded snapshot hash and
//! `view:` nodes on the mini backend, inside a second. `aibus-graph-causl` runs the same
//! functions against `CauslGraph` (docs/TESTING.md, "Running the suites against another backend").

use std::collections::BTreeSet;

use aibus_core::replay::{msg_from_json, replay_hash};
use aibus_graph::mini::MiniGraph;
use aibus_testkit::replay::{assert_replays, assert_same_on_both, fixtures};

#[test]
fn there_are_about_twenty_fixtures() {
    let names: Vec<String> = fixtures().into_iter().map(|f| f.name).collect();
    assert!(names.len() >= 20, "{names:?}");
    assert!(names.contains(&"explore-fake".to_owned()));
}

#[test]
fn every_fixture_replays_to_its_hash_and_views_on_mini() {
    for fx in fixtures() {
        assert_replays(&fx, MiniGraph::new);
    }
}

#[test]
fn the_mini_backend_agrees_with_itself_which_is_what_the_differential_asserts_for_two() {
    for fx in fixtures() {
        assert_same_on_both(&fx, MiniGraph::new, MiniGraph::new);
    }
}

#[test]
fn every_recorded_line_decodes_and_every_view_is_a_view() {
    for fx in fixtures() {
        for line in fx.msgs.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            msg_from_json(&v).unwrap_or_else(|e| panic!("{}: {e}", fx.name));
        }
        assert!(!fx.views.is_empty(), "{}: no view: nodes recorded", fx.name);
        assert!(
            fx.views.keys().all(|k| k.starts_with("view:")),
            "{}",
            fx.name
        );
    }
}

#[test]
fn a_dropped_message_moves_the_hash() {
    for fx in fixtures() {
        let cut: String = fx
            .msgs
            .lines()
            .rev()
            .skip(1)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|l| format!("{l}\n"))
            .collect();
        assert_ne!(replay_hash(&cut).unwrap(), fx.hash, "{}", fx.name);
    }
}

/// The set of message kinds the fixtures cover is the spec's M1 to M5 exits taken together.
#[test]
fn the_fixtures_cover_the_milestone_exits() {
    let mut seen = BTreeSet::new();
    for fx in fixtures() {
        for line in fx.msgs.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            seen.insert(v["msg"].as_str().unwrap().to_owned());
        }
    }
    for want in [
        "RunStarted",
        "Stream",
        "ToolCallRequested",
        "ToolCallFinished",
        "RunFinished",
        "RunFailed",
        "PermissionNeeded",
        "PermissionDecided",
        "UsageUpdate",
        "HookFired",
        "RouterAnswered",
        "AdvisorSpoke",
        "CheckpointHit",
        "Compacted",
    ] {
        assert!(seen.contains(want), "no fixture records {want}: {seen:?}");
    }
}
