//! #294: `aibus runs compare | repro` and the JSON inspector, over the recorded spike traces. The binary prints what
//! `aibus_trace::compare` renders, never needs a provider, a network or a CLI on PATH, writes nothing
//! and refuses what it cannot read.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use aibus_trace::compare::{compare_runs, render, RunInput};
use aibus_trace::eval::{
    Assessor, CheckKind, CheckOutcome, CheckResult, EvaluationResult, PrivacyPolicy, RunContext,
    Verdict, RESULT_VERSION,
};
use aibus_trace::view::Recorded;
use common::*;

fn traces() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/claude-code/traces-284")
}

/// `aibus [--json] runs <args> --root <traces>` with an empty PATH and a HOME of its own.
fn runs(home: &Path, json: bool, args: &[&str]) -> std::process::Output {
    let mut c = command(home);
    c.env("PATH", "")
        .env("AIBUS_CONFIG", home.join("none.toml"));
    if json {
        c.arg("--json");
    }
    c.arg("runs").args(args).arg("--root").arg(traces());
    c.output().unwrap()
}

fn out(o: &std::process::Output) -> String {
    String::from_utf8(o.stdout.clone()).unwrap()
}

fn result(variant: &str, pass: usize, revision: &str) -> EvaluationResult {
    EvaluationResult {
        result_version: RESULT_VERSION,
        suite: "suite".into(),
        suite_version: 1,
        case_id: "case-1".into(),
        variant: variant.into(),
        workflow_run_ids: vec![],
        context: RunContext {
            repository_revision: revision.into(),
            extension_version: "1.0.0".into(),
            cli_version: None,
            concurrent_runs: 1,
            cache_note: None,
        },
        assessor: Assessor::Deterministic {
            tool: "cargo".into(),
            version: "1".into(),
        },
        verdict: Verdict::Pass,
        checks: (0..pass)
            .map(|n| CheckResult {
                kind: CheckKind::UnitTests,
                name: format!("t{n}"),
                outcome: CheckOutcome::Pass,
                evidence: vec![],
            })
            .collect(),
        assessments: vec![],
        metrics: vec![],
        privacy: PrivacyPolicy::MetadataOnly,
    }
}

/// The workflow run id of a spike recording.
fn run_id(name: &str) -> uuid::Uuid {
    Recorded::load(&traces().join(name)).unwrap().events[0].workflow_run_id
}

