//! #283: the declarative format grew optional fields, and none of them may change what a workflow
//! written before them means. Every shipped example and every workflow fixture is checked against
//! what the code before #283 said about it: the validation report, the parse result and the
//! prompts a dry run renders. The expected file was written by the code before #283
//! (`AIBUS_BLESS=1` rewrites it, which is only right when a change to the old format is meant).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aibus_core::workflow::{preview, reads, validate, Workflow};
use aibus_core::AgentName;
use serde_json::{json, Value};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn roster() -> Vec<AgentName> {
    [
        "lead", "explore", "plan", "general", "advisor", "router", "search", "grep", "audit",
    ]
    .map(|n| AgentName::new(n).unwrap())
    .to_vec()
}

/// Every workflow file written before #283, keyed by its path in the repository.
fn old_files() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    // Written after #283 (cross-provider came with #360), so the code before it never saw them.
    let newer = ["examples/workflows/cross-provider.toml"];
    let dirs = [
        "examples/workflows",
        "fixtures/workflows/valid",
        "fixtures/workflows/invalid",
        "fixtures/workflows/semantic",
        "fixtures/workflows/warnings",
        "fixtures/workflows/saved",
    ];
    for dir in dirs {
        for entry in std::fs::read_dir(root().join(dir)).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|x| x == "toml") {
                let name = path.file_name().unwrap().to_string_lossy();
                let key = format!("{dir}/{name}");
                if !newer.contains(&key.as_str()) {
                    out.insert(key, std::fs::read_to_string(&path).unwrap());
                }
            }
        }
    }
    assert!(out.len() >= 20, "{}", out.len());
    out
}

fn report(stem: &str, text: &str) -> Value {
    let parsed = match Workflow::parse(stem, text) {
        Ok(wf) => {
            let steps: Vec<Value> = wf
                .steps
                .iter()
                .map(|s| {
                    json!({
                        "agent": s.agent.as_str(),
                        "prompt": s.prompt,
                        "gate": s.gate,
                        "retries": s.retries,
                        "reads": reads(&s.prompt),
                    })
                })
                .collect();
            json!({
                "name": wf.name,
                "description": wf.description,
                "steps": steps,
                "preview": preview(&wf, "INPUT"),
            })
        }
        Err(e) => json!({ "error": e.to_string() }),
    };
    json!({
        "validate": serde_json::to_value(validate(text, stem, &roster())).unwrap(),
        "parse": parsed,
    })
}

#[test]
fn every_workflow_written_before_the_compiler_reads_exactly_as_it_did() {
    let got: BTreeMap<String, Value> = old_files()
        .iter()
        .map(|(path, text)| {
            let stem = path.rsplit('/').next().unwrap().trim_end_matches(".toml");
            (path.clone(), report(stem, text))
        })
        .collect();
    let expected_path = root().join("fixtures/workflows/compat/before-283.json");
    if std::env::var_os("AIBUS_BLESS").is_some() {
        let text = serde_json::to_string_pretty(&got).unwrap() + "\n";
        std::fs::create_dir_all(expected_path.parent().unwrap()).unwrap();
        std::fs::write(&expected_path, text).unwrap();
    }
    let expected: BTreeMap<String, Value> =
        serde_json::from_str(&std::fs::read_to_string(&expected_path).unwrap()).unwrap();
    assert_eq!(
        got.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>()
    );
    for (path, value) in &got {
        assert_eq!(value, &expected[path], "{path}");
    }
}
