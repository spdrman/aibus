//! #289: aibus-owned run state survives a restart. A restart mid-workflow rebuilds every decision
//! and resumes only what aibus owns, a crash after an irreversible action that may have happened
//! waits for a person instead of running it again, and a retry is a new attempt with a new id while
//! the old trace stays as it was.

use std::path::Path;

use aibus_core::agent::discover::discover;
use aibus_core::workflow::compile::compile;
use aibus_core::workflow::durable::{attempts, trace, DurableError, Input, Run};
use aibus_core::workflow::handoff::{
    item, preview, preview_sha256, record, Context, Decision, HandoffError, Party, Transfer,
};
use aibus_core::workflow::sched::{Action, EventKind, Outcome, RunState, Status};
use aibus_core::workflow::{Runtime, Workflow};
use aibus_core::{AgentDef, AgentName, Redactor};
use indexmap::IndexMap;
use serde_json::Value;

fn agents() -> IndexMap<AgentName, AgentDef> {
    let dir = tempfile::tempdir().unwrap();
    discover(None, dir.path(), None, &[], &[]).unwrap().agents
}

fn plan_of(text: &str) -> Value {
    let wf = Workflow::parse("t", text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    compile(&wf, text, &agents(), &Redactor::new()).unwrap_or_else(|p| panic!("{p:?}"))
}

/// A plan step, then an aibus step and a Claude CLI step at once. Every step is held for a yes.
const FAN: &str = r#"
name = "fan"
description = "plan, then look and scan at once"

[[step]]
id = "plan"
agent = "explore"
requires = ["repo_read"]
prompt = "Plan: {{input}}"

[[step]]
id = "look"
agent = "explore"
runtime = "cli.claude"
requires = ["repo_read"]
prompt = "Look: {{input}}"

[[step]]
id = "scan"
agent = "explore"
requires = ["repo_read"]
prompt = "Scan: {{input}}"

[[route]]
from = "plan"
to = "look"
when = "plan.completed"

[[route]]
from = "plan"
to = "scan"
when = "plan.completed"

[limits]
max_parallel = 2

[approvals]
required_for = ["repo_read"]
"#;

/// One step that runs a shell: it can change things outside aibus that cannot be taken back.
const DEPLOY: &str = r#"
name = "deploy"
description = "one irreversible step"

[[step]]
id = "deploy"
agent = "general"
requires = ["shell"]
prompt = "Deploy: {{input}}"

[approvals]
required_for = ["shell"]
"#;

const ONE_READ: &str = r#"
name = "one-read"
description = "one read"

[[step]]
id = "a"
agent = "explore"
requires = ["repo_read"]
prompt = "Look: {{input}}"
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

fn go(run: &mut Run, input: Input) -> Vec<Action> {
    run.apply(input).unwrap_or_else(|e| panic!("{e}"))
}

fn advance(run: &mut Run) -> Vec<Action> {
    go(run, Input::Advance)
}

fn approve(run: &mut Run, step: &str) {
    go(
        run,
        Input::Approve {
            step: step.into(),
            by: "rom".into(),
        },
    );
}

fn report_ok(run: &mut Run, step: &str) {
    go(
        run,
        Input::Report {
            step: step.into(),
            outcome: Outcome::Completed {
                artifact_valid: true,
            },
        },
    );
}

fn accounts(run: &mut Run, keys: &[&str]) {
    for k in keys {
        go(
            run,
            Input::SetAccount {
                step: (*k).into(),
                provider: "anthropic".into(),
                account: "main".into(),
            },
        );
    }
}

fn reasons<'a>(run: &'a Run, step: &str) -> Vec<&'a str> {
    run.scheduler()
        .events()
        .iter()
        .filter(|e| e.step.as_deref() == Some(step))
        .map(|e| e.reason_code.as_str())
        .collect()
}

