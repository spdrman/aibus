//! The confirmation step over the wire (E12, #107): what a client with and without `elicitation`
//! sees on `workflow_create`, on both protocol revisions. The decisions behind it are tested
//! directly in `crates/aibus-mcp/tests/confirm.rs`.
//!
//! These drive `workflow_create {name, scope, toml}` behind `--allow write` (#103), so they go
//! green with that tool; until it is on the branch they fail at the first call.

mod common;

use std::fs;
use std::path::Path;

use aibus_testkit::xdg_temp;
use common::mcp_client::{mcp_command, Answer, McpClient, Protocol};
use serde_json::{json, Value};

const TOML: &str = "name = \"ship\"\ndescription = \"Plan, build\"\n\n\
                    [[step]]\nagent = \"plan\"\nprompt = \"Plan this: {{input}}\"\n";

fn ship_file(home: &Path) -> std::path::PathBuf {
    home.join("proj/.aibus/workflows/ship.toml")
}

fn args() -> Value {
    json!({ "name": "ship", "scope": "project", "toml": TOML })
}

/// A client past its handshake, with or without the `elicitation` capability.
fn client(home: &Path, p: Protocol, elicitation: bool, extra: &[&str]) -> McpClient {
    fs::create_dir_all(home.join("proj/.aibus")).unwrap();
    let mut flags = vec!["--allow", "write"];
    flags.extend_from_slice(extra);
    let mut c = McpClient::spawn(mcp_command(home, &flags), p, home);
    if elicitation {
        c.capabilities = json!({ "elicitation": { "form": {} } });
    }
    if p == Protocol::V2025_11_25 {
        c.initialize_legacy().expect("initialize");
    }
    c
}

fn is_error(result: &Value) -> bool {
    result["isError"] == true
}

fn text(result: &Value) -> String {
    result["content"][0]["text"].as_str().unwrap_or_default().to_owned()
}

#[test]
fn a_client_without_elicitation_sees_no_confirmation() {
    for p in Protocol::BOTH {
        let home = xdg_temp();
        let mut c = client(home.path(), p, false, &[]);
        let r = c.call("workflow_create", args()).expect("workflow_create");
        assert!(!is_error(&r), "{p:?}: {r}");
        assert!(r.get("resultType").is_none_or(|t| t == "complete"), "{p:?}: {r}");
        assert!(c.elicitations.is_empty(), "{p:?}: a client that never declared elicitation was asked");
        assert!(ship_file(home.path()).exists(), "{p:?}");
    }
}

#[test]
fn legacy_clients_get_one_elicitation_and_need_accept_with_the_name() {
    let accept = |name: &'static str| -> Answer {
        Box::new(move |_| json!({ "action": "accept", "content": { "name": name } }))
    };
    let decide = |action: &'static str| -> Answer {
        Box::new(move |_| json!({ "action": action }))
    };

    // accept with the name: exactly one question, then the write.
    let home = xdg_temp();
    let mut c = client(home.path(), Protocol::V2025_11_25, true, &[]);
    c.answer_elicitation = Some(accept("ship"));
    let r = c.call("workflow_create", args()).unwrap();
    assert!(!is_error(&r), "{r}");
    assert_eq!(c.elicitations.len(), 1, "{:?}", c.elicitations);
    let form = &c.elicitations[0];
    assert!(form["message"].as_str().unwrap().contains("ship"), "{form}");
    assert_eq!(form["requestedSchema"]["required"], json!(["name"]));
    assert!(ship_file(home.path()).exists());

    // decline, cancel and a wrong name: isError naming the choice, and nothing is written.
    for (answer, want) in [
        (decide("decline"), "declined"),
        (decide("cancel"), "cancelled"),
        (accept("shop"), "not the workflow's name"),
    ] {
        let home = xdg_temp();
        let mut c = client(home.path(), Protocol::V2025_11_25, true, &[]);
        c.answer_elicitation = Some(answer);
        let r = c.call("workflow_create", args()).unwrap();
        assert!(is_error(&r) && text(&r).contains(want), "{want}: {r}");
        assert_eq!(c.elicitations.len(), 1);
        assert!(!ship_file(home.path()).exists(), "{want}: the file was written");
    }
}

#[test]
fn stateless_clients_get_input_required_and_a_valid_retry_completes() {
    let home = xdg_temp();
    let mut c = client(home.path(), Protocol::V2026_07_28, true, &[]);
    let first = c.call("workflow_create", args()).unwrap();
    assert_eq!(first["resultType"], "input_required", "{first}");
    assert_eq!(first["inputRequests"]["confirm"]["method"], "elicitation/create", "{first}");
    let state = first["requestState"].as_str().expect("a requestState").to_owned();
    assert!(c.elicitations.is_empty(), "2026-07-28 servers send no requests of their own");
    assert!(!ship_file(home.path()).exists(), "nothing is written before the answer");

    let retry = |c: &mut McpClient, state: &str, name: &str| {
        c.request(
            "tools/call",
            json!({
                "name": "workflow_create", "arguments": args(), "requestState": state,
                "inputResponses": { "confirm": { "action": "accept", "content": { "name": name } } },
            }),
        )
        .unwrap()
    };
    // An edited state, and a wrong name under a good state, write nothing.
    let mut edited = state.clone().into_bytes();
    edited[state.len() / 2] ^= 1;
    let r = retry(&mut c, &String::from_utf8_lossy(&edited), "ship");
    assert!(is_error(&r), "{r}");
    assert!(!ship_file(home.path()).exists());

    let fresh = c.call("workflow_create", args()).unwrap()["requestState"].as_str().unwrap().to_owned();
    let done = retry(&mut c, &fresh, "ship");
    assert!(!is_error(&done), "{done}");
    assert!(ship_file(home.path()).exists());
    // The same retry again is a replay and the state is spent.
    let again = retry(&mut c, &fresh, "ship");
    assert!(is_error(&again), "{again}");
}

#[test]
fn confirm_never_skips_it() {
    for p in Protocol::BOTH {
        let home = xdg_temp();
        let mut c = client(home.path(), p, true, &["--confirm", "never"]);
        let r = c.call("workflow_create", args()).unwrap();
        assert!(!is_error(&r) && r.get("inputRequests").is_none(), "{p:?}: {r}");
        assert!(c.elicitations.is_empty(), "{p:?}");
        assert!(ship_file(home.path()).exists(), "{p:?}");
    }
}
