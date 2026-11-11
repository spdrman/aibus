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
