//! `aibus run` end to end on the fake provider.

use std::process::{Command, Output};

use aibus_core::replay::replay_hash;
use tempfile::TempDir;

fn aibus(dir: &TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aibus"))
        .arg("run")
        .args(args)
        .current_dir(dir.path())
        .output()
        .unwrap()
}

fn ndjson(out: &Output) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn explore_prints_the_msg_sequence_and_records_a_replayable_fixture() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("README.md"), "hello").unwrap();
    let rec = dir.path().join("rec");
    let out = aibus(
        &dir,
        &["--agent", "explore", "--record", rec.to_str().unwrap(), "map this repo"],
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let lines = ndjson(&out);
    let mut names: Vec<&str> = lines.iter().map(|l| l["msg"].as_str().unwrap()).collect();
    names.dedup();
    assert_eq!(
        names,
        [
            "RunStarted",
            "Stream",
            "ToolCallRequested",
            "ToolCallFinished",
            "Stream",
            "RunFinished",
            "result"
        ]
    );
    let result = lines.last().unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(result["turns"], 2);

    let jsonl = std::fs::read_to_string(rec.join("msgs.jsonl")).unwrap();
    let hash = std::fs::read_to_string(rec.join("snapshot.hash")).unwrap();
    assert_eq!(replay_hash(&jsonl).unwrap(), hash.trim());
    assert!(rec.join("script.ron").is_file());
}

#[test]
fn bad_usage_exits_2() {
    let dir = TempDir::new().unwrap();
    assert_eq!(aibus(&dir, &[]).status.code(), Some(2));
    assert_eq!(aibus(&dir, &["--agent", "nope", "hi"]).status.code(), Some(2));
    assert_eq!(aibus(&dir, &["--model", "gpt-9", "hi"]).status.code(), Some(2));
}

#[test]
fn a_failing_provider_exits_10() {
    let dir = TempDir::new().unwrap();
    let script = dir.path().join("fail.ron");
    std::fs::write(
        &script,
        r#"[Error((kind: Server, retryable: false, message: "down"))]"#,
    )
    .unwrap();
    let out = aibus(
        &dir,
        &["--agent", "explore", "--script", script.to_str().unwrap(), "hi"],
    );
    assert_eq!(out.status.code(), Some(10));
    assert_eq!(ndjson(&out).last().unwrap()["status"], "failed");
}
