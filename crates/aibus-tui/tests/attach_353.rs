//! #353: a workflow step with `runtime = "attached"` reaches the attached agent through the same
//! pull as #322, on screen, tagged with the client, and the workflow hears how it ended. Nothing
//! changes in who accepts: the typed code is still the keyboard's.

use std::time::Duration;

mod common;

use aibus_core::attach::{Attachments, Dispatch, StepState};
use aibus_core::remote::{Ask, Remote};
use aibus_core::state::value::{as_str, get};
use aibus_graph::Graph;
use aibus_tui::{remote, Theme};
use common::*;
use serde_json::Value as Json;

fn rig(attach: bool) -> Rig {
    let mut r = Rig::new(session(), (120, 40), Theme::recording());
    r.ui.attach = Attachments::new(attach, false);
    r
}

fn call(r: &mut Rig, ask: Ask) -> Result<Json, String> {
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    let event = Remote::Call {
        client: "editor".into(),
        ask,
        reply: tx,
    };
    remote::handle(&mut r.app, &mut r.ui, event, false, &mut |app, m| {
        app.step(m)
    })
    .unwrap();
    r.settle();
    rx.try_recv().expect("every call is answered at once")
}

fn send(r: &mut Rig, d: Dispatch) {
    remote::workflow_step(&mut r.app, &mut r.ui, d, &mut |app, m| app.step(m)).unwrap();
    r.settle();
}

fn log_text(r: &Rig) -> String {
    r.app
        .store
        .log_rows(200)
        .iter()
        .filter_map(|row| get(row, "text").and_then(as_str).map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n")
}

fn attached(r: &mut Rig) {
    let event = Remote::Connected {
        client: "editor".into(),
    };
    remote::handle(&mut r.app, &mut r.ui, event, false, &mut |app, m| {
        app.step(m)
    })
    .unwrap();
    call(
        r,
        Ask::Attach {
            label: None,
            lineage: vec![],
            pid: Some(4123),
            cwd: None,
        },
    )
    .unwrap();
    let log = log_text(r);
    let at = log.rfind("code ").unwrap() + 5;
    let line = format!("/attach accept {}", &log[at..at + 4]);
    remote::keyboard(
        &line,
        &mut r.app,
        &mut r.ui,
        std::time::Instant::now(),
        &mut |app, m| app.step(m),
    )
    .unwrap()
    .unwrap();
    r.settle();
}

fn step_ask(
    name: &str,
    prompt: &str,
) -> (
    Dispatch,
    tokio::sync::oneshot::Receiver<Result<aibus_core::attach::StepEnd, String>>,
) {
    let (reply, rx) = tokio::sync::oneshot::channel();
    (
        Dispatch::Step {
            name: name.into(),
            prompt: prompt.into(),
            within: Duration::from_secs(60),
            reply,
        },
        rx,
    )
}

#[test]
fn a_workflow_step_is_pulled_answered_and_the_workflow_hears_it_with_a_tagged_log() {
    let mut r = rig(true);
    attached(&mut r);
    let (ask, mut ended) = step_ask("plan", "Review what was found");
    send(&mut r, ask);
    assert!(log_text(&r).contains("workflow step"), "{}", log_text(&r));
    assert!(
        ended.try_recv().is_err(),
        "the workflow waits for the agent"
    );
    let pulled = call(&mut r, Ask::NextStep).unwrap();
    assert!(pulled["step"]["prompt"]
        .as_str()
        .unwrap()
        .contains("Review what was found"));
    let token = pulled["step"]["token"].as_str().unwrap().to_owned();
    call(
        &mut r,
        Ask::StepResult {
            token,
            status: "done".into(),
            summary: "two findings".into(),
        },
    )
    .unwrap();
    let end = ended.try_recv().expect("the workflow was told").unwrap();
    assert_eq!(end.state, StepState::Done);
    assert_eq!(end.summary, "two findings");
    assert!(r
        .app
        .store
        .log_rows(200)
        .iter()
        .any(|row| get(row, "via").and_then(as_str) == Some("mcp")));
}

#[test]
fn a_step_is_refused_when_nobody_is_attached_or_the_session_did_not_opt_in() {
    for opted_in in [true, false] {
        let mut r = rig(opted_in);
        let (ask, mut ended) = step_ask("plan", "x");
        send(&mut r, ask);
        let why = ended.try_recv().expect("answered at once").unwrap_err();
        assert!(why.contains("no attached session"), "{why}");
        assert!(r.ui.attach.steps().is_empty());
    }
}

#[test]
fn a_client_that_goes_away_ends_the_workflows_step() {
    let mut r = rig(true);
    attached(&mut r);
    let (ask, mut ended) = step_ask("plan", "x");
    send(&mut r, ask);
    remote::handle(
        &mut r.app,
        &mut r.ui,
        Remote::Disconnected {
            client: "editor".into(),
        },
        false,
        &mut |app, m| app.step(m),
    )
    .unwrap();
    assert_eq!(
        ended.try_recv().unwrap().unwrap().state,
        StepState::Unavailable
    );
}
