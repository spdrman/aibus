//! Build-time facts about the workspace: dependency direction and `forbid(unsafe_code)`.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Normal (non-dev, non-build) dependency closure of `name`, by package name.
fn closure(name: &str) -> BTreeSet<String> {
    let out = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--locked"])
        .current_dir(root())
        .output()
        .expect("run cargo metadata");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let meta: Value = serde_json::from_slice(&out.stdout).expect("metadata json");

    let names: HashMap<&str, &str> = meta["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["id"].as_str().unwrap(), p["name"].as_str().unwrap()))
        .collect();
    let edges: HashMap<&str, Vec<&str>> = meta["resolve"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| {
            let deps = n["deps"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|d| {
                    d["dep_kinds"].as_array().unwrap().iter().any(|k| k["kind"].is_null())
                })
                .map(|d| d["pkg"].as_str().unwrap())
                .collect();
            (n["id"].as_str().unwrap(), deps)
        })
        .collect();

    let start = names.iter().find(|(_, n)| **n == name).map(|(id, _)| *id).expect("crate exists");
    let mut seen = BTreeSet::new();
    let mut todo = vec![start];
    while let Some(id) = todo.pop() {
        for dep in &edges[id] {
            if seen.insert(names[dep].to_string()) {
                todo.push(dep);
            }
        }
    }
    seen
}

#[test]
fn core_never_sees_ratatui_or_a_concrete_provider() {
    let deps = closure("aibus-core");
    for banned in ["ratatui", "crossterm", "aibus-tui", "aibus-provider-anthropic", "aibus-provider-openai"] {
        assert!(!deps.contains(banned), "aibus-core depends on {banned}");
    }
}

#[test]
fn tui_never_sees_reqwest_or_a_provider_crate() {
    let deps = closure("aibus-tui");
    for banned in ["reqwest", "aibus-provider-anthropic", "aibus-provider-openai"] {
        assert!(!deps.contains(banned), "aibus-tui depends on {banned}");
    }
}

#[test]
fn graph_has_no_workspace_deps() {
    let deps: Vec<_> = closure("aibus-graph").into_iter().filter(|d| d.starts_with("aibus")).collect();
    assert!(deps.is_empty(), "aibus-graph depends on {deps:?}");
}

#[test]
fn every_crate_forbids_unsafe_code() {
    let mut checked = 0;
    for entry in std::fs::read_dir(root().join("crates")).unwrap() {
        let dir = entry.unwrap().path();
        for file in ["src/lib.rs", "src/main.rs"] {
            let path = dir.join(file);
            if path.exists() {
                let src = std::fs::read_to_string(&path).unwrap();
                assert!(
                    src.lines().any(|l| l.trim() == "#![forbid(unsafe_code)]"),
                    "{} lacks #![forbid(unsafe_code)]",
                    path.display()
                );
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 8, "expected one root file per crate");
}

#[test]
fn msrv_is_pinned_to_1_85() {
    let toolchain = std::fs::read_to_string(root().join("rust-toolchain.toml")).unwrap();
    assert!(toolchain.contains("channel = \"1.85\""));
    let manifest = std::fs::read_to_string(root().join("Cargo.toml")).unwrap();
    assert!(manifest.contains("rust-version = \"1.85\""));
}
