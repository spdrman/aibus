//! H1.3 (#390): the coordinator profile and the Claude Code spellings in a definition. The mcp__
//! test is the reproduction the issue asks for before the mapping is touched: it says whether a
//! `mcp__server__tool` entry in `tools` reaches the run's allow-list at all.

use std::fs;
use std::path::Path;

use aibus_core::agent::def::parse;
use aibus_core::agent::validate::{validate_path, ValidateCx};
use aibus_core::*;

fn def(text: &str) -> (AgentDef, Vec<String>) {
    parse(
        text,
        DefSource {
            path: None,
            scope: Scope::Project,
        },
    )
    .unwrap()
}

fn tools(d: &AgentDef) -> Vec<String> {
    d.tools
        .allow
        .as_ref()
        .unwrap()
        .iter()
        .map(|p| p.tool.to_string())
        .collect()
}

fn run_tools(d: &AgentDef, role: Role) -> Vec<String> {
    effective_tools(d, role, 0, false, 3)
        .allow
        .iter()
        .map(|p| p.tool.to_string())
        .collect()
}

#[test]
fn an_mcp_double_underscore_entry_reaches_the_run_as_the_colon_pattern() {
    let (d, _) = def("---\nname: a\ndescription: d\ntools: [read, mcp__files__read_file]\n---\nB");
    let got = run_tools(&d, Role::Worker);
    assert!(got.contains(&"mcp:files:read_file".to_owned()), "{got:?}");
    assert!(!got.iter().any(|t| t.contains("__")), "{got:?}");
}

#[test]
fn claude_code_tool_names_become_the_aibus_ones_in_both_lists() {
    let (d, w) = def(
        "---\nname: a\ndescription: d\ntools: [Read, WebFetch, Task, Agent, mcp__srv__do_it]\ndisallowed_tools: [WebFetch, mcp__srv__bad]\n---\nB",
    );
    assert_eq!(
        tools(&d),
        ["read", "web_fetch", "delegate", "delegate", "mcp:srv:do_it"]
    );
    let deny: Vec<_> = d.tools.deny.iter().map(|p| p.tool.to_string()).collect();
    assert_eq!(deny, ["web_fetch", "mcp:srv:bad"]);
    assert!(w.is_empty(), "{w:?}");
    assert!(run_tools(&d, Role::Lead).contains(&"delegate".to_owned()));
}

#[test]
fn a_name_with_no_counterpart_warns_when_read_and_in_validate_and_is_not_an_error() {
    let text =
        "---\nname: a\ndescription: d\ntools: [Read, TodoWrite, NotebookEdit, WebSearch]\n---\nB";
    let (_, w) = def(text);
    for name in ["todowrite", "notebookedit", "websearch"] {
        assert!(w.iter().any(|m| m.contains(name)), "{name} in {w:?}");
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("coordinator_390");
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("cc.md");
    fs::write(&file, text).unwrap();
    let cx = ValidateCx {
        accounts: &[],
        model_resolves: &|_| true,
        project_dir: &dir,
    };
    let r = validate_path(&file, &cx);
    assert!(r.ok && r.errors.is_empty(), "{r:?}");
    assert_eq!(
        r.warnings
            .iter()
            .filter(|f| f.message.contains("todowrite"))
            .count(),
        1,
        "{r:?}"
    );
}

#[test]
fn the_principal_profile_routes_and_reviews_but_has_no_tool_that_changes_files() {
    let text = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/agents/principal.md"),
    )
    .unwrap();
    let (d, w) = def(&text);
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(d.role, Role::Lead);
    assert_eq!(
        tools(&d),
        [
            "delegate",
            "resume_run",
            "send_message",
            "recall",
            "read",
            "ledger_update",
            "create_handoff"
        ]
    );
    let got = run_tools(&d, Role::Lead);
    for t in ["bash", "write", "edit"] {
        assert!(!got.contains(&t.to_owned()), "{t} in {got:?}");
    }
    assert!(got.contains(&"delegate".to_owned()));

    let (lead, _) = def(include_str!("../../../examples/agents/lead.md"));
    assert!(
        run_tools(&lead, Role::Lead).contains(&"bash".to_owned()),
        "the built-in lead keeps every tool"
    );
}
