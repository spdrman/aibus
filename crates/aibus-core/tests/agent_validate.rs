//! B1.1: validation against the session, the extra-dir limits, the roster budget, the shipped
//! examples and the `show` rendering.

use std::fs;
use std::path::{Path, PathBuf};

use aibus_core::agent::builtin::BUILTINS;
use aibus_core::agent::def::{parse, to_json, to_markdown};
use aibus_core::agent::discover::{discover, MAX_FILES_PER_SCOPE};
use aibus_core::agent::roster;
use aibus_core::agent::validate::{validate, validate_path, ValidateCx};
use aibus_core::*;

fn tree(test: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("agent_validate").join(test);
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

fn write(dir: &Path, file: &str, text: &str) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let p = dir.join(file);
    fs::write(&p, text).unwrap();
    p
}

fn def(text: &str) -> AgentDef {
    let source = DefSource {
        path: Some("agent.md".into()),
        scope: Scope::Project,
    };
    parse(text, source).unwrap().0
}

fn accounts() -> Vec<AccountName> {
    ["fake", "work"].map(|a| AccountName(a.into())).to_vec()
}

/// A model resolves when it is `known-1` or `sonnet`.
fn check(text: &str, project: &Path) -> aibus_core::agent::validate::Report {
    let accounts = accounts();
    let resolves = |m: &ModelRef| {
        matches!(m, ModelRef::Alias(a) if a == "sonnet")
            || matches!(m, ModelRef::Full(id) if id.0 == "known-1")
    };
    let cx = ValidateCx {
        accounts: &accounts,
        model_resolves: &resolves,
        project_dir: project,
    };
    validate(&def(text), &cx)
}

fn fields(findings: &[aibus_core::agent::validate::Finding]) -> Vec<(&str, &str)> {
    findings
        .iter()
        .map(|f| (f.field.as_str(), f.message.as_str()))
        .collect()
}

#[test]
fn a_clean_definition_has_no_findings() {
    let r = check(
        "---\nname: a\ndescription: d\ntools: [read, 'bash(git *)', 'mcp:github:*']\nmodel: sonnet\naccount: work\n---\nBody",
        Path::new("/"),
    );
    assert!(r.ok && r.errors.is_empty() && r.warnings.is_empty(), "{r:?}");
}

#[test]
fn unknown_tools_in_either_list_are_errors_and_mcp_patterns_pass() {
    let r = check(
        "---\nname: a\ndescription: d\ntools: [read, teleport]\ndisallowed_tools: [bogus, mcp:x:*]\n---\n",
        Path::new("/"),
    );
    assert!(!r.ok);
    assert_eq!(
        fields(&r.errors),
        [
            ("tools", "unknown tool `teleport`"),
            ("disallowed_tools", "unknown tool `bogus`")
        ]
    );
}

#[test]
fn model_and_account_must_exist_but_inherit_and_reserved_aliases_always_do() {
    let r = check(
        "---\nname: a\ndescription: d\nmodel: opus\naccount: gone\n---\n",
        Path::new("/"),
    );
    assert_eq!(
        fields(&r.errors),
        [
            ("model", "model `opus` does not resolve on any account"),
            ("account", "account `gone` does not exist")
        ]
    );
    for ok in ["inherit", "known-1", "sonnet", "advisor_model", "swarm_model"] {
        let r = check(&format!("---\nname: a\ndescription: d\nmodel: {ok}\n---\n"), Path::new("/"));
        assert!(r.ok, "{ok}: {r:?}");
    }
}

#[test]
fn checkpoints_only_on_advisors_and_router_only_on_routers() {
    let r = check(
        "---\nname: a\ndescription: d\ncheckpoints: [before_plan]\nrouter: {threshold: 0.5}\n---\n",
        Path::new("/"),
    );
    assert_eq!(
        fields(&r.errors),
        [
            ("checkpoints", "checkpoints are only for advisor agents"),
            ("router", "router settings are only for router agents")
        ]
    );
    let r = check(
        "---\nname: a\ndescription: d\nrole: advisor\ncheckpoints: [before_plan]\n---\n",
        Path::new("/"),
    );
    assert!(r.ok, "{r:?}");
    let r = check(
        "---\nname: a\ndescription: d\nrole: router\nrouter: {threshold: 0.5}\n---\n",
        Path::new("/"),
    );
    assert!(r.ok, "{r:?}");
}

