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

/// #283: the optional fields are in the same v1 schema, so every file in the new shape passes it,
/// and a value outside one of its closed sets (a runtime, a capability) fails it as it fails parse.
#[test]
fn the_graph_fixtures_pass_the_v1_schema_and_closed_sets_are_enforced() {
    for (file, text) in toml_files(&repo("fixtures/workflows/graph")) {
        let errors = schema_errors(&text);
        assert!(errors.is_empty(), "{file}: {errors:?}");
    }
    for file in ["unknown-runtime.toml", "unknown-capability.toml", "bad-schema-version.toml", "limits-zero.toml"] {
        let text = std::fs::read_to_string(repo("fixtures/workflows/graph-invalid").join(file)).unwrap();
        assert!(!schema_errors(&text).is_empty(), "{file} passes the schema");
        assert!(Workflow::parse("t", &text).is_err(), "{file}");
    }
}

/// #283: the committed plans fit `plan-v1.json`, a run manifest of each fits
/// `run-manifest-v1.json`, the Jev fixtures fit the two Jev schemas, and every one of these
/// schemas is one the checker understands in full.
#[test]
fn plans_manifests_and_jev_records_fit_their_schemas() {
    use aibus_core::workflow::manifest::{run_manifest, RunFacts};
    let plan_schema = load_from("workflow", "plan-v1");
    let manifest_schema = load_from("workflow", "run-manifest-v1");
    let (request, decision) = (load_from("jev", "decision-request-v1"), load_from("jev", "decision-v1"));
    for s in [&plan_schema, &manifest_schema, &request, &decision] {
        assert_eq!(common::schema::lint_2020_12(s), Vec::<String>::new(), "{}", s["$id"]);
    }
    let mut n = 0;
    for e in std::fs::read_dir(repo("fixtures/workflows/plans")).unwrap() {
        let path = e.unwrap().path();
        let plan: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(check_against(&plan_schema, &plan), Vec::<String>::new(), "{}", path.display());
        for (input, commit) in [(Some("task".to_owned()), Ok("abc".to_owned())), (None, Err("not a repository".to_owned()))] {
            let facts = RunFacts {
                input,
                aibus_version: "0.0.0".into(),
                os: "linux".into(),
                arch: "x86_64".into(),
                base_commit: commit,
                dirty: Ok(false),
                env_names: vec!["PATH".into()],
                budgets: serde_json::json!({}),
            };
            let m = run_manifest(&plan, &facts);
            assert_eq!(check_against(&manifest_schema, &m), Vec::<String>::new(), "{}", path.display());
        }
        n += 1;
    }
    assert!(n >= 7);
    for e in std::fs::read_dir(repo("fixtures/jev")).unwrap() {
        let path = e.unwrap().path();
        let case: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(check_against(&request, &case["request"]), Vec::<String>::new(), "{}", path.display());
        assert_eq!(check_against(&decision, &case["decision"]), Vec::<String>::new(), "{}", path.display());
    }
}
