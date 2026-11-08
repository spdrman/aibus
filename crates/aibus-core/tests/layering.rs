mod common;

use std::collections::BTreeSet;

use aibus_core::state::value::as_list;
use aibus_core::state::{namespace, NAMESPACES};
use aibus_core::*;
use aibus_graph::{Graph, Node, Value};
use common::*;

fn transitive_keys(a: &A, node: Node) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        for d in a.graph.dependencies(n) {
            if seen.insert(a.graph.explain(d).key.to_string()) {
                stack.push(d);
            }
        }
    }
    seen
}

#[test]
fn every_registered_key_is_in_a_known_namespace_and_layers_hold() {
    let mut a = app();
    running(&mut a);
    a.step(Msg::RouterAnswered {
        fork: ForkId(1),
        question: RouterQuestion {
            kind: "which_file".into(),
            prompt: String::new(),
        },
        answer: RouterAnswer {
            choice: String::new(),
            p: 1.0,
            verdict: Verdict::Sharp,
        },
    })
    .unwrap();
    assert!(a.keys.len() > 40);
    for key in &a.keys {
        let ns = namespace(key).unwrap_or_else(|| panic!("{key} is outside {NAMESPACES:?}"));
        let node = a
            .graph
            .node(key)
            .unwrap_or_else(|| panic!("{key} not registered"));
        let deps = transitive_keys(&a, node);
        let allowed: &[&str] = match ns {
            "model" => &["model", "eng"],
            "eng" => &["eng"],
            "ctrl" => &[],
            _ => &["model", "eng", "ctrl", "view"],
        };
        for d in deps {
            let dns = namespace(&d).unwrap();
            assert!(allowed.contains(&dns), "{key} ({ns}) reads {d} ({dns})");
        }
    }
    // Every input in a snapshot was registered through the layered path.
    for k in a.graph.snapshot().inputs.keys() {
        assert!(
            a.keys.contains(k),
            "{k} is in the snapshot but was never registered"
        );
    }
}

#[test]
fn every_key_of_03_section_5_exists() {
    let a = app();
    let wanted = [
        "model:session:id",
        "model:session:topic",
        "model:agents:defs",
        "model:agents:warnings",
        "model:accounts",
        "model:runs:order",
        "model:tools:pending",
        "model:permissions:open",
        "model:router:forks",
        "model:advisor:state",
        "model:advisor:advice",
        "model:log",
        "model:ledger",
        "model:handoff:latest",
        "model:limits",
        "model:last_msg",
        "model:hooks:due",
        "model:router:queue",
        "ctrl:focus",
        "ctrl:selected:card",
        "ctrl:selected:log",
        "ctrl:scroll:cards",
        "ctrl:modal",
        "ctrl:palette:text",
        "ctrl:log:filter",
        "ctrl:size",
        "model:orchestrator:runnable",
        "model:orchestrator:counts",
        "model:checkpoint:pending",
        "model:persist:due",
        "view:animating",
        "eng:tick",
        "eng:ticks_dropped",
        "eng:clock_ms",
        "eng:schema_version",
    ];
    for k in wanted {
        assert!(a.graph.node(k).is_some(), "{k} missing");
    }
    // A run has the nine fields of 03 section 5.
    let mut a = app();
    running(&mut a);
    for f in aibus_core::state::RUN_FIELDS {
        assert!(
            a.graph.node(&aibus_core::state::run_key(R(), f)).is_some(),
            "run field {f}"
        );
    }
}

#[test]
fn accounts_have_no_secret_shaped_field() {
    // The type: exactly these five fields.
    let v = serde_json::to_value(AccountView {
        name: AccountName("a".into()),
        provider: "p".into(),
        label: "l".into(),
        default_model: None,
        key_ref: "env:AIBUS_ACCOUNT_A_KEY".into(),
    })
    .unwrap();
    let fields: BTreeSet<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        fields,
        ["default_model", "key_ref", "label", "name", "provider"]
            .into_iter()
            .collect()
    );
    // The node: after a reload, no map key anywhere looks like a secret.
    let mut a = app();
    for c in cases()
        .into_iter()
        .filter(|c| c.msg.name() == "AccountsReloaded")
    {
        a.step(c.msg).unwrap();
    }
    let accounts = a.graph.read_key("model:accounts").unwrap();
    assert_eq!(as_list(accounts).len(), 1);
    let json = serde_json::to_string(accounts).unwrap().to_lowercase();
    for bad in [
        "secret",
        "password",
        "token",
        "api_key",
        "apikey",
        "credential",
        "bearer",
    ] {
        assert!(!json.contains(bad), "accounts value mentions {bad}: {json}");
    }
    assert!(json.contains("keyring:aibus/anthropic-work"));
    let _ = Value::Null;
}