#[test]
fn a_restart_mid_workflow_rebuilds_the_decisions_and_resumes_only_aibus_owned_steps() {
    let root = tempfile::tempdir().unwrap();
    let mut run = Run::create(root.path(), &plan_of(FAN)).unwrap();
    let id = run.run_id();
    accounts(&mut run, &["plan", "look", "scan"]);
    assert!(started(&advance(&mut run)).is_empty());
    approve(&mut run, "plan");
    assert_eq!(started(&advance(&mut run)), ["plan"]);
    report_ok(&mut run, "plan");
    advance(&mut run);
    approve(&mut run, "look");
    approve(&mut run, "scan");
    assert_eq!(started(&advance(&mut run)), ["look", "scan"]);
    let before = run.scheduler().clone();
    // The process dies with both running.
    drop(run);

    let back = Run::open(root.path(), id).unwrap();
    // Every decision is back as it was made, plus the interrupt the restart records.
    let events = back.scheduler().events();
    assert_eq!(&events[..before.events().len()], before.events());
    let granted: Vec<(&str, &Value)> = events
        .iter()
        .filter(|e| e.kind == EventKind::ApprovalGranted)
        .map(|e| (e.step.as_deref().unwrap(), &e.detail["by"]))
        .collect();
    assert_eq!(
        granted,
        [
            ("plan", &"rom".into()),
            ("look", &"rom".into()),
            ("scan", &"rom".into())
        ]
    );
    assert_eq!(back.scheduler().status("plan"), Some(&Status::Completed));
    assert_eq!(back.scheduler().state(), RunState::Interrupted);
    // Opening it again records nothing more: recovery is idempotent.
    let lines = trace(root.path(), id, back.attempt_id()).unwrap().len();
    drop(back);
    let mut back = Run::open(root.path(), id).unwrap();
    assert_eq!(
        trace(root.path(), id, back.attempt_id()).unwrap().len(),
        lines
    );

    go(&mut back, Input::Resume);
    assert!(started(&advance(&mut back)).is_empty());
    let s = back.scheduler();
    // The aibus step only asks for its approval again, as every attempt does.
    assert_eq!(s.pending("scan"), ["approval:repo_read"]);
    // The vendor's session is not restored: starting over in a new one waits for a person.
    assert!(s
        .pending("look")
        .contains(&"restart:vendor_session".to_owned()));
    assert!(reasons(&back, "look").contains(&"vendor_session_not_restored"));
    assert!(reasons(&back, "scan").contains(&"resumed_aibus_owned"));
    approve(&mut back, "scan");
    assert_eq!(started(&advance(&mut back)), ["scan"]);
    assert_eq!(
        back.scheduler().status("look"),
        Some(&Status::WaitingApproval)
    );
    // Nothing anywhere claims the vendor's session came back.
    assert!(!back
        .scheduler()
        .events()
        .iter()
        .any(|e| e.reason_code.contains("vendor_resume")));
}

#[test]
fn a_crash_after_an_irreversible_action_requires_reconciliation() {
    let root = tempfile::tempdir().unwrap();
    let mut run = Run::create(root.path(), &plan_of(DEPLOY)).unwrap();
    let id = run.run_id();
    accounts(&mut run, &["deploy"]);
    advance(&mut run);
    approve(&mut run, "deploy");
    assert_eq!(started(&advance(&mut run)), ["deploy"]);
    // It may have deployed before the process died; nobody knows.
    drop(run);

    let mut back = Run::open(root.path(), id).unwrap();
    go(&mut back, Input::Resume);
    assert_eq!(
        back.scheduler().status("deploy"),
        Some(&Status::RequiresReconciliation)
    );
    assert_eq!(back.scheduler().state(), RunState::RequiresReconciliation);
    // It is not run again on its own, not by advancing and not by a retry.
    assert!(started(&advance(&mut back)).is_empty());
    go(&mut back, Input::Cancel { by: "rom".into() });
    assert_eq!(
        back.scheduler().status("deploy"),
        Some(&Status::RequiresReconciliation)
    );
    assert!(matches!(
        back.retry(),
        Err(DurableError::NeedsReconciliation(s)) if s == ["deploy"]
    ));
    // And the question survives another restart.
    drop(back);
    let mut back = Run::open(root.path(), id).unwrap();
    assert_eq!(
        back.scheduler().status("deploy"),
        Some(&Status::RequiresReconciliation)
    );
    // A person answers, by name.
    assert!(back
        .apply(Input::Reconcile {
            step: "deploy".into(),
            applied: true,
            by: " ".into(),
        })
        .is_err());
    go(
        &mut back,
        Input::Reconcile {
            step: "deploy".into(),
            applied: true,
            by: "rom".into(),
        },
    );
    assert_eq!(back.scheduler().status("deploy"), Some(&Status::Completed));
    let answer = back
        .scheduler()
        .events()
        .iter()
        .find(|e| e.kind == EventKind::Reconciled)
        .unwrap();
    assert_eq!(answer.reason_code, "applied");
    assert_eq!(answer.detail["by"], "rom");
}

