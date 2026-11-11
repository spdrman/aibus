//! `aibus mcp` and secrets (SECURITY.md): whatever a client sends, nothing key-shaped comes back
//! over stdio, reaches the audit log or the trace on stderr.

mod common;

use aibus_testkit::xdg_temp;
use common::mcp_client::{lines_that_are_not_json_rpc, mcp_command, McpClient, Protocol};
use serde_json::json;

/// Key-shaped for the redactor's `anthropic-key` pattern, and unique to this test.
const KEY: &str = "sk-ant-api03-Zq8wLm2Xv5Rt7Yb1Nc4Hd6Gf9Jk3P";

#[test]
fn no_secret_leaves_over_stdio() {
    let mut home = xdg_temp();
    // The temp tree is searched for it when the test ends, the audit log included.
    home.forbid(KEY);
    for p in Protocol::BOTH {
        let mut cmd = mcp_command(home.path(), &[]);
        cmd.env("AIBUS_LOG", "trace");
        let mut c = McpClient::spawn(cmd, p, home.path());
        match p {
            Protocol::V2026_07_28 => drop(c.discover().expect("discover")),
            Protocol::V2025_11_25 => drop(c.initialize_legacy().expect("initialize")),
        }
        // The key as an argument, as a tool name and as a made-up cursor.
        let refused = c.call("workflow_list", json!({ "api_key": KEY })).unwrap_err();
        let unknown = c.call(&format!("tool-{KEY}"), json!({})).unwrap_err();
        let paged = c.list_tools_page(Some(KEY)).unwrap_err();
        for e in [&refused, &unknown, &paged] {
            assert!(!e.message.contains(KEY), "{p:?}: {e:?}");
        }
        c.call("workflow_list", json!({})).expect("a normal call still works");
        let (done, lines) = c.finish();
        assert_eq!(lines_that_are_not_json_rpc(&lines), Vec::<String>::new(), "{p:?}");
        assert!(!lines.join("\n").contains(KEY), "{p:?}: the key came back over stdout");
        assert!(!done.stderr.contains(KEY), "{p:?}: the key reached stderr");
    }
    // One audit file per process, each with the key redacted by pattern.
    let dir = home.path().join(".local/state/aibus/logs/mcp");
    let mut seen = 0;
    for f in std::fs::read_dir(&dir).expect("the audit directory").flatten() {
        let text = std::fs::read_to_string(f.path()).unwrap();
        assert!(!text.contains(KEY), "{}", f.path().display());
        seen += text.matches("[redacted:pattern:anthropic-key]").count();
    }
    assert!(seen >= 2, "the audit log did not record the redacted arguments ({seen})");
}

const FLOW: &str = "[[step]]\nagent = \"explore\"\nprompt = \"Look: {{input}}\"\n";

/// Every path below `dir`.
fn files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        out.push(e.path());
        out.extend(files(&e.path()));
    }
    out.sort();
    out
}

#[cfg(unix)]
#[test]
fn a_symlinked_workflows_dir_cannot_be_used_to_write_outside_it() {
    for p in Protocol::BOTH {
        let home = xdg_temp();
        let outside = home.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(home.path().join("proj/.aibus")).unwrap();
        std::os::unix::fs::symlink(&outside, home.path().join("proj/.aibus/workflows")).unwrap();
        let mut c = McpClient::start(home.path(), p, &["--allow", "write"]);
        let r = c
            .call("workflow_create", json!({ "name": "ship", "scope": "project", "toml": FLOW }))
            .expect("a tool error");
        assert_eq!(r["isError"], true, "{p:?}: {r}");
        assert_eq!(files(&outside), Vec::<std::path::PathBuf>::new(), "{p:?}");
    }
}

