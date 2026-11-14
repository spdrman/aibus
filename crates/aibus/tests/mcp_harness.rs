//! The MCP conformance harness tested on its own (#96), against the recorded stdio server in
//! `fixtures/mcp/server.sh`, the one the client tests in aibus-core use. These pass without
//! `aibus mcp`; `mcp_server.rs` is what needs the server.

mod common;

use std::process::Command;

use aibus_testkit::xdg_temp;
use common::mcp_client::{lines_that_are_not_json_rpc, McpClient, Protocol};
use common::transcript::Transcript;
use serde_json::{json, Value};

fn fixture_server(home: &std::path::Path) -> McpClient {
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/mcp/server.sh");
    let mut cmd = Command::new("sh");
    cmd.arg(script);
    let mut c = McpClient::spawn(cmd, Protocol::V2025_11_25, home);
    c.initialize_legacy()
        .expect("the fixture answers initialize");
    c
}

#[test]
fn the_harness_drives_the_fixture_server() {
    let home = xdg_temp();
    let mut c = fixture_server(home.path());

    let tools = c.list_tools();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["echo", "inject", "fail", "hang", "crash"]);

    let echoed = c.call("echo", json!({ "text": "hi" })).unwrap();
    assert_eq!(echoed["content"][0]["text"], "echo: hi");
    let failed = c.call("fail", json!({})).unwrap();
    assert_eq!(failed["isError"], true);
    // The fixture answers a tool it does not have with -32602, and the client says so as an Err.
    let unknown = c.call("nothing", json!({})).unwrap_err();
    assert_eq!(unknown.code, -32602);

    let (done, lines) = c.finish();
    assert!(done.status.success(), "{:?}", done.status);
    assert_eq!(lines_that_are_not_json_rpc(&lines), Vec::<String>::new());
    // initialize, tools/list, three calls.
    assert_eq!(lines.len(), 5, "{lines:?}");
}

#[test]
fn the_client_reports_a_server_that_exits_instead_of_panicking_on_a_missing_field() {
    let home = xdg_temp();
    let mut cmd = Command::new("sh");
    cmd.args([
        "-c",
        "echo 'error: unrecognized subcommand mcp' >&2; exit 2",
    ]);
    let mut c = McpClient::spawn(cmd, Protocol::V2025_11_25, home.path());
    let said = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.initialize_legacy()))
        .expect_err("a server that exits cannot answer");
    let said = said.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(said.contains("closed stdout before it answered"), "{said}");
    assert!(said.contains("unrecognized subcommand mcp"), "{said}");
    assert!(said.contains("exit status"), "{said}");
}

/// One scenario on the fixture: the transcript lines it leaves, under the given home.
fn scenario(home: &std::path::Path) -> Vec<String> {
    let mut c = fixture_server(home);
    c.list_tools();
    let text = format!(
        "at 2026-11-11T10:04:43Z id 01HZX3BP5Y4Q8N2R6T7V9W0XYZ in {}/proj/a.toml",
        home.display()
    );
    c.call("echo", json!({ "text": text })).unwrap();
    c.transcript.lines()
}

#[test]
fn the_normaliser_is_deterministic() {
    let (a, b) = (xdg_temp(), xdg_temp());
    assert_ne!(a.path(), b.path());
    let (one, two) = (scenario(a.path()), scenario(b.path()));
    assert_eq!(one, two);
    // And it took the volatile parts out rather than agreeing on them.
    let echo: Value = serde_json::from_str(&one[1]).unwrap();
    assert_eq!(
        echo["response"]["result"]["content"][0]["text"],
        "echo: at <time> id <ulid> in <home>/proj/a.toml"
    );
}

#[test]
fn the_normaliser_drops_ids_meta_and_framing_and_sorts_json_held_in_a_string() {
    let home = xdg_temp();
    let mut t = Transcript::new(home.path());
    let request = json!({
        "jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": { "_meta": { "x": 1 }, "name": "t", "arguments": {} },
    });
    let response = json!({
        "jsonrpc": "2.0", "id": 7,
        "result": {
            "resultType": "complete", "ttlMs": 0, "cacheScope": "private",
            "content": [{ "type": "text", "text": "{\"b\":1,\"a\":[2,1]}" }],
        },
    });
    t.record(&request, &response);
    let line: Value = serde_json::from_str(&t.lines()[0]).unwrap();
    assert_eq!(
        line,
        json!({
            "request": { "method": "tools/call", "params": { "name": "t", "arguments": {} } },
            "response": { "result": { "content": [{ "type": "text", "text": "{\"a\":[2,1],\"b\":1}" }] } },
        })
    );
}
