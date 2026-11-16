//! The Codex CLI extension (#357, A2.2a): the official `codex exec --json` as an extension on the
//! A2.1 contract. The fixtures are streams recorded from codex-cli 0.167.0 on 2026-11-15 (ids and
//! token counts replaced, nothing else touched); `fixtures/codex/cli-0.167.0/` holds the version
//! and help text that same CLI printed. Nothing here starts the real CLI except the ignored live
//! test at the bottom.
#![cfg(unix)]

mod common;

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use aibus_core::Redactor;
use aibus_extension::conformance::observe;
use aibus_extension::manifest::{Capability, Manifest, ManifestError, ValueKind};
use aibus_extension::negotiate::{negotiate, Verdict};
use aibus_extension::package;
use aibus_extension::sandbox::{engine, prepare, Grant};
use aibus_supervisor::cli;
use aibus_supervisor::{supervise, Limits};
use aibus_trace::{
    AttemptRef, AttemptStatus, DataQuality, EventKind, PayloadPolicy, StoreLimits, TraceStore,
    Visibility,
};
use common::{edit, repo, template_manifest};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn codex_dir() -> PathBuf {
    repo().join("examples/extensions/codex")
}

fn cli_text(name: &str) -> String {
    fs::read_to_string(repo().join("fixtures/codex/cli-0.167.0").join(name)).unwrap()
}

fn manifest() -> Manifest {
    Manifest::parse(&fs::read_to_string(codex_dir().join("aibus-extension.toml")).unwrap()).unwrap()
}

// ---------------------------------------------------------------- the manifest

#[test]
fn the_manifest_is_a_schema_2_extension_for_the_official_codex_cli() {
    let m = manifest();
    assert_eq!(m.schema_version, "2");
    assert_eq!(m.id, "org.devshell.aibus.codex-cli");
    assert_eq!(m.provider, "OpenAI");
    assert_eq!(m.cli.executable_candidates, ["codex"]);
    assert_eq!(m.auth.login_argv, ["login"]);
    assert_eq!(m.auth.status_argv, ["login", "status"]);
    // The prompt goes on stdin, never in argv, and nothing in argv asks the CLI to run unasked.
    assert_eq!(m.execution.argv.first().map(String::as_str), Some("exec"));
    assert!(m.execution.argv.iter().any(|a| a == "--json"));
    let typed: Vec<_> = m
        .execution
        .typed
        .iter()
        .map(|t| (t.flag.as_str(), t.kind))
        .collect();
    assert_eq!(
        typed,
        [
            ("--model", ValueKind::ModelId),
            ("--output-last-message", ValueKind::WorkdirPath)
        ]
    );
}

/// Every flag the manifest passes is one the real `codex exec --help` lists, so the allowlist
/// entry was written against the CLI and not against a guess.
#[test]
fn every_flag_in_the_manifest_is_in_the_real_exec_help() {
    let exec_help = cli_text("exec-help.txt");
    let m = manifest();
    let listed = |flag: &str| {
        exec_help
            .lines()
            .any(|l| l.contains(&format!("{flag} ")) || l.trim_end().ends_with(flag))
    };
    let argv_flags = m.execution.argv.iter().filter(|a| a.starts_with("--"));
    let typed_flags = m.execution.typed.iter().map(|t| &t.flag);
    for flag in argv_flags.chain(typed_flags) {
        assert!(listed(flag), "{flag} is not in `codex exec --help` 0.167.0");
    }
}

// ------------------------------------------------- rule conditions (schema 2)

/// The template as schema 2 with one rule that also needs a second line to match.
fn with_when(extra_when: &str, schema: &str) -> String {
    let t = edit(&template_manifest(), "schema_version = \"1\"", schema);
    let t = edit(&t, ">=1.0.0, <2.0.0", ">=1.1.0, <2.0.0");
    edit(
        &t,
        "kind = \"tool_started\"\n",
        &format!("kind = \"tool_started\"\n{extra_when}\n"),
    )
}

const WHEN_ONE: &str = "when = [{ pointer = \"/item/type\", equals = \"command_execution\" }]";

