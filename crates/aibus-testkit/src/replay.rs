//! The replay harness of 10 section 5, generic over the graph backend.
//!
//! A fixture is a directory under `tests/replay/` with `msgs.jsonl`, `script.ron`,
//! `snapshot.hash` and `views.json`, all written by `aibus run --record`. A second backend is
//! tested by calling the same functions with its constructor; see `docs/TESTING.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aibus_core::replay::{replay_into, snapshot_hash};
use aibus_core::App;
use aibus_graph::Graph;
use aibus_provider::fake::Script;

/// What one fixture directory recorded.
pub struct Fixture {
    pub name: String,
    pub msgs: String,
    pub hash: String,
    pub views: BTreeMap<String, serde_json::Value>,
}

/// A replay must finish inside this, whatever the backend (10 section 5).
pub const REPLAY_BUDGET: Duration = Duration::from_secs(1);

/// `tests/replay/` at the repository root.
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/replay")
}

/// Every fixture, sorted by name. Each must hold all four files and a script the fake can play.
pub fn fixtures() -> Vec<Fixture> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root())
        .expect("tests/replay exists")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs.iter()
        .map(|d| {
            let name = d.file_name().unwrap().to_string_lossy().into_owned();
            let read = |f: &str| {
                std::fs::read_to_string(d.join(f)).unwrap_or_else(|e| panic!("{name}/{f}: {e}"))
            };
            Script::from_ron(&read("script.ron"))
                .unwrap_or_else(|e| panic!("{name}/script.ron: {e}"));
            Fixture {
                msgs: read("msgs.jsonl"),
                hash: read("snapshot.hash").trim().to_owned(),
                views: serde_json::from_str(&read("views.json"))
                    .unwrap_or_else(|e| panic!("{name}/views.json: {e}")),
                name,
            }
        })
        .collect()
}

/// Replays `fx` on the backend `mk` builds and asserts 10 section 5 items 1 and 3: the final
/// snapshot hash and every `view:` node equal what was recorded, inside `REPLAY_BUDGET`.
pub fn assert_replays<G: Graph>(fx: &Fixture, mk: fn() -> G) {
    let start = Instant::now();
    let mut app = App::new(mk(), || 0);
    replay_into(&mut app, &fx.msgs).unwrap_or_else(|e| panic!("{}: {e}", fx.name));
    let hash = snapshot_hash(&app.graph);
    let views = aibus_tui::replay_views(mk(), &fx.msgs).unwrap_or_else(|e| panic!("{}: {e}", fx.name));
    let took = start.elapsed();
    assert_eq!(hash, fx.hash, "{}: the snapshot hash moved", fx.name);
    assert_eq!(views, fx.views, "{}: a view: node moved", fx.name);
    assert!(took < REPLAY_BUDGET, "{}: replay took {took:?}", fx.name);
}

/// Item 2, the differential gate: two backends leave byte-identical snapshots and the same
/// `commit_log` intents, in the same order.
pub fn assert_same_on_both<A: Graph, B: Graph>(fx: &Fixture, a: fn() -> A, b: fn() -> B) {
    let mut left = App::new(a(), || 0);
    let mut right = App::new(b(), || 0);
    replay_into(&mut left, &fx.msgs).unwrap_or_else(|e| panic!("{}: {e}", fx.name));
    replay_into(&mut right, &fx.msgs).unwrap_or_else(|e| panic!("{}: {e}", fx.name));
    assert!(
        serde_json::to_vec(&left.graph.snapshot()).unwrap()
            == serde_json::to_vec(&right.graph.snapshot()).unwrap(),
        "{}: the snapshots differ",
        fx.name
    );
    let intents = |log: &[aibus_graph::Commit]| -> Vec<String> {
        log.iter().map(|c| c.intent.to_string()).collect()
    };
    assert_eq!(
        intents(left.graph.commit_log()),
        intents(right.graph.commit_log()),
        "{}: the commit logs differ",
        fx.name
    );
    assert_eq!(left.graph.now(), right.graph.now(), "{}: graph time differs", fx.name);
}
