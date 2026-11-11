//! Alone in its file because it sets an environment variable for the whole test process.
#![cfg(unix)]

mod common;

use std::fs;
use std::time::Instant;

use aibus_core::Redactor;
use aibus_mcp::{Caps, RunConfig};
use serde_json::json;

#[test]
fn bypass_is_unreachable_through_mcp_even_with_aibus_i_know() {
    std::env::set_var("AIBUS_I_KNOW", "1");
    let (server, home) = common::server(Caps::new(false, false, true, vec![]).unwrap(), Redactor::new());
    let dir = home.path().join("proj/.aibus/workflows");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("ship.toml"), "[[step]]\nagent = \"plan\"\nprompt = \"{{input}}\"\n").unwrap();
    let fake_dir = home.path().join("fake");
    fs::create_dir_all(&fake_dir).unwrap();
    let server = server.with_runs(RunConfig::new(common::fake_aibus(&fake_dir), 20.0));

    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let args = json!({ "name": "ship", "scope": "project", "input": "ok" }).as_object().cloned();
    let started = rt.block_on(server.call_async(None, None, "workflow_run", args, Instant::now())).unwrap();
    let run = serde_json::to_value(started).unwrap()["structuredContent"]["run"].as_str().unwrap().to_owned();
    for _ in 0..50 {
        let args = json!({ "run": run, "wait_ms": 1000 }).as_object().cloned();
        let body = rt.block_on(server.call_async(None, None, "run_status", args, Instant::now())).unwrap();
        if !serde_json::to_value(body).unwrap()["structuredContent"]["exit_code"].is_null() {
            break;
        }
    }
    // The server's own environment has the variable; the run's does not, and its mode is fixed
    // on its command line. There is no argument of any tool that changes either.
    let env = fs::read_to_string(fake_dir.join("env.txt")).unwrap();
    assert!(!env.lines().any(|l| l.starts_with("AIBUS_I_KNOW")), "{env}");
    let argv = fs::read_to_string(fake_dir.join("argv.txt")).unwrap();
    assert!(argv.contains("--permission-mode\ndont-ask"), "{argv}");
    assert!(!argv.contains("bypass"), "{argv}");
}
