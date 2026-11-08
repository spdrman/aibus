//! The first replay fixture (10 section 5): `aibus run --agent explore "map this repo"` on the
//! fake, recorded with `--record`. Replaying `msgs.jsonl` through `reduce` on the mini engine
//! must reproduce `snapshot.hash`.

use std::path::PathBuf;

use aibus_core::replay::{msg_from_json, msg_to_json, replay_hash};
use aibus_provider::fake::Script;

fn fixture(file: &str) -> String {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "../../tests/replay/explore-fake",
        file,
    ]
    .iter()
    .collect();
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn the_recorded_stream_reproduces_the_snapshot_hash() {
    let hash = replay_hash(&fixture("msgs.jsonl")).unwrap();
    assert_eq!(hash, fixture("snapshot.hash").trim());
}

#[test]
fn the_recorded_stream_is_the_m1_shape() {
    let mut names: Vec<String> = fixture("msgs.jsonl")
        .lines()
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            v["msg"].as_str().unwrap().to_owned()
        })
        .collect();
    names.dedup();
    assert_eq!(
        names,
        [
            "RunStarted",
            "Stream",
            "ToolCallRequested",
            "ToolCallFinished",
            "Stream",
            "RunFinished"
        ]
    );
}

#[test]
fn every_recorded_line_survives_a_decode_and_encode() {
    for line in fixture("msgs.jsonl").lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let msg = msg_from_json(&v).unwrap();
        assert_eq!(msg_to_json(&msg).unwrap(), v);
    }
}

#[test]
fn the_script_is_the_one_the_fake_plays() {
    let script = Script::from_ron(&fixture("script.ron")).unwrap();
    assert!(script.remaining() > 0);
}

#[test]
fn dropping_a_message_changes_the_hash() {
    let full = fixture("msgs.jsonl");
    let without_last: String = full.lines().rev().skip(1).collect::<Vec<_>>().into_iter().rev().map(|l| format!("{l}\n")).collect();
    assert_ne!(replay_hash(&without_last).unwrap(), replay_hash(&full).unwrap());
}
