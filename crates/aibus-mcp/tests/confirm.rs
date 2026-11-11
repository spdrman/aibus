//! The confirmation step (E12, #107) at the level of its decisions: the form, the verdict on an
//! answer, and the `requestState` of the `2026-07-28` path. The protocol-level scenarios (a client
//! with and without `elicitation`, both revisions) are `crates/aibus/tests/mcp_confirm.rs`.
//!
//! The write and run tools are other PRs. These tests name them as the issue does and call the
//! confirmation functions directly, so they do not need the tools to exist.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use aibus_core::Redactor;
use aibus_mcp::{Caps, ConfirmConfig, Server};
use rmcp::model::{ElicitResult, ElicitationAction, Implementation, InputResponses};
use serde_json::{json, Value};

fn server(config: ConfirmConfig) -> (Server, aibus_testkit::Xdg) {
    let (server, home) = common::server(Caps::default(), Redactor::new());
    (server.with_confirm(config), home)
}

fn client(name: &str) -> Implementation {
    Implementation::new(name, "1.0.0")
}

fn answer(action: ElicitationAction, content: Option<Value>) -> ElicitResult {
    let r = ElicitResult::new(action);
    match content {
        Some(c) => r.with_content(c),
        None => r,
    }
}

fn responses(a: &ElicitResult) -> InputResponses {
    BTreeMap::from([("confirm".to_owned(), serde_json::to_value(a).unwrap())])
}

/// The state string of a first leg.
fn state_of(asked: &rmcp::model::InputRequiredResult) -> String {
    asked.request_state.clone().expect("a requestState")
}

fn properties(form: &rmcp::model::ElicitRequestParams) -> Value {
    serde_json::to_value(form).unwrap()["requestedSchema"].clone()
}

#[test]
fn confirm_never_skips_it() {
    let (server, _home) = server(ConfirmConfig { elicit: false, ..ConfirmConfig::default() });
    let args = json!({ "name": "ship" });
    for tool in ["workflow_create", "workflow_update", "workflow_run"] {
        assert!(server.confirmation_form(tool, &args).is_none(), "{tool}");
        assert!(server.confirmation_ask(tool, &args, None).is_none(), "{tool}");
    }
}

#[test]
fn only_the_three_tools_with_a_name_get_a_form_and_the_name_is_pinned() {
    let (server, _home) = server(ConfirmConfig::default());
    for tool in ["workflow_list", "workflow_validate", "run_status", "run_cancel", "workflow_get"] {
        assert!(server.confirmation_form(tool, &json!({ "name": "ship" })).is_none(), "{tool}");
    }
    // No name to retype: the tool refuses the call on its own.
    assert!(server.confirmation_form("workflow_create", &json!({})).is_none());
    assert!(server.confirmation_form("workflow_create", &json!({ "name": "" })).is_none());
    assert!(server.confirmation_form("workflow_create", &json!({ "name": 7 })).is_none());

    let form = server.confirmation_form("workflow_create", &json!({ "name": "ship" })).expect("a form");
    let schema = properties(&form);
    assert_eq!(schema["required"], json!(["name"]));
    assert_eq!(schema["properties"]["name"]["minLength"], 4);
    assert_eq!(schema["properties"]["name"]["maxLength"], 4);
    assert!(schema["properties"].get("max_cost_usd").is_none(), "a write has no cost field");
}

#[test]
fn a_run_form_has_the_cost_field_only_under_a_cap() {
    let (uncapped, _h) = server(ConfirmConfig::default());
    let form = uncapped.confirmation_form("workflow_run", &json!({ "name": "ship" })).unwrap();
    assert!(properties(&form)["properties"].get("max_cost_usd").is_none());

    let (capped, _h) = server(ConfirmConfig { run_cap_usd: Some(5.0), ..ConfirmConfig::default() });
    let schema = properties(&capped.confirmation_form("workflow_run", &json!({ "name": "ship" })).unwrap());
    assert_eq!(schema["properties"]["max_cost_usd"]["type"], "number");
    assert_eq!(schema["properties"]["max_cost_usd"]["maximum"], 5.0);
    assert_eq!(schema["required"], json!(["name", "max_cost_usd"]));
    // A create under the same cap is still just the name.
    let create = capped.confirmation_form("workflow_create", &json!({ "name": "ship" })).unwrap();
    assert!(properties(&create)["properties"].get("max_cost_usd").is_none());
}

