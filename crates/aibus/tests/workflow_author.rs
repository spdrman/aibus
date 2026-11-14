//! G6: the built-in `workflow-author` agent on this program's own `aibus mcp`, over stdio. The
//! TUI's `/workflow new <goal>` is the lead delegating the goal to the author; what is testable
//! without a terminal is the author itself, so these run it as the session's agent on the scripted
//! fake (`aibus run --agent workflow-author`), with the MCP calls pre-approved for the run.

mod common;

use std::fs;
use std::path::Path;
use std::process::Output;

use aibus_core::workflow::{validate, Workflow};
use aibus_core::AgentName;
use aibus_testkit::xdg_temp;
use common::{aibus, command, ndjson, stderr};
use serde_json::{json, Value};

const GOOD: &str = "name = \"pr-review\"\ndescription = \"Review a pull request and leave a summary\"\n\n[[step]]\nagent = \"explore\"\nprompt = \"Find what changed: {{input}}\"\n\n[[step]]\nagent = \"plan\"\nprompt = \"Review it: {{prev.summary}}\"\n";

/// The text as a RON string literal (a JSON string is one).
fn ron(text: &str) -> String {
    serde_json::to_string(text).unwrap()
}

fn tool(name: &str, input: Value) -> String {
    format!("ToolCall(name: {}, input: {}), Stop(ToolUse),", ron(name), input)
}

/// `aibus run --agent workflow-author` with `script`, the aibus tools pre-approved.
fn author(home: &Path, script: &str) -> Output {
    let file = home.join("s.ron");
    fs::write(&file, script).unwrap();
    command(home)
        .args(["run", "--json", "--agent", "workflow-author", "--allow", "mcp:aibus:*", "--script"])
        .arg(&file)
        .arg("review a PR and leave a summary")
        .output()
        .unwrap()
}

/// Every line of the one audit file the `aibus mcp` child wrote, `server/start` included.
fn audit_lines_all(home: &Path) -> Vec<Value> {
    let dir = home.join(".local/state/aibus/logs/mcp");
    let files: Vec<_> = fs::read_dir(&dir).unwrap().flatten().collect();
    assert_eq!(files.len(), 1, "one `aibus mcp` child");
    fs::read_to_string(files[0].path()).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

/// The tool calls in that file.
fn audit_lines(home: &Path) -> Vec<Value> {
    audit_lines_all(home).into_iter().filter(|l| l["method"] == "tools/call").collect()
}

fn answer() -> String {
    format!("Text({}), Stop(EndTurn)", ron(&format!("Here it is.\n```toml\n{GOOD}```\nexplore finds the change, plan reviews it.")))
}

/// The scripted model: lists the agents, validates a draft with a bad step and then a good one.
fn script() -> String {
    let bad = GOOD.replace("agent = \"plan\"", "agent = \"ghost\"");
    format!(
        "[{} {} {} {}]",
        tool("mcp__aibus__agent_list", json!({})),
        tool("mcp__aibus__workflow_validate", json!({"toml": bad})),
        tool("mcp__aibus__workflow_validate", json!({"toml": GOOD})),
        answer()
    )
}

#[test]
fn new_delegates_to_the_author_and_returns_validated_toml() {
    let home = xdg_temp();
    let out = author(home.path(), &script());
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let lines = ndjson(&out);
    let calls: Vec<&str> = lines.iter().filter(|l| l["type"] == "ToolCallRequested").map(|l| l["call"]["name"].as_str().unwrap()).collect();
    assert_eq!(calls, ["mcp__aibus__agent_list", "mcp__aibus__workflow_validate", "mcp__aibus__workflow_validate"]);
    // The child server saw exactly those three calls, and the bad draft is in its audit log.
    let audit = audit_lines(home.path());
    let tools: Vec<&str> = audit.iter().filter_map(|l| l["tool"].as_str()).collect();
    assert_eq!(tools, ["agent_list", "workflow_validate", "workflow_validate"]);
    assert!(audit[1]["args"].to_string().contains("ghost") || audit[2]["args"].to_string().contains("ghost"));
    assert!(lines.iter().filter(|l| l["type"] == "ToolCallFinished").all(|l| l["ok"] == true), "{lines:?}");
    // The result is a fenced block that parses and validates against the builtin roster.
    let last = lines.iter().rev().find(|l| l["type"] == "RunFinished").unwrap();
    let summary = last["summary"].as_str().unwrap();
    let toml = summary.split("```toml\n").nth(1).and_then(|s| s.split("```").next()).unwrap();
    assert!(Workflow::parse("draft", toml).is_ok());
    let roster: Vec<AgentName> = ["explore", "plan"].map(|n| AgentName::new(n).unwrap()).to_vec();
    assert!(validate(toml, "draft", &roster).ok);
}

#[test]
fn the_child_is_read_only_and_the_tool_list_has_no_write_tool() {
    let home = xdg_temp();
    let script = format!(
        "[{} {}]",
        tool("mcp__aibus__workflow_create", json!({"name": "x", "scope": "project", "toml": GOOD})),
        "Text(\"done\"), Stop(EndTurn)"
    );
    let out = author(home.path(), &script);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let lines = ndjson(&out);
    // The agent never saw a write tool, so asking for one is an error and nothing is written.
    let finished = lines.iter().find(|l| l["type"] == "ToolCallFinished").unwrap();
    assert_eq!(finished["ok"], false, "{finished}");
    assert!(!home.path().join("proj/.aibus/workflows/x.toml").exists());
    // The child's own log says it started with no capability flag.
    let first = &audit_lines_all(home.path())[0];
    assert_eq!(first["method"], "server/start");
    assert_eq!(first["args"]["write"], false);
    assert_eq!(first["args"]["gates"], false);
    assert_eq!(first["args"]["run"], false);
}

#[test]
fn a_user_mcp_aibus_entry_wins() {
    let home = xdg_temp();
    let doctor = |home: &Path| -> String {
        let out = aibus(home, &["doctor", "--json"]);
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        let row = v["checks"].as_array().unwrap().iter().find(|c| c["name"] == "workflow author").unwrap().clone();
        assert_eq!(row["ok"], true, "{row}");
        row["detail"].as_str().unwrap().to_owned()
    };
    // Without an entry the binary's own `aibus mcp` is the server, read-only.
    let own = doctor(home.path());
    assert!(own.ends_with(" mcp (read-only, over stdio)"), "{own}");
    // With one, theirs is kept and the binary adds nothing.
    let config = home.path().join(".config/aibus/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(&config, "[mcp.aibus]\ncommand = \"/opt/aibus\"\nargs = [\"mcp\", \"--cwd\", \"/srv/app\"]\n").unwrap();
    assert_eq!(doctor(home.path()), "your [mcp.aibus]: /opt/aibus mcp --cwd /srv/app");
}