fn refused(text: &str) -> String {
    match Manifest::parse(text) {
        Err(ManifestError::Invalid { field, .. }) => field,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn a_rule_may_name_up_to_three_more_conditions_in_schema_2() {
    let text = with_when(WHEN_ONE, "schema_version = \"2\"");
    let m = Manifest::parse(&text).unwrap_or_else(|e| panic!("{e:?}"));
    let rule = m
        .normalizer
        .rule
        .iter()
        .find(|r| !r.when.is_empty())
        .expect("the rule keeps its condition");
    assert_eq!(rule.when.len(), 1);
    assert_eq!(rule.when[0].pointer, "/item/type");
    assert_eq!(rule.when[0].equals, "command_execution");

    let four = "when = [{ pointer = \"/a\", equals = \"1\" }, { pointer = \"/b\", equals = \"1\" }, { pointer = \"/c\", equals = \"1\" }, { pointer = \"/d\", equals = \"1\" }]";
    assert_eq!(
        refused(&with_when(four, "schema_version = \"2\"")),
        "normalizer.rule"
    );
    let bad_pointer = "when = [{ pointer = \"item/type\", equals = \"x\" }]";
    assert_eq!(
        refused(&with_when(bad_pointer, "schema_version = \"2\"")),
        "normalizer.rule"
    );
}

#[test]
fn schema_1_has_no_rule_conditions() {
    // Schema 1 keeps its one-pointer rules: a host reads `when` only in schema 2.
    let v1 = edit(
        &template_manifest(),
        "kind = \"tool_started\"\nid_pointer = \"/id\"",
        &format!("kind = \"tool_started\"\n{WHEN_ONE}\nid_pointer = \"/id\""),
    );
    assert_eq!(refused(&v1), "normalizer.rule");
}

// ------------------------------------------------- the real CLI's version and help

#[test]
fn the_probe_reads_a_version_that_has_the_program_name_in_front() {
    use aibus_supervisor::cli::{parse_version, Version};
    assert_eq!(
        parse_version("codex-cli 0.167.0\n"),
        Some(Version(0, 167, 0))
    );
    assert_eq!(
        parse_version("2.1.311 (Claude Code)"),
        Some(Version(2, 1, 311))
    );
    assert_eq!(parse_version("0.40.1\n"), Some(Version(0, 40, 1)));
    assert_eq!(parse_version("not a version"), None);
}

#[test]
fn negotiating_against_the_real_0_167_0_text_gives_the_documented_capability_vector() {
    let probe = cli::parse_help(&cli_text("version.txt"), &cli_text("help.txt")).unwrap();
    assert_eq!(probe.version, cli::Version(0, 167, 0));
    let n = negotiate(&manifest(), "macos", &probe).unwrap();
    for c in [
        Capability::LoginStatus,
        Capability::OneShot,
        Capability::StructuredEvents,
        Capability::ToolEvents,
        Capability::UsageReport,
        Capability::Cancel,
        Capability::Headless,
    ] {
        assert!(n.supports(c), "{c:?}");
    }
    // Missing capabilities stay explicit, each with its reason.
    for c in [
        Capability::NativeChildLineage,
        Capability::SessionResume,
        Capability::InteractiveApproval,
        Capability::WorkspaceControl,
        Capability::HookChannel,
    ] {
        assert!(
            matches!(&n.capabilities[&c], Verdict::Unsupported(why) if !why.is_empty()),
            "{c:?} should be unsupported with a reason"
        );
    }
}

#[test]
fn a_codex_older_than_the_tested_range_is_refused_by_name() {
    let probe = cli::parse_help("codex-cli 0.12.0\n", &cli_text("help.txt")).unwrap();
    let e = negotiate(&manifest(), "macos", &probe).unwrap_err();
    assert!(e.to_string().contains("0.12.0"), "{e}");
}

// ------------------------------------------------- the recorded streams

fn kinds(fixture: &str, ending: &str) -> Vec<EventKind> {
    let pkg = package::read(&codex_dir()).unwrap();
    observe(&pkg, &format!("fixtures/{fixture}.jsonl"), ending)
        .unwrap()
        .kinds
}

#[test]
fn a_plain_answer_is_a_message_and_the_reported_usage() {
    use EventKind::*;
    assert_eq!(
        kinds("basic", "exit:0"),
        [
            AttemptStarted,
            TraceWarning, // thread.started: a line with no trace meaning is said, never dropped
            TraceWarning, // turn.started
            MessageChunk,
            UsageReported,
            AttemptCompleted
        ]
    );
    let pkg = package::read(&codex_dir()).unwrap();
    let o = observe(&pkg, "fixtures/basic.jsonl", "exit:0").unwrap();
    let usage = o
        .events
        .iter()
        .find(|e| e.kind == UsageReported)
        .expect("usage");
    let metrics = usage.data["metrics"].as_array().unwrap();
    let get = |name: &str| {
        metrics
            .iter()
            .find(|m| m["name"] == name)
            .unwrap_or_else(|| panic!("{name} missing"))
    };
    assert_eq!(get("input_tokens")["value"], 1200.0);
    assert_eq!(get("input_tokens")["quality"], "reported");
    assert_eq!(get("output_tokens")["value"], 5.0);
    assert_eq!(usage.visibility, Visibility::Observed);
    assert!(usage
        .evidence_refs
        .iter()
        .any(|r| r.starts_with("raw:stdout:L")));
}

#[test]
fn a_shell_command_is_a_tool_that_started_and_completed_with_the_same_id() {
    use EventKind::*;
    let pkg = package::read(&codex_dir()).unwrap();
    let o = observe(&pkg, "fixtures/tool-run.jsonl", "exit:0").unwrap();
    let tool: Vec<_> = o
        .events
        .iter()
        .filter(|e| matches!(e.kind, ToolStarted | ToolCompleted))
        .collect();
    assert_eq!(tool.len(), 2);
    assert_eq!(tool[0].kind, ToolStarted);
    assert_eq!(tool[1].kind, ToolCompleted);
    assert_eq!(tool[0].native.tool_use_id.as_deref(), Some("item_0"));
    assert_eq!(tool[1].native.tool_use_id.as_deref(), Some("item_0"));
    assert_eq!(
        tool[1].native.native_type.as_deref(),
        Some("item.completed/command_execution")
    );
    // The tool closed, so nothing is left to reconcile.
    assert!(!o.kinds.contains(&GapDetected));
    assert_eq!(
        o.events.last().unwrap().data["requires_reconciliation"],
        false
    );
}

#[test]
fn a_command_that_failed_is_tool_failed_and_the_attempt_still_completes() {
    use EventKind::*;
    let pkg = package::read(&codex_dir()).unwrap();
    let o = observe(&pkg, "fixtures/tool-failed.jsonl", "exit:0").unwrap();
    assert!(o.kinds.contains(&ToolFailed));
    assert!(!o.kinds.contains(&ToolCompleted));
    assert_eq!(o.status, AttemptStatus::Completed);
    let failed = o.events.iter().find(|e| e.kind == ToolFailed).unwrap();
    assert_eq!(failed.native.tool_use_id.as_deref(), Some("item_1"));
    assert_eq!(
        failed.native.native_type.as_deref(),
        Some("item.completed/command_execution/failed")
    );
}

#[test]
fn a_rejected_model_ends_the_attempt_failed_and_its_text_stays_out_of_the_trace() {
    let pkg = package::read(&codex_dir()).unwrap();
    let o = observe(&pkg, "fixtures/turn-failed.jsonl", "exit:1").unwrap();
    assert_eq!(o.status, AttemptStatus::Failed);
    assert_eq!(*o.kinds.last().unwrap(), EventKind::AttemptFailed);
    let wire = serde_json::to_string(&o.events).unwrap();
    assert!(
        !wire.contains("not supported when using Codex"),
        "the CLI's own error text is not copied into an event"
    );
    // The error lines are said as warnings so the failure is visible in the trace.
    assert!(
        o.kinds
            .iter()
            .filter(|k| **k == EventKind::TraceWarning)
            .count()
            >= 2
    );
}

#[test]
fn every_fixture_meets_the_event_contract() {
    let pkg = package::read(&codex_dir()).unwrap();
    let report = aibus_extension::conformance::run(&pkg);
    assert!(report.passed(), "{:?}", report.checks);
    assert_eq!(pkg.manifest.fixtures.files.len(), 4);
}

// ------------------------------------------------- through the sandbox and the supervisor

fn stand_in(stream: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let bin = t.path().join("bin");
    let work = t.path().join("work");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&work).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(bin.join("version.txt"), cli_text("version.txt")).unwrap();
    fs::write(bin.join("help.txt"), cli_text("help.txt")).unwrap();
    fs::write(bin.join("stream.jsonl"), stream).unwrap();
    let script = format!(
        "#!/bin/sh\nD={}\ncase \"$1\" in --version) cat \"$D/version.txt\"; exit 0;; --help) cat \"$D/help.txt\"; exit 0;; esac\ncat > /dev/null\ncat \"$D/stream.jsonl\"\n",
        bin.display()
    );
    let exe = bin.join("codex");
    fs::write(&exe, script).unwrap();
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
    (t, bin, work)
}

#[tokio::test]
async fn the_extension_runs_a_recorded_codex_turn_through_the_sandbox_and_the_supervisor() {
    let stream = fs::read_to_string(codex_dir().join("fixtures/tool-run.jsonl")).unwrap();
    let (t, bin, work) = stand_in(&stream);
    let m = manifest();
    let grant = Grant {
        workdir: work,
        search_dirs: vec![bin],
        parent_env: vec![
            (OsString::from("HOME"), OsString::from("/home/x")),
            (OsString::from("OPENAI_API_KEY"), OsString::from("sk-never")),
            (
                OsString::from("CODEX_HOME"),
                OsString::from("/home/x/.codex"),
            ),
        ],
        ceiling: Limits::default(),
        extra_programs: Vec::new(),
        os: "macos".into(),
        typed: Default::default(),
    };
    let mut p = prepare(&m, &grant, &[]).await.unwrap();
    p.launch.stdin = b"say ok\n".to_vec();
    let args: Vec<String> = p
        .launch
        .args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(args.first().map(String::as_str), Some("exec"));
    assert!(args.iter().any(|a| a == "--json"));
    assert!(
        !args.iter().any(|a| a.contains("say ok")),
        "prompt is not argv"
    );
    let env_names: BTreeSet<String> = p
        .launch
        .env
        .iter()
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    assert!(env_names.contains("CODEX_HOME"), "its own sign-in location");
    assert!(
        !env_names.contains("OPENAI_API_KEY"),
        "no key reaches the CLI"
    );

    let dir = t.path().join("trace");
    let (mut store, _) = TraceStore::open(
        &dir,
        Redactor::new(),
        PayloadPolicy::MetadataOnly,
        StoreLimits::default(),
    )
    .unwrap();
    let attempt = AttemptRef {
        workflow_run_id: Uuid::from_u128(1),
        attempt_id: Uuid::from_u128(2),
        parent_attempt_id: None,
        provider_extension: m.id.clone(),
    };
    let mut eng = engine(&m, attempt);
    let out = supervise(p.launch, eng.as_mut(), &mut store, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(out.status, AttemptStatus::Completed);
    drop(store);
    let (events, _) = TraceStore::read(&dir).unwrap();
    assert!(events
        .iter()
        .all(|e| e.provider_extension == "org.devshell.aibus.codex-cli"));
    assert!(events.iter().any(|e| e.kind == EventKind::ToolCompleted));
    let usage = events
        .iter()
        .find(|e| e.kind == EventKind::UsageReported)
        .unwrap();
    assert!(usage.data["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .all(|m| m["quality"] != serde_json::to_value(DataQuality::Derived).unwrap()));
}

// ---------------------------------------------------------------- the optional live mode

/// Live mode (#357): one tiny turn with the codex that is installed and signed in on this machine.
/// Ignored by default; `AIBUS_CODEX_LIVE=1` and `AIBUS_CODEX_LIVE_OUT=<dir>` turn it on. It never
/// reads a credential (the CLI uses its own sign-in) and writes only what the trace says.
#[tokio::test]
#[ignore = "live: starts the real codex; set AIBUS_CODEX_LIVE=1 and run with --ignored"]
async fn live_codex_one_short_turn() {
    if std::env::var_os("AIBUS_CODEX_LIVE").is_none() {
        eprintln!("AIBUS_CODEX_LIVE is not set; live mode skipped");
        return;
    }
    let out_dir = PathBuf::from(std::env::var_os("AIBUS_CODEX_LIVE_OUT").expect("an output dir"));
    let work = out_dir.join("work");
    fs::create_dir_all(&work).unwrap();
    let m = manifest();
    let grant = Grant {
        workdir: work,
        search_dirs: std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect(),
        parent_env: std::env::vars_os().collect(),
        ceiling: Limits {
            wall_time: Duration::from_secs(120),
            ..Limits::default()
        },
        extra_programs: Vec::new(),
        os: if cfg!(target_os = "macos") {
            "macos"
        } else {
            "linux"
        }
        .into(),
        typed: Default::default(),
    };
    let mut p = prepare(&m, &grant, &[]).await.unwrap();
    let p_version = cli::capture(
        &p.launch.exe,
        &["--version"],
        &p.launch.env,
        Duration::from_secs(10),
        4096,
    )
    .await
    .map(|b| String::from_utf8_lossy(&b).trim().to_owned())
    .unwrap();
    p.launch.stdin = b"Reply with the single word: ok\n".to_vec();
    let trace_dir = out_dir.join("trace");
    let (mut store, _) = TraceStore::open(
        &trace_dir,
        Redactor::new(),
        PayloadPolicy::MetadataOnly,
        StoreLimits::default(),
    )
    .unwrap();
    let attempt = AttemptRef {
        workflow_run_id: Uuid::now_v7(),
        attempt_id: Uuid::now_v7(),
        parent_attempt_id: None,
        provider_extension: m.id.clone(),
    };
    let mut eng = engine(&m, attempt);
    let out = supervise(p.launch, eng.as_mut(), &mut store, CancellationToken::new())
        .await
        .unwrap();
    drop(store);
    let (events, _) = TraceStore::read(&trace_dir).unwrap();
    // What happened, with no message text and no path: kinds, evidence levels, the CLI's version.
    let count = |f: &dyn Fn(&aibus_trace::TraceEvent) -> String| {
        let mut m = std::collections::BTreeMap::<String, u64>::new();
        for e in &events {
            *m.entry(f(e)).or_default() += 1;
        }
        m
    };
    let report = serde_json::json!({
        "cli": "codex",
        "cli_version": p_version,
        "extension": m.id,
        "ending": format!("{:?}", out.ending),
        "attempt_status": out.status,
        "events": events.len(),
        "kinds": count(&|e| serde_json::to_value(e.kind).unwrap().as_str().unwrap().to_owned()),
        "evidence_levels": count(&|e| serde_json::to_value(e.visibility).unwrap().as_str().unwrap().to_owned()),
        "parsers": count(&|e| e.source.parser.clone()),
        "note": "One live turn, n=1, a one-word prompt. Not a benchmark.",
    });
    fs::write(
        out_dir.join("report.json"),
        serde_json::to_string_pretty(&report).unwrap() + "\n",
    )
    .unwrap();
    eprintln!("ending {:?}, {} events", out.ending, events.len());
    assert_eq!(out.status, AttemptStatus::Completed);
    assert!(events.iter().any(|e| e.kind == EventKind::MessageChunk));
    assert!(events.iter().any(|e| e.kind == EventKind::UsageReported));
}

/// A flag with no short form is indented six columns in Codex's help (`      --json`), and the
/// probe has to see it too, or a `probe_required` capability on it could never be offered.
#[test]
fn the_probe_sees_long_only_flags_in_the_real_help() {
    let probe = cli::parse_help(&cli_text("version.txt"), &cli_text("exec-help.txt")).unwrap();
    for f in [
        "--json",
        "--skip-git-repo-check",
        "--ephemeral",
        "--output-last-message",
        "--model",
    ] {
        assert!(probe.flags.contains(f), "{f}");
    }
}