#[test]
fn only_accept_with_the_exact_name_goes_on() {
    let (server, _home) = server(ConfirmConfig { run_cap_usd: Some(5.0), ..ConfirmConfig::default() });
    let args = json!({ "name": "ship" });
    let accept = |c: Value| answer(ElicitationAction::Accept, Some(c));

    assert!(server.confirmation_check("workflow_create", &args, &accept(json!({ "name": "ship" }))).is_ok());
    for wrong in [json!({ "name": "Ship" }), json!({ "name": "ship " }), json!({ "name": 4 }), json!({})] {
        let e = server.confirmation_check("workflow_create", &args, &accept(wrong.clone())).unwrap_err();
        assert!(e.contains("not the workflow's name"), "{wrong}: {e}");
    }
    assert!(server.confirmation_check("workflow_create", &args, &answer(ElicitationAction::Accept, None)).is_err());

    let declined = server.confirmation_check("workflow_create", &args, &answer(ElicitationAction::Decline, None)).unwrap_err();
    let cancelled = server.confirmation_check("workflow_create", &args, &answer(ElicitationAction::Cancel, None)).unwrap_err();
    assert!(declined.contains("declined"), "{declined}");
    assert!(cancelled.contains("cancelled"), "{cancelled}");

    // A run also needs a cost under the cap.
    assert!(server.confirmation_check("workflow_run", &args, &accept(json!({ "name": "ship", "max_cost_usd": 2.5 }))).is_ok());
    for bad in [json!({ "name": "ship" }), json!({ "name": "ship", "max_cost_usd": 5.01 }), json!({ "name": "ship", "max_cost_usd": 0 })] {
        assert!(server.confirmation_check("workflow_run", &args, &accept(bad.clone())).is_err(), "{bad}");
    }
}

#[test]
fn a_valid_state_and_answer_complete_the_retry_once() {
    let (server, _home) = server(ConfirmConfig::default());
    let (args, who) = (json!({ "name": "ship", "scope": "project" }), client("c1"));
    let asked = server.confirmation_ask("workflow_create", &args, Some(&who)).expect("first leg");
    let ask = serde_json::to_value(&asked).unwrap();
    assert_eq!(ask["resultType"], "input_required");
    assert_eq!(ask["inputRequests"]["confirm"]["method"], "elicitation/create");
    let yes = responses(&answer(ElicitationAction::Accept, Some(json!({ "name": "ship" }))));
    let state = state_of(&asked);
    assert!(server.confirmation_resume("workflow_create", &args, Some(&who), &state, Some(&yes)).is_ok());
    // The same state and answer again is a replay.
    assert!(server.confirmation_resume("workflow_create", &args, Some(&who), &state, Some(&yes)).is_err());
}

