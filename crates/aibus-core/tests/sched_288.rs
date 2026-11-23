//! #288: the deterministic DAG scheduler. The same plan and the same outcomes give the same
//! actions and the same event log, parallel steps respect the limit, an approval blocks a step
//! until a person gives it, an irreversible step is never run again on its own, and nothing on the
//! routing path calls a model.

use std::path::{Path, PathBuf};

use aibus_core::agent::discover::discover;
use aibus_core::workflow::compile::compile;
use aibus_core::workflow::sched::{
    dry_run, Action, EventKind, Outcome, RunState, Scheduler, Status,
};
use aibus_core::workflow::Workflow;
use aibus_core::{AgentDef, AgentName, Redactor};
use indexmap::IndexMap;
use serde_json::{json, Value};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn agents() -> IndexMap<AgentName, AgentDef> {
    let dir = tempfile::tempdir().unwrap();
    discover(None, dir.path(), None, &[], &[]).unwrap().agents
}

fn plan_of(text: &str) -> Value {
    let wf = Workflow::parse("t", text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    compile(&wf, text, &agents(), &Redactor::new()).unwrap_or_else(|p| panic!("{p:?}"))
}

fn fan_out() -> String {
    std::fs::read_to_string(root().join("fixtures/workflows/graph/fan-out.toml")).unwrap()
}

/// Two read-only steps in a chain, the second retried once on failure.
const READ_CHAIN: &str = r#"
name = "read-chain"
description = "two reads"

[[step]]
id = "a"
agent = "explore"
requires = ["repo_read"]
prompt = "Look: {{input}}"

[[step]]
id = "b"
agent = "explore"
requires = ["repo_read"]
prompt = "Look again: {{steps.a.summary}}"
on_fail = "retry:1"
"#;

fn started(actions: &[Action]) -> Vec<&str> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Start { step, .. } => Some(step.as_str()),
            Action::Stop { .. } => None,
        })
        .collect()
}

fn ok() -> Outcome {
    Outcome::Completed {
        artifact_valid: true,
    }
}

fn same_account(s: &mut Scheduler, keys: &[&str]) {
    for k in keys {
        s.set_account(k, "anthropic", "main");
    }
}

#[test]
fn the_same_plan_and_outcomes_give_the_same_decisions() {
    let plan = plan_of(&fan_out());
    let drive = || {
        let mut s = Scheduler::new(&plan).unwrap();
        same_account(&mut s, &["plan", "code", "tests", "build"]);
        let mut actions = Vec::new();
        for _ in 0..20 {
            let a = s.advance();
            actions.push(format!("{a:?}"));
            for w in ["code", "tests", "build"] {
                if s.status(w) == Some(&Status::WaitingApproval) {
                    s.approve(w, "rom").unwrap();
                }
            }
            for step in ["plan", "code", "tests", "build"] {
                if s.status(step) == Some(&Status::Running) {
                    s.report(step, ok()).unwrap();
                }
            }
            if s.state() == RunState::Completed {
                break;
            }
        }
        (actions, s.events().to_vec(), s.state())
    };
    let (a1, e1, s1) = drive();
    let (a2, e2, s2) = drive();
    assert_eq!(s1, RunState::Completed);
    assert_eq!((a1, &e1, s1), (a2, &e2, s2));
    // Every decision names the policy that made it, and says it was not a model.
    let decisions: Vec<&Value> = e1
        .iter()
        .filter(|e| e.kind == EventKind::Started)
        .map(|e| &e.detail)
        .collect();
    assert_eq!(decisions.len(), 4);
    for d in decisions {
        assert_eq!(d["decision"]["decision_source"], "jev_deterministic");
        assert_eq!(d["policy_revision"], d["request"]["policy_revision"]);
    }
}