#[cfg(unix)]
#[test]
fn hook_commands_must_exist_and_be_executable() {
    use std::os::unix::fs::PermissionsExt;
    let root = tree("hooks");
    let run = write(&root.join("scripts"), "run.sh", "#!/bin/sh\n");
    fs::set_permissions(&run, fs::Permissions::from_mode(0o755)).unwrap();
    let plain = write(&root.join("scripts"), "plain.sh", "#!/bin/sh\n");
    fs::set_permissions(&plain, fs::Permissions::from_mode(0o644)).unwrap();

    let hooks = |cmd: &str| {
        format!(
            "---\nname: a\ndescription: d\nhooks:\n  pre_tool_use:\n    - command: {cmd}\n---\n"
        )
    };
    assert!(check(&hooks("./scripts/run.sh --flag"), &root).ok);
    assert!(check(&hooks("sh -c true"), &root).ok, "sh is on PATH");
    let r = check(&hooks("./scripts/missing.sh"), &root);
    assert_eq!(fields(&r.errors), [("hooks", "pre_tool_use: `./scripts/missing.sh` does not exist")]);
    let r = check(&hooks("./scripts/plain.sh"), &root);
    assert_eq!(fields(&r.errors), [("hooks", "pre_tool_use: `./scripts/plain.sh` is not executable")]);
    let r = check(&hooks("no-such-binary-anywhere"), &root);
    assert_eq!(fields(&r.errors), [("hooks", "pre_tool_use: `no-such-binary-anywhere` is not on PATH")]);

    let r = check(
        "---\nname: a\ndescription: d\nhooks:\n  on_a_whim:\n    - command: sh\n---\n",
        &root,
    );
    assert!(r.ok);
    assert_eq!(fields(&r.warnings), [("hooks", "unknown hook event `on_a_whim`")]);
}

#[test]
fn the_built_ins_validate_clean() {
    let accounts = accounts();
    let resolves = |_: &ModelRef| false;
    let cx = ValidateCx {
        accounts: &accounts,
        model_resolves: &resolves,
        project_dir: Path::new("/"),
    };
    for text in BUILTINS {
        let d = def(text);
        let r = validate(&d, &cx);
        assert!(r.ok && r.warnings.is_empty(), "{}: {r:?}", d.name);
    }
}

#[test]
fn a_directory_reports_parse_failures_warnings_and_duplicate_names() {
    let root = tree("dir");
    write(&root, "a.md", "---\nname: same\ndescription: one\nflavour: x\n---\n");
    write(&root, "b.md", "---\nname: same\ndescription: two\n---\n");
    write(&root, "c.md", "no frontmatter");
    write(&root, "notes.txt", "not a definition");
    let accounts = accounts();
    let resolves = |_: &ModelRef| true;
    let cx = ValidateCx {
        accounts: &accounts,
        model_resolves: &resolves,
        project_dir: &root,
    };
    let r = validate_path(&root, &cx);
    assert!(!r.ok);
    let f = |x: &[aibus_core::agent::validate::Finding]| {
        x.iter()
            .map(|f| format!("{}|{}", f.path.as_ref().unwrap().file_name().unwrap().to_string_lossy(), f.field))
            .collect::<Vec<_>>()
    };
    assert_eq!(f(&r.errors), ["b.md|name", "c.md|frontmatter"]);
    assert_eq!(f(&r.warnings), ["a.md|flavour"]);

    let missing = validate_path(&root.join("nope.md"), &cx);
    assert!(!missing.ok);
    assert_eq!(missing.errors[0].field, "path");
}

#[test]
fn extra_dir_definitions_lose_hooks_and_get_permission_mode_default() {
    let root = tree("extra");
    let extra = root.join("claude");
    let text = "---\nname: wild\ndescription: d\npermission_mode: bypass\nhooks:\n  stop:\n    - command: ./x.sh\n---\n";
    write(&extra, "wild.md", text);
    write(&extra, "tame.md", "---\nname: tame\ndescription: d\n---\n");
    for trusted in [vec![], vec![extra.clone()]] {
        let found = discover(None, &root, None, std::slice::from_ref(&extra), &trusted).unwrap();
        let wild = &found.agents[&"wild".parse::<AgentName>().unwrap()];
        assert_eq!(wild.permission_mode, PermissionMode::Default);
        assert_eq!(wild.hooks.is_empty(), trusted.is_empty());
        assert!(found.warnings.iter().any(|w| w.starts_with("wild: permission_mode reset to default")), "{:?}", found.warnings);
        assert!(!found.warnings.iter().any(|w| w.starts_with("tame:")));
    }
    // The same file in the user's own scope keeps its mode.
    let user = root.join("user");
    write(&user, "wild.md", text);
    let found = discover(None, &root, Some(&user), &[], &[]).unwrap();
    assert_eq!(found.agents[&"wild".parse::<AgentName>().unwrap()].permission_mode, PermissionMode::Bypass);
}

