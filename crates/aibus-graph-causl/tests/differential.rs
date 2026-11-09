#![cfg(feature = "causl-rs")]
//! Every recorded stream under `tests/replay/` replayed through both engines must leave
//! byte-identical snapshots, the same graph time, the same commit intents in the same order
//! (spec 03 section 3, 10 section 5 item 2), and on `causl` the recorded snapshot hash.
//!
//! The `view:` nodes (item 3) need the dashboard crate; `docs/TESTING.md` says how to add
//! `aibus-testkit` as a dev-dependency here and call `aibus_testkit::replay::assert_replays`
//! and `assert_same_on_both` instead, which does all three items.

use aibus_core::replay::{replay_into, snapshot_hash};
use aibus_core::App;
use aibus_graph::mini::MiniGraph;
use aibus_graph::Graph;
use aibus_graph_causl::CauslGraph;

struct Replayed {
    snapshot: Vec<u8>,
    hash: String,
    now: u64,
    intents: Vec<String>,
}

fn replay<G: Graph>(graph: G, jsonl: &str) -> Replayed {
    let mut app = App::new(graph, || 0);
    replay_into(&mut app, jsonl).unwrap();
    Replayed {
        snapshot: serde_json::to_vec(&app.graph.snapshot()).unwrap(),
        hash: snapshot_hash(&app.graph),
        now: app.graph.now(),
        intents: app.graph.commit_log().iter().map(|c| c.intent.to_string()).collect(),
    }
}

#[test]
fn every_recorded_stream_leaves_identical_snapshots_on_both_engines() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/replay");
    let mut dirs: Vec<_> = std::fs::read_dir(root)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    assert!(dirs.len() >= 20, "{dirs:?}");
    for dir in dirs {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let jsonl = std::fs::read_to_string(dir.join("msgs.jsonl")).unwrap();
        let recorded = std::fs::read_to_string(dir.join("snapshot.hash")).unwrap();
        let mini = replay(MiniGraph::new(), &jsonl);
        let causl = replay(CauslGraph::new(), &jsonl);
        assert!(mini.now > 0, "{name}: the stream commits something");
        assert_eq!(causl.hash, recorded.trim(), "{name}: causl moved the hash");
        assert!(mini.snapshot == causl.snapshot, "{name}: the snapshots differ");
        assert_eq!(mini.now, causl.now, "{name}");
        assert_eq!(mini.intents, causl.intents, "{name}");
        assert_eq!(mini.hash, causl.hash, "{name}");
    }
}