#[test]
fn parallel_steps_respect_the_limit() {
    let text = fan_out();
    for (limit, first_wave) in [(2usize, 2usize), (1, 1)] {
        let text = text.replace("max_parallel = 2", &format!("max_parallel = {limit}"));
        let mut s = Scheduler::new(&plan_of(&text)).unwrap();
        same_account(&mut s, &["plan", "code", "tests", "build"]);
        assert_eq!(started(&s.advance()), ["plan"]);
        s.report("plan", ok()).unwrap();
        let wave = s.advance();
        assert_eq!(started(&wave).len(), first_wave, "limit {limit}");
        let running = ["code", "tests"]
            .iter()
            .filter(|k| s.status(k) == Some(&Status::Running))
            .count();
        assert!(running <= limit);
        if limit == 1 {
            // The second one waits, and the log says why.
            assert!(s
                .events()
                .iter()
                .any(|e| e.kind == EventKind::Deferred && e.reason_code == "max_parallel"));
            s.report("code", ok()).unwrap();
            assert_eq!(started(&s.advance()), ["tests"]);
        }
    }
}

#[test]
fn an_approval_blocks_the_step_until_it_is_given() {
    let mut s = Scheduler::new(&plan_of(&fan_out())).unwrap();
    same_account(&mut s, &["plan", "code", "tests", "build"]);
    s.advance();
    s.report("plan", ok()).unwrap();
    s.advance();
    s.report("code", ok()).unwrap();
    s.report("tests", ok()).unwrap();
    // build can shell out and write: the workflow holds both.
    assert!(started(&s.advance()).is_empty());
    assert_eq!(s.status("build"), Some(&Status::WaitingApproval));
    assert_eq!(s.state(), RunState::WaitingApproval);
    assert_eq!(
        s.pending("build"),
        ["approval:repo_write", "approval:shell"]
    );
    // Asking again does not start it, and a blank name is no approval.
    assert!(started(&s.advance()).is_empty());
    assert!(s.approve("build", "  ").is_err());
    s.approve("build", "rom").unwrap();
    assert_eq!(started(&s.advance()), ["build"]);
    let granted = s
        .events()
        .iter()
        .find(|e| e.kind == EventKind::ApprovalGranted)
        .unwrap();
    assert_eq!(granted.detail["by"], "rom");
}

#[test]
fn a_denied_approval_fails_the_step_and_stops_the_run() {
    let mut s = Scheduler::new(&plan_of(&fan_out())).unwrap();
    same_account(&mut s, &["plan", "code", "tests", "build"]);
    s.advance();
    s.report("plan", ok()).unwrap();
    s.advance();
    s.report("code", ok()).unwrap();
    s.report("tests", ok()).unwrap();
    s.advance();
    s.deny("build", "rom").unwrap();
    assert_eq!(s.state(), RunState::Failed);
    assert!(s
        .events()
        .iter()
        .any(|e| e.reason_code == "approval_denied" && e.kind == EventKind::Escalated));
}

#[test]
fn an_irreversible_step_is_not_run_again_on_its_own() {
    let mut s = Scheduler::new(&plan_of(&fan_out())).unwrap();
    same_account(&mut s, &["plan", "code", "tests", "build"]);
    s.advance();
    s.report("plan", ok()).unwrap();
    s.advance();
    s.report("code", ok()).unwrap();
    s.report("tests", ok()).unwrap();
    s.advance();
    s.approve("build", "rom").unwrap();
    s.advance();
    // The route back to plan fires on build.failed, and build has side effects (a shell).
    s.report(
        "build",
        Outcome::Failed {
            retryable: true,
            reason: "timeout".into(),
        },
    )
    .unwrap();
    assert!(!s.events().iter().any(|e| e.kind == EventKind::Rerun));
    assert!(s
        .events()
        .iter()
        .any(|e| e.reason_code == "irreversible_not_retried"));
    assert!(started(&s.advance()).is_empty());
    assert_eq!(s.state(), RunState::Failed);
}

