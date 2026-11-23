//! Schema 2 (#334): a typed flag has a value kind and the host fills the value from the plan. A
//! manifest can never put a value, a template or an argument string of its own into the launch.
#![cfg(unix)]

mod common;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use aibus_extension::manifest::{escalations, Manifest, ManifestError, ValueKind};
use aibus_extension::negotiate::{check_host, PROTOCOL};
use aibus_extension::sandbox::{check_launch, prepare, typed_args, Agent, Grant, TypedValue};
use aibus_extension::version::{Range, Version};
use aibus_supervisor::Limits;
use common::{edit, reference_dir, repo, template_manifest};

// ---- manifests ----

fn refused(text: &str) -> (String, String) {
    match Manifest::parse(text) {
        Err(ManifestError::Invalid { field, why }) => (field, why),
        other => panic!("expected an Invalid refusal, got {other:?}"),
    }
}

/// The reference manifest without its own typed blocks, as it was in schema 1.
fn reference_v1() -> String {
    let m = fs::read_to_string(reference_dir().join("aibus-extension.toml")).unwrap();
    let from = m
        .find("# Flags whose value the host fills")
        .expect("the typed blocks");
    let to = m.find("kind = \"agent_list\"\n").expect("the agents block")
        + "kind = \"agent_list\"\n".len();
    let m = format!("{}{}", &m[..from], &m[to..]);
    m.replace("schema_version = \"2\"", "schema_version = \"1\"")
        .replace(">=1.1.0, <2.0.0", ">=1.0.0, <2.0.0")
}

/// The reference manifest as schema 2 with `extra` appended (typed blocks).
fn reference_v2(extra: &str) -> String {
    let m = reference_v1()
        .replace("schema_version = \"1\"", "schema_version = \"2\"")
        .replace(">=1.0.0, <2.0.0", ">=1.1.0, <2.0.0");
    format!("{m}\n{extra}\n")
}

fn typed(flag: &str, kind: &str) -> String {
    format!("[[execution.typed]]\nflag = \"{flag}\"\nkind = \"{kind}\"\n")
}

const MODEL_AND_AGENTS: [(&str, &str); 2] = [("--model", "model_id"), ("--agents", "agent_list")];

