//! #359 (A2.2c): the runtimes of A2.2 as Jev knows them, and the rule that OpenRouter, the one
//! direct API runtime, is never picked for a run unless the user approved it for that run.

use aibus_core::agent::discover::discover;
use aibus_core::workflow::compile::compile;
use aibus_core::workflow::jev::{self, decide, Request};
use aibus_core::workflow::{Runtime, Workflow};
use aibus_core::Redactor;
use serde_json::Value;

fn request(candidates: &[&str], approved: &[&str]) -> Request {
    Request {
        task_id: "step".into(),
        requires: vec!["headless".into(), "one_shot".into()],
        candidates: candidates.iter().map(|c| (*c).to_owned()).collect(),
        policy_revision: jev::policy_revision(),
        budget_class: "bounded".into(),
        risk_class: "read_only".into(),
        requested_mode: Default::default(),
        parent_runtime: None,
        approved: approved.iter().map(|c| (*c).to_owned()).collect(),
        account_provider: None,
    }
}

#[test]
fn the_codex_and_gemini_clis_are_runtimes_with_the_capabilities_their_extensions_declare() {
    let codex = jev::runtime_info(Runtime::CodexCli);
    assert_eq!((codex.id, codex.status), ("cli.codex", "experimental"));
    for c in [
        "cancel",
        "headless",
        "login_status",
        "one_shot",
        "structured_events",
        "tool_events",
        "usage_report",
    ] {
        assert!(codex.capabilities.contains(&c), "codex {c}");
    }
    for c in [
        "native_child_lineage",
        "session_resume",
        "interactive_approval",
        "workspace_control",
    ] {
        assert!(!codex.capabilities.contains(&c), "codex must not claim {c}");
    }
    // Gemini: no login status (no documented subcommand), and still unverified.
    let gemini = jev::runtime_info(Runtime::GeminiCli);
    // Never run: its own status, so it is not read as live-tested like Codex, and it claims no
    // capability its manifest leaves to a probe.
    assert_eq!((gemini.id, gemini.status), ("cli.gemini", "unverified"));
    assert!(!gemini.capabilities.contains(&"structured_events"));
    assert!(
        gemini.capabilities.contains(&"one_shot") && gemini.capabilities.contains(&"usage_report")
    );
    assert!(!gemini.capabilities.contains(&"login_status"));
    assert!(!gemini.capabilities.contains(&"native_child_lineage"));
}

#[test]
fn openrouter_is_a_separate_runtime_with_usage_and_no_vendor_cli_capabilities() {
    let or = jev::runtime_info(Runtime::OpenRouter);
    assert_eq!((or.id, or.status), ("openrouter", "experimental"));
    for c in ["cancel", "headless", "one_shot", "usage_report"] {
        assert!(or.capabilities.contains(&c), "openrouter {c}");
    }
    for c in ["login_status", "native_child_lineage", "session_resume"] {
        assert!(
            !or.capabilities.contains(&c),
            "openrouter must not claim {c}"
        );
    }
}

#[test]
fn openrouter_alone_is_not_picked_without_approval_and_the_decision_shows_what_changes() {
    let d = decide(&request(&["openrouter"], &[]));
    assert_eq!(d.selected_runtime, None);
    assert_eq!(d.reason_code, "openrouter_needs_approval");
    // The proposal a person is shown: billing, models, data and capabilities, each distinct.
    for needle in [
        "proposal:billing",
        "proposal:models",
        "proposal:data",
        "proposal:capabilities",
    ] {
        assert!(
            d.policy_evidence.iter().any(|e| e.starts_with(needle)),
            "{needle} missing from {:?}",
            d.policy_evidence
        );
    }
    assert!(d
        .policy_evidence
        .iter()
        .any(|e| e.contains("not your") && e.contains("subscription")));
}

