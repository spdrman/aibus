//! G2: `Command::SetModel` in the reducer. The reducer cannot resolve an alias (it has no
//! accounts), so the command queues a request in `model:session:assist.pending`, the executor
//! answers with `Msg::ModelSwitched`, and that answer is what changes the graph.

mod common;

use aibus_core::state::value::{as_list, as_str, get};
use aibus_core::*;
use aibus_graph::{Graph, Value};
use common::*;

fn start(a: &mut A, run: RunId, agent: &str, role: Role, model: &str, account: &str) {
    a.step(Msg::RunStarted {
        run,
        agent: agent.parse().unwrap(),
        role,
        parent: None,
        model: ModelId(model.into()),
        account: AccountName(account.into()),
    })
    .unwrap();
}

fn lead(a: &mut A) {
    start(
        a,
        R(),
        "lead",
        Role::Lead,
        "claude-sonnet-4-5",
        "anthropic-work",
    );
}

fn meta(a: &A, run: RunId, key: &str) -> String {
    let m = a.graph.read_key(&format!("model:run:{run}:meta")).unwrap();
    get(m, key).and_then(as_str).unwrap().to_owned()
}

fn assist(a: &A) -> Value {
    a.graph.read_key("model:session:assist").unwrap().clone()
}

/// The requests the reducer queued for the executor, oldest first, as `(id, target, model)`.
fn pending(a: &A) -> Vec<(i64, String, String)> {
    let v = assist(a);
    get(&v, "pending")
        .map(as_list)
        .unwrap_or(&[])
        .iter()
        .map(|p| {
            (
                aibus_core::state::value::as_i64(get(p, "id").unwrap()).unwrap(),
                get(p, "target").and_then(as_str).unwrap().to_owned(),
                get(p, "model").and_then(as_str).unwrap().to_owned(),
            )
        })
        .collect()
}

fn logged(a: &A, needle: &str) -> bool {
    a.store.log_rows(500).iter().any(|r| {
        get(r, "text")
            .and_then(as_str)
            .is_some_and(|t| t.contains(needle))
    })
}

fn set(a: &mut A, target: ModelTarget, model: &str) -> u64 {
    a.step(Msg::Command(Command::SetModel {
        target,
        model: model.into(),
    }))
    .unwrap();
    pending(a).last().expect("the request was queued").0 as u64
}

fn switched(a: &mut A, id: u64, target: ModelTarget, to: Result<Option<(&str, &str)>, &str>) {
    let to = to
        .map(|o| o.map(|(m, acct)| (ModelId(m.into()), AccountName(acct.into()))))
        .map_err(str::to_owned);
    a.step(Msg::ModelSwitched { id, target, to }).unwrap();
}

#[test]
fn set_model_lead_changes_defaults_and_logs() {
    let mut a = app();
    lead(&mut a);
    let id = set(&mut a, ModelTarget::Lead, "haiku");
    assert_eq!(
        meta(&a, R(), "model"),
        "claude-sonnet-4-5",
        "nothing changes until the executor answers"
    );

    switched(
        &mut a,
        id,
        ModelTarget::Lead,
        Ok(Some(("claude-haiku-4-5", "anthropic-work"))),
    );
    assert_eq!(meta(&a, R(), "model"), "claude-haiku-4-5");
    assert_eq!(meta(&a, R(), "account"), "anthropic-work");
    assert!(pending(&a).is_empty(), "the answer clears the request");
    assert!(logged(
        &a,
        "model: lead -> claude-haiku-4-5 (anthropic-work)"
    ));
}

#[test]
fn set_model_run_changes_that_run_only() {
    let mut a = app();
    lead(&mut a);
    start(
        &mut a,
        rid(2),
        "grep",
        Role::Worker,
        "claude-sonnet-4-5",
        "anthropic-work",
    );
    let id = set(&mut a, ModelTarget::Run(rid(2)), "haiku");
    switched(
        &mut a,
        id,
        ModelTarget::Run(rid(2)),
        Ok(Some(("claude-haiku-4-5", "anthropic-work"))),
    );
    assert_eq!(meta(&a, rid(2), "model"), "claude-haiku-4-5");
    assert_eq!(
        meta(&a, R(), "model"),
        "claude-sonnet-4-5",
        "the lead keeps its model"
    );
}

#[test]
fn set_model_unknown_alias_changes_nothing() {
    let mut a = app();
    lead(&mut a);
    let before = (meta(&a, R(), "model"), meta(&a, R(), "account"));
    let id = set(&mut a, ModelTarget::Lead, "nope");
    switched(&mut a, id, ModelTarget::Lead, Err("no account serves nope"));
    assert_eq!((meta(&a, R(), "model"), meta(&a, R(), "account")), before);
    assert!(pending(&a).is_empty());
    assert!(logged(&a, "model: no account serves nope"));
}

#[test]
fn set_model_off_for_a_seat() {
    let mut a = app();
    lead(&mut a);
    let id = set(&mut a, ModelTarget::Advisor, "off");
    switched(&mut a, id, ModelTarget::Advisor, Ok(None));
    let v = assist(&a);
    assert_eq!(
        get(get(&v, "advisor").unwrap(), "enabled"),
        Some(&Value::Bool(false))
    );
    assert_eq!(
        get(get(&v, "router").unwrap(), "enabled"),
        Some(&Value::Bool(true)),
        "the router is untouched"
    );
    assert!(logged(&a, "model: advisor -> off"));

    // A seat that is switched on again shows its model.
    let id = set(&mut a, ModelTarget::Advisor, "opus");
    switched(
        &mut a,
        id,
        ModelTarget::Advisor,
        Ok(Some(("claude-opus-4-5", "anthropic-work"))),
    );
    let v = assist(&a);
    assert_eq!(
        get(get(&v, "advisor").unwrap(), "enabled"),
        Some(&Value::Bool(true))
    );
    assert_eq!(
        get(get(&v, "advisor").unwrap(), "label").and_then(as_str),
        Some("claude-opus-4-5")
    );
}

#[test]
fn the_lead_and_a_run_cannot_be_turned_off() {
    let mut a = app();
    lead(&mut a);
    a.step(Msg::Command(Command::SetModel {
        target: ModelTarget::Lead,
        model: "off".into(),
    }))
    .unwrap();
    assert!(pending(&a).is_empty());
    assert!(logged(&a, "model: the lead cannot be turned off"));
}

#[test]
fn a_slash_line_never_falls_through_to_the_lead_as_text() {
    let mut a = app();
    lead(&mut a);
    a.step(Msg::SubmitPrompt {
        text: "/model opus".into(),
    })
    .unwrap();
    assert_eq!(
        pending(&a),
        [(pending(&a)[0].0, "lead".to_owned(), "opus".to_owned())]
    );
    assert!(!logged(&a, "prompt for the running lead"));
}
