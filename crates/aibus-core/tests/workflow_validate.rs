//! E2: `workflow::validate`, the semantic checks `parse` does not make, and the fixtures.

use std::path::{Path, PathBuf};

use aibus_core::workflow::{validate, Workflow, WorkflowError, EXAMPLES};
use aibus_core::AgentName;
use serde_json::Value;

fn roster() -> Vec<AgentName> {
    [
        "lead", "explore", "plan", "general", "advisor", "router", "search", "grep", "audit",
    ]
    .map(|n| AgentName::new(n).unwrap())
    .to_vec()
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/workflows")
}

#[test]
fn an_unknown_agent_is_an_error_with_its_step() {
    let text = "[[step]]\nagent = \"explore\"\nprompt = \"a {{input}}\"\n\n[[step]]\nagent = \"ghost\"\nprompt = \"b {{prev.summary}}\"\n";
    let v = validate(text, "t", &roster());
    assert!(!v.ok);
    assert_eq!(v.errors.len(), 1, "{v:?}");
    assert_eq!(v.errors[0].step, Some(2));
    assert_eq!(v.errors[0].field.as_deref(), Some("agent"));
    assert!(v.errors[0].message.contains("ghost"), "{v:?}");
}

#[test]
fn parse_errors_and_roster_errors_come_together() {
    // Step 1 has a bad template, step 2 an unknown agent: one pass shows both.
    let text = "[[step]]\nagent = \"explore\"\nprompt = \"{{nope}}\"\n\n[[step]]\nagent = \"ghost\"\nprompt = \"x {{prev.summary}}\"\n";
    let v = validate(text, "t", &roster());
    let steps: Vec<_> = v.errors.iter().map(|e| e.step).collect();
    assert_eq!(steps, [Some(1), Some(2)], "{v:?}");
}

#[test]
fn a_file_that_is_not_toml_or_has_unknown_keys_is_an_error_without_a_step() {
    for text in [
        "step = [",
        "flavour = 1\n[[step]]\nagent = \"explore\"\nprompt = \"p\"\n",
    ] {
        let v = validate(text, "t", &roster());
        assert!(!v.ok, "{text}");
        assert_eq!(v.errors.len(), 1, "{v:?}");
        assert_eq!(v.errors[0].step, None);
    }
}

#[test]
fn a_gate_is_a_warning_that_quotes_the_command() {
    let text = "[[step]]\nagent = \"explore\"\nprompt = \"a\"\ngate = \"grep -q ok\"\n";
    let v = validate(text, "t", &roster());
    assert!(v.ok, "{v:?}");
    assert_eq!(v.warnings.len(), 1, "{v:?}");
    assert_eq!(v.warnings[0].step, Some(1));
    assert!(v.warnings[0].message.contains("grep -q ok"), "{v:?}");
}

#[test]
fn an_unread_summary_is_a_warning() {
    let text = "[[step]]\nagent = \"explore\"\nprompt = \"a\"\n\n[[step]]\nagent = \"general\"\nprompt = \"b {{input}}\"\n";
    let v = validate(text, "t", &roster());
    assert!(v.ok);
    assert_eq!(v.warnings.len(), 1, "{v:?}");
    assert_eq!(v.warnings[0].step, Some(1));
    // Read by name from two steps on, it is not unread.
    let named = "[[step]]\nagent = \"explore\"\nprompt = \"a\"\n\n[[step]]\nagent = \"plan\"\nprompt = \"b\"\n\n[[step]]\nagent = \"general\"\nprompt = \"{{steps.explore.summary}} {{prev.summary}}\"\n";
    let v = validate(named, "t", &roster());
    let unread: Vec<_> = v.warnings.iter().filter_map(|w| w.step).collect();
    assert!(unread.is_empty(), "{v:?}");
}

#[test]
fn a_same_named_step_in_between_hides_the_earlier_summary() {
    let text = "[[step]]\nagent = \"explore\"\nprompt = \"a\"\n\n[[step]]\nagent = \"explore\"\nprompt = \"b\"\n\n[[step]]\nagent = \"general\"\nprompt = \"{{steps.explore.summary}}\"\n";
    let v = validate(text, "t", &roster());
    let unread: Vec<_> = v.warnings.iter().filter_map(|w| w.step).collect();
    assert_eq!(unread, [1], "{v:?}");
}

#[test]
fn marker_shapes_in_a_prompt_are_a_warning() {
    for marker in [
        "[hint]",
        "[run ls]",
        "[mcp x]",
        "[hook x]",
        "[web x]",
        "[advisor]",
    ] {
        let text = format!("[[step]]\nagent = \"explore\"\nprompt = \"see {marker}\"\n");
        let v = validate(&text, "t", &roster());
        assert!(v.ok);
        assert_eq!(v.warnings.len(), 1, "{marker}: {v:?}");
        assert_eq!(v.warnings[0].field.as_deref(), Some("prompt"));
    }
    let plain = validate(
        "[[step]]\nagent = \"explore\"\nprompt = \"see [1] and [x]\"\n",
        "t",
        &roster(),
    );
    assert!(plain.warnings.is_empty(), "{plain:?}");
}

#[test]
fn the_shipped_examples_pass_against_the_builtin_roster() {
    for (name, text) in EXAMPLES {
        let v = validate(text, name, &roster());
        assert!(v.ok, "{name}: {v:?}");
    }
}

fn files(dir: &str) -> Vec<(String, String)> {
    let mut out: Vec<_> = std::fs::read_dir(fixtures().join(dir))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read_to_string(&p).unwrap(),
            )
        })
        .collect();
    out.sort();
    out
}

#[test]
fn every_fixture_matches_its_expected_json() {
    for dir in ["valid", "invalid", "warnings", "semantic"] {
        let expected: Value = serde_json::from_str(
            &std::fs::read_to_string(fixtures().join("expected").join(format!("{dir}.json")))
                .unwrap(),
        )
        .unwrap();
        let found = files(dir);
        assert!(!found.is_empty(), "{dir}");
        assert_eq!(
            expected.as_object().unwrap().len(),
            found.len(),
            "{dir}: a fixture and its expected entry go together"
        );
        for (file, text) in found {
            let stem = file.trim_end_matches(".toml");
            let got = serde_json::to_value(validate(&text, stem, &roster())).unwrap();
            assert_eq!(got, expected[&file], "{dir}/{file}");
        }
    }
}

#[test]
fn invalid_fixtures_fail_parse_and_validate_names_the_same_step() {
    for (file, text) in files("invalid") {
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
fn save_writes_the_golden_file_byte_for_byte_on_a_fixed_clock() {
    use aibus_core::workflow::save;
    use std::time::{Duration, SystemTime};
    let dir = tempfile::tempdir().unwrap();
    let dirs = vec![dir.path().join("project")];
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let path = save(&dirs, false, "review", EXAMPLES[2].1, false, &roster(), at).unwrap();
    let golden = std::fs::read_to_string(fixtures().join("saved/review.toml")).unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), golden);
    // What `save` wrote is a valid workflow with the same steps as the example.
    assert_eq!(
        Workflow::parse("review", &golden),
        Workflow::parse("review", EXAMPLES[2].1)
    );
}
