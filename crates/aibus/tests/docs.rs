//! Keeps the docs and the code pointing at each other: a test named in `docs/SECURITY.md` has to
//! exist, and the config snippet in the README has to start the binary the way it says.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every `` `path.rs::name` `` in SECURITY.md, and the `::name` continuations after it in the
/// same row (`` `a.rs::one`, `::two` ``), as (file, test name).
fn references(text: &str) -> Vec<(String, String)> {
    let more = regex::Regex::new(r"`(?:([A-Za-z0-9_./-]+\.rs))?::(\w+)`").unwrap();
    let mut out = Vec::new();
    for row in text.lines().filter(|l| l.starts_with('|')) {
        let mut file = None;
        for c in more.captures_iter(row) {
            if let Some(f) = c.get(1) {
                file = Some(f.as_str().to_owned());
            }
            if let Some(f) = &file {
                out.push((f.clone(), c[2].to_owned()));
            }
        }
    }
    out
}

#[test]
fn every_test_named_in_security_md_exists() {
    let text = fs::read_to_string(root().join("docs/SECURITY.md")).unwrap();
    let found = references(&text);
    assert!(found.len() > 10, "found only {} references", found.len());
    let mut missing = Vec::new();
    for (file, name) in found {
        // `security/x.rs` is under aibus-core/tests/, the rest under crates/.
        let candidates = [root().join("crates").join(&file), root().join("crates/aibus-core/tests").join(&file)];
        let hit = candidates
            .iter()
            .find_map(|p| fs::read_to_string(p).ok())
            .is_some_and(|src| src.contains(&format!("fn {name}")));
        if !hit {
            missing.push(format!("{file}::{name}"));
        }
    }
    assert!(missing.is_empty(), "named in docs/SECURITY.md and not in the tree: {missing:#?}");
}

#[test]
fn the_readme_config_snippet_parses_and_names_the_binary() {
    let readme = fs::read_to_string(root().join("README.md")).unwrap();
    let section = readme.split("### Author workflows from an MCP client").nth(1).expect("the MCP section");
    let fenced = section.split("```json").nth(1).and_then(|s| s.split("```").next()).expect("a json block");
    let config: Value = serde_json::from_str(fenced.trim()).expect("the snippet is JSON");
    assert_eq!(config["command"], "aibus");
    assert_eq!(config["args"], serde_json::json!(["mcp"]));
}
