//! #353 (phase 1 of #346): `runtime = "attached"`. The schema accepts it, the compiler puts the
//! step in the plan and its hash, a run hands the step to the attached session through the same
//! `next_step` / `step_result` path as #322, and a workflow with such a step starts nothing when
//! no session is attached. Same rules as #274 and #322: the keyboard accepts, one step at a time,
//! no secret relay, the agent may decline.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aibus_core::agent::discover::discover;
use aibus_core::attach::{Attachments, Dispatch, StepState};
use aibus_core::config::Hooks;
use aibus_core::tool::Tool;
use aibus_core::tools::RunWorkflow;
use aibus_core::workflow::compile::{compile, plan_hash};
use aibus_core::workflow::Workflow;
use aibus_core::{
    AgentDef, AgentName, HookRunner, Limits, Orchestrator, Redactor, Roster, ToolCx, ToolLimits,
};
use indexmap::IndexMap;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const ATTACHED: &str = r#"name = "att"
description = "Hand one step to the attached session"

[[step]]
agent = "explore"
runtime = "attached"
prompt = "Look into {{input}} and say where it lives."
"#;

fn agents() -> IndexMap<AgentName, AgentDef> {
    let dir = tempfile::tempdir().unwrap();
    discover(None, dir.path(), None, &[], &[]).unwrap().agents
}

fn plan_of(text: &str) -> Value {
    let wf = Workflow::parse("att", text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    compile(&wf, text, &agents(), &Redactor::new()).unwrap_or_else(|p| panic!("{p:?}"))
}

#[test]
fn the_schema_accepts_an_attached_step_and_the_chain_runner_does_not_call_it_plan_only() {
    let wf = Workflow::parse("att", ATTACHED).unwrap();
    assert_eq!(wf.steps[0].runtime.as_str(), "attached");
    assert_eq!(
        wf.plan_only(),
        None,
        "an attached step runs from a session that has one attached"
    );
    let roster: Vec<AgentName> = agents().keys().cloned().collect();
    let checked = aibus_core::workflow::validate(ATTACHED, "att", &roster);
    assert!(checked.ok, "{:?}", checked.errors);
    // The warning about a runtime that does not run yet is for cli.claude, not for this one.
    assert!(
        !checked
            .warnings
            .iter()
            .any(|w| w.message.contains("does not run yet")),
        "{:?}",
        checked.warnings
    );
    // The JSON schemas list it too.
    for rel in [
        "docs/schemas/workflow/v1.json",
        "docs/schemas/workflow/plan-v1.json",
    ] {
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(rel),
        )
        .unwrap();
        assert!(
            text.contains("\"attached\""),
            "{rel} does not list the attached runtime"
        );
    }
}

#[test]
fn the_plan_has_the_step_its_runtime_its_decision_and_a_hash_that_moves_with_it() {
    let plan = plan_of(ATTACHED);
    assert_eq!(plan["steps"][0]["runtime"], "attached");
    assert_eq!(plan["steps"][0]["invocation"]["kind"], "attached_session");
    assert_eq!(
        plan["steps"][0]["invocation"]["via"],
        "next_step and step_result"
    );
    assert_eq!(plan["runtimes"]["attached"]["status"], "experimental");
    assert_eq!(plan["runnable_now"], true);
    assert_eq!(
        plan["jev"]["decisions"][0]["decision"]["selected_runtime"],
        "attached"
    );
    let same_with_aibus = plan_of(&ATTACHED.replace("runtime = \"attached\"\n", ""));
    assert_ne!(plan_hash(&plan), plan_hash(&same_with_aibus));
    assert_eq!(
        plan_hash(&plan),
        plan_hash(&plan_of(ATTACHED)),
        "the same file gives the same hash"
    );
}

