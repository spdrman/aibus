//! Keeps the docs and the code pointing at each other: a test named in `docs/SECURITY.md` has to
//! exist, and the config snippet in the README has to start the binary the way it says.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

mod common;

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

/// #182: every long flag `aibus mcp --help` lists is in docs/MCP.md, the confirmation has its own
/// section, and the README and both help texts no longer read as stdio only.
#[test]
fn mcp_md_names_every_flag_and_describes_the_confirmation() {
    let home = aibus_testkit::xdg_temp();
    let flag = regex::Regex::new(r"(?m)^\s+(?:-\w, )?(--[a-z][a-z-]*)").unwrap();
    let flags_of = |args: &[&str]| -> Vec<String> {
        let help = String::from_utf8(common::aibus(home.path(), args).stdout).unwrap();
        flag.captures_iter(&help).map(|c| c[1].to_owned()).collect()
    };
    // The global flags belong to the CLI page; the rest are the server's own.
    let global = flags_of(&["--help"]);
    let found: Vec<String> = flags_of(&["mcp", "--help"]).into_iter().filter(|f| !global.contains(f)).collect();
    assert!(found.iter().any(|f| f == "--confirm") && found.iter().any(|f| f == "--http"), "{found:?}");
    let md = fs::read_to_string(root().join("docs/MCP.md")).unwrap();
    let missing: Vec<&String> = found.iter().filter(|f| !md.contains(&format!("`{f}"))).collect();
    assert!(missing.is_empty(), "docs/MCP.md does not mention {missing:?}");
    let section = md.split("\n## Confirmation").nth(1).expect("a Confirmation section in docs/MCP.md");
    let section = section.split("\n## ").next().unwrap();
    for word in ["elicitation", "retype", "--confirm never", "input_required", "requestState"] {
        assert!(section.contains(word), "the Confirmation section does not say {word:?}");
    }
    assert!(md.contains("permission relay") || md.contains("Relaying a run's permission prompts"));
}

#[test]
fn the_readme_and_the_help_text_say_http_and_confirmation() {
    let readme = fs::read_to_string(root().join("README.md")).unwrap();
    let section = readme.split("### Author workflows from an MCP client").nth(1).unwrap();
    let section = section.split("\n### ").next().unwrap();
    for word in ["--http", "--confirm"] {
        assert!(section.contains(word), "the README section does not mention {word}");
    }
    let home = aibus_testkit::xdg_temp();
    let top = String::from_utf8(common::aibus(home.path(), &["--help"]).stdout).unwrap();
    let line = top.lines().find(|l| l.trim_start().starts_with("mcp ")).expect("an mcp line in --help");
    assert!(line.contains("HTTP"), "{line}");
    let own = String::from_utf8(common::aibus(home.path(), &["mcp", "--help"]).stdout).unwrap();
    let first = own.lines().next().unwrap();
    assert!(first.contains("HTTP"), "{first}");
}