#[test]
fn a_not_applied_answer_runs_the_step_again_only_after_its_approval() {
    let root = tempfile::tempdir().unwrap();
    let mut run = Run::create(root.path(), &plan_of(DEPLOY)).unwrap();
    let id = run.run_id();
    accounts(&mut run, &["deploy"]);
    advance(&mut run);
    approve(&mut run, "deploy");
    advance(&mut run);
    drop(run);
    let mut back = Run::open(root.path(), id).unwrap();
    go(&mut back, Input::Resume);
    go(
        &mut back,
        Input::Reconcile {
            step: "deploy".into(),
            applied: false,
            by: "rom".into(),
        },
    );
    assert!(started(&advance(&mut back)).is_empty());
    assert_eq!(back.scheduler().pending("deploy"), ["approval:shell"]);
    approve(&mut back, "deploy");
    assert!(matches!(
        advance(&mut back).as_slice(),
        [Action::Start { step, attempt: 2, .. }] if step == "deploy"
    ));
}

#[test]
fn a_retry_is_a_new_attempt_and_the_old_trace_stays_readable() {
    let root = tempfile::tempdir().unwrap();
    let mut run = Run::create(root.path(), &plan_of(ONE_READ)).unwrap();
    let id = run.run_id();
    accounts(&mut run, &["a"]);
    assert_eq!(started(&advance(&mut run)), ["a"]);
    go(
        &mut run,
        Input::Report {
            step: "a".into(),
            outcome: Outcome::Failed {
                retryable: false,
                reason: "refused".into(),
            },
        },
    );
    assert_eq!(run.scheduler().state(), RunState::Failed);
    let old = run.attempt_id();
    let old_path = root
        .path()
        .join(id.to_string())
        .join(format!("{old}.jsonl"));
    let old_trace = trace(root.path(), id, old).unwrap();
    let old_bytes = std::fs::read(&old_path).unwrap();

    run.retry().unwrap();
    let new = run.attempt_id();
    assert_ne!(new, old);
    assert_eq!(attempts(root.path(), id).unwrap(), [old, new]);
    // The new attempt starts from the pinned plan and says which attempt it retries.
    assert_eq!(run.scheduler().state(), RunState::Queued);
    assert!(matches!(
        &trace(root.path(), id, new).unwrap()[0].input,
        Input::Begin { previous: Some(p), attempt_id, .. } if *p == old && *attempt_id == new
    ));
    accounts(&mut run, &["a"]);
    assert_eq!(started(&advance(&mut run)), ["a"]);
    report_ok(&mut run, "a");
    assert_eq!(run.scheduler().state(), RunState::Completed);

    // The old attempt is exactly as it was, readable, and no longer writable.
    assert_eq!(trace(root.path(), id, old).unwrap(), old_trace);
    assert_eq!(std::fs::read(&old_path).unwrap(), old_bytes);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&old_path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o400);
    }
    // A restart opens the latest attempt.
    drop(run);
    let back = Run::open(root.path(), id).unwrap();
    assert_eq!(back.attempt_id(), new);
    assert_eq!(back.scheduler().state(), RunState::Completed);
}

#[test]
fn every_record_has_an_id_a_time_and_the_next_sequence_number() {
    let root = tempfile::tempdir().unwrap();
    let mut run = Run::create(root.path(), &plan_of(ONE_READ)).unwrap();
    accounts(&mut run, &["a"]);
    advance(&mut run);
    // Asking again changes nothing, so nothing is written.
    advance(&mut run);
    assert!(run
        .apply(Input::Approve {
            step: "a".into(),
            by: "rom".into(),
        })
        .is_err());
    let recs = trace(root.path(), run.run_id(), run.attempt_id()).unwrap();
    assert_eq!(recs.len(), 3, "{recs:#?}");
    for (i, r) in recs.iter().enumerate() {
        assert_eq!(r.seq, i as u64 + 1);
        assert_eq!(r.id.get_version_num(), 7);
        assert!(r.at_unix_ms > 0);
    }
    let ids: std::collections::BTreeSet<_> = recs.iter().map(|r| r.id).collect();
    assert_eq!(ids.len(), recs.len());
    assert!(matches!(
        run.apply(Input::Begin {
            run_id: run.run_id(),
            attempt_id: run.attempt_id(),
            previous: None,
            plan_hash: String::new(),
            spent_handoffs: Default::default(),
        }),
        Err(DurableError::Refused(_))
    ));
}

