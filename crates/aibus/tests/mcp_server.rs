//! The conformance suite for `aibus mcp` (#96, epic #95), written before the server so that the
//! server's PR turns these green instead of shipping tests written to match it. Each test runs
//! against both protocols the server has to answer, `2026-07-28` and `2025-11-25`.
//!
//! With no `mcp` subcommand every test fails at its first request with the server's own words
//! ("unrecognized subcommand 'mcp'") and its exit status, because the client reports a server
//! that closes stdout before it answers instead of failing on a missing field.

mod common;

use std::collections::HashSet;
use std::fs;

use aibus_testkit::xdg_temp;
use common::mcp_client::{lines_that_are_not_json_rpc, mcp_command, McpClient, Protocol};
use common::schema;
use serde_json::{json, Value};

const SERVER_INFO_KEY: &str = "io.modelcontextprotocol/serverInfo";

/// The `serverInfo` of a `server/discover` result (which may carry it in `_meta`) or of an
/// `initialize` result.
fn server_info(result: &Value) -> &Value {
    result.get("serverInfo").unwrap_or(&result["_meta"][SERVER_INFO_KEY])
}

/// A client past its handshake (`server/discover` is stateless, so there is nothing to do for
/// `2026-07-28`).
fn client(home: &std::path::Path, p: Protocol, args: &[&str]) -> McpClient {
    McpClient::start(home, p, args)
}

#[test]
fn every_stdout_line_is_json_rpc_even_with_trace_logging() {
    for p in Protocol::BOTH {
        let home = xdg_temp();
        let mut cmd = mcp_command(home.path(), &[]);
        cmd.env("AIBUS_LOG", "trace").env("RUST_LOG", "trace");
        let mut c = McpClient::spawn(cmd, p, home.path());
        match p {
            Protocol::V2026_07_28 => drop(c.discover().expect("server/discover")),
            Protocol::V2025_11_25 => drop(c.initialize_legacy().expect("initialize")),
        }
        c.list_tools();
        c.call("workflow_list", json!({})).expect("workflow_list");
        // A method nobody implements still gets one JSON-RPC line back, not a log line.
        let line = json!({ "jsonrpc": "2.0", "id": 900, "method": "no/such/method", "params": {} });
        let answer = c.raw(&line.to_string());
        assert!(answer.get("error").is_some(), "{p:?}: {answer}");
        let (done, lines) = c.finish();
        assert!(lines.len() >= 4, "{p:?}: {lines:?}");
        assert_eq!(lines_that_are_not_json_rpc(&lines), Vec::<String>::new(), "{p:?}");
        assert!(done.status.success(), "{p:?}: {:?}\n{}", done.status, done.stderr);
        // The trace went somewhere, and it was stderr.
        assert!(!done.stderr.is_empty(), "{p:?}: RUST_LOG=trace left stderr empty");
    }
}

#[test]
fn discover_and_legacy_initialize_both_answer_with_the_server_name_and_version() {
    let home = xdg_temp();

    let mut c = McpClient::v2026_07_28(home.path(), &[]);
    let found = c.discover().expect("server/discover");
    let info = server_info(&found);
    assert_eq!(info["name"], "aibus", "{found}");
    assert_eq!(info["version"], env!("CARGO_PKG_VERSION"), "{found}");
    let versions = found["supportedVersions"].as_array().expect("supportedVersions");
    for v in ["2026-07-28", "2025-11-25"] {
        assert!(versions.contains(&json!(v)), "{v} is not in {versions:?}");
    }
    assert!(found["capabilities"]["tools"].is_object(), "{found}");

    let mut c = McpClient::spawn(mcp_command(home.path(), &[]), Protocol::V2025_11_25, home.path());
    let init = c.initialize_legacy().expect("initialize");
    assert_eq!(init["serverInfo"]["name"], "aibus", "{init}");
    assert_eq!(init["serverInfo"]["version"], env!("CARGO_PKG_VERSION"), "{init}");
    assert_eq!(init["protocolVersion"], "2025-11-25", "{init}");
    assert!(init["capabilities"]["tools"].is_object(), "{init}");
}

