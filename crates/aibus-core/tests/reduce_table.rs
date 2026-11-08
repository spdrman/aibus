mod common;

use std::collections::BTreeSet;

use aibus_core::state::reduce::reduce;
use aibus_core::state::value::{as_str, get};
use aibus_graph::{Graph, Value};
use common::*;

#[test]
fn write_sets_are_exact_for_every_msg() {
    for case in cases() {
        let mut a = app();
        (case.setup)(&mut a);
        let writes = reduce(&a.graph, &a.store, &case.msg, env()).unwrap();
        let mut want: Vec<String> = case.expect.iter().map(|k| (*k).to_owned()).collect();
        want.push("eng:msg_seq".into());
        want.push("model:last_msg".into());
        want.sort();
        assert_eq!(
            norm_keys(&writes),
            want,
            "{} ({:?})",
            case.name,
            case.msg.name()
        );
    }
}

#[test]
fn every_variant_and_every_command_has_a_case() {
    let names: BTreeSet<&str> = cases().iter().map(|c| c.msg.name()).collect();
    assert_eq!(names, ALL_NAMES.into_iter().collect::<BTreeSet<_>>());
    let verbs: BTreeSet<&str> = cases()
        .iter()
        .filter_map(|c| match &c.msg {
            aibus_core::Msg::Command(cmd) => Some(aibus_core::msg::command_name(cmd)),
            _ => None,
        })
        .collect();
    assert_eq!(verbs.len(), 20);
}

#[test]
fn a_big_tool_input_goes_behind_a_handle_and_a_small_one_stays_inline() {
    let mut a = app();
    running(&mut a);
    let mut small = tool_call(false);
    small.input = serde_json::json!({"command": "ls"});
    a.step(aibus_core::Msg::ToolCallRequested {
        run: R(),
        call: small,
    })
    .unwrap();
    let mut big = tool_call(false);
    big.id = aibus_core::ToolCallId(ulid::Ulid::from_parts(1, 99));
    big.input = serde_json::json!({"content": "x".repeat(5000)});
    a.step(aibus_core::Msg::ToolCallRequested {
        run: R(),
        call: big,
    })
    .unwrap();

    let Some(Value::List(items)) = a.graph.read_key("model:tools:pending") else {
        panic!("not a list")
    };
    assert!(matches!(get(&items[0], "input"), Some(Value::Map(_))));
    let Some(Value::Handle(h)) = get(&items[1], "input") else {
        panic!("big input was not turned into a handle")
    };
    assert!(a.store.text(h).unwrap().contains("xxxx"));
    assert_eq!(as_str(get(&items[1], "name").unwrap()), Some("bash"));
}

#[test]
fn the_agent_prompt_body_never_enters_the_graph() {
    let mut a = app();
    for c in cases()
        .into_iter()
        .filter(|c| c.msg.name() == "AgentsReloaded")
    {
        a.step(c.msg).unwrap();
    }
    let Some(Value::List(items)) = a.graph.read_key("model:agents:defs") else {
        panic!()
    };
    assert!(matches!(get(&items[0], "prompt"), Some(Value::Handle(_))));
    let snapshot = serde_json::to_string(&a.graph.snapshot()).unwrap();
    // The description is summary text and may appear; the body behind the handle may not.
    assert_eq!(
        snapshot.matches(SENTINEL).count(),
        2,
        "description and warning only"
    );
}