#[test]
fn a_scope_reads_at_most_1000_files_and_says_so() {
    let root = tree("cap");
    let user = root.join("user");
    for i in 0..=MAX_FILES_PER_SCOPE {
        write(&user, &format!("a{i:04}.md"), &format!("---\nname: a{i:04}\ndescription: d\n---\n"));
    }
    let found = discover(None, &root, Some(&user), &[], &[]).unwrap();
    assert_eq!(found.agents.len(), MAX_FILES_PER_SCOPE + 9);
    assert!(found.agents.contains_key(&"a0999".parse::<AgentName>().unwrap()));
    assert!(!found.agents.contains_key(&"a1000".parse::<AgentName>().unwrap()));
    let cap: Vec<_> = found.warnings.iter().filter(|w| w.contains("more than 1000")).collect();
    assert_eq!(cap.len(), 1, "{:?}", found.warnings);

    let exactly = tree("cap_exact");
    for i in 0..MAX_FILES_PER_SCOPE {
        write(&exactly, &format!("a{i:04}.md"), &format!("---\nname: a{i:04}\ndescription: d\n---\n"));
    }
    let found = discover(None, &root, Some(&exactly), &[], &[]).unwrap();
    assert!(!found.warnings.iter().any(|w| w.contains("more than")));
}

#[test]
fn the_roster_lists_non_built_ins_and_warns_past_15000_tokens_but_loads_all() {
    let root = tree("roster");
    let user = root.join("user");
    write(&user, "short.md", "---\nname: short\ndescription: Does a thing.\n---\n");
    let found = discover(None, &root, Some(&user), &[], &[]).unwrap();
    assert_eq!(found.roster, "- short: Does a thing.\n");
    assert!(found.warnings.is_empty());

    // 60,000 characters of description is 15,000 tokens by the fallback; the line adds a little.
    let long = "x".repeat(60_000);
    write(&user, "long.md", &format!("---\nname: long\ndescription: {long}\n---\n"));
    let found = discover(None, &root, Some(&user), &[], &[]).unwrap();
    assert_eq!(found.agents.len(), 11);
    let tokens = roster::estimate_tokens(&found.roster);
    assert!(tokens > roster::BUDGET_TOKENS);
    assert_eq!(
        found.warnings,
        [format!(
            "agent roster is {tokens} tokens, over the 15000 token budget; shorten descriptions (detail belongs in the body)"
        )]
    );
}

#[test]
fn token_estimate_is_four_characters_each_rounded_up_and_the_budget_is_strict() {
    assert_eq!(roster::estimate_tokens(""), 0);
    assert_eq!(roster::estimate_tokens("abcd"), 1);
    assert_eq!(roster::estimate_tokens("abcde"), 2);
    assert!(roster::warning(roster::BUDGET_TOKENS).is_none());
    assert!(roster::warning(roster::BUDGET_TOKENS + 1).is_some());
}

#[test]
fn examples_agents_are_the_built_in_sources() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/agents");
    let mut on_disk: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".md"))
        .collect();
    on_disk.sort();
    let mut want: Vec<String> = BUILTINS.iter().map(|t| format!("{}.md", def(t).name)).collect();
    want.sort();
    assert_eq!(on_disk, want);
    for text in BUILTINS {
        let name = def(text).name;
        let file = fs::read_to_string(dir.join(format!("{name}.md"))).unwrap();
        assert_eq!(file, *text, "examples/agents/{name}.md has drifted from builtin.rs");
    }
}

#[test]
fn show_output_parses_back_to_the_same_definition() {
    let maestro = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/agents/valid/maestro.md"),
    )
    .unwrap();
    let advisor = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/agents/valid/advisor.md"),
    )
    .unwrap();
    for text in BUILTINS.iter().copied().chain([maestro.as_str(), advisor.as_str()]) {
        let d = def(text);
        let md = to_markdown(&d);
        let (back, warnings) = parse(&md, d.source.clone()).unwrap_or_else(|e| panic!("{e}\n{md}"));
        assert!(warnings.is_empty(), "{}: {warnings:?}", d.name);
        assert_eq!(back, d, "{md}");
    }
}

#[test]
fn show_json_is_snake_case_and_carries_the_prompt() {
    let d = def(
        "---\nname: a\ndescription: d\nmaxTurns: 7\npermissionMode: dontAsk\ndisallowedTools: Write\ntools: Read, Bash(git *)\n---\nDo it.",
    );
    let j = to_json(&d);
    assert_eq!(j["max_turns"], 7);
    assert_eq!(j["permission_mode"], "dont_ask");
    assert_eq!(j["disallowed_tools"], serde_json::json!(["write"]));
    assert_eq!(j["tools"], serde_json::json!(["read", "bash(git *)"]));
    assert_eq!(j["prompt"], "Do it.");
    assert_eq!(j["role"], "worker");
    assert!(j.get("account").is_none());
    assert!(j.as_object().unwrap().keys().all(|k| !k.chars().any(|c| c.is_ascii_uppercase())));
}