#[test]
fn a_spent_handoff_stays_spent_across_a_restart_and_a_retry() {
    let party = |step: &str, runtime, provider: &str, account: &str| Party {
        step: step.into(),
        runtime,
        provider: provider.into(),
        account: account.into(),
    };
    let t = Transfer {
        context: Context {
            run_id: "run-1".into(),
            plan_hash: "sha256:plan-1".into(),
            from: party("draft", Runtime::CodexCli, "openai", "chatgpt-1"),
            to: party("review", Runtime::ClaudeCli, "anthropic", "claude-1"),
        },
        items: vec![item("prev.summary", "step_output", b"a draft")],
    };
    let shown = preview_sha256(&preview(&t));
    let a = record(
        t.clone(),
        Decision::Granted { by: "rom" },
        &shown,
        "01937a3e-5b2c-7d4e-8f10-0123456789ab",
        1_700_000_000,
    )
    .unwrap();
    let contents = || vec![("prev.summary".to_owned(), b"a draft".to_vec())];

    let root = tempfile::tempdir().unwrap();
    let mut run = Run::create(root.path(), &plan_of(ONE_READ)).unwrap();
    let id = run.run_id();
    // A refusal does not use it up, and writes nothing.
    let bad = vec![("prev.summary".to_owned(), b"edited".to_vec())];
    assert!(run.deliver(&a, &t.context, bad).is_err());
    assert_eq!(run.deliver(&a, &t.context, contents()).unwrap(), contents());
    drop(run);

    let mut back = Run::open(root.path(), id).unwrap();
    assert!(matches!(
        back.deliver(&a, &t.context, contents()),
        Err(DurableError::Handoff(HandoffError::Spent))
    ));
    go(&mut back, Input::Cancel { by: "rom".into() });
    back.retry().unwrap();
    assert!(matches!(
        back.deliver(&a, &t.context, contents()),
        Err(DurableError::Handoff(HandoffError::Spent))
    ));
}

#[test]
fn a_live_run_is_not_opened_twice() {
    let root = tempfile::tempdir().unwrap();
    let run = Run::create(root.path(), &plan_of(ONE_READ)).unwrap();
    assert!(matches!(
        Run::open(root.path(), run.run_id()),
        Err(DurableError::Live(_))
    ));
}

fn journal_of(root: &Path, run: &Run) -> std::path::PathBuf {
    root.join(run.run_id().to_string())
        .join(format!("{}.jsonl", run.attempt_id()))
}

#[test]
fn an_edited_journal_is_refused_and_a_torn_tail_is_dropped() {
    let root = tempfile::tempdir().unwrap();
    let mut run = Run::create(root.path(), &plan_of(DEPLOY)).unwrap();
    let id = run.run_id();
    accounts(&mut run, &["deploy"]);
    advance(&mut run);
    approve(&mut run, "deploy");
    let path = journal_of(root.path(), &run);
    drop(run);
    let good = std::fs::read_to_string(&path).unwrap();

    // Half a record nobody was told about is cut off.
    std::fs::write(&path, format!("{good}{{\"id\":\"0193")).unwrap();
    let back = Run::open(root.path(), id).unwrap();
    assert_eq!(back.scheduler().status("deploy"), Some(&Status::Ready));
    drop(back);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), good);

    // Who approved it is part of the decisions, so changing it does not replay.
    std::fs::write(&path, good.replace("\"by\":\"rom\"", "\"by\":\"eve\"")).unwrap();
    let err = Run::open(root.path(), id).err().unwrap().to_string();
    assert!(
        err.contains("does not give the decisions recorded"),
        "{err}"
    );

    // A journal others can write is not read at all.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&path, &good).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = Run::open(root.path(), id).err().unwrap().to_string();
        assert!(err.contains("owner-only"), "{err}");
    }
}