#[test]
fn an_edited_expired_or_foreign_request_state_is_refused_and_writes_nothing() {
    let (server, home) = server(ConfirmConfig::default());
    let (args, who) = (json!({ "name": "ship" }), client("c1"));
    let yes = responses(&answer(ElicitationAction::Accept, Some(json!({ "name": "ship" }))));
    let fresh = || state_of(&server.confirmation_ask("workflow_create", &args, Some(&who)).unwrap());
    let refused = |r: Result<(), String>, what: &str| {
        let e = r.expect_err(what);
        assert!(e.contains("Nothing was written or started"), "{what}: {e}");
    };

    // Edited: every position of the string, one character changed, never opens.
    let good = fresh();
    // Not the last characters: the unused bits of the final base64 digit do not change the bytes.
    for i in (0..good.len() - 3).step_by(7) {
        let mut edited: Vec<u8> = good.clone().into_bytes();
        edited[i] = if edited[i] == b'A' { b'B' } else { b'A' };
        let edited = String::from_utf8(edited).unwrap();
        if edited != good {
            refused(server.confirmation_resume("workflow_create", &args, Some(&who), &edited, Some(&yes)), "edited");
        }
    }
    refused(server.confirmation_resume("workflow_create", &args, Some(&who), "rs1.not.a.state", Some(&yes)), "garbage");
    refused(server.confirmation_resume("workflow_create", &args, Some(&who), "", Some(&yes)), "empty");

    // Foreign: another tool, other arguments, another client, another process.
    refused(server.confirmation_resume("workflow_update", &args, Some(&who), &fresh(), Some(&yes)), "other tool");
    refused(
        server.confirmation_resume("workflow_create", &json!({ "name": "ship", "scope": "user" }), Some(&who), &fresh(), Some(&yes)),
        "other arguments",
    );
    refused(server.confirmation_resume("workflow_create", &args, Some(&client("c2")), &fresh(), Some(&yes)), "other client");
    refused(server.confirmation_resume("workflow_create", &args, None, &fresh(), Some(&yes)), "no client");
    let (other, _h) = common::server(Caps::default(), Redactor::new());
    refused(other.confirmation_resume("workflow_create", &args, Some(&who), &fresh(), Some(&yes)), "other process");

    // Reordered keys are the same call.
    let reordered = json!({ "scope": "project", "name": "ship" });
    let a = server.confirmation_ask("workflow_create", &reordered, Some(&who)).unwrap();
    let swapped: Value = serde_json::from_str(r#"{"name":"ship","scope":"project"}"#).unwrap();
    assert!(server.confirmation_resume("workflow_create", &swapped, Some(&who), &state_of(&a), Some(&yes)).is_ok());

    // Expired.
    let (short, _h) = server_with_ttl(Duration::from_millis(1));
    let old = state_of(&short.confirmation_ask("workflow_create", &args, Some(&who)).unwrap());
    std::thread::sleep(Duration::from_millis(60));
    refused(short.confirmation_resume("workflow_create", &args, Some(&who), &old, Some(&yes)), "expired");

    // A good state with no answer, or the wrong answer, is also not a go; and a refused retry
    // spends the state, so the right answer afterwards does not revive it.
    let s = fresh();
    assert!(server.confirmation_resume("workflow_create", &args, Some(&who), &s, None).is_err());
    assert!(server.confirmation_resume("workflow_create", &args, Some(&who), &s, Some(&yes)).is_err());

    // Nothing was written anywhere: the project and the user's workflow directories are empty.
    assert!(!home.path().join("proj/.aibus").exists());
    assert!(std::fs::read_dir(&home.paths.config).map_or(true, |d| d.count() == 0));
}

fn server_with_ttl(ttl: Duration) -> (Server, aibus_testkit::Xdg) {
    server(ConfirmConfig { ttl, ..ConfirmConfig::default() })
}

/// Every form the server can emit, for every gated tool, with and without a cap and with names of
/// every shape. None may ask for a secret or anything shaped like one (the spec forbids it, and so
/// does the epic).
#[test]
fn no_form_ever_asks_for_a_credential_shaped_field() {
    const BANNED: [&str; 12] =
        ["key", "token", "secret", "password", "passwd", "credential", "auth", "bearer", "apikey", "api_key", "cookie", "pin"];
    let names: Vec<String> =
        ["ship", "a", "sk-ant-api03-Zq8wLm2Xv5Rt7Yb1Nc4Hd6Gf9Jk3P", "héllo wörld"].map(String::from).into();
    let names: Vec<String> = names.into_iter().chain([ "x".repeat(64)]).collect();
    for cap in [None, Some(1.0), Some(100.0)] {
        let (server, _home) = server(ConfirmConfig { run_cap_usd: cap, ..ConfirmConfig::default() });
        for tool in ["workflow_create", "workflow_update", "workflow_run"] {
            for name in &names {
                let Some(form) = server.confirmation_form(tool, &json!({ "name": name })) else { continue };
                let wire = serde_json::to_value(&form).unwrap();
                assert!(!wire.to_string().contains("sk-ant-"), "the redactor missed a key-shaped name in {wire}");
                let props = wire["requestedSchema"]["properties"].as_object().expect("properties");
                for (field, def) in props {
                    assert!(["name", "max_cost_usd"].contains(&field.as_str()), "{tool}: unexpected field {field}");
                    let kind = def["type"].as_str().unwrap_or_default();
                    assert!(kind == "string" || kind == "number", "{tool}.{field}: type {kind}");
                    for word in BANNED {
                        assert!(!field.to_lowercase().contains(word), "{tool}: field {field} looks like a credential");
                    }
                    assert!(def.get("format").is_none(), "{tool}.{field} has a format, so it is not plain text");
                    let text = format!("{} {}", def["title"], def["description"]).to_lowercase();
                    for word in ["password", "secret", "token", "api key", "credential"] {
                        assert!(!text.contains(word), "{tool}.{field}: the label mentions {word}");
                    }
                }
            }
        }
    }
}
