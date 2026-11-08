//! `aibus session ...` and what `aibus run` leaves on disk (07 sections 1 to 4), end to end on
//! the fake provider.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{aibus, ndjson, stderr};
use serde_json::Value;
use tempfile::TempDir;

const FAKE_KEY: &str = "fake-key-DO-NOT-LOG-7f3a9c";

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(files(&p));
        } else {
            out.push(p);
        }
    }
    out
}

/// No file under `home`, compressed or not, holds the key.
fn assert_no_credential(home: &Path) {
    for f in files(home) {
        let raw = fs::read(&f).unwrap();
        let name = f.to_string_lossy().into_owned();
        let text = if name.ends_with(".zst") {
            zstd::stream::decode_all(raw.as_slice()).unwrap_or_default()
        } else {
            raw.clone()
        };
        for bytes in [raw, text] {
            assert!(
                !String::from_utf8_lossy(&bytes).contains(FAKE_KEY),
                "{} holds the credential",
                f.display()
            );
        }
    }
}

fn state(home: &Path) -> PathBuf {
    home.join(".local/state/aibus")
}

/// Runs the built-in explore script and returns the session id.
fn run(home: &Path, extra: &[&str], prompt: &str) -> String {
    let mut args = vec!["run", "--json", "--agent", "explore"];
    args.extend_from_slice(extra);
    args.push(prompt);
    let out = aibus(home, &args);
    assert!(out.status.success(), "{}", stderr(&out));
    ndjson(&out).last().unwrap()["session"].as_str().unwrap().to_owned()
}

