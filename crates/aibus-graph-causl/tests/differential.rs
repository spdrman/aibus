#![cfg(feature = "causl-rs")]
//! The recorded `explore-fake` message stream replayed through both engines must leave
//! byte-identical snapshots and the same graph time (spec 03, section 3).

use aibus_core::replay::msg_from_json;
use aibus_core::App;
use aibus_graph::mini::MiniGraph;
use aibus_graph::Graph;
use aibus_graph_causl::CauslGraph;

fn replay<G: Graph>(graph: G, jsonl: &str) -> (Vec<u8>, u64, usize) {
    let mut app = App::new(graph, || 0);
    for line in jsonl.lines().filter(|l| !l.is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        app.step(msg_from_json(&v).unwrap()).unwrap();
    }
    (
        serde_json::to_vec(&app.graph.snapshot()).unwrap(),
        app.graph.now(),
        app.graph.commit_log().len(),
    )
}

#[test]
fn the_recorded_stream_leaves_identical_snapshots_on_both_engines() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/replay/explore-fake/msgs.jsonl"
    );
    let jsonl = std::fs::read_to_string(path).unwrap();
    let mini = replay(MiniGraph::new(), &jsonl);
    let causl = replay(CauslGraph::new(), &jsonl);
    assert!(mini.1 > 0, "the stream commits something");
    assert_eq!(mini, causl);
}
