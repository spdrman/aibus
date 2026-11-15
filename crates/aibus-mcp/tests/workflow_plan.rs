//! #283 over MCP: `workflow_plan` is a seventh read tool that hands a client the compiled plan,
//! the read tools older clients call keep answering inside their schemas for a workflow in the
//! new format, and none of the write and run trust rules move: a gate still needs
//! `--allow gates`, and a workflow only the scheduler can run starts nothing.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use aibus_core::agent::discover::discover;
use aibus_core::workflow::compile::compile;
use aibus_core::workflow::Workflow;
use aibus_core::Redactor;
use aibus_mcp::Caps;
use serde_json::{json, Value};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture(rel: &str) -> String {
    fs::read_to_string(root().join(rel)).unwrap()
}

fn project_workflow(home: &aibus_testkit::Xdg, name: &str, text: &str) {
    let dir = home.path().join("proj/.aibus/workflows");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(format!("{name}.toml")), text).unwrap();
}

fn call(server: &aibus_mcp::Server, tool: &str, args: Value) -> rmcp::model::CallToolResult {
    server
        .call(None, tool, args.as_object().cloned(), Instant::now())
        .unwrap()
}

fn structured(server: &aibus_mcp::Server, tool: &str, args: Value) -> Value {
    let r = call(server, tool, args);
    assert_ne!(r.is_error, Some(true), "{tool}: {r:?}");
    r.structured_content.expect("structured content")
}