#[test]
fn a_retryable_failure_of_a_read_only_step_is_retried_within_its_bound() {
    let plan = plan_of(READ_CHAIN);
    let mut s = Scheduler::new(&plan).unwrap();
    same_account(&mut s, &["a", "b"]);
    s.advance();
    s.report("a", ok()).unwrap();
    s.advance();
    s.report(
        "b",
        Outcome::Failed {
            retryable: true,
            reason: "timeout".into(),
        },
    )
    .unwrap();
    // The step before and this one run again, once.
    assert_eq!(started(&s.advance()), ["a"]);
    s.report("a", ok()).unwrap();
    assert_eq!(started(&s.advance()), ["b"]);
    s.report(
        "b",
        Outcome::Failed {
            retryable: true,
            reason: "timeout".into(),
        },
    )
    .unwrap();
    assert!(started(&s.advance()).is_empty());
    assert_eq!(s.state(), RunState::Failed);
    assert!(s
        .events()
        .iter()
        .any(|e| e.reason_code == "retries_exhausted"));
}

#[test]
fn a_failure_that_is_not_retryable_is_escalated_at_once() {
    let mut s = Scheduler::new(&plan_of(READ_CHAIN)).unwrap();
    same_account(&mut s, &["a", "b"]);
    s.advance();
    s.report("a", ok()).unwrap();
    s.advance();
    s.report(
        "b",
        Outcome::Failed {
            retryable: false,
            reason: "refused".into(),
        },
    )
    .unwrap();
    assert!(!s.events().iter().any(|e| e.kind == EventKind::Rerun));
    assert!(s
        .events()
        .iter()
        .any(|e| e.reason_code == "not_retryable" && e.kind == EventKind::Escalated));
    assert_eq!(s.state(), RunState::Failed);
}

#[test]
fn a_route_that_is_not_taken_skips_what_only_it_leads_to() {
    let mut s = Scheduler::new(&plan_of(&fan_out())).unwrap();
    same_account(&mut s, &["plan", "code", "tests", "build"]);
    s.advance();
    s.report(
        "plan",
        Outcome::Completed {
            artifact_valid: false,
        },
    )
    .unwrap();
    assert!(started(&s.advance()).is_empty());
    for k in ["code", "tests", "build"] {
        assert_eq!(s.status(k), Some(&Status::Skipped), "{k}");
    }
    assert_eq!(s.state(), RunState::Completed);
    assert!(s
        .events()
        .iter()
        .any(|e| e.reason_code == "route_not_taken"));
}

#[test]
fn a_plan_that_was_edited_or_has_a_cycle_is_refused_before_anything_starts() {
    let plan = plan_of(&fan_out());
    let mut edited = plan.clone();
    edited["envelope"]["max_parallel"] = json!(50);
    let why = Scheduler::new(&edited).unwrap_err();
    assert!(why.iter().any(|w| w.contains("plan_hash")), "{why:?}");

    let mut stale = plan.clone();
    stale["jev"]["policy_revision"] = json!("sha256:0");
    stale["plan_hash"] = json!(aibus_core::workflow::compile::plan_hash(&stale));
    let why = Scheduler::new(&stale).unwrap_err();
    assert!(why.iter().any(|w| w.contains("policy revision")), "{why:?}");

    let mut cyclic = plan.clone();
    cyclic["edges"].as_array_mut().unwrap().push(json!({
        "kind": "route", "from": "build", "to": "plan", "when": "build.completed"
    }));
    cyclic["plan_hash"] = json!(aibus_core::workflow::compile::plan_hash(&cyclic));
    let why = Scheduler::new(&cyclic).unwrap_err();
    assert!(why.iter().any(|w| w.contains("cycle")), "{why:?}");
}