#[test]
fn runs_compare_prints_what_the_library_renders() {
    let home = aibus_testkit::xdg_temp();
    let o = runs(home.path(), false, &["compare", "06-basic", "07-deleg"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let (a, b) = (
        Recorded::load(&traces().join("06-basic")).unwrap(),
        Recorded::load(&traces().join("07-deleg")).unwrap(),
    );
    let want = render(&compare_runs(
        &RunInput {
            id: "06-basic",
            rec: &a,
            result: None,
        },
        &RunInput {
            id: "07-deleg",
            rec: &b,
            result: None,
        },
    ));
    assert_eq!(out(&o), want);
    for section in [
        "what ran",
        "routing",
        "quality",
        "git and worktree",
        "tools",
        "environment",
    ] {
        assert!(want.contains(&format!("\n{section}\n")), "{section}");
    }
    assert!(want.contains("no evaluation result"), "{want}");
}

#[test]
fn runs_compare_json_keeps_unknowns_as_gaps_and_names_the_mismatch() {
    let home = aibus_testkit::xdg_temp();
    let o = runs(home.path(), true, &["compare", "06-basic", "07-deleg"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(v["a"]["worktree_diff"]["gap"].is_string(), "{v}");
    assert!(v["a"]["quality"]["gap"].is_string(), "{v}");
    assert!(
        v["comparable"].get("no").is_some() || v["comparable"].get("unknown").is_some(),
        "{v}"
    );
    assert!(
        v["comparable"].get("yes").is_none() && v["comparable"] != "yes",
        "{v}"
    );
}

#[test]
fn runs_compare_reads_evaluation_results_and_shows_which_variant_passed_more() {
    let home = aibus_testkit::xdg_temp();
    let dir = tempfile::tempdir().unwrap();
    let (pa, pb) = (dir.path().join("a.json"), dir.path().join("b.json"));
    let (mut ra, mut rb) = (result("a", 2, "abc"), result("b", 5, "abc"));
    ra.workflow_run_ids = vec![run_id("06-basic")];
    rb.workflow_run_ids = vec![run_id("07-deleg")];
    fs::write(&pa, serde_json::to_vec(&ra).unwrap()).unwrap();
    fs::write(&pb, serde_json::to_vec(&rb).unwrap()).unwrap();
    let mut c = command(home.path());
    c.env("PATH", "")
        .args(["runs", "compare", "06-basic", "07-deleg", "--root"])
        .arg(traces());
    c.arg("--result-a").arg(&pa).arg("--result-b").arg(&pb);
    let o = c.output().unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    let t = out(&o);
    assert!(t.contains("2 passed, 0 failed, 0 not run"), "{t}");
    assert!(t.contains("5 passed, 0 failed, 0 not run"), "{t}");
    assert!(t.contains("revision abc"), "{t}");

    // A file that is not a result is a usage error.
    fs::write(&pa, b"{}").unwrap();
    let mut c = command(home.path());
    c.env("PATH", "")
        .args(["runs", "compare", "06-basic", "07-deleg", "--root"])
        .arg(traces());
    c.arg("--result-a").arg(&pa);
    let o = c.output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("evaluation result"), "{}", stderr(&o));
}

#[test]
fn runs_compare_with_a_missing_run_is_a_usage_error() {
    let home = aibus_testkit::xdg_temp();
    let o = runs(home.path(), false, &["compare", "06-basic", "99-nothing"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(o.stdout.is_empty());
    assert!(stderr(&o).contains("no run 99-nothing"), "{}", stderr(&o));
}

#[test]
fn runs_repro_separates_replay_from_rerun() {
    let home = aibus_testkit::xdg_temp();
    let o = runs(home.path(), false, &["repro", "07-deleg"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let t = out(&o);
    assert!(t.contains("\nreplay\n") && t.contains("\nrerun\n"), "{t}");
    assert!(
        t.contains("not a replay") && t.contains("may differ"),
        "{t}"
    );
    assert!(t.contains("repository_revision: gap"), "{t}");
    let o = runs(home.path(), true, &["repro", "07-deleg"]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["rerun"]["label"], "rerun");
    assert_eq!(v["rerun"]["available"], false);
    assert_eq!(v["replay"]["offline"], true);
}

#[test]
fn runs_inspect_node_json_is_the_typed_inspector() {
    let home = aibus_testkit::xdg_temp();
    let rec = Recorded::load(&traces().join("07-deleg")).unwrap();
    let agent = aibus_trace::project(&rec.events).agents[0].agent_id.clone();
    let o = runs(
        home.path(),
        true,
        &["inspect", "07-deleg", "--node", &agent],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["node"], agent.as_str());
    assert!(
        v["prompt"]["gap"].is_string() || v["prompt"]["value"].is_object(),
        "{v}"
    );
    assert!(v["worktree_diff"]["gap"].is_string(), "{v}");
    assert!(v["source_evidence"].as_array().unwrap().len() > 1, "{v}");
    // Without --json the text inspector is unchanged.
    let o = runs(
        home.path(),
        false,
        &["inspect", "07-deleg", "--node", &agent],
    );
    assert!(out(&o).starts_with("agent "), "{}", out(&o));
    let o = runs(
        home.path(),
        true,
        &["inspect", "07-deleg", "--node", "nobody"],
    );
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn runs_compare_refuses_a_result_of_another_run_or_another_version() {
    let home = aibus_testkit::xdg_temp();
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("r.json");
    let run = |p: &Path| {
        let mut c = command(home.path());
        c.env("PATH", "")
            .args(["runs", "compare", "06-basic", "07-deleg", "--root"])
            .arg(traces());
        c.arg("--result-a").arg(p);
        c.output().unwrap()
    };
    // Names no run, then names another run.
    for ids in [vec![], vec![uuid::Uuid::from_u128(7)]] {
        let mut r = result("a", 1, "abc");
        r.workflow_run_ids = ids;
        fs::write(&p, serde_json::to_vec(&r).unwrap()).unwrap();
        let o = run(&p);
        assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
        assert!(o.stdout.is_empty());
        assert!(stderr(&o).contains("workflow_run_ids"), "{}", stderr(&o));
    }
    // The right run, a version this build does not read.
    let mut r = result("a", 1, "abc");
    r.workflow_run_ids = vec![run_id("06-basic")];
    r.result_version = RESULT_VERSION + 98;
    fs::write(&p, serde_json::to_vec(&r).unwrap()).unwrap();
    let o = run(&p);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert!(stderr(&o).contains("result_version"), "{}", stderr(&o));
}

#[test]
fn runs_inspect_json_writes_no_bidi_or_c1_character_from_a_hostile_usage_metric() {
    let home = aibus_testkit::xdg_temp();
    let root = tempfile::tempdir().unwrap();
    let run = root.path().join("hostile");
    fs::create_dir(&run).unwrap();
    let events = fs::read_to_string(traces().join("07-deleg/events.jsonl"))
        .unwrap()
        .replace("task_notification.usage", "tn\\u202e\\u009busage");
    fs::write(run.join("events.jsonl"), events).unwrap();
    let rec = Recorded::load(&run).unwrap();
    // The child whose usage the trace reports at agent scope.
    let agent = "a1d31999c4945865c";
    assert!(rec
        .events
        .iter()
        .any(|e| e.agent_id.as_deref() == Some(agent)));
    let mut c = command(home.path());
    c.env("PATH", "").args([
        "--json", "runs", "inspect", "hostile", "--node", agent, "--root",
    ]);
    c.arg(root.path());
    let o = c.output().unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    let text = out(&o);
    assert!(
        text.contains("tn\\\\u{202e}"),
        "usage is shown, cleaned: {text}"
    );
    for bad in ['\u{202e}', '\u{9b}'] {
        assert!(!text.contains(bad), "{bad:?} in {text}");
    }
}
