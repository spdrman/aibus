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
    // `gates` lists nothing of its own: it widens what `workflow_create` and `workflow_update`
    // accept. The read tools are listed whatever the flags are, and come first.
    let (plain, _home) = common::server(Caps::default(), Redactor::new());
    let reading: Vec<String> = plain.tools().iter().map(|t| t.name.to_string()).collect();
    assert_eq!(reading[0], "workflow_list");
    for gated in ["workflow_create", "workflow_update", "workflow_run", "run_status", "run_cancel"] {
        assert!(!reading.iter().any(|n| n == gated), "{gated} is listed with no flag");
    }
    for (write, gates, run) in [
        (false, false, false),
        (true, false, false),
        (true, true, false),
        (false, false, true),
        (true, true, true),
    ] {
        let (server, _home) = common::server(Caps::new(write, gates, run, vec![]).unwrap(), Redactor::new());
        let names: Vec<_> = server.tools().iter().map(|t| t.name.to_string()).collect();
        let mut want: Vec<&str> = reading.iter().map(String::as_str).collect();
        if write {
            want.extend(["workflow_create", "workflow_update"]);
        }
        assert_eq!(names, want, "write={write} gates={gates} run={run}");
        // What is not listed cannot be called, with or without the flag.
        for off in ["workflow_create", "workflow_update", "workflow_run", "run_status", "run_cancel"] {
            if names.iter().any(|n| n == off) {
                continue;
            }
            let e = server.call(None, off, Some(json!({}).as_object().unwrap().clone()), Instant::now()).unwrap_err();
            assert_eq!(e.code.0, -32602, "{off}");
        }
    }
}

#[test]
fn every_listed_tool_says_what_it_does_to_the_world() {
    let caps = Caps::new(true, true, true, vec![]).unwrap();
    let (server, _home) = common::server(caps, Redactor::new());
    for t in server.tools() {
        let a = t.annotations.as_ref().expect("annotations");
        assert!(a.read_only_hint.is_some() && a.destructive_hint.is_some(), "{}", t.name);
        assert!(a.idempotent_hint.is_some() && a.open_world_hint.is_some(), "{}", t.name);
        assert_eq!(t.input_schema["additionalProperties"], false, "{}", t.name);
        assert!(t.output_schema.is_some(), "{}", t.name);
    }
}

#[test]
fn the_write_tools_say_what_they_do() {
    let caps = Caps::new(true, true, true, vec![]).unwrap();
    let (server, _home) = common::server(caps, Redactor::new());
    let hints = |name: &str| {
        let tools = server.tools();
        let a = tools.iter().find(|t| t.name == name).unwrap().annotations.clone().unwrap();
        (a.read_only_hint, a.destructive_hint, a.idempotent_hint)
    };
    assert_eq!(hints("workflow_create"), (Some(false), Some(false), Some(true)));
    assert_eq!(hints("workflow_update"), (Some(false), Some(true), Some(true)));
}