#[test]
fn no_write_tool_can_reach_settings_or_the_user_config() {
    let home = xdg_temp();
    std::fs::create_dir_all(home.path().join("proj/.aibus")).unwrap();
    std::fs::create_dir_all(home.path().join(".config/aibus")).unwrap();
    let settings = home.path().join("proj/.aibus/settings.toml");
    let config = home.path().join(".config/aibus/config.toml");
    std::fs::write(&settings, "# mine\n").unwrap();
    std::fs::write(&config, "# mine\n").unwrap();
    let mut c = McpClient::start(home.path(), Protocol::V2026_07_28, &["--allow", "write", "--allow", "gates"]);
    for (scope, name) in [
        ("project", "../settings"),
        ("project", "../settings.toml"),
        ("user", "../config"),
        ("user", "../config.toml"),
        ("user", "../../../../.config/aibus/config"),
        ("project", "/etc/passwd"),
        ("project", "~/.config/aibus/config"),
    ] {
        let r = c.call("workflow_create", json!({ "name": name, "scope": scope, "toml": FLOW })).expect("a tool error");
        assert_eq!(r["isError"], true, "{scope} {name}: {r}");
        let u = c
            .call(
                "workflow_update",
                json!({ "name": name, "scope": scope, "toml": FLOW, "expected_revision": "0".repeat(64) }),
            )
            .expect("a tool error");
        assert_eq!(u["isError"], true, "{scope} {name}: {u}");
    }
    // The names that are valid land in the workflows directory and nowhere else.
    for (scope, name) in [("project", "settings"), ("user", "config")] {
        let r = c.call("workflow_create", json!({ "name": name, "scope": scope, "toml": FLOW })).expect("create");
        assert_ne!(r["isError"], true, "{r}");
    }
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), "# mine\n");
    assert_eq!(std::fs::read_to_string(&config).unwrap(), "# mine\n");
    assert!(home.path().join("proj/.aibus/workflows/settings.toml").is_file());
    assert!(home.path().join(".config/aibus/workflows/config.toml").is_file());
}

#[test]
fn no_credential_reaches_stdout_or_the_audit_log_during_a_run() {
    let mut home = xdg_temp();
    home.forbid(KEY);
    let dir = home.path().join("proj/.aibus/workflows");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("ship.toml"), FLOW).unwrap();
    for p in Protocol::BOTH {
        let mut cmd = mcp_command(home.path(), &["--allow", "run"]);
        cmd.env("AIBUS_LOG", "trace");
        let mut c = McpClient::spawn(cmd, p, home.path());
        match p {
            Protocol::V2026_07_28 => drop(c.discover().expect("discover")),
            Protocol::V2025_11_25 => drop(c.initialize_legacy().expect("initialize")),
        }
        // The key as the run's input and as a made-up handle.
        let started = c
            .call("workflow_run", json!({ "name": "ship", "scope": "project", "input": format!("use {KEY}") }))
            .expect("workflow_run");
        let run = started["structuredContent"]["run"].as_str().expect("a handle").to_owned();
        for _ in 0..120 {
            let r = c.call("run_status", json!({ "run": run, "wait_ms": 5000 })).expect("run_status");
            if !r["structuredContent"]["exit_code"].is_null() {
                break;
            }
        }
        // The run resources carry the same promise: the key as the input must not come back.
        let log = c.read_resource(&format!("aibus://runs/{run}/log")).expect("the log resource");
        let status = c.read_resource(&format!("aibus://runs/{run}")).expect("the status resource");
        assert!(!format!("{log}{status}").contains(KEY), "{p:?}: the key came back in a run resource");
        assert!(c.read_resource(&format!("aibus://runs/run-{KEY}")).is_err(), "{p:?}");
        let unknown = c.call("run_status", json!({ "run": format!("run-{KEY}") })).expect("a tool error");
        assert_eq!(unknown["isError"], true);
        let (done, lines) = c.finish();
        assert_eq!(lines_that_are_not_json_rpc(&lines), Vec::<String>::new(), "{p:?}");
        assert!(!lines.join("\n").contains(KEY), "{p:?}: the key came back over stdout");
        assert!(!done.stderr.contains(KEY), "{p:?}: the key reached stderr");
    }
    // The audit files and the sessions of the runs are searched again when `home` is dropped.
    let dir = home.path().join(".local/state/aibus/logs/mcp");
    for f in std::fs::read_dir(&dir).expect("the audit directory").flatten() {
        let text = std::fs::read_to_string(f.path()).unwrap();
        assert!(!text.contains(KEY), "{}", f.path().display());
    }
}