#[test]
fn a_step_on_an_openrouter_account_waits_for_approval_and_a_switch_mid_run_is_rechecked() {
    let mut s = Scheduler::new(&plan_of(READ_CHAIN)).unwrap();
    s.set_account("a", "openrouter", "or");
    s.set_account("b", "anthropic", "main");
    // Not started: Jev holds an aibus step whose account is OpenRouter's.
    assert!(started(&s.advance()).is_empty());
    assert_eq!(s.pending("a"), ["runtime:openrouter"]);
    s.approve("a", "rom").unwrap();
    assert_eq!(started(&s.advance()), ["a"]);

    // A fallback that moves a running step onto OpenRouter on a run nobody approved it for.
    let mut t = Scheduler::new(&plan_of(READ_CHAIN)).unwrap();
    same_account(&mut t, &["a", "b"]);
    assert_eq!(started(&t.advance()), ["a"]);
    let stop = t.set_account("a", "openrouter", "or");
    assert_eq!(
        stop,
        [Action::Stop {
            step: "a".into(),
            reason: "approval_rechecked".into()
        }]
    );
    assert_eq!(t.status("a"), Some(&Status::WaitingApproval));
}

#[test]
fn a_handoff_to_another_account_waits_for_consent() {
    let mut s = Scheduler::new(&plan_of(READ_CHAIN)).unwrap();
    s.set_account("a", "anthropic", "main");
    s.set_account("b", "anthropic", "other");
    s.advance();
    s.report("a", ok()).unwrap();
    assert!(started(&s.advance()).is_empty());
    assert_eq!(s.pending("b"), ["handoff:a->b"]);
    s.approve("b", "rom").unwrap();
    assert_eq!(started(&s.advance()), ["b"]);
}

#[test]
fn the_run_budget_and_wall_clock_stop_the_run_with_an_escalation() {
    let text = READ_CHAIN.to_owned() + "\n[limits]\nmax_agent_runs = 2\nmax_wall_seconds = 60\n";
    let mut s = Scheduler::new(&plan_of(&text)).unwrap();
    same_account(&mut s, &["a", "b"]);
    s.advance();
    s.report("a", ok()).unwrap();
    s.advance();
    s.report(
        "b",
        Outcome::Failed {
            retryable: true,
            reason: "timeout".into(),
        },
    )
    .unwrap();
    // Two runs are spent, so the retry of a is not started.
    assert!(started(&s.advance()).is_empty());
    assert!(s
        .events()
        .iter()
        .any(|e| e.reason_code == "run_budget_exhausted"));
    assert_eq!(s.state(), RunState::Failed);

    let mut w = Scheduler::new(&plan_of(&text)).unwrap();
    same_account(&mut w, &["a", "b"]);
    w.advance();
    assert!(w.elapsed(60).is_empty());
    assert_eq!(
        w.elapsed(61),
        [Action::Stop {
            step: "a".into(),
            reason: "wall_clock_exhausted".into()
        }]
    );
    assert_eq!(w.state(), RunState::Failed);
}

#[test]
fn an_interrupted_run_resumes_but_never_reruns_an_irreversible_step() {
    let mut s = Scheduler::new(&plan_of(&fan_out())).unwrap();
    same_account(&mut s, &["plan", "code", "tests", "build"]);
    s.advance();
    s.interrupt();
    assert_eq!(s.state(), RunState::Interrupted);
    assert!(started(&s.advance()).is_empty());
    s.resume();
    // plan is read-only, so it comes back.
    assert_eq!(started(&s.advance()), ["plan"]);
    s.report("plan", ok()).unwrap();
    s.advance();
    s.report("code", ok()).unwrap();
    s.report("tests", ok()).unwrap();
    s.advance();
    s.approve("build", "rom").unwrap();
    s.advance();
    s.interrupt();
    s.resume();
    assert_eq!(s.status("build"), Some(&Status::Escalated));
    assert_eq!(s.state(), RunState::Failed);
}

#[test]
fn cancel_stops_what_runs_and_starts_nothing_more() {
    let mut s = Scheduler::new(&plan_of(READ_CHAIN)).unwrap();
    same_account(&mut s, &["a", "b"]);
    s.advance();
    assert_eq!(
        s.cancel("rom"),
        [Action::Stop {
            step: "a".into(),
            reason: "cancelled".into()
        }]
    );
    assert_eq!(s.state(), RunState::Cancelled);
    assert!(started(&s.advance()).is_empty());
}