#[test]
fn a_2026_request_without_its_meta_is_minus_32602() {
    let home = xdg_temp();
    let mut c = McpClient::v2026_07_28(home.path(), &[]);
    let version = "io.modelcontextprotocol/protocolVersion";
    let bare = json!({ "jsonrpc": "2.0", "id": 801, "method": "tools/list", "params": {} });
    let no_caps = json!({
        "jsonrpc": "2.0", "id": 802, "method": "tools/list",
        "params": { "_meta": { version: "2026-07-28" } },
    });
    for line in [bare, no_caps] {
        let answer = c.raw(&line.to_string());
        assert_eq!(answer["error"]["code"], -32602, "{line}: {answer}");
    }
}

#[test]
fn tools_list_is_deterministic_and_pages_when_asked() {
    for p in Protocol::BOTH {
        let home = xdg_temp();
        let mut c = client(home.path(), p, &[]);
        let first = c.list_tools();
        assert_eq!(first, c.list_tools(), "{p:?}: two listings differ");
        let names: Vec<&str> = first.iter().map(|t| t["name"].as_str().unwrap()).collect();
        let unique: HashSet<_> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "{p:?}: a tool is listed twice: {names:?}");

        // A server that pages hands out a cursor on the first page; following it must lead to
        // the rest, with no tool twice. One that does not page has no cursor to follow.
        let page = c.list_tools_page(None).expect("first page");
        let on_first = page["tools"].as_array().unwrap().len();
        assert!(on_first <= names.len(), "{p:?}");
        assert_eq!(page.get("nextCursor").is_some(), on_first < names.len(), "{p:?}: {page}");

        // A cursor the server never handed out is invalid params (the spec's -32602).
        let bad = c.list_tools_page(Some("no-such-cursor")).expect_err("a made-up cursor");
        assert_eq!(bad.code, -32602, "{p:?}: {bad:?}");
    }
}

#[test]
fn every_tool_schema_is_valid_2020_12_and_every_result_matches_its_output_schema() {
    for p in Protocol::BOTH {
        let home = xdg_temp();
        let mut c = client(home.path(), p, &[]);
        let tools = c.list_tools();
        assert!(!tools.is_empty(), "{p:?}: the server lists no tools");
        for tool in &tools {
            let name = tool["name"].as_str().unwrap();
            assert!(
                regex::Regex::new(r"^[A-Za-z0-9_.-]{1,128}$").unwrap().is_match(name),
                "{p:?}: {name} is not a legal tool name"
            );
            let (input, output) = (&tool["inputSchema"], &tool["outputSchema"]);
            assert!(output.is_object(), "{p:?}: {name} has no outputSchema");
            for (which, s) in [("inputSchema", input), ("outputSchema", output)] {
                assert_eq!(schema::lint_2020_12(s), Vec::<String>::new(), "{p:?}: {name} {which}");
                assert_eq!(s["type"], "object", "{p:?}: {name} {which}");
                assert_eq!(s["additionalProperties"], false, "{p:?}: {name} {which}");
            }
            // Hints are set, not left to the protocol's pessimistic defaults.
            for hint in ["readOnlyHint", "destructiveHint", "idempotentHint", "openWorldHint"] {
                assert!(tool["annotations"][hint].is_boolean(), "{p:?}: {name} leaves {hint} unset");
            }

            // A tool that needs no arguments can be called as it is, and what comes back has to
            // fit the schema it was listed with, and be mirrored as text.
            if input.get("required").and_then(Value::as_array).is_some_and(|r| !r.is_empty()) {
                continue;
            }
            let result = c.call(name, json!({})).unwrap_or_else(|e| panic!("{p:?}: {name}: {e:?}"));
            assert_ne!(result["isError"], true, "{p:?}: {name}: {result}");
            let structured = &result["structuredContent"];
            assert_eq!(schema::check_against(output, structured), Vec::<String>::new(), "{p:?}: {name}: {structured}");
            let text = result["content"][0]["text"].as_str().expect("a text block");
            // A tool that returns text from files puts a one-line data header above the JSON.
            let body = if text.starts_with("The following is") { text.split_once('\n').map_or("", |x| x.1) } else { text };
            assert_eq!(&serde_json::from_str::<Value>(body).unwrap(), structured, "{p:?}: {name}");
        }
    }
}