fn json(home: &Path, args: &[&str]) -> Value {
    let mut a = vec!["--json"];
    a.extend_from_slice(args);
    let out = aibus(home, &a);
    assert!(out.status.success(), "{args:?}: {}", stderr(&out));
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn a_run_leaves_the_07_layout_and_no_credential() {
    let home = TempDir::new().unwrap();
    let id = run(home.path(), &[], &format!("map the repo, key {FAKE_KEY}"));
    let dir = state(home.path()).join("sessions").join(&id);
    for f in ["session.json", "graph.snapshot.json.zst", "commits.jsonl.zst", "msgs.jsonl.zst"] {
        assert!(dir.join(f).is_file(), "{f}");
    }
    let runs: Vec<_> = fs::read_dir(dir.join("runs")).unwrap().flatten().collect();
    assert_eq!(runs.len(), 1);
    let transcript = fs::read_to_string(runs[0].path().join("transcript.jsonl")).unwrap();
    assert!(transcript.contains("[redacted:fake]"), "{transcript}");
    assert!(runs[0].path().join("meta.json").is_file());
    assert!(home.path().join(".local/share/aibus/index.sqlite").is_file());
    assert_no_credential(home.path());
}

#[test]
fn list_and_show_read_the_index_and_deleting_it_loses_nothing() {
    let home = TempDir::new().unwrap();
    let id = run(home.path(), &["--topic", "layout"], "hi");

    let list = json(home.path(), &["session", "list"]);
    assert_eq!(list[0]["id"], id.as_str());
    assert_eq!(list[0]["runs"], 1);
    assert_eq!(list[0]["outcome"], "completed");
    assert!(list[0]["ended_at"].is_string());

    let shown = json(home.path(), &["session", "show", &id]);
    assert_eq!(shown["session"]["id"], id.as_str());
    assert_eq!(shown["session"]["lead"], "explore");
    assert_eq!(shown["runs"][0]["agent"], "explore");
    assert_eq!(shown["runs"][0]["status"], "completed");
    assert_eq!(shown["runs"][0]["turns"], 2);
    assert_eq!(shown["runs"][0]["usage"]["input_tokens"], 400);
    assert!(shown.get("handoff").is_some() && shown.get("ledger").is_some());

    let index = home.path().join(".local/share/aibus/index.sqlite");
    for f in files(index.parent().unwrap()) {
        fs::remove_file(f).unwrap();
    }
    assert_eq!(json(home.path(), &["session", "list"]), list);
    assert!(index.is_file(), "rebuilt");
    assert_eq!(json(home.path(), &["session", "list", "--rebuild"]), list);
    assert_no_credential(home.path());
}

#[test]
fn log_prints_the_commits_as_ndjson_and_replay_prints_the_recorded_hash() {
    let home = TempDir::new().unwrap();
    let rec = home.path().join("rec");
    let id = run(home.path(), &["--record", rec.to_str().unwrap()], "map this repo");

    let out = aibus(home.path(), &["session", "log", &id]);
    assert!(out.status.success(), "{}", stderr(&out));
    let lines = ndjson(&out);
    assert!(lines.len() >= 6);
    for l in &lines {
        assert!(l["time"].is_u64() && l["intent"].is_string() && l["changed"].is_u64());
        assert!(l["at"].as_str().unwrap().ends_with('Z'));
    }
    assert!(lines.iter().any(|l| l["intent"] == "RunFinished"));

    let out = aibus(home.path(), &["session", "replay", &id, "--provider", "fake"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let want = fs::read_to_string(rec.join("snapshot.hash")).unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), want.trim());
    assert_no_credential(home.path());
}

#[test]
fn export_renders_markdown_per_run_and_jsonl_on_stdout() {
    let home = TempDir::new().unwrap();
    let id = run(home.path(), &[], "map this repo");

    let out = aibus(home.path(), &["session", "export", &id, "--format", "md"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let path = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let md = fs::read_to_string(&path).unwrap();
    assert!(md.starts_with("# Run "), "{md}");
    assert!(md.contains("Tool call `ls`") && md.contains("Rust workspace"), "{md}");

    let out = aibus(home.path(), &["session", "export", &id, "--format", "jsonl"]);
    let lines = ndjson(&out);
    assert_eq!(lines.first().unwrap()["role"], "user");
    assert_eq!(lines.last().unwrap()["type"], "end");
    assert_no_credential(home.path());
}

#[test]
fn resume_hydrates_the_snapshot() {
    let home = TempDir::new().unwrap();
    let id = run(home.path(), &[], "hi");
    let out = json(home.path(), &["session", "resume", &id]);
    assert_eq!(out["session"], id.as_str());
    assert_eq!(out["runs"], 1);
    // The snapshot is the last thing written, so only `Persisted` comes after it.
    assert_eq!(out["replayed"], 1);
    assert_eq!(aibus(home.path(), &["session", "show", "../etc"]).status.code(), Some(2));
}

#[test]
fn prune_removes_old_sessions_unless_pinned_and_takes_hook_logs_with_them() {
    let home = TempDir::new().unwrap();
    let old = run(home.path(), &[], "one");
    let pinned = run(home.path(), &[], "two");
    let keep = state(home.path()).join("sessions").join(&pinned).join("session.json");
    let mut meta: Value = serde_json::from_str(&fs::read_to_string(&keep).unwrap()).unwrap();
    meta["pinned"] = true.into();
    fs::write(&keep, meta.to_string()).unwrap();
    let hooks = state(home.path()).join("logs/hooks").join(&old);
    fs::create_dir_all(&hooks).unwrap();
    fs::write(hooks.join("stop-1-0.log"), "x").unwrap();

    // Nothing is 30 days old yet.
    let kept = json(home.path(), &["session", "prune"]);
    assert_eq!(kept["removed"].as_array().unwrap().len(), 0);

    std::thread::sleep(std::time::Duration::from_millis(1100));
    let gone = json(home.path(), &["session", "prune", "--older-than", "0h"]);
    assert_eq!(gone["removed"], serde_json::json!([old]));
    assert!(!state(home.path()).join("sessions").join(&old).exists());
    assert!(!hooks.exists());
    assert!(state(home.path()).join("sessions").join(&pinned).is_dir());
    let list = json(home.path(), &["session", "list"]);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(aibus(home.path(), &["session", "prune", "--older-than", "soon"]).status.code(), Some(2));
}