#[cfg(unix)]
#[test]
fn a_saved_run_is_owner_only_and_keeps_the_spent_handoffs() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runs/r1.json");
    let mut s = Scheduler::new(&plan_of(READ_CHAIN)).unwrap();
    same_account(&mut s, &["a", "b"]);
    s.advance();
    s.spent_handoffs.insert("sha256:aa".into());
    s.save(&path).unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(path.parent().unwrap()), 0o700);
    let back = Scheduler::load(&path).unwrap();
    assert_eq!(back, s);
    assert!(back.spent_handoffs.contains("sha256:aa"));
    // Somebody else could have changed the approvals in a file others can write.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(Scheduler::load(&path).unwrap_err().contains("owner-only"));
    // A symlink is not followed, even to a file that would pass.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let link = dir.path().join("runs/link.json");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(Scheduler::load(&link)
        .unwrap_err()
        .contains("not a regular file"));
    // A temp file planted where save writes is replaced, never written through.
    let planted = dir.path().join("elsewhere");
    std::fs::write(&planted, b"keep").unwrap();
    std::os::unix::fs::symlink(&planted, path.with_extension("tmp")).unwrap();
    s.save(&path).unwrap();
    assert_eq!(std::fs::read(&planted).unwrap(), b"keep");
    assert_eq!(mode(&path), 0o600);
}

#[test]
fn the_dry_run_shows_candidates_approvals_and_artifact_contracts() {
    let d = dry_run(&plan_of(&fan_out())).unwrap();
    assert_eq!(d["waves"], json!([["plan"], ["code", "tests"], ["build"]]));
    let step = |k: &str| {
        d["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["key"] == k)
            .unwrap()
            .clone()
    };
    let plan = step("plan");
    let to: Vec<&str> = plan["route_candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["to"].as_str().unwrap())
        .collect();
    assert_eq!(to, ["code", "tests"]);
    assert_eq!(
        plan["route_candidates"][0]["when"],
        "plan.completed && plan.artifact_valid"
    );
    assert_eq!(plan["artifact"]["output"], "json");
    assert_eq!(
        plan["artifact"]["validity_tested_by_routes_to"],
        json!(["code", "tests"])
    );
    assert_eq!(plan["decision"]["reason_code"], "pinned_runtime");
    let build = step("build");
    assert_eq!(
        build["approvals"],
        json!(["approval:repo_write", "approval:shell"])
    );
    // Accounts are not known yet, so every data edge is a handoff a run would have to settle.
    assert_eq!(
        build["handoffs"],
        json!(["handoff:code->build", "handoff:tests->build"])
    );
    assert_eq!(build["route_candidates"][0]["back"], true);
    assert_eq!(build["route_candidates"][0]["max_iterations"], 2);
    // A plan that would be refused says why instead of a dry run.
    let mut bad = plan_of(&fan_out());
    bad["steps"] = json!([]);
    assert!(dry_run(&bad).is_err());
}

#[test]
fn routing_makes_no_model_call() {
    // The scheduler and the policy are plain synchronous functions over data: no provider, no
    // runtime, no network. Their sources say so, and the log says every decision was Jev's rules.
    for file in ["sched.rs", "jev.rs"] {
        let src = std::fs::read_to_string(root().join("crates/aibus-core/src/workflow").join(file))
            .unwrap();
        for banned in [
            "aibus_provider",
            "tokio",
            "async fn",
            "reqwest",
            "StepHost",
            "std::net",
            "std::process",
        ] {
            assert!(!src.contains(banned), "{file} mentions {banned}");
        }
    }
    let mut s = Scheduler::new(&plan_of(READ_CHAIN)).unwrap();
    same_account(&mut s, &["a", "b"]);
    s.advance();
    assert!(s
        .events()
        .iter()
        .filter(|e| e.kind == EventKind::Started)
        .all(|e| e.detail["decision"]["decision_source"] == "jev_deterministic"));
}