#[test]
fn an_unapproved_openrouter_is_skipped_for_a_cli_that_fits_and_never_silently_replaces_one() {
    let d = decide(&request(&["openrouter", "cli.codex"], &[]));
    assert_eq!(d.selected_runtime.as_deref(), Some("cli.codex"));
    // A CLI that cannot do the step: the answer is a stop for a person, not OpenRouter.
    let mut r = request(&["cli.gemini", "openrouter"], &[]);
    r.requires.push("native_child_lineage".into());
    let d = decide(&r);
    assert_eq!(d.selected_runtime, None);
    assert_ne!(
        d.reason_code, "openrouter_needs_approval",
        "OpenRouter lacks it too: {d:?}"
    );
}

#[test]
fn an_approved_openrouter_is_selected_and_the_approval_is_in_the_evidence() {
    let d = decide(&request(&["openrouter"], &["openrouter"]));
    assert_eq!(d.selected_runtime.as_deref(), Some("openrouter"));
    assert!(
        d.policy_evidence.iter().any(|e| e == "approved:openrouter"),
        "{:?}",
        d.policy_evidence
    );
    // Approving another runtime is not approving OpenRouter.
    let d = decide(&request(&["openrouter"], &["cli.codex"]));
    assert_eq!(d.selected_runtime, None);
}

#[test]
fn a_workflow_step_on_openrouter_plans_with_its_billing_and_credential_source_and_no_decision_to_run(
) {
    let text =
        "[[step]]\nagent = \"explore\"\nruntime = \"openrouter\"\nprompt = \"p {{input}}\"\n";
    let wf = Workflow::parse("t", text).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let agents = discover(None, dir.path(), None, &[], &[]).unwrap().agents;
    let plan: Value = compile(&wf, text, &agents, &Redactor::new()).unwrap();
    let step = &plan["steps"][0];
    assert_eq!(step["runtime"], "openrouter");
    assert_eq!(step["invocation"]["kind"], "openrouter_run");
    assert_eq!(step["invocation"]["billing"], "separate_openrouter_credits");
    assert_eq!(
        step["invocation"]["credential"],
        "the openrouter account's own key only"
    );
    // Nobody approved it yet, so the plan records the stop, not a runtime.
    let decision = &plan["jev"]["decisions"][0]["decision"];
    assert_eq!(decision["reason_code"], "openrouter_needs_approval");
    assert!(decision["selected_runtime"].is_null());
}

#[test]
fn a_cli_step_plans_as_a_cli_run_naming_its_extension() {
    for (rt, id) in [
        ("cli.codex", "org.devshell.aibus.codex-cli"),
        ("cli.gemini", "org.devshell.aibus.gemini-cli"),
    ] {
        let text = format!(
            "[[step]]\nagent = \"explore\"\nruntime = \"{rt}\"\nprompt = \"p {{{{input}}}}\"\n"
        );
        let wf = Workflow::parse("t", &text).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let agents = discover(None, dir.path(), None, &[], &[]).unwrap().agents;
        let plan: Value = compile(&wf, &text, &agents, &Redactor::new()).unwrap();
        let inv = &plan["steps"][0]["invocation"];
        assert_eq!(inv["kind"], "cli_run", "{rt}");
        assert_eq!(inv["extension"], id, "{rt}");
        assert_eq!(
            plan["jev"]["decisions"][0]["decision"]["selected_runtime"],
            rt
        );
    }
}

#[test]
fn an_aibus_step_on_an_openrouter_account_needs_the_same_approval() {
    let mut r = request(&["aibus"], &[]);
    r.account_provider = Some("openrouter".into());
    let d = decide(&r);
    assert_eq!(d.selected_runtime, None);
    assert_eq!(d.reason_code, "openrouter_needs_approval");
    assert!(d
        .policy_evidence
        .iter()
        .any(|e| e.starts_with("proposal:billing")));
    r.approved = vec!["openrouter".into()];
    let d = decide(&r);
    assert_eq!(d.selected_runtime.as_deref(), Some("aibus"));
    assert!(d.policy_evidence.iter().any(|e| e == "approved:openrouter"));
    // Another provider's account is not the rule's business.
    let mut other = request(&["aibus"], &[]);
    other.account_provider = Some("anthropic".into());
    assert_eq!(decide(&other).selected_runtime.as_deref(), Some("aibus"));
}