fn text_of(r: &rmcp::model::CallToolResult) -> String {
    serde_json::to_value(r).unwrap()["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn workflow_plan_is_a_read_only_tool_after_the_six_older_ones() {
    let (server, _home) = common::server(Caps::default(), Redactor::new());
    let names: Vec<String> = server.tools().iter().map(|t| t.name.to_string()).collect();
    assert_eq!(
        names,
        [
            "workflow_list",
            "workflow_get",
            "workflow_explain",
            "workflow_validate",
            "workflow_dry_run",
            "agent_list",
            "workflow_plan"
        ]
    );
    let plan = server
        .tools()
        .into_iter()
        .find(|t| t.name == "workflow_plan")
        .unwrap();
    let a = plan.annotations.expect("annotations");
    assert_eq!(
        (a.read_only_hint, a.destructive_hint, a.open_world_hint),
        (Some(true), Some(false), Some(false))
    );
}

#[test]
fn workflow_plan_gives_the_same_plan_the_compiler_does_by_name_or_as_a_draft() {
    let (server, home) = common::server(Caps::default(), Redactor::new());
    let text = fixture("fixtures/workflows/graph/repo-review.toml");
    project_workflow(&home, "repo-review", &text);
    let by_name = structured(&server, "workflow_plan", json!({ "name": "repo-review" }));
    let draft = structured(&server, "workflow_plan", json!({ "toml": text }));
    assert_eq!(by_name["plan_hash"], draft["plan_hash"]);
    let wf = Workflow::parse("repo-review", &text).unwrap();
    let agents = discover(
        None,
        &home.path().join("proj"),
        Some(&home.paths.config.join("agents")),
        &[],
        &[],
    )
    .unwrap()
    .agents;
    let direct = compile(&wf, &text, &agents, &Redactor::new()).unwrap();
    assert_eq!(by_name["plan_hash"], direct["plan_hash"]);
    assert_eq!(by_name["steps"][1]["mode"], "native");
}

#[test]
fn workflow_plan_refuses_an_invalid_or_secret_bearing_workflow() {
    let (server, home) = common::server(Caps::default(), Redactor::new());
    project_workflow(
        &home,
        "cycle",
        &fixture("fixtures/workflows/graph-invalid/cycle.toml"),
    );
    let r = call(&server, "workflow_plan", json!({ "name": "cycle" }));
    assert_eq!(r.is_error, Some(true));
    assert!(text_of(&r).contains("cycle"), "{}", text_of(&r));
    let key = format!("sk-ant-{}", "b2".repeat(15));
    let r = call(
        &server,
        "workflow_plan",
        json!({ "toml": format!("[[step]]\nagent = \"plan\"\nprompt = \"{key}\"\n") }),
    );
    assert_eq!(r.is_error, Some(true));
    assert!(!text_of(&r).contains(&key), "{}", text_of(&r));
    // Arguments outside the schema are a protocol error, as for the other read tools.
    let bad = server.call(
        None,
        "workflow_plan",
        json!({ "name": "x", "toml": "y" }).as_object().cloned(),
        Instant::now(),
    );
    assert!(bad.is_err());
}

#[test]
fn the_older_read_tools_answer_a_graph_workflow_inside_their_old_schemas() {
    let (server, home) = common::server(Caps::default(), Redactor::new());
    project_workflow(
        &home,
        "fan-out",
        &fixture("fixtures/workflows/graph/fan-out.toml"),
    );
    let got = structured(&server, "workflow_get", json!({ "name": "fan-out" }));
    for step in got["workflow"]["steps"].as_array().unwrap() {
        let keys: Vec<&String> = step.as_object().unwrap().keys().collect();
        assert!(
            keys.iter()
                .all(|k| ["agent", "prompt", "gate", "retries"].contains(&k.as_str())),
            "{keys:?}"
        );
    }
    let valid = structured(&server, "workflow_validate", json!({ "name": "fan-out" }));
    assert_eq!(valid["ok"], true, "{valid}");
    let explained = structured(&server, "workflow_explain", json!({ "name": "fan-out" }));
    // The edges follow the routes: build reads code (step 2) and tests (step 3).
    let into_build: Vec<i64> = explained["edges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["to"] == 4)
        .map(|e| e["from"].as_i64().unwrap())
        .collect();
    assert_eq!(into_build, [2, 3], "{explained}");
    let dry = structured(
        &server,
        "workflow_dry_run",
        json!({ "name": "fan-out", "input": "add a flag" }),
    );
    assert_eq!(
        dry["steps"][3]["prompt_rendered"],
        "Build it. Code: <summary of step 2 (explore)> Tests: <summary of step 3 (search)>"
    );
}

#[test]
fn a_written_workflow_with_a_gate_and_the_new_fields_still_needs_allow_gates() {
    let (server, home) = common::server(
        Caps::new(true, false, false, vec![]).unwrap(),
        Redactor::new(),
    );
    fs::create_dir_all(home.path().join("proj/.aibus")).unwrap();
    let gated = "revision = \"1\"\n[[step]]\nid = \"a\"\nagent = \"plan\"\nrequires = [\"repo_read\"]\nprompt = \"p {{input}}\"\n\n[[step]]\nid = \"b\"\nagent = \"general\"\nprompt = \"q\"\ngate = \"cargo test\"\n\n[[route]]\nfrom = \"a\"\nto = \"b\"\n";
    let r = call(
        &server,
        "workflow_create",
        json!({ "name": "gated", "scope": "project", "toml": gated }),
    );
    assert_eq!(r.is_error, Some(true));
    assert!(text_of(&r).contains("--allow gates"), "{}", text_of(&r));
    assert!(!home
        .path()
        .join("proj/.aibus/workflows/gated.toml")
        .exists());
}

#[cfg(unix)]
#[test]
fn workflow_run_refuses_a_plan_only_workflow_before_spawning_anything() {
    let (server, home) = common::server(
        Caps::new(false, false, true, vec![]).unwrap(),
        Redactor::new(),
    );
    project_workflow(
        &home,
        "fan-out",
        &fixture("fixtures/workflows/graph/fan-out.toml"),
    );
    let fake_dir = home.path().join("fake");
    fs::create_dir_all(&fake_dir).unwrap();
    let fake = common::fake_aibus(&fake_dir);
    let server = server.with_runs(aibus_mcp::RunConfig::new(fake, 20.0));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let r = rt
        .block_on(
            server.call_async(
                None,
                None,
                "workflow_run",
                json!({ "name": "fan-out", "scope": "project", "input": "ok" })
                    .as_object()
                    .cloned(),
                Instant::now(),
            ),
        )
        .unwrap();
    assert_eq!(r.is_error, Some(true));
    assert!(
        text_of(&r).contains("the scheduler (#288)"),
        "{}",
        text_of(&r)
    );
    assert!(!fake_dir.join("argv.txt").exists(), "no run was started");
}

// #353: a child run has no attached session to hand an attached step to, so workflow_run refuses
// the workflow before it spawns anything.
#[cfg(unix)]
#[test]
fn workflow_run_refuses_a_workflow_with_an_attached_step_before_spawning_anything() {
    let (server, home) = common::server(
        Caps::new(false, false, true, vec![]).unwrap(),
        Redactor::new(),
    );
    project_workflow(
        &home,
        "review-attached",
        &fixture("fixtures/workflows/attached/review-attached.toml"),
    );
    let fake_dir = home.path().join("fake");
    fs::create_dir_all(&fake_dir).unwrap();
    let fake = common::fake_aibus(&fake_dir);
    let server = server.with_runs(aibus_mcp::RunConfig::new(fake, 20.0));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let r = rt
        .block_on(
            server.call_async(
                None,
                None,
                "workflow_run",
                json!({ "name": "review-attached", "scope": "project", "input": "ok" })
                    .as_object()
                    .cloned(),
                Instant::now(),
            ),
        )
        .unwrap();
    assert_eq!(r.is_error, Some(true));
    assert!(text_of(&r).contains("attached step"), "{}", text_of(&r));
    assert!(!fake_dir.join("argv.txt").exists(), "no run was started");
}