/// A session loop in miniature: it owns the registry, takes what the workflow sends, and plays the
/// agent when `answer` says so.
fn session(
    mut att: Attachments,
    mut rx: mpsc::Receiver<Dispatch>,
    answer: Option<(&'static str, &'static str)>,
) -> tokio::task::JoinHandle<(Attachments, Vec<String>)> {
    tokio::spawn(async move {
        let mut prompts = Vec::new();
        while let Some(ask) = rx.recv().await {
            let now = Instant::now();
            att.handle(ask, now);
            if let Some((status, summary)) = answer {
                let pulled = att.next_step("claude-code", now).unwrap();
                if let Some(p) = pulled["step"]["prompt"].as_str() {
                    prompts.push(p.to_owned());
                    let token = pulled["step"]["token"].as_str().unwrap().to_owned();
                    att.step_result("claude-code", &token, status, summary, now)
                        .unwrap();
                }
            }
            att.settle(now);
        }
        (att, prompts)
    })
}

fn accepted() -> Attachments {
    let now = Instant::now();
    let mut a = Attachments::new(true, false);
    let out = a
        .request_attach("claude-code", None, &[], (None, None), now)
        .unwrap();
    a.accept(out["code"].as_str().unwrap(), now).unwrap();
    a
}

fn call(
    wf: &str,
    bridge: Option<mpsc::Sender<Dispatch>>,
) -> (RunWorkflow, ToolCx, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let wfs = dir.path().join(".aibus/workflows");
    std::fs::create_dir_all(&wfs).unwrap();
    std::fs::write(wfs.join("att.toml"), wf).unwrap();
    let (tx, _rx) = mpsc::channel(64);
    let orch = Orchestrator::new(Limits::default(), tx.clone());
    let defs = agents();
    orch.set_roster(Roster::of(defs.values()));
    let hooks = Arc::new(HookRunner::new(
        &Hooks::default(),
        vec![],
        dir.path().to_path_buf(),
        "s".into(),
        None,
        None,
    ));
    let mut tool = RunWorkflow::new(orch, hooks, aibus_core::workflow::dirs(dir.path(), None));
    if let Some(b) = bridge {
        tool = tool.with_attached(b);
    }
    let cx = ToolCx {
        run: aibus_core::RunId::new(),
        cwd: dir.path().to_path_buf(),
        cancel: CancellationToken::new(),
        limits: ToolLimits::default(),
        tx,
        memory: None,
    };
    (tool, cx, dir)
}

#[tokio::test]
async fn a_run_delivers_the_step_to_the_attached_session_and_reports_what_came_back() {
    let (tx, rx) = mpsc::channel(8);
    let loop_ = session(accepted(), rx, Some(("done", "it lives in src/login.rs")));
    let (tool, cx, _dir) = call(ATTACHED, Some(tx));
    let out = tool
        .call(&cx, json!({"name": "att", "input": "the login bug"}))
        .await
        .unwrap();
    assert!(
        out.content.contains("it lives in src/login.rs"),
        "{}",
        out.content
    );
    drop(tool);
    let (att, prompts) = loop_.await.unwrap();
    assert_eq!(prompts, ["Look into the login bug and say where it lives."]);
    assert_eq!(att.steps().len(), 1);
    assert_eq!(att.steps()[0].state, StepState::Done);
}

#[tokio::test]
async fn a_declined_step_is_a_failed_step_with_the_agents_words() {
    let (tx, rx) = mpsc::channel(8);
    let loop_ = session(accepted(), rx, Some(("declined", "I will not touch that")));
    let (tool, cx, _dir) = call(ATTACHED, Some(tx));
    let out = tool
        .call(&cx, json!({"name": "att", "input": "x"}))
        .await
        .unwrap();
    assert!(
        out.content.contains("I will not touch that"),
        "{}",
        out.content
    );
    assert!(!out.content.contains("completed"), "{}", out.content);
    drop(tool);
    assert_eq!(loop_.await.unwrap().0.steps()[0].state, StepState::Declined);
}

