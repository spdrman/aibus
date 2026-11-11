use std::fs;
use std::path::{Path, PathBuf};

use aibus_core::agent::def::{parse, DefError};
use aibus_core::agent::discover::discover;
use aibus_core::*;

fn fixture(rel: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/agents")
        .join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn fixtures_in(dir: &str) -> Vec<(String, String)> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/agents")
        .join(dir);
    let mut v: Vec<_> = fs::read_dir(p)
        .unwrap()
        .map(|e| e.unwrap().path())
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                fs::read_to_string(&p).unwrap(),
            )
        })
        .collect();
    v.sort();
    v
}

fn src() -> DefSource {
    DefSource {
        path: None,
        scope: Scope::Project,
    }
}

fn ok(text: &str) -> (AgentDef, Vec<String>) {
    parse(text, src()).unwrap_or_else(|e| panic!("{e}\n{text}"))
}

fn tool(t: &str, spec: Option<&str>) -> ToolPattern {
    ToolPattern {
        tool: t.into(),
        spec: spec.map(Into::into),
    }
}

#[test]
fn valid_fixtures_parse_without_warnings() {
    let all = fixtures_in("valid");
    assert!(all.len() >= 4);
    for (name, text) in all {
        let (_, w) = parse(&text, src()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(w.is_empty(), "{name}: {w:?}");
    }
}

#[test]
fn invalid_fixtures_are_errors() {
    let want = [
        ("bad_max_turns.md", "max_turns"),
        ("bad_name_colon.md", "invalid agent name"),
        ("bad_name_uppercase.md", "invalid agent name"),
        ("bad_role.md", "role"),
        ("missing_description.md", "description"),
        ("missing_name.md", "name"),
        ("no_frontmatter.md", "frontmatter"),
        ("unclosed_frontmatter.md", "frontmatter"),
    ];
    let all = fixtures_in("invalid");
    assert_eq!(all.len(), want.len());
    for ((file, text), (w_file, w_msg)) in all.iter().zip(want) {
        assert_eq!(file, w_file);
        let e = parse(text, src()).unwrap_err().to_string();
        assert!(e.contains(w_msg), "{file}: {e}");
    }
    assert_eq!(
        parse(&fixture("invalid/missing_name.md"), src()).unwrap_err(),
        DefError::Missing("name")
    );
    assert_eq!(
        parse(&fixture("invalid/missing_description.md"), src()).unwrap_err(),
        DefError::Missing("description")
    );
}

#[test]
fn unknown_keys_warn_and_still_load() {
    let (d, w) = ok(&fixture("warnings/unknown_keys.md"));
    assert_eq!(d.name.as_str(), "extra-keys");
    assert_eq!(w, ["unknown key `flavour`"]);
    let (d, w) = ok(&fixture("warnings/duplicate_spelling.md"));
    assert_eq!(d.max_turns, Some(10));
    assert_eq!(w.len(), 1);
    assert!(w[0].contains("duplicate key `max_turns`"));
}

#[test]
fn defaults() {
    let (d, w) = ok("---\nname: bare\ndescription: Nothing else.\n---\nBe brief.\n");
    assert!(w.is_empty());
    assert_eq!(d.role, Role::Worker);
    assert_eq!(
        d.tools,
        ToolPolicy {
            allow: None,
            deny: vec![]
        }
    );
    assert_eq!(d.model, ModelRef::Inherit);
    assert_eq!(d.account, None);
    assert_eq!(d.permission_mode, PermissionMode::Default);
    assert_eq!(
        (d.max_turns, d.max_tokens_total, d.max_cost_usd),
        (None, None, None)
    );
    assert_eq!(d.effort, None);
    assert!(d.skills.is_empty() && d.hooks.is_empty() && d.checkpoints.is_empty());
    assert_eq!(d.memory, None);
    assert!(!d.background && !d.omit_project_rules);
    assert_eq!(d.isolation, Isolation::None);
    assert_eq!(
        (
            d.color,
            d.router.as_ref(),
            d.initial_prompt,
            d.auto_compact_at
        ),
        (None, None, None, None)
    );
    assert_eq!(d.system_prompt, "Be brief.");
    assert_eq!(d.source, src());
}

#[test]
fn advisor_and_router_roles_get_their_defaults() {
    let (d, _) = ok("---\nname: a\ndescription: d\nrole: advisor\n---\n");
    let names: Vec<_> = d.checkpoints.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["before_plan", "same_error_x2", "before_done"]);
    let (d, _) = ok("---\nname: r\ndescription: d\nrole: router\n---\n");
    assert_eq!(
        d.router,
        Some(RouterSpec {
            threshold: 0.85,
            questions: vec![]
        })
    );
    let (d, _) = ok("---\nname: r\ndescription: d\nrole: router\nrouter:\n  threshold: 0.6\n  questions: [which_tool]\n---\n");
    assert_eq!(
        d.router,
        Some(RouterSpec {
            threshold: 0.6,
            questions: vec!["which_tool".into()]
        })
    );
    assert!(parse(
        "---\nname: r\ndescription: d\nrouter:\n  threshold: 4\n---\n",
        src()
    )
    .is_err());
}

