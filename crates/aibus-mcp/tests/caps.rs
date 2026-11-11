mod common;

use std::time::Instant;

use aibus_core::Redactor;
use aibus_mcp::Caps;
use serde_json::json;

#[test]
fn gates_without_write_is_a_usage_error() {
    let err = Caps::new(false, true, false, vec![]).unwrap_err();
    assert!(err.contains("--allow write"), "{err}");
    assert!(Caps::new(true, true, false, vec![]).is_ok());
    assert!(Caps::new(true, false, true, vec!["Read".into()]).is_ok());
}

#[test]
fn the_tool_list_follows_the_flags_and_the_fixed_order() {
    // Until the write and run tools land, no flag lists anything but the read tool. The loop is
    // here so the PR that adds a tool has to say which flag lists it.
    for (write, gates, run) in [
        (false, false, false),
        (true, false, false),
        (true, true, false),
        (false, false, true),
        (true, true, true),
    ] {
        let (server, _home) = common::server(Caps::new(write, gates, run, vec![]).unwrap(), Redactor::new());
        let names: Vec<_> = server.tools().iter().map(|t| t.name.to_string()).collect();
        assert_eq!(names, ["workflow_list"], "write={write} gates={gates} run={run}");
        // What is not listed cannot be called, with or without the flag.
        for off in ["workflow_create", "workflow_update", "workflow_run", "run_status", "run_cancel"] {
            let e = server.call(None, off, Some(json!({}).as_object().unwrap().clone()), Instant::now()).unwrap_err();
            assert_eq!(e.code.0, -32602, "{off}");
        }
    }
}

#[test]
fn every_listed_tool_says_what_it_does_to_the_world() {
    let (server, _home) = common::server(common::caps(), Redactor::new());
    for t in server.tools() {
        let a = t.annotations.as_ref().expect("annotations");
        assert!(a.read_only_hint.is_some() && a.destructive_hint.is_some(), "{}", t.name);
        assert!(a.idempotent_hint.is_some() && a.open_world_hint.is_some(), "{}", t.name);
        assert_eq!(t.input_schema["additionalProperties"], false, "{}", t.name);
        assert!(t.output_schema.is_some(), "{}", t.name);
    }
}
