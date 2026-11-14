//! #283: the workflow compiler. A workflow and the agents it names compile to one plan with typed
//! edges, versions for every prompt and agent, the invocation each step would get and the routing
//! decisions; the same input gives the same bytes, a changed prompt shows up as drift, and a
//! workflow the static checks refuse never gets a plan.

use std::path::{Path, PathBuf};

use aibus_core::agent::discover::discover;
use aibus_core::workflow::compile::{
    canonical_json, capability_problems, compile, content_hash, plan_hash,
};
use aibus_core::workflow::jev::{self, Decision, Request};
use aibus_core::workflow::manifest::{diff, run_manifest, RunFacts};
use aibus_core::workflow::{self, native, preview, Workflow};
use aibus_core::{AgentDef, AgentName, Redactor};
use indexmap::IndexMap;
use serde_json::{json, Value};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The built-in agents, found the way a project with no agents of its own finds them.
fn agents() -> IndexMap<AgentName, AgentDef> {
    let dir = tempfile::tempdir().unwrap();
    discover(None, dir.path(), None, &[], &[]).unwrap().agents
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap()
}

fn plan_of(text: &str) -> Value {
    let wf = Workflow::parse("t", text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    compile(&wf, text, &agents(), &Redactor::new()).unwrap_or_else(|p| panic!("{p:?}"))
}

fn edges(plan: &Value, kind: &str) -> Vec<(String, String)> {
    plan["edges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == kind)
        .map(|e| {
            (
                e["from"].as_str().unwrap().to_owned(),
                e["to"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn pair(a: &str, b: &str) -> (String, String) {
    (a.to_owned(), b.to_owned())
}

#[test]
fn a_chain_compiles_to_sequence_data_and_bounded_retry_edges() {
    let plan = plan_of(&read("examples/workflows/fix.toml"));
    let keys: Vec<&str> = plan["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["explore", "plan", "general#3", "general#4"]);
    assert_eq!(
        edges(&plan, "sequence"),
        [
            pair("explore", "plan"),
            pair("plan", "general#3"),
            pair("general#3", "general#4")
        ]
    );
    assert_eq!(
        edges(&plan, "data"),
        [
            pair("input", "explore"),
            pair("explore", "plan"),
            pair("explore", "general#3"),
            pair("plan", "general#3"),
            pair("input", "general#4"),
        ]
    );
    // on_fail = "retry:2" is a loop back over the pair, bounded at two.
    let retry: Vec<&Value> = plan["edges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "retry")
        .collect();
    assert_eq!(retry.len(), 1);
    assert_eq!(
        (retry[0]["from"].as_str(), retry[0]["to"].as_str()),
        (Some("general#4"), Some("general#3"))
    );
    assert_eq!(retry[0]["max_iterations"], 2);
    assert_eq!(plan["envelope"]["worst_case_agent_runs"], 8);
    assert_eq!(plan["runnable_now"], true);
    assert_eq!(plan["entry"], "explore");
    // The gate is in the plan, as the command it is, and the plan never ran it.
    assert!(plan["steps"][1]["gate"]
        .as_str()
        .unwrap()
        .contains("RISK: HIGH"));
    for s in plan["steps"].as_array().unwrap() {
        assert_eq!(s["runtime"], "aibus");
        assert_eq!(s["invocation"]["kind"], "aibus_run");
        assert!(s["prompt"]["sha256"]
            .as_str()
            .unwrap()
            .starts_with("sha256:"));
        assert!(s["agent_sha256"].as_str().unwrap().starts_with("sha256:"));
    }
}

#[test]
fn every_example_and_graph_fixture_compiles_to_the_same_bytes_every_time_and_on_every_host() {
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for dir in ["examples/workflows", "fixtures/workflows/graph"] {
        for e in std::fs::read_dir(root().join(dir)).unwrap() {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "toml") {
                files.push((p.file_stem().unwrap().to_string_lossy().into_owned(), p));
            }
        }
    }
    files.sort();
    assert!(files.len() >= 7);
    for (stem, path) in files {
        let text = std::fs::read_to_string(&path).unwrap();
        let wf = Workflow::parse(&stem, &text).unwrap();
        let one = serde_json::to_string_pretty(
            &compile(&wf, &text, &agents(), &Redactor::new()).unwrap(),
        )
        .unwrap();
        let two = serde_json::to_string_pretty(
            &compile(&wf, &text, &agents(), &Redactor::new()).unwrap(),
        )
        .unwrap();
        assert_eq!(one, two, "{stem}: two compilations differ");
        // The plan hash is the hash of the plan's canonical JSON, recomputable from the bytes.
        let value: Value = serde_json::from_str(&one).unwrap();
        assert_eq!(
            value["plan_hash"].as_str().unwrap(),
            plan_hash(&value),
            "{stem}"
        );
        // The committed plan was written on another machine; the same bytes here mean the plan
        // depends on nothing about the host.
        let golden = root().join(format!("fixtures/workflows/plans/{stem}.plan.json"));
        if std::env::var_os("AIBUS_BLESS").is_some() {
            std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
            std::fs::write(&golden, format!("{one}\n")).unwrap();
        }
        let expected = std::fs::read_to_string(&golden)
            .unwrap_or_else(|e| panic!("{}: {e}", golden.display()));
        assert_eq!(
            format!("{one}\n"),
            expected,
            "{stem}: the plan differs from {}",
            golden.display()
        );
    }
}

#[test]
fn content_hashes_ignore_line_end_conversion_and_canonical_json_sorts_keys() {
    assert_eq!(content_hash("a\r\nb\r\n"), content_hash("a\nb\n"));
    assert_ne!(content_hash("a\nb"), content_hash("a\nb\n"));
    assert_eq!(
        content_hash(""),
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        canonical_json(&json!({"b": 1, "a": [true, null, "x"]})),
        r#"{"a":[true,null,"x"],"b":1}"#
    );
    // A file checked out with CRLF line ends plans to the same hash.
    let text = read("examples/workflows/review.toml");
    let crlf = text.replace('\n', "\r\n");
    assert_eq!(plan_of(&text)["plan_hash"], plan_of(&crlf)["plan_hash"]);
}

#[test]
fn a_changed_prompt_is_a_new_version_and_the_diff_says_which_step_drifted() {
    let text = read("fixtures/workflows/graph/fan-out.toml");
    let before = plan_of(&text);
    let after = plan_of(&text.replace(
        "Find the tests this plan touches",
        "Find every test this plan touches",
    ));
    assert_ne!(before["plan_hash"], after["plan_hash"]);
    let changes = diff(&before, &after);
    let paths: Vec<&str> = changes.iter().map(|c| c.path.as_str()).collect();
    assert!(paths.contains(&"steps.tests.prompt.sha256"), "{paths:?}");
    assert!(paths.contains(&"plan_hash"), "{paths:?}");
    // Only the step whose prompt changed has a new version.
    assert!(
        !paths
            .iter()
            .any(|p| p.starts_with("steps.code.") || p.starts_with("steps.build.")),
        "{paths:?}"
    );
    // A plan compared with itself has no changes, and moving a step in the file is not one.
    assert!(diff(&before, &before).is_empty());
    let a = "[[step]]\nid = \"a\"\nagent = \"plan\"\nprompt = \"p {{input}}\"\n\n[[step]]\nid = \"b\"\nagent = \"general\"\nprompt = \"q\"\n\n[[step]]\nid = \"c\"\nagent = \"explore\"\nprompt = \"r\"\n\n[[route]]\nfrom = \"a\"\nto = \"b\"\n\n[[route]]\nfrom = \"a\"\nto = \"c\"\n";
    let b = "[[step]]\nid = \"a\"\nagent = \"plan\"\nprompt = \"p {{input}}\"\n\n[[step]]\nid = \"c\"\nagent = \"explore\"\nprompt = \"r\"\n\n[[step]]\nid = \"b\"\nagent = \"general\"\nprompt = \"q\"\n\n[[route]]\nfrom = \"a\"\nto = \"c\"\n\n[[route]]\nfrom = \"a\"\nto = \"b\"\n";
    let moved: Vec<String> = diff(&plan_of(a), &plan_of(b))
        .into_iter()
        .map(|c| c.path)
        .collect();
    assert!(
        moved
            .iter()
            .all(|p| p == "plan_hash" || p == "workflow.source_sha256" || p.ends_with(".n")),
        "{moved:?}"
    );
}

#[test]
fn a_plan_edited_by_hand_no_longer_matches_its_hash() {
    let mut plan = plan_of(&read("fixtures/workflows/graph/repo-review.toml"));
    assert_eq!(plan["plan_hash"].as_str().unwrap(), plan_hash(&plan));
    plan["steps"][0]["prompt"]["sha256"] = json!("sha256:00");
    assert_ne!(plan["plan_hash"].as_str().unwrap(), plan_hash(&plan));
}

/// Each file in `fixtures/workflows/graph-invalid` starts with `# expect: <text>`, and reading it
/// fails with an error that contains that text.
#[test]
fn every_static_error_is_refused_when_the_file_is_read_and_says_where() {
    let dir = root().join("fixtures/workflows/graph-invalid");
    let mut n = 0;
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    for path in files {
        let text = std::fs::read_to_string(&path).unwrap();
        let want = text
            .lines()
            .next()
            .unwrap()
            .strip_prefix("# expect: ")
            .expect("an # expect: line");
        let err = Workflow::parse("t", &text)
            .expect_err(&path.display().to_string())
            .to_string();
        assert!(
            err.contains(want),
            "{}: want {want:?}, got {err:?}",
            path.display()
        );
        // `validate` reports the same thing, so the CLI and MCP refuse it too.
        let v = workflow::validate(&text, "t", &agents().keys().cloned().collect::<Vec<_>>());
        assert!(!v.ok, "{}", path.display());
        n += 1;
    }
    assert!(n >= 25, "{n}");
}

#[test]
fn a_missing_capability_and_an_unknown_agent_are_refused_before_any_plan() {
    // explore has read, grep, glob and ls: no shell.
    let text = "[[step]]\nagent = \"explore\"\nrequires = [\"repo_read\", \"shell\"]\nprompt = \"p {{input}}\"\n";
    let wf = Workflow::parse("t", text).unwrap();
    let problems = capability_problems(&wf, &agents());
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].step, Some(1));
    assert_eq!(problems[0].field.as_deref(), Some("requires"));
    assert!(
        problems[0]
            .message
            .contains("requires shell, and agent explore has no tool that grants it (bash)"),
        "{problems:?}"
    );
    let refused = compile(&wf, text, &agents(), &Redactor::new()).unwrap_err();
    assert!(
        refused.iter().any(|p| p.message.contains("requires shell")),
        "{refused:?}"
    );

    let ghost = "[[step]]\nagent = \"ghost\"\nprompt = \"p\"\n";
    let wf = Workflow::parse("t", ghost).unwrap();
    let refused = compile(&wf, ghost, &agents(), &Redactor::new()).unwrap_err();
    assert!(
        refused
            .iter()
            .any(|p| p.message.contains("no agent named `ghost`")),
        "{refused:?}"
    );
}

#[test]
fn routes_parents_and_native_steps_compile_to_typed_edges_fan_out_and_an_envelope() {
    let plan = plan_of(&read("fixtures/workflows/graph/fan-out.toml"));
    assert_eq!(
        edges(&plan, "route"),
        [
            pair("plan", "code"),
            pair("plan", "tests"),
            pair("code", "build"),
            pair("tests", "build")
        ]
    );
    assert_eq!(edges(&plan, "loop"), [pair("build", "plan")]);
    assert!(
        edges(&plan, "sequence").is_empty(),
        "routes replace the chain"
    );
    let when: Vec<&str> = plan["edges"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["when"].as_str())
        .collect();
    assert!(
        when.contains(&"plan.completed && plan.artifact_valid"),
        "{when:?}"
    );
    assert!(when.contains(&"build.failed && !plan.failed"), "{when:?}");
    assert_eq!(
        plan["fan_out"],
        json!([{ "from": "plan", "to": ["code", "tests"] }])
    );
    // Four steps, and the loop may run all four twice more.
    assert_eq!(plan["envelope"]["worst_case_agent_runs"], 12);
    assert_eq!(plan["envelope"]["max_agent_runs"], 12);
    assert_eq!(plan["envelope"]["max_parallel"], 2);
    // build can write and run commands, both held for approval; the readers are not.
    assert_eq!(
        plan["approvals"]["steps"]["build"],
        json!(["repo_write", "shell"])
    );
    assert!(plan["approvals"]["steps"].get("code").is_none());
    assert_eq!(plan["runnable_now"], false);
    assert!(plan["plan_only_because"]
        .as_str()
        .unwrap()
        .contains("[[route]]"));

    let plan = plan_of(&read("fixtures/workflows/graph/repo-review.toml"));
    assert_eq!(plan["workflow"]["revision"], "1.0.0");
    let search = &plan["steps"][1];
    assert_eq!(
        (
            search["key"].as_str(),
            search["mode"].as_str(),
            search["mode_requested"].as_str()
        ),
        (Some("search"), Some("native"), Some("native_or_managed"))
    );
    assert_eq!(search["invocation"]["kind"], "native_subagent");
    assert_eq!(search["invocation"]["launched_by"], "lead");
    let audit = &plan["steps"][2];
    assert_eq!(
        (audit["mode"].as_str(), audit["isolation"].as_str()),
        (Some("aibus_managed"), Some("git_worktree"))
    );
    let delegation: Vec<(String, String, String)> = plan["edges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "delegation")
        .map(|e| {
            (
                e["from"].as_str().unwrap().into(),
                e["to"].as_str().unwrap().into(),
                e["evidence"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        delegation,
        [
            ("lead".into(), "search".into(), "native".into()),
            ("lead".into(), "audit".into(), "aibus_managed".into())
        ]
    );
    // Native subagents run inside the vendor process; aibus starts two runs.
    assert_eq!(plan["envelope"]["worst_case_agent_runs"], 2);
}

#[test]
fn the_claude_invocation_is_an_argv_with_inline_agents_and_writes_nothing() {
    let plan = plan_of(&read("fixtures/workflows/graph/repo-review.toml"));
    let lead = &plan["steps"][0]["invocation"];
    assert_eq!(lead["kind"], "process");
    assert_eq!(lead["program"], "claude");
    assert_eq!(lead["shell"], false);
    assert_eq!(lead["stdin"], "null");
    assert_eq!(lead["writes_files"], json!([]));
    let argv: Vec<&str> = lead["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    for flag in [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--no-session-persistence",
        "--permission-prompts",
        "none",
        "--agents",
    ] {
        assert!(argv.contains(&flag), "{flag} missing from {argv:?}");
    }
    // No shell, no permission bypass, and the prompt is not on the plan's argv (it is rendered at
    // run time from the versioned template).
    for bad in [
        "sh",
        "-c",
        "--dangerously-skip-permissions",
        "bypassPermissions",
    ] {
        assert!(!argv.contains(&bad), "{bad} in {argv:?}");
    }
    assert!(
        argv[1].starts_with("<prompt of step lead, template sha256:"),
        "{argv:?}"
    );
    // The native subagent goes in --agents, with its definition's prompt first (the stable
    // prefix) and the step's instructions after it.
    let search = &lead["agents"]["search"];
    assert_eq!(search["tools"], json!(["Glob", "Grep", "Read"]));
    let prompt = search["prompt"].as_str().unwrap();
    assert!(
        prompt.ends_with(
            "Find the code the change touches, its callers and its tests. Quote paths and lines."
        ),
        "{prompt}"
    );
    assert_eq!(
        lead["delegation"],
        json!({ "foreground": "requested_in_prompt", "host_enforced": false })
    );
    let env: Vec<&str> = lead["env_allowlist"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert!(env.contains(&"CLAUDE_CONFIG_DIR"), "{env:?}");
}

#[test]
fn a_prompt_whose_task_text_comes_first_is_noted_for_the_prompt_cache() {
    let text =
        "[[step]]\nagent = \"plan\"\nprompt = \"{{input}} is the task. Plan it step by step.\"\n";
    let plan = plan_of(text);
    assert_eq!(plan["steps"][0]["prompt"]["stable_prefix_bytes"], 0);
    let notes: Vec<&str> = plan["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap())
        .collect();
    assert!(
        notes
            .iter()
            .any(|n| n.contains("step plan") && n.contains("stable prefix")),
        "{notes:?}"
    );
    // The shipped review example puts the task at the end: nothing to say.
    let plan = plan_of(&read("examples/workflows/review.toml"));
    assert!(
        plan["steps"][0]["prompt"]["stable_prefix_bytes"]
            .as_u64()
            .unwrap()
            > 40
    );
}

#[test]
fn the_run_manifest_pins_its_inputs_by_hash_and_says_what_is_unknown() {
    let plan = plan_of(&read("fixtures/workflows/graph/repo-review.toml"));
    let facts = RunFacts {
        input: Some("review PR 17".into()),
        aibus_version: "0.2.1".into(),
        os: "linux".into(),
        arch: "x86_64".into(),
        base_commit: Ok("6d8679fd0b0e8d5a0b0d6b3bb7f3c1d5f0f00a11".into()),
        dirty: Err("not a git repository".into()),
        env_names: vec!["PATH".into(), "HOME".into()],
        budgets: json!({ "max_turns": 50 }),
    };
    let m = run_manifest(&plan, &facts);
    assert_eq!(m["kind"], "aibus.run-manifest");
    assert_eq!(m["workflow"]["plan_hash"], plan["plan_hash"]);
    assert_eq!(m["workflow"]["revision"], json!({ "value": "1.0.0" }));
    assert_eq!(
        m["task_input"]["sha256"],
        json!({ "value": content_hash("review PR 17") })
    );
    assert_eq!(m["task_input"]["bytes"], json!({ "value": 12 }));
    assert!(
        !m.to_string().contains("review PR 17"),
        "the input itself is never stored"
    );
    assert_eq!(
        m["repository"]["base_commit"],
        json!({ "value": "6d8679fd0b0e8d5a0b0d6b3bb7f3c1d5f0f00a11" })
    );
    assert_eq!(
        m["repository"]["dirty_before"],
        json!({ "unknown": "not a git repository" })
    );
    assert_eq!(m["environment"]["variable_names"], json!(["HOME", "PATH"]));
    // What only a started run can know is unknown, and says why.
    for path in [
        &m["execution"]["started_at"],
        &m["execution"]["attempt_id"],
        &m["repository"]["dirty_after"],
        &m["approvals"]["outcomes"],
    ] {
        assert!(path.get("unknown").is_some_and(Value::is_string), "{path}");
    }
    // The Claude Code CLI's path and version are probed at launch (#282), not guessed here.
    let claude = m["runtimes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "cli.claude")
        .unwrap();
    assert!(claude["executable"].get("unknown").is_some(), "{claude}");
    assert!(claude["version"].get("unknown").is_some(), "{claude}");
    assert_eq!(
        m["provider_session_id"],
        json!({ "not_applicable": "aibus does not keep vendor session ids before #282 decides they may be kept" })
    );
    // Model selection: what the agent asks for is known, what the vendor reports is not.
    let lead = m["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["step"] == "lead")
        .unwrap();
    assert_eq!(lead["configured"], json!({ "value": "inherit" }));
    assert!(lead["reported"].get("unknown").is_some());
    assert_eq!(
        m["jev"]["policy_revision"],
        json!({ "value": jev::policy_revision() })
    );
    // No input at all is not the empty string.
    let m = run_manifest(
        &plan,
        &RunFacts {
            input: None,
            ..facts
        },
    );
    assert!(
        m["task_input"]["sha256"].get("unknown").is_some(),
        "{}",
        m["task_input"]
    );
}

#[test]
fn the_jev_routing_fixtures_decide_as_recorded() {
    let dir = root().join("fixtures/jev");
    let mut n = 0;
    for e in std::fs::read_dir(&dir).unwrap() {
        let path = e.unwrap().path();
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("{{policy_revision}}", &jev::policy_revision());
        let case: Value = serde_json::from_str(&text).unwrap();
        let req: Request = serde_json::from_value(case["request"].clone()).unwrap();
        let want: Decision = serde_json::from_value(case["decision"].clone()).unwrap();
        assert_eq!(jev::decide(&req), want, "{}", path.display());
        // The same request decides the same way every time.
        assert_eq!(jev::decide(&req), jev::decide(&req.clone()));
        n += 1;
    }
    assert!(n >= 8, "{n}");
    assert!(jev::policy_revision().starts_with("sha256:"));
}

#[test]
fn every_step_in_a_plan_carries_its_jev_decision() {
    let plan = plan_of(&read("fixtures/workflows/graph/repo-review.toml"));
    let decisions = plan["jev"]["decisions"].as_array().unwrap();
    assert_eq!(decisions.len(), 3);
    let search = decisions.iter().find(|d| d["step"] == "search").unwrap();
    assert_eq!(search["decision"]["execution_mode"], "native");
    assert_eq!(search["decision"]["decision_source"], "jev_deterministic");
    assert_eq!(search["request"]["parent_runtime"], "cli.claude");
    assert_eq!(plan["jev"]["policy_revision"], jev::policy_revision());
}

#[test]
fn a_workflow_that_only_the_scheduler_can_run_is_refused_by_load_before_any_step() {
    let dir = tempfile::tempdir().unwrap();
    let wf_dir = dir.path().join(".aibus/workflows");
    std::fs::create_dir_all(&wf_dir).unwrap();
    std::fs::write(
        wf_dir.join("fan-out.toml"),
        read("fixtures/workflows/graph/fan-out.toml"),
    )
    .unwrap();
    std::fs::write(
        wf_dir.join("chain-ids.toml"),
        read("fixtures/workflows/graph/chain-ids.toml"),
    )
    .unwrap();
    let dirs = workflow::dirs(dir.path(), None);
    let err = workflow::load(&dirs, "fan-out").unwrap_err();
    assert!(
        err.contains("the scheduler (#288)") && err.contains("nothing was started"),
        "{err}"
    );
    assert!(
        err.contains("aibus workflow plan fan-out --dry-run"),
        "{err}"
    );
    // A chain that only adds ids, a revision and requires runs as a chain always did.
    let wf = workflow::load(&dirs, "chain-ids").unwrap();
    assert_eq!(wf.plan_only(), None);
}

#[test]
fn ids_tell_two_steps_of_one_agent_apart() {
    let wf = Workflow::parse(
        "chain-ids",
        &read("fixtures/workflows/graph/chain-ids.toml"),
    )
    .unwrap();
    assert_eq!((wf.key(0), wf.key(1)), ("draft", "check"));
    let rendered = preview(&wf, "the bug");
    assert_eq!(
        rendered[1],
        "Check the draft against the task. Draft: <summary of step 1 (general)> Task: the bug"
    );
    // A key that names a later step is still refused in a chain.
    let bad = "[[step]]\nid = \"a\"\nagent = \"general\"\nprompt = \"{{steps.b.summary}}\"\n\n[[step]]\nid = \"b\"\nagent = \"general\"\nprompt = \"x\"\n";
    assert!(Workflow::parse("t", bad)
        .unwrap_err()
        .to_string()
        .contains("does not come before"));
}

#[test]
fn routed_and_native_previews_read_the_step_each_template_means() {
    let wf = Workflow::parse("fan-out", &read("fixtures/workflows/graph/fan-out.toml")).unwrap();
    let rendered = preview(&wf, "add a flag");
    assert_eq!(
        rendered[1],
        "Find the code this plan touches: <summary of step 1 (plan)>"
    );
    assert_eq!(
        rendered[3],
        "Build it. Code: <summary of step 2 (explore)> Tests: <summary of step 3 (search)>"
    );
}

#[test]
fn the_native_export_shows_a_diff_and_never_overwrites_without_the_consent_for_it() {
    let text = read("fixtures/workflows/graph/repo-review.toml");
    let wf = Workflow::parse("repo-review", &text).unwrap();
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".claude/agents");
    // Nothing there yet: one new file, and the preview writes nothing.
    let export = native::preview(&wf, &agents(), &dir).unwrap();
    assert_eq!(export.files.len(), 1);
    assert_eq!(export.files[0].name, "search.md");
    assert!(export.diff.contains("+++ search.md"), "{}", export.diff);
    assert!(export.hash.starts_with("sha256:"));
    assert!(!dir.exists(), "a preview writes nothing");
    // A wrong consent writes nothing.
    assert!(native::apply(&export, "sha256:not-the-diff").is_err());
    assert!(!dir.join("search.md").exists());
    native::apply(&export, &export.hash).unwrap();
    let written = std::fs::read_to_string(dir.join("search.md")).unwrap();
    assert!(written.starts_with("---\nname: search\n"), "{written}");
    assert!(written.contains("tools: Glob, Grep, Read"), "{written}");

    // The person edits the file. A new preview shows their edit going away; the old consent no
    // longer applies, and their file is untouched.
    std::fs::write(dir.join("search.md"), "my own search agent\n").unwrap();
    assert!(
        native::apply(&export, &export.hash).is_err(),
        "the disk changed since that preview"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("search.md")).unwrap(),
        "my own search agent\n"
    );
    let again = native::preview(&wf, &agents(), &dir).unwrap();
    assert!(
        again.diff.contains("-my own search agent"),
        "{}",
        again.diff
    );
    assert_ne!(again.hash, export.hash);
    native::apply(&again, &again.hash).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("search.md")).unwrap(),
        written
    );
    // Exported again with nothing changed: no diff, and nothing to consent to.
    let same = native::preview(&wf, &agents(), &dir).unwrap();
    assert!(same.diff.is_empty(), "{}", same.diff);

    // A symlink in place of the file is refused, not written through.
    #[cfg(unix)]
    {
        let target = home.path().join("elsewhere.md");
        std::fs::write(&target, "keep\n").unwrap();
        std::fs::remove_file(dir.join("search.md")).unwrap();
        std::os::unix::fs::symlink(&target, dir.join("search.md")).unwrap();
        assert!(native::preview(&wf, &agents(), &dir).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep\n");
    }
}

#[test]
fn a_workflow_with_no_native_subagents_exports_nothing() {
    let text = read("examples/workflows/fix.toml");
    let wf = Workflow::parse("fix", &text).unwrap();
    let home = tempfile::tempdir().unwrap();
    let export = native::preview(&wf, &agents(), &home.path().join("agents")).unwrap();
    assert!(export.files.is_empty());
    assert!(export.diff.is_empty());
}