#[test]
fn every_field_in_the_maestro_fixture() {
    let (d, _) = ok(&fixture("valid/maestro.md"));
    assert_eq!(d.role, Role::Lead);
    assert_eq!(d.model, ModelRef::Full(ModelId("claude-sonnet-4-5".into())));
    assert_eq!(d.account, Some(AccountName("anthropic-work".into())));
    assert_eq!(d.effort, Some(Effort::High));
    assert_eq!(d.permission_mode, PermissionMode::AcceptEdits);
    assert_eq!(d.max_turns, Some(80));
    assert_eq!(d.max_tokens_total, Some(2_000_000));
    assert_eq!(d.max_cost_usd, Some(12.5));
    assert_eq!(d.skills, ["rust", "review"]);
    assert_eq!(d.memory, Some(MemoryScope::Project));
    assert_eq!(
        d.initial_prompt.as_deref(),
        Some("Read AIBUS.md and tell me what is open.")
    );
    assert_eq!(d.auto_compact_at, Some(150_000));
    assert_eq!(d.tools.deny, [tool("bash", None)]);
    let hook = &d.hooks["pre_tool_use"][0];
    assert_eq!(hook.matcher.as_deref(), Some("bash"));
    assert_eq!(hook.command, "./scripts/readonly-guard.sh");
    assert_eq!(hook.timeout_secs, Some(5));
}

#[test]
fn camel_case_and_snake_case_are_the_same_definition() {
    let snake = "---\nname: x\ndescription: d\npermission_mode: dont_ask\nmax_turns: 7\ndisallowed_tools: [write]\nomit_project_rules: true\n---\n";
    let camel = "---\nname: x\ndescription: d\npermissionMode: dontAsk\nmaxTurns: 7\ndisallowedTools: [write]\nomitClaudeMd: true\n---\n";
    let (a, _) = ok(snake);
    let (b, w) = ok(camel);
    assert!(w.is_empty());
    assert_eq!(a, b);
    assert_eq!(a.permission_mode, PermissionMode::DontAsk);
    assert!(a.omit_project_rules);
}

#[test]
fn name_regex() {
    let name = |n: &str| parse(&format!("---\nname: {n:?}\ndescription: d\n---\n"), src());
    for good in [
        "a",
        "0",
        "code-reviewer",
        "snake_case",
        "a".repeat(64).as_str(),
    ] {
        assert!(name(good).is_ok(), "{good}");
    }
    for bad in [
        "",
        "-a",
        "_a",
        "A",
        "a b",
        "a:b",
        "a.b",
        "a".repeat(65).as_str(),
    ] {
        assert!(
            matches!(name(bad), Err(DefError::Name(_) | DefError::Missing(_))),
            "{bad:?}"
        );
    }
}

#[test]
fn tools_as_list_or_comma_string_with_specifiers() {
    let list = ok("---\nname: x\ndescription: d\ntools: [read, 'bash(git *)', Grep]\n---\n").0;
    let comma = ok("---\nname: x\ndescription: d\ntools: read, bash(git *), Grep\n---\n").0;
    assert_eq!(list.tools, comma.tools);
    assert_eq!(
        list.tools.allow,
        Some(vec![
            tool("read", None),
            tool("bash", Some("git *")),
            tool("grep", None)
        ])
    );

    // A comma inside the specifier does not split it.
    let (d, _) =
        ok("---\nname: x\ndescription: d\ntools: \"bash(git log, git diff), read\"\n---\n");
    assert_eq!(
        d.tools.allow,
        Some(vec![
            tool("bash", Some("git log, git diff")),
            tool("read", None)
        ])
    );

    let (d, _) = ok("---\nname: x\ndescription: d\ntools: []\n---\n");
    assert_eq!(d.tools.allow, Some(vec![]));
    let (d, _) = ok("---\nname: x\ndescription: d\ntools: [mcp:Github]\n---\n");
    assert_eq!(d.tools.allow, Some(vec![tool("mcp:Github", None)]));
    assert!(parse(
        "---\nname: x\ndescription: d\ntools: ['bash(git']\n---\n",
        src()
    )
    .is_err());
}