#[test]
fn an_unknown_tool_is_minus_32602() {
    for p in Protocol::BOTH {
        let home = xdg_temp();
        let mut c = client(home.path(), p, &[]);
        let e = c.call("no_such_tool", json!({})).expect_err("an unknown tool");
        assert_eq!(e.code, -32602, "{p:?}: {e:?}");
    }
}

#[test]
fn the_default_flags_list_no_write_or_run_tool() {
    let off = ["workflow_create", "workflow_update", "workflow_run", "run_status", "run_cancel"];
    for p in Protocol::BOTH {
        let home = xdg_temp();
        let mut c = client(home.path(), p, &[]);
        let names: Vec<String> =
            c.list_tools().iter().map(|t| t["name"].as_str().unwrap().to_owned()).collect();
        assert!(names.iter().any(|n| n == "workflow_list"), "{p:?}: {names:?}");
        for n in off {
            assert!(!names.iter().any(|x| x == n), "{p:?}: {n} is listed by default");
            // Off is the same as unknown: the client never sees a tool it cannot call.
            let e = c.call(n, json!({})).expect_err(n);
            assert_eq!(e.code, -32602, "{p:?}: {n}: {e:?}");
        }
    }
}

#[test]
fn the_list_scenario_matches_its_golden_transcript() {
    for p in Protocol::BOTH {
        let home = xdg_temp();
        let dir = home.path().join("proj/.aibus/workflows");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("ship.toml"),
            "name = \"ship\"\ndescription = \"Plan, build, check\"\n\n\
             [[step]]\nagent = \"plan\"\nprompt = \"Plan this: {{input}}\"\n\n\
             [[step]]\nagent = \"build\"\nprompt = \"Build it: {{prev.summary}}\"\n",
        )
        .unwrap();
        let mut c = client(home.path(), p, &[]);
        c.list_tools();
        let r = c.call("workflow_list", json!({})).expect("workflow_list");
        assert_eq!(r["structuredContent"]["workflows"][0]["name"], "ship", "{p:?}: {r}");
        c.transcript.assert_matches("list");
    }
}

#[test]
fn the_read_scenarios_match_their_golden_transcripts() {
    for (scenario, tool, args) in [
        ("get", "workflow_get", json!({ "name": "fix" })),
        ("explain", "workflow_explain", json!({ "name": "fix" })),
        ("validate-good", "workflow_validate", json!({ "toml": "[[step]]\nagent = \"explore\"\nprompt = \"{{input}}\"\n" })),
        ("validate-bad", "workflow_validate", json!({ "toml": "[[step]]\nagent = \"nobody\"\nprompt = \"{{input}}\"\n" })),
        ("dry-run", "workflow_dry_run", json!({ "name": "fix", "input": "login fails" })),
        ("agents", "agent_list", json!({})),
    ] {
        for p in Protocol::BOTH {
            let home = xdg_temp();
            let dir = home.path().join("proj/.aibus/workflows");
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("fix.toml"), include_str!("../../../examples/workflows/fix.toml")).unwrap();
            let mut c = client(home.path(), p, &[]);
            let r = c.call(tool, args.clone()).unwrap_or_else(|e| panic!("{p:?}: {tool}: {e:?}"));
            assert_ne!(r["isError"], true, "{p:?}: {tool}: {r}");
            c.transcript.assert_matches(scenario);
        }
    }
}

#[test]
fn the_resource_and_prompt_scenarios_match_their_golden_transcripts() {
    for p in Protocol::BOTH {
        let home = xdg_temp();
        let dir = home.path().join("proj/.aibus/workflows");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("fix.toml"), include_str!("../../../examples/workflows/fix.toml")).unwrap();
        let mut c = client(home.path(), p, &[]);
        c.request("resources/list", json!({})).expect("resources/list");
        c.request("resources/templates/list", json!({})).expect("resources/templates/list");
        c.read_resource("aibus://workflows/project/fix").expect("a workflow resource");
        c.read_resource("aibus://schema/workflow").expect("the schema resource");
        assert_eq!(c.read_resource("aibus://workflows/project/%2e%2e").expect_err("a path-shaped name").code, -32602);
        c.list_prompts().expect("prompts/list");
        c.get_prompt("review_workflow", json!({ "name": "fix" })).expect("review_workflow");
        c.transcript.assert_matches("resources-prompts");
    }
}