#[tokio::test]
async fn the_step_is_refused_and_nothing_starts_when_no_session_is_attached() {
    // No bridge at all (a headless run, or a session without --control-attach).
    let (tool, cx, _dir) = call(ATTACHED, None);
    let why = tool
        .call(&cx, json!({"name": "att", "input": "x"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(why.contains("--control-attach"), "{why}");
    // A bridge, but the keyboard never accepted anyone.
    let (tx, rx) = mpsc::channel(8);
    let loop_ = session(Attachments::new(true, false), rx, None);
    let (tool, cx, _dir) = call(ATTACHED, Some(tx));
    let why = tool
        .call(&cx, json!({"name": "att", "input": "x"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(why.contains("no attached session"), "{why}");
    drop(tool);
    assert!(
        loop_.await.unwrap().0.steps().is_empty(),
        "nothing was queued"
    );
}

#[tokio::test]
async fn an_unanswered_step_times_out_and_a_cancelled_workflow_cancels_its_step() {
    let now = Instant::now();
    let mut a = accepted();
    let (reply, mut ended) = tokio::sync::oneshot::channel();
    a.handle(
        Dispatch::Step {
            name: "explore".into(),
            prompt: "p".into(),
            within: Duration::from_secs(5),
            reply,
        },
        now,
    );
    assert!(a.settle(now).is_empty());
    assert!(ended.try_recv().is_err(), "still waiting");
    a.settle(now + Duration::from_secs(6));
    assert_eq!(
        ended.try_recv().unwrap().unwrap().state,
        StepState::TimedOut
    );

    let (reply, ended) = tokio::sync::oneshot::channel();
    a.handle(
        Dispatch::Step {
            name: "explore".into(),
            prompt: "p".into(),
            within: Duration::from_secs(60),
            reply,
        },
        now,
    );
    drop(ended);
    let notes = a.settle(now);
    assert!(
        notes
            .iter()
            .any(|n| n.contains("session not interruptible")),
        "{notes:?}"
    );
    assert_eq!(a.steps().last().unwrap().state, StepState::Cancelled);
}

#[test]
fn one_step_at_a_time_and_a_check_that_says_why() {
    let now = Instant::now();
    let mut a = accepted();
    let (check, mut got) = tokio::sync::oneshot::channel();
    a.handle(Dispatch::Check(check), now);
    assert_eq!(got.try_recv().unwrap(), Ok(()));
    let (r1, _e1) = tokio::sync::oneshot::channel();
    a.handle(
        Dispatch::Step {
            name: "a".into(),
            prompt: "p".into(),
            within: Duration::from_secs(60),
            reply: r1,
        },
        now,
    );
    let (r2, mut e2) = tokio::sync::oneshot::channel();
    a.handle(
        Dispatch::Step {
            name: "b".into(),
            prompt: "p".into(),
            within: Duration::from_secs(60),
            reply: r2,
        },
        now,
    );
    assert!(e2.try_recv().unwrap().unwrap_err().contains("busy"));
    let (check, mut got) = tokio::sync::oneshot::channel();
    a.handle(Dispatch::Check(check), now);
    assert!(got.try_recv().unwrap().unwrap_err().contains("busy"));
}

#[test]
fn the_banner_shows_the_code_before_a_long_directory_can_push_it_off_the_row() {
    let now = Instant::now();
    let mut a = Attachments::new(true, false);
    let cwd = format!("/private/tmp/{}", "deep/".repeat(40));
    a.request_attach(
        "claude-code",
        Some("claude-live"),
        &[],
        (Some(4123), Some(&cwd)),
        now,
    )
    .unwrap();
    let banner = a.banner().unwrap();
    let at = banner.find("code ").unwrap();
    assert!(at < 40, "{banner}");
    assert!(
        banner.contains("/attach accept")
            && banner.contains("4123")
            && banner.contains("claude-live"),
        "{banner}"
    );
}