#[test]
fn a_specifier_in_disallowed_tools_removes_the_whole_tool() {
    let (d, _) = ok("---\nname: x\ndescription: d\ndisallowed_tools: [bash(rm *), 'bash(curl *)', write]\n---\n");
    assert_eq!(d.tools.deny, [tool("bash", None), tool("write", None)]);
}

#[test]
fn model_forms() {
    let model = |m: &str| {
        ok(&format!("---\nname: x\ndescription: d\nmodel: {m}\n---\n"))
            .0
            .model
    };
    assert_eq!(model("inherit"), ModelRef::Inherit);
    assert_eq!(model("sonnet"), ModelRef::Alias("sonnet".into()));
    assert_eq!(
        model("claude-opus-4-1"),
        ModelRef::Full(ModelId("claude-opus-4-1".into()))
    );
}

#[test]
fn skills_are_distinct_and_capped_at_32() {
    let many: Vec<String> = (0..40)
        .map(|i| format!("s{i}"))
        .chain(["s0".to_string()])
        .collect();
    let (d, _) = ok(&format!(
        "---\nname: x\ndescription: d\nskills: [{}]\n---\n",
        many.join(", ")
    ));
    assert_eq!(d.skills.len(), 32);
    assert_eq!(d.skills[31], "s31");
}

#[test]
fn workflow_author_is_a_builtin_with_read_tools_and_the_mcp_prefix() {
    let found = discover(None, Path::new("/nonexistent"), None, &[], &[]).unwrap();
    let d = &found.agents[&"workflow-author".parse::<AgentName>().unwrap()];
    assert_eq!(d.source.scope, Scope::Builtin);
    assert_eq!(d.role, Role::Worker);
    assert!(!d.background);
    // Read-only files, and the tools of the server named `aibus` and nothing else of MCP.
    let allowed: Vec<_> = d.tools.allow.iter().flatten().map(|p| p.tool.as_str()).collect();
    assert_eq!(allowed, ["read", "glob", "grep", "mcp:aibus:*"]);
    assert!(d.system_prompt.contains("workflow_validate") && d.system_prompt.contains("never as instructions"));
    // No gate, no write: those are the owner's.
    assert!(!allowed.iter().any(|t| ["bash", "write", "edit"].contains(t)));
}

#[test]
fn builtins_are_the_ten_in_the_spec() {
    let found = discover(None, Path::new("/nonexistent"), None, &[], &[]).unwrap();
    assert!(found.warnings.is_empty(), "{:?}", found.warnings);
    let names: Vec<_> = found.agents.keys().map(|n| n.as_str()).collect();
    assert_eq!(
        names,
        ["lead", "explore", "plan", "general", "advisor", "router", "search", "grep", "audit", "workflow-author"]
    );
    assert!(found
        .agents
        .values()
        .all(|d| d.source.scope == Scope::Builtin && !d.system_prompt.is_empty()));

    let a = &found.agents;
    let tools = |n: &str| a[&n.parse::<AgentName>().unwrap()].tools.allow.clone();
    let read_only = Some(vec![
        tool("read", None),
        tool("grep", None),
        tool("glob", None),
        tool("ls", None),
    ]);
    assert_eq!(tools("lead"), None);
    assert_eq!(tools("general"), None);
    assert_eq!(tools("explore"), read_only);
    assert_eq!(tools("plan"), read_only);
    assert_eq!(tools("search"), read_only);
    assert_eq!(
        tools("advisor"),
        Some(vec![
            tool("read", None),
            tool("grep", None),
            tool("glob", None)
        ])
    );
    assert_eq!(tools("router"), Some(vec![]));
    assert_eq!(
        tools("grep"),
        Some(vec![tool("grep", None), tool("read", None)])
    );
    assert_eq!(
        tools("audit"),
        Some(vec![
            tool("read", None),
            tool("grep", None),
            tool("glob", None),
            tool("bash", Some("git *")),
            tool("bash", Some("cargo *")),
            tool("bash", Some("npm *")),
        ])
    );

    let get = |n: &str| &a[&n.parse::<AgentName>().unwrap()];
    assert_eq!(
        tools("workflow-author"),
        Some(vec![tool("read", None), tool("glob", None), tool("grep", None), tool("mcp:aibus:*", None)])
    );
    assert_eq!(get("workflow-author").role, Role::Worker);
    assert!(!get("workflow-author").background);
    assert_eq!(get("workflow-author").model, ModelRef::Inherit);
    assert_eq!(get("lead").role, Role::Lead);
    assert_eq!(get("advisor").role, Role::Advisor);
    assert_eq!(get("router").role, Role::Router);
    assert_eq!(get("explore").role, Role::Worker);
    assert_eq!(get("audit").isolation, Isolation::Worktree);
    assert!(get("explore").omit_project_rules);
    assert_eq!(get("explore").model, ModelRef::Inherit);
    assert_eq!(
        get("advisor").model,
        ModelRef::Alias("advisor_model".into())
    );
    assert_eq!(get("router").model, ModelRef::Alias("router_model".into()));
    assert_eq!(get("search").model, ModelRef::Alias("swarm_model".into()));
    assert_eq!(get("lead").model, ModelRef::Alias("default_model".into()));
    assert_eq!(get("advisor").checkpoints.len(), 3);
}

