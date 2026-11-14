//! #271: the read tools see the workflows of `.aibus` bundles in the project and user directories,
//! say which bundle each came from, and `workflow_run` does not start a bundled workflow with a
//! gate (it has to be installed with `--allow gates` first).

mod common;

use std::fs;
use std::time::Instant;

use aibus_core::workflow::bundle;
use aibus_core::Redactor;
use aibus_mcp::Caps;
use serde_json::{json, Value};

const SHIP: &str = "description = \"Plan, build\"\n[[step]]\nagent = \"plan\"\nprompt = \"Plan this: {{input}}\"\n";
const GATED: &str = "description = \"Plan, check\"\n[[step]]\nagent = \"plan\"\nprompt = \"Plan this: {{input}}\"\ngate = \"cargo test\"\n";

fn kit(flows: &[(&str, &str)]) -> Vec<u8> {
    let flows: Vec<(String, String)> = flows
        .iter()
        .map(|(n, t)| ((*n).to_owned(), (*t).to_owned()))
        .collect();
    bundle::pack("kit", "", &flows, &[], None, &Redactor::new()).unwrap()
}

fn call(server: &aibus_mcp::Server, tool: &str, args: Value) -> rmcp::model::CallToolResult {
    server
        .call(None, tool, args.as_object().cloned(), Instant::now())
        .unwrap()
}

#[test]
fn workflow_list_shows_bundled_workflows_with_their_bundle_and_a_refused_bundle_says_why() {
    let (server, home) = common::server(Caps::default(), Redactor::new());
    let project = home.path().join("proj/.aibus/workflows");
    let user = home.paths.config.join("workflows");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&user).unwrap();
    fs::write(
        user.join("kit.aibus"),
        kit(&[("ship", SHIP), ("check", GATED)]),
    )
    .unwrap();
    fs::write(project.join("cut.aibus"), &kit(&[("ship", SHIP)])[..700]).unwrap();

    let result = call(&server, "workflow_list", json!({}));
    let rows = result.structured_content.unwrap()["workflows"].clone();
    let by = |n: &str| {
        rows.as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == n)
            .unwrap_or_else(|| panic!("{n} in {rows}"))
            .clone()
    };
    assert_eq!(by("ship")["scope"], "user");
    assert_eq!(by("ship")["bundle"], "kit.aibus");
    let path = by("ship")["path"].as_str().unwrap().to_owned();
    assert!(
        path.starts_with("~/") && path.ends_with("workflows/kit.aibus"),
        "{path}"
    );
    assert_eq!(by("ship")["runnable"], true);
    assert_eq!(by("check")["ok"], true);
    assert_eq!(by("check")["runnable"], false);
    assert!(by("check")["error"]
        .as_str()
        .unwrap()
        .contains("--allow gates"));
    assert_eq!(by("cut.aibus")["ok"], false);
    assert!(by("cut.aibus")["error"]
        .as_str()
        .unwrap()
        .contains("bundle refused"));
    // The answer still has no absolute path of the machine.
    assert!(
        !rows
            .to_string()
            .contains(&home.path().display().to_string()),
        "{rows}"
    );
}

#[test]
fn the_read_tools_read_a_bundled_workflow_in_memory() {
    let (server, home) = common::server(Caps::default(), Redactor::new());
    let project = home.path().join("proj/.aibus/workflows");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("kit.aibus"), kit(&[("ship", SHIP)])).unwrap();

    let got = call(&server, "workflow_get", json!({ "name": "ship" }));
    assert_ne!(got.is_error, Some(true), "{got:?}");
    let got = got.structured_content.unwrap();
    assert_eq!(got["toml"], SHIP);
    assert_eq!(got["scope"], "project");
    assert_eq!(got["path"], ".aibus/workflows/kit.aibus");
    for (tool, args) in [
        ("workflow_explain", json!({ "name": "ship" })),
        ("workflow_dry_run", json!({ "name": "ship", "input": "x" })),
        (
            "workflow_validate",
            json!({ "name": "ship", "scope": "project" }),
        ),
    ] {
        let r = call(&server, tool, args);
        assert_ne!(r.is_error, Some(true), "{tool}: {r:?}");
    }
    // Nothing was unpacked next to the bundle.
    let names: Vec<_> = fs::read_dir(&project)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(names, ["kit.aibus"]);
}

#[cfg(unix)]
#[test]
fn workflow_run_does_not_start_a_bundled_gate() {
    let (server, home) = common::server(
        Caps::new(false, false, true, vec![]).unwrap(),
        Redactor::new(),
    );
    let project = home.path().join("proj/.aibus/workflows");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("kit.aibus"), kit(&[("check", GATED)])).unwrap();
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
                json!({ "name": "check", "scope": "project", "input": "ok" })
                    .as_object()
                    .cloned(),
                Instant::now(),
            ),
        )
        .unwrap();
    let r = serde_json::to_value(r).unwrap();
    assert_eq!(r["isError"], true, "{r}");
    assert!(
        r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("--allow gates"),
        "{r}"
    );
    assert!(!fake_dir.join("argv.txt").exists(), "nothing was started");
}
