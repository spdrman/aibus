//! E2: `docs/schemas/workflow/v1.json` against the examples and the fixtures. The schema is
//! checked with the checker in `common/schema.rs`; its `KNOWN` list gained `$id` and nothing else.

mod common;

use std::path::{Path, PathBuf};

use aibus_core::workflow::{validate, Workflow, WorkflowError};
use aibus_core::AgentName;
use common::schema::{check_against, load_from};
use serde_json::Value;

fn repo(sub: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(sub)
}

fn toml_files(dir: &Path) -> Vec<(String, String)> {
    let mut out: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read_to_string(&p).unwrap()))
        .collect();
    out.sort();
    assert!(!out.is_empty(), "{}", dir.display());
    out
}

fn schema_errors(text: &str) -> Vec<String> {
    let schema = load_from("workflow", "v1");
    let doc: Value = toml::from_str(text).unwrap();
    check_against(&schema, &doc)
}

fn roster() -> Vec<AgentName> {
    ["lead", "explore", "plan", "general", "advisor", "router", "search", "grep", "audit"]
        .map(|n| AgentName::new(n).unwrap())
        .to_vec()
}

#[test]
fn valid_fixtures_and_examples_pass_the_schema() {
    let dirs = [repo("examples/workflows"), repo("fixtures/workflows/valid")];
    for dir in dirs {
        for (file, text) in toml_files(&dir) {
            let errors = schema_errors(&text);
            assert!(errors.is_empty(), "{file}: {errors:?}");
            let v = validate(&text, file.trim_end_matches(".toml"), &roster());
            assert!(v.ok, "{file}: {v:?}");
        }
    }
    // Template and roster problems are not the schema's business: it lets these through.
    for (file, text) in toml_files(&repo("fixtures/workflows/semantic")) {
        let errors = schema_errors(&text);
        assert!(errors.is_empty(), "{file}: {errors:?}");
    }
}

#[test]
fn invalid_fixtures_fail_the_schema_and_parse_on_the_same_step() {
    for (file, text) in toml_files(&repo("fixtures/workflows/invalid")) {
        assert!(!schema_errors(&text).is_empty(), "{file} passes the schema");
        let err = Workflow::parse("t", &text).expect_err(&file);
        let v = validate(&text, "t", &roster());
        let want = match err {
            WorkflowError::Step { step, .. } => Some(step),
            _ => None,
        };
        assert_eq!(v.errors[0].step, want, "{file}");
    }
}

#[test]
fn the_schema_rejects_unknown_keys_at_every_level() {
    let top = "flavour = 1\n[[step]]\nagent = \"explore\"\nprompt = \"p\"\n";
    let step = "[[step]]\nagent = \"explore\"\nprompt = \"p\"\nflavour = 1\n";
    for text in [top, step] {
        assert!(!schema_errors(text).is_empty(), "{text}");
    }
}