// Scratch trees live under the target dir, one per test.
fn tree(test: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("agent_def")
        .join(test);
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

fn write(dir: &Path, file: &str, text: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join(file), text).unwrap();
}

fn explore(desc: &str) -> String {
    format!("---\nname: explore\ndescription: {desc}\nhooks:\n  stop:\n    - command: ./notify.sh\n---\nprompt from {desc}\n")
}

#[test]
fn same_name_in_all_scopes_follows_the_precedence_walk() {
    let root = tree("all_scopes");
    let project = root.join("proj");
    let cwd = project.join("src/deep");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(project.join(".git")).unwrap();
    write(
        &project.join(".aibus/agents"),
        "explore.md",
        &explore("project"),
    );
    let user = root.join("user");
    write(&user, "explore.md", &explore("user"));
    let extra = root.join("extra");
    write(&extra, "explore.md", &explore("extra"));
    let cli = r#"{"explore":{"description":"cli","prompt":"prompt from cli"}}"#;

    let who = |cli: Option<&str>, user: Option<&Path>, extra: &[PathBuf], cwd: &Path| {
        let found = discover(cli, cwd, user, extra, &[]).unwrap();
        let d = found
            .agents
            .get(&"explore".parse::<AgentName>().unwrap())
            .unwrap()
            .clone();
        (d.description, d.source.scope, found.agents.len())
    };
    let e = [extra.clone()];
    assert_eq!(
        who(Some(cli), Some(&user), &e, &cwd),
        ("cli".into(), Scope::Cli, 9)
    );
    assert_eq!(
        who(None, Some(&user), &e, &cwd),
        ("project".into(), Scope::Project, 9)
    );
    // Not inside a project: project scope is empty.
    assert_eq!(
        who(None, Some(&user), &e, &root.join("user")),
        ("user".into(), Scope::User, 9)
    );
    assert_eq!(
        who(None, None, &e, &root.join("user")),
        ("extra".into(), Scope::Extra(extra.clone()), 9)
    );
    assert_eq!(
        who(None, None, &[], &root.join("user")),
        (
            "Read-only codebase search. Say how thorough to be (quick, medium, very_thorough)."
                .into(),
            Scope::Builtin,
            9
        )
    );

    let cli_def = discover(Some(cli), &cwd, None, &[], &[])
        .unwrap()
        .agents
        .swap_remove(&"explore".parse::<AgentName>().unwrap())
        .unwrap();
    assert_eq!(cli_def.system_prompt, "prompt from cli");
}

#[test]
fn project_root_is_the_nearest_ancestor_with_aibus_or_git() {
    let root = tree("project_root");
    let outer = root.join("outer");
    let inner = outer.join("inner");
    fs::create_dir_all(inner.join(".aibus")).unwrap();
    fs::create_dir_all(outer.join(".git")).unwrap();
    write(
        &outer.join(".aibus/agents"),
        "p.md",
        "---\nname: outer-only\ndescription: d\n---\n",
    );
    write(
        &inner.join(".aibus/agents"),
        "p.md",
        "---\nname: inner-only\ndescription: d\n---\n",
    );
    let found = discover(None, &inner.join("a"), None, &[], &[]).unwrap();
    assert!(found
        .agents
        .contains_key(&"inner-only".parse::<AgentName>().unwrap()));
    assert!(!found
        .agents
        .contains_key(&"outer-only".parse::<AgentName>().unwrap()));
}

