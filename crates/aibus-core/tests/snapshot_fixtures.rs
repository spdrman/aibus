//! `fixtures/snapshots/v*/` (03 section 9): a snapshot written by every schema version that has
//! shipped. Each one must still migrate and hydrate.

use std::path::PathBuf;

use aibus_core::replay::msg_from_json;
use aibus_core::state::migrate::{chain, version_of};
use aibus_core::state::{SCHEMA, SCHEMA_VERSION};
use aibus_core::App;
use aibus_graph::mini::MiniGraph;
use aibus_graph::{Graph, Snapshot, Value};

fn root() -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "../../fixtures/snapshots"].iter().collect()
}

/// `(schema version from the directory name, file)`.
fn fixtures() -> Vec<(i64, PathBuf)> {
    let mut found = Vec::new();
    for dir in std::fs::read_dir(root()).unwrap().flatten() {
        let name = dir.file_name().to_string_lossy().into_owned();
        let version: i64 = name.strip_prefix('v').and_then(|v| v.parse().ok()).unwrap();
        for f in std::fs::read_dir(dir.path()).unwrap().flatten() {
            found.push((version, f.path()));
        }
    }
    found.sort();
    found
}

#[test]
fn every_fixture_migrates_and_hydrates() {
    let all = fixtures();
    assert!(
        (1..=SCHEMA).all(|v| all.iter().any(|(fv, _)| *fv == v)),
        "every schema version up to {SCHEMA} needs a fixture: {all:?}"
    );
    for (version, file) in all {
        let snap: Snapshot = serde_json::from_str(&std::fs::read_to_string(&file).unwrap())
            .unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        assert_eq!(version_of(&snap), version, "{}", file.display());
        let snap = chain(version, snap).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        let mut app = App::new(MiniGraph::new(), || 0);
        app.hydrate(snap, &[]).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        assert_eq!(app.graph.read_key(SCHEMA_VERSION), Some(&Value::I64(SCHEMA)));
        assert!(app.graph.read_key("model:session:id").is_some());
    }
}

/// Writes the fixture for the current schema from the recorded explore run. Run it once when
/// `SCHEMA` goes up, with `--ignored`; never to refresh an older version's file.
#[test]
#[ignore]
fn write_the_fixture_for_the_current_schema() {
    let msgs = std::fs::read_to_string(
        [env!("CARGO_MANIFEST_DIR"), "../../tests/replay/explore-fake/msgs.jsonl"]
            .iter()
            .collect::<PathBuf>(),
    )
    .unwrap();
    let mut app = App::new(MiniGraph::new(), || 0);
    for line in msgs.lines() {
        app.step(msg_from_json(&serde_json::from_str(line).unwrap()).unwrap()).unwrap();
    }
    let dir = root().join(format!("v{SCHEMA}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("explore-fake.json"),
        serde_json::to_string_pretty(&app.graph.snapshot()).unwrap() + "\n",
    )
    .unwrap();
}