fn both() -> String {
    MODEL_AND_AGENTS
        .iter()
        .map(|(f, k)| typed(f, k))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The template as schema 2 for another official CLI, with its argv and typed blocks.
fn as_cli(cli: &str, argv: &str, typed_blocks: &str) -> String {
    let m = edit(
        &template_manifest(),
        "executable_candidates = [\"examplecli\"]",
        &format!("executable_candidates = [\"{cli}\"]"),
    );
    let m = edit(
        &m,
        "argv = [\"run\", \"--json\"]",
        &format!("argv = {argv}"),
    );
    let optional = match cli {
        "claude" => "--verbose",
        "codex" => "--json",
        "gemini" => "--debug",
        _ => "--print",
    };
    let m = edit(&m, "flag = \"--usage\"", &format!("flag = \"{optional}\""));
    let m = edit(
        &m,
        "flags = [\"--usage\"]",
        &format!("flags = [\"{optional}\"]"),
    );
    let m = edit(&m, "schema_version = \"1\"", "schema_version = \"2\"");
    let m = edit(
        &m,
        "protocol = \">=1.0.0, <2.0.0\"",
        "protocol = \">=1.1.0, <2.0.0\"",
    );
    format!("{m}\n{typed_blocks}\n")
}

#[test]
fn issue_334_a_schema_2_manifest_with_typed_flags_parses() {
    let m = Manifest::parse(&reference_v2(&both())).unwrap();
    assert_eq!(m.schema_version, "2");
    let got: Vec<(&str, ValueKind)> = m
        .execution
        .typed
        .iter()
        .map(|t| (t.flag.as_str(), t.kind))
        .collect();
    assert_eq!(
        got,
        [
            ("--model", ValueKind::ModelId),
            ("--agents", ValueKind::AgentList)
        ]
    );
    // Every official CLI has a model flag; a path kind exists for Codex's output file.
    for cli in ["claude", "codex", "gemini", "kimi"] {
        let argv = match cli {
            "codex" => r#"["exec"]"#,
            "claude" => r#"["-p"]"#,
            "gemini" => r#"["--debug"]"#,
            _ => r#"["--print"]"#,
        };
        Manifest::parse(&as_cli(cli, argv, &typed("--model", "model_id")))
            .unwrap_or_else(|e| panic!("{cli}: {e:?}"));
    }
    Manifest::parse(&as_cli(
        "codex",
        r#"["exec"]"#,
        &typed("--output-last-message", "workdir_path"),
    ))
    .unwrap();
}

#[test]
fn issue_334_a_schema_1_manifest_cannot_carry_typed_flags() {
    let m = format!("{}\n{}\n", reference_v1(), typed("--model", "model_id"));
    let (field, why) = refused(&m);
    assert_eq!(field, "execution.typed");
    assert!(why.contains("schema 2"), "{why}");
}

/// A schema 1 host reads `compat.protocol` before anything else and the host speaks 1.0.0, so a
/// schema 2 manifest has to ask for 1.1.0 or later: a host that reads protocols tells itself apart this way (a host from before schema 2 fails closed earlier, at the unknown field).
#[test]
fn issue_334_a_schema_2_manifest_asks_for_a_protocol_a_schema_1_host_does_not_speak() {
    let m = Manifest::parse(&reference_v2(&both())).unwrap();
    let range = Range::parse(&m.compat.protocol).unwrap();
    assert!(
        !range.contains(&Version(1, 0, 0)),
        "a protocol 1.0.0 host would accept it"
    );
    assert!(range.contains(&PROTOCOL));
    assert_eq!(PROTOCOL, Version(1, 1, 0));
    assert!(check_host(&m, "linux").is_ok());
    // An author cannot write a schema 2 manifest that a schema 1 host would let through.
    let m = reference_v2(&both()).replace(">=1.1.0, <2.0.0", ">=1.0.0, <2.0.0");
    assert_eq!(refused(&m).0, "compat.protocol");
    // A schema 1 manifest still negotiates on this host.
    let old = Manifest::parse(&template_manifest()).unwrap();
    assert!(check_host(&old, "linux").is_ok());
}

/// A schema this host does not read is one clean refusal that names the schemas it does read, even
/// when the manifest uses keys this host has never heard of.
#[test]
fn issue_334_an_unknown_schema_is_refused_by_name_before_its_keys_are() {
    let m = format!(
        "{}\n[[execution.future]]\nsomething = 1\n",
        reference_v2("").replace("schema_version = \"2\"", "schema_version = \"3\"")
    );
    let (field, why) = refused(&m);
    assert_eq!(field, "schema_version");
    assert!(
        why.contains("\"1\"") && why.contains("\"2\"") && why.contains("\"3\""),
        "{why}"
    );
}

#[test]
fn issue_334_a_typed_flag_has_to_be_one_the_host_lists_for_that_cli_and_kind() {
    for (cli, argv, flag, kind) in [
        ("claude", r#"["-p"]"#, "--agents", "workdir_path"),
        ("claude", r#"["-p"]"#, "--model", "agent_list"),
        ("claude", r#"["-p"]"#, "--add-dir", "workdir_path"),
        ("claude", r#"["-p"]"#, "--settings", "workdir_path"),
        ("claude", r#"["-p"]"#, "--mcp-config", "workdir_path"),
        ("claude", r#"["-p"]"#, "--plugin-dir", "workdir_path"),
        ("codex", r#"["exec"]"#, "--profile", "model_id"),
        ("codex", r#"["exec"]"#, "--agents", "agent_list"),
        ("codex", r#"["exec"]"#, "--output-last-message", "model_id"),
        ("gemini", r#"["--debug"]"#, "--agents", "agent_list"),
        (
            "kimi",
            r#"["--print"]"#,
            "--mcp-config-file",
            "workdir_path",
        ),
    ] {
        let (field, why) = refused(&as_cli(cli, argv, &typed(flag, kind)));
        assert_eq!(field, "execution.typed", "{cli} {flag} {kind}: {why}");
        assert!(why.contains(flag), "{why}");
    }
    // A name the host has no list for has no typed flags either.
    let m = reference_v2(&typed("--model", "model_id")).replace(
        "executable_candidates = [\"claude\"]",
        "executable_candidates = [\"mycli\"]",
    );
    assert_ne!(Manifest::parse(&m).map(|_| ()), Ok(()));
}

#[test]
fn issue_334_a_typed_flag_cannot_also_be_written_into_argv() {
    for static_flag in [r#""--model", "x""#, r#""--model=x""#, r#""--agents", "x""#] {
        let m = reference_v2(&both()).replace(
            "\"--setting-sources\", \"user\"]",
            &format!("\"--setting-sources\", \"user\", {static_flag}]"),
        );
        assert_eq!(refused(&m).0, "execution.argv", "{static_flag}");
    }
    // The same flag twice.
    let m = reference_v2(&format!(
        "{}\n{}",
        typed("--model", "model_id"),
        typed("--model", "model_id")
    ));
    assert_eq!(refused(&m).0, "execution.typed");
}

/// The manifest names a flag and a kind, nothing else: no value, no default, no template, no
/// argument string, no free-text kind.
#[test]
fn issue_334_a_manifest_cannot_write_a_value_a_template_or_an_argument_string() {
    for extra in [
        "value = \"gpt-5\"",
        "default = \"gpt-5\"",
        "template = \"--model {model}\"",
        "argv = [\"--yolo\"]",
        "args = \"--yolo\"",
        "choices = [\"a\"]",
        "pattern = \".*\"",
    ] {
        let m = reference_v2(&format!("{}{extra}\n", typed("--model", "model_id")));
        assert!(
            matches!(Manifest::parse(&m), Err(ManifestError::Syntax(_))),
            "{extra}"
        );
    }
    for kind in [
        "string",
        "free_text",
        "argv",
        "shell",
        "path",
        "json",
        "ModelId",
    ] {
        let m = reference_v2(&typed("--model", kind));
        assert!(
            matches!(Manifest::parse(&m), Err(ManifestError::Syntax(_))),
            "{kind}"
        );
    }
    // The flag itself is a plain long flag, never a `--flag=value` or a prompt placeholder.
    for flag in [
        "--model=gpt-5",
        "model",
        "-m",
        "--Model",
        "{prompt}",
        "--model x",
        "--yolo",
    ] {
        let m = reference_v2(&typed(flag, "model_id"));
        assert_eq!(refused(&m).0, "execution.typed", "{flag}");
    }
}

#[test]
fn issue_334_a_new_typed_flag_is_an_escalation_the_user_accepts_by_name() {
    let old = Manifest::parse(&reference_v2(&typed("--model", "model_id"))).unwrap();
    let new = Manifest::parse(&reference_v2(&both())).unwrap();
    let e = escalations(&old, &new);
    assert!(e.contains(&"typed:--agents".to_owned()), "{e:?}");
    assert!(!e.iter().any(|x| x == "typed:--model"), "{e:?}");
    // A kind that changes under the same flag is one too.
    let flip = Manifest::parse(&as_cli(
        "codex",
        r#"["exec"]"#,
        &typed("--output-last-message", "workdir_path"),
    ))
    .unwrap();
    let none = Manifest::parse(&as_cli("codex", r#"["exec"]"#, "")).unwrap();
    assert!(escalations(&none, &flip).contains(&"typed:--output-last-message".to_owned()));
}

// ---- values ----

fn workdir() -> (tempfile::TempDir, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let work = fs::canonicalize(t.path()).unwrap().join("work");
    fs::create_dir_all(work.join("reports")).unwrap();
    (t, work)
}

fn values(flag: &str, v: TypedValue) -> BTreeMap<String, TypedValue> {
    BTreeMap::from([(flag.to_owned(), v)])
}

fn model_manifest() -> Manifest {
    Manifest::parse(&reference_v2(&both())).unwrap()
}

fn strs(args: &[OsString]) -> Vec<String> {
    args.iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn issue_334_the_host_fills_a_model_id_as_one_argument_after_its_flag() {
    let (_t, work) = workdir();
    let args = typed_args(
        &model_manifest(),
        &values("--model", TypedValue::Model("claude-sonnet-4".into())),
        &work,
    )
    .unwrap();
    assert_eq!(strs(&args), ["--model", "claude-sonnet-4"]);
    for ok in [
        "gpt-5",
        "anthropic/claude-3.5-sonnet",
        "kimi-k2:thinking",
        "gemini-2.5-pro",
    ] {
        assert!(
            typed_args(
                &model_manifest(),
                &values("--model", TypedValue::Model(ok.into())),
                &work
            )
            .is_ok(),
            "{ok}"
        );
    }
}

/// What a model id can never be: another flag, a second argument, a shell fragment.
#[test]
fn issue_334_a_model_id_cannot_smuggle_a_flag_or_an_argument() {
    let (_t, work) = workdir();
    let long = "a".repeat(129);
    for bad in [
        "",
        " ",
        "x --dangerously-skip-permissions",
        "--yolo",
        "-x",
        "a b",
        "a\nb",
        "a\0b",
        "$(id)",
        "`id`",
        "a;b",
        "a|b",
        "a&b",
        "a'b",
        "a\"b",
        "{x}",
        "a=b",
        "é",
        &long,
    ] {
        let r = typed_args(
            &model_manifest(),
            &values("--model", TypedValue::Model(bad.into())),
            &work,
        );
        assert!(r.is_err(), "{bad:?} was accepted: {r:?}");
    }
}

fn agent(name: &str) -> Agent {
    Agent {
        name: name.into(),
        description: "reviews code".into(),
        prompt: "You review.".into(),
    }
}

#[test]
fn issue_334_agents_are_rendered_as_one_json_object_the_host_builds() {
    let (_t, work) = workdir();
    let tricky = Agent {
        name: "reviewer".into(),
        description: "says \"hi\", with {braces}".into(),
        prompt: "line one\nline two with a quote \" and a backslash \\ and --yolo".into(),
    };
    let args = typed_args(
        &model_manifest(),
        &values(
            "--agents",
            TypedValue::Agents(vec![tricky.clone(), agent("tester")]),
        ),
        &work,
    )
    .unwrap();
    let a = strs(&args);
    assert_eq!(a[0], "--agents");
    assert_eq!(a.len(), 2);
    let v: serde_json::Value = serde_json::from_str(&a[1]).unwrap();
    assert_eq!(v.as_object().unwrap().len(), 2);
    assert_eq!(v["reviewer"]["description"], tricky.description);
    assert_eq!(v["reviewer"]["prompt"], tricky.prompt);
    assert_eq!(v["tester"].as_object().unwrap().len(), 2);
}

#[test]
fn issue_334_agents_have_names_sizes_and_counts_the_host_enforces() {
    let (_t, work) = workdir();
    let run = |agents: Vec<Agent>| {
        typed_args(
            &model_manifest(),
            &values("--agents", TypedValue::Agents(agents)),
            &work,
        )
    };
    for name in [
        "",
        "Bad Name",
        "../x",
        "--yolo",
        "UPPER",
        "a".repeat(49).as_str(),
        "x y",
        "a\"b",
    ] {
        assert!(run(vec![agent(name)]).is_err(), "{name:?}");
    }
    assert!(run(vec![]).is_err(), "an empty list");
    assert!(
        run(vec![agent("a"), agent("a")]).is_err(),
        "a duplicate name"
    );
    assert!(
        run((0..17).map(|i| agent(&format!("a{i}"))).collect()).is_err(),
        "17 agents"
    );
    assert!(
        run((0..16).map(|i| agent(&format!("a{i}"))).collect()).is_ok(),
        "16 agents"
    );
    let mut big = agent("a");
    big.description = "d".repeat(201);
    assert!(run(vec![big]).is_err());
    let mut big = agent("a");
    big.prompt = "p".repeat(8001);
    assert!(run(vec![big]).is_err());
    let mut empty = agent("a");
    empty.prompt = String::new();
    assert!(run(vec![empty]).is_err());
}

fn codex_path_manifest() -> Manifest {
    Manifest::parse(&as_cli(
        "codex",
        r#"["exec"]"#,
        &typed("--output-last-message", "workdir_path"),
    ))
    .unwrap()
}

#[test]
fn issue_334_a_path_stays_inside_the_working_directory() {
    let (t, work) = workdir();
    let m = codex_path_manifest();
    let run = |p: &str| {
        typed_args(
            &m,
            &values("--output-last-message", TypedValue::Path(p.into())),
            &work,
        )
    };
    let ok = strs(&run("reports/last.txt").unwrap());
    assert_eq!(
        ok,
        [
            "--output-last-message".to_owned(),
            work.join("reports/last.txt").to_string_lossy().into_owned()
        ]
    );
    assert!(run("last.txt").is_ok());
    // Outside, or not a plain relative path.
    std::os::unix::fs::symlink(t.path(), work.join("escape")).unwrap();
    for bad in [
        "",
        "/etc/passwd",
        "../x",
        "reports/../../x",
        "a/../../outside",
        "escape/x",
        "escape",
        "reports/\n",
        "a\0b",
        &"x/".repeat(300),
    ] {
        let r = run(bad);
        assert!(r.is_err(), "{bad:?} was accepted: {r:?}");
    }
    // A symlink inside the directory that stays inside is fine.
    std::os::unix::fs::symlink(work.join("reports"), work.join("alias")).unwrap();
    assert!(run("alias/out.txt").is_ok());
}

#[test]
fn issue_334_a_value_of_the_wrong_kind_or_for_an_undeclared_flag_is_refused() {
    let (_t, work) = workdir();
    let m = model_manifest();
    assert!(typed_args(&m, &values("--model", TypedValue::Path("x".into())), &work).is_err());
    assert!(typed_args(
        &m,
        &values("--model", TypedValue::Agents(vec![agent("a")])),
        &work
    )
    .is_err());
    assert!(typed_args(
        &m,
        &values("--agents", TypedValue::Model("gpt-5".into())),
        &work
    )
    .is_err());
    // A flag the manifest never declared gets no value, however the plan asks.
    assert!(typed_args(&m, &values("--yolo", TypedValue::Model("x".into())), &work).is_err());
    assert!(typed_args(
        &m,
        &values("--output-last-message", TypedValue::Path("x".into())),
        &work
    )
    .is_err());
    // A declared flag with no value in the plan is simply left out.
    assert_eq!(
        typed_args(&m, &BTreeMap::new(), &work).unwrap(),
        Vec::<OsString>::new()
    );
    // Both values: in the manifest's order.
    let mut both = values("--agents", TypedValue::Agents(vec![agent("a")]));
    both.insert("--model".into(), TypedValue::Model("gpt-5".into()));
    let a = strs(&typed_args(&m, &both, &work).unwrap());
    assert_eq!((a[0].as_str(), a[2].as_str()), ("--model", "--agents"));
}

// ---- the launch ----

fn spike(name: &str) -> String {
    fs::read_to_string(repo().join("fixtures/claude-code/spike-281").join(name)).unwrap()
}

fn stand_in() -> (tempfile::TempDir, Grant) {
    let t = tempfile::tempdir().unwrap();
    let bin = t.path().join("bin");
    let work = t.path().join("work");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&work).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(bin.join("version.txt"), spike("01-version.txt")).unwrap();
    fs::write(bin.join("help.txt"), spike("02-help.txt")).unwrap();
    let script = format!(
        "#!/bin/sh\nD={}\ncase \"$1\" in --version) cat \"$D/version.txt\"; exit 0;; --help) cat \"$D/help.txt\"; exit 0;; esac\ncat > /dev/null\n",
        bin.display()
    );
    let exe = bin.join("claude");
    fs::write(&exe, script).unwrap();
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
    let grant = Grant {
        workdir: work,
        search_dirs: vec![bin],
        parent_env: vec![(OsString::from("HOME"), OsString::from("/home/x"))],
        ceiling: Limits::default(),
        extra_programs: Vec::new(),
        os: if cfg!(target_os = "macos") {
            "macos".into()
        } else {
            "linux".into()
        },
        typed: BTreeMap::new(),
    };
    (t, grant)
}

#[tokio::test]
async fn issue_334_prepare_puts_the_hosts_typed_values_in_the_launch() {
    let (_t, mut grant) = stand_in();
    grant.typed.insert(
        "--model".into(),
        TypedValue::Model("claude-sonnet-4".into()),
    );
    grant.typed.insert(
        "--agents".into(),
        TypedValue::Agents(vec![agent("reviewer")]),
    );
    let m = model_manifest();
    let p = prepare(&m, &grant, &[]).await.unwrap();
    let args = strs(&p.launch.args.to_vec());
    let n = m.execution.argv.len();
    assert_eq!(&args[..n], &m.execution.argv[..]);
    assert_eq!(&args[n..n + 2], ["--model", "claude-sonnet-4"]);
    assert_eq!(args[n + 2], "--agents");
    assert!(serde_json::from_str::<serde_json::Value>(&args[n + 3]).is_ok());
    check_launch(&m, &p.launch, &[]).unwrap();
    // A bad value stops the launch before anything is built.
    grant
        .typed
        .insert("--model".into(), TypedValue::Model("--yolo".into()));
    assert!(prepare(&m, &grant, &[]).await.is_err());
}

#[tokio::test]
async fn issue_334_the_launch_check_refuses_a_typed_flag_with_a_value_the_host_would_not_make() {
    let (_t, mut grant) = stand_in();
    grant
        .typed
        .insert("--model".into(), TypedValue::Model("gpt-5".into()));
    let m = model_manifest();
    let good = prepare(&m, &grant, &[]).await.unwrap().launch;
    check_launch(&m, &good, &[]).unwrap();
    let n = m.execution.argv.len();

    let with = |tail: &[&str]| {
        let mut l = good.clone();
        l.args.truncate(n);
        l.args.extend(tail.iter().map(OsString::from));
        l
    };
    for tail in [
        &["--model", "--yolo"][..],
        &["--model", "a b"][..],
        &["--model"][..],
        &["--model", "gpt-5", "--model", "gpt-5"][..],
        &["--model=gpt-5"][..],
        &["--agents", "not json"][..],
        &[
            "--agents",
            r#"{"a":{"description":"d","prompt":"p","tools":["Bash"]}}"#,
        ][..],
        &["--agents", r#"["a"]"#][..],
        &["--output-last-message", "/etc/passwd"][..],
    ] {
        let r = check_launch(&m, &with(tail), &[]);
        // `--agents` with a well-formed object is the one thing that may pass; none of these is.
        assert!(r.is_err(), "{tail:?} was accepted");
    }
    assert!(check_launch(&m, &with(&["--model", "gpt-5"]), &[]).is_ok());
    // A manifest that declares no typed flag takes none.
    let plain = Manifest::parse(&reference_v1()).unwrap();
    assert!(check_launch(&plain, &with(&["--model", "gpt-5"]), &[]).is_err());
}

/// Review of #345: the short alias and the config key of a typed flag are the host's too.
#[test]
fn issue_334_a_typed_flags_alias_or_config_key_cannot_be_written_as_a_static_token() {
    for (cli, argv) in [
        ("codex", r#"["exec", "-m", "gpt-x"]"#),
        ("codex", r#"["exec", "-mgpt-x"]"#),
        ("codex", r#"["exec", "-c", "model=x"]"#),
        ("codex", r#"["exec", "-cmodel=x"]"#),
        ("codex", r#"["exec", "--config", "model=x"]"#),
        ("codex", r#"["exec", "--config=model=x"]"#),
        ("codex", r#"["exec", "--model", "x"]"#),
        ("gemini", r#"["-m", "x"]"#),
        ("kimi", r#"["--print", "-m", "x"]"#),
    ] {
        let (field, _) = refused(&as_cli(cli, argv, &typed("--model", "model_id")));
        assert_eq!(field, "execution.argv", "{cli} {argv}");
    }
    // Other config keys are still the manifest's to set.
    let ok = as_cli(
        "codex",
        r#"["exec", "-c", "model_reasoning_effort=low"]"#,
        &typed("--model", "model_id"),
    );
    assert!(Manifest::parse(&ok).is_ok());
}

/// The plain-relative-path rule in `typed_args` holds on its own, not only through the directory check.
#[test]
fn issue_334_a_path_has_to_be_plain_and_relative_even_when_it_points_inside() {
    let (_t, work) = workdir();
    let m = codex_path_manifest();
    let inside = work.join("reports/x.txt").to_string_lossy().into_owned();
    for bad in [
        inside.as_str(),
        "./x.txt",
        "reports/./x.txt",
        "reports//x.txt",
    ] {
        let r = typed_args(
            &m,
            &values("--output-last-message", TypedValue::Path(bad.into())),
            &work,
        );
        assert!(r.is_err(), "{bad:?} was accepted: {r:?}");
    }
}