#[test]
fn extra_dirs_lose_their_hooks_unless_trusted() {
    let root = tree("trust");
    let extra = root.join(".claude/agents");
    write(&extra, "explore.md", &explore("extra"));
    let get = |trusted: &[PathBuf]| {
        discover(None, &root, None, std::slice::from_ref(&extra), trusted).unwrap()
    };
    let key = "explore".parse::<AgentName>().unwrap();

    let found = get(&[]);
    assert!(found.agents[&key].hooks.is_empty());
    assert!(
        found.warnings.iter().any(|w| w.contains("hooks ignored")),
        "{:?}",
        found.warnings
    );

    let found = get(std::slice::from_ref(&extra));
    assert_eq!(found.agents[&key].hooks["stop"][0].command, "./notify.sh");
    assert!(found.warnings.is_empty());
}

#[test]
fn project_hooks_are_kept() {
    let root = tree("project_hooks");
    fs::create_dir_all(root.join(".aibus")).unwrap();
    write(
        &root.join(".aibus/agents"),
        "explore.md",
        &explore("project"),
    );
    let found = discover(None, &root, None, &[], &[]).unwrap();
    assert!(!found.agents[&"explore".parse::<AgentName>().unwrap()]
        .hooks
        .is_empty());
}

#[test]
fn bad_files_warn_and_are_skipped() {
    let root = tree("bad_files");
    fs::create_dir_all(root.join(".aibus")).unwrap();
    let dir = root.join(".aibus/agents");
    write(&dir, "a.md", "---\nname: dup\ndescription: first\n---\n");
    write(&dir, "b.md", "---\nname: dup\ndescription: second\n---\n");
    write(&dir, "broken.md", "no frontmatter");
    write(&dir, "notes.txt", "ignored");
    let found = discover(None, &root, None, &[], &[]).unwrap();
    assert_eq!(
        found.agents[&"dup".parse::<AgentName>().unwrap()].description,
        "first"
    );
    assert_eq!(found.warnings.len(), 2, "{:?}", found.warnings);
    assert!(found.warnings.iter().any(|w| w.contains("broken.md")));
    assert!(found.warnings.iter().any(|w| w.contains("duplicate name")));
}

#[test]
fn cli_json_errors_are_errors() {
    let run = |json: &str| discover(Some(json), Path::new("/nonexistent"), None, &[], &[]);
    assert!(matches!(run("not json"), Err(DefError::Cli(_))));
    assert!(matches!(run("[]"), Err(DefError::Cli(_))));
    assert!(
        matches!(run(r#"{"x":{"prompt":"p"}}"#), Err(DefError::Cli(m)) if m.contains("description"))
    );
    assert!(matches!(
        run(r#"{"Bad":{"description":"d"}}"#),
        Err(DefError::Cli(_))
    ));
    let ok = run(r#"{"r":{"description":"d","tools":["read","grep"],"model":"sonnet","maxTurns":3,"extra":1}}"#).unwrap();
    let d = &ok.agents[&"r".parse::<AgentName>().unwrap()];
    assert_eq!(
        (d.max_turns, &d.model),
        (Some(3), &ModelRef::Alias("sonnet".into()))
    );
    assert_eq!(ok.warnings, ["--agents r: unknown key `extra`"]);
}

#[test]
fn fallback_is_absent_a_list_or_an_empty_list_that_pins_the_run() {
    let (d, w) = ok("---\nname: a\ndescription: x\n---\nBe brief.\n");
    assert!(w.is_empty());
    assert_eq!(d.fallback, None);
    let (d, w) = ok("---\nname: a\ndescription: x\nfallback: [anthropic-personal, bedrock]\n---\nBe brief.\n");
    assert!(w.is_empty());
    assert_eq!(d.fallback, Some(vec![AccountName("anthropic-personal".into()), AccountName("bedrock".into())]));
    let (d, w) = ok("---\nname: a\ndescription: x\nfallback: []\n---\nBe brief.\n");
    assert!(w.is_empty());
    assert_eq!(d.fallback, Some(Vec::new()), "[] stays put, which is not the same as absent");
}
