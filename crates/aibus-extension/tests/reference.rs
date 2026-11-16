//! The reference extension (#286): the official Claude Code CLI as an extension built on the
//! supervisor, proven end to end offline against the spike's recorded streams. Nothing here starts
//! the real CLI: the stand-in `claude` answers `--version` and `--help` with the real 2.1.311 text
//! and then replays a recorded stream.
//!
//! `AIBUS_BLESS=1 cargo test -p aibus-extension --test reference bless` rewrites the `.expected.json`
//! files of the template and the reference extension from what the engines produce now.
#![cfg(unix)]

mod common;

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use aibus_core::Redactor;
use aibus_extension::manifest::{Capability, Manifest};
use aibus_extension::negotiate::{negotiate, Verdict};
use aibus_extension::package;
use aibus_extension::sandbox::{engine, prepare, Grant};
use aibus_supervisor::cli::{self, Request};
use aibus_supervisor::{supervise, Ending, Limits};
use aibus_trace::{
    project, AttemptRef, AttemptStatus, EventKind, PayloadPolicy, StoreLimits, TraceStore,
    Visibility,
};
use common::{reference_dir, repo, template_dir};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn spike(name: &str) -> String {
    fs::read_to_string(repo().join("fixtures/claude-code/spike-281").join(name)).unwrap()
}

fn manifest() -> Manifest {
    Manifest::parse(&fs::read_to_string(reference_dir().join("aibus-extension.toml")).unwrap())
        .unwrap()
}

#[test]
fn the_manifest_argv_is_what_the_supervisor_builds_for_a_default_request() {
    let probe = cli::probe(&spike("01-version.txt"), &spike("02-help.txt")).unwrap();
    let built = cli::argv(
        &probe,
        &Request {
            model: None,
            setting_sources: "user".into(),
            max_budget_usd: None,
            include_hook_events: false,
            forward_subagent_text: false,
            agents_json: None,
        },
    )
    .unwrap();
    let from_manifest: Vec<OsString> = manifest()
        .execution
        .argv
        .iter()
        .map(OsString::from)
        .collect();
    assert_eq!(built, from_manifest);
}

#[test]
fn negotiating_against_the_real_help_text_gives_the_documented_capability_vector() {
    let probe = cli::probe(&spike("01-version.txt"), &spike("02-help.txt")).unwrap();
    let n = negotiate(&manifest(), "macos", &probe).unwrap();
    for c in [
        Capability::StructuredEvents,
        Capability::NativeChildLineage,
        Capability::ToolEvents,
        Capability::UsageReport,
        Capability::Cancel,
        Capability::Headless,
    ] {
        assert!(n.supports(c), "{c:?}");
    }
    for c in [
        Capability::SessionResume,
        Capability::InteractiveApproval,
        Capability::WorkspaceControl,
    ] {
        assert!(!n.supports(c), "{c:?}");
    }
    // Amendment E4: hook delivery is unsupported, with the reason.
    assert!(matches!(
        n.capabilities[&Capability::HookChannel],
        Verdict::Unsupported(_)
    ));
}

/// What the extension's engine produces for a recorded stream is what the supervisor's own
/// normaliser produces: the SDK carries the knowledge instead of replacing it.
#[test]
fn the_reference_engine_is_the_supervisors_claude_normaliser() {
    use aibus_supervisor::{Engine, Normalizer};
    use std::time::SystemTime;
    let m = manifest();
    let attempt = AttemptRef {
        workflow_run_id: Uuid::from_u128(1),
        attempt_id: Uuid::from_u128(2),
        parent_attempt_id: None,
        provider_extension: m.id.clone(),
    };
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let lines: Vec<String> = spike("11-foreground.stdout.jsonl")
        .lines()
        .map(str::to_owned)
        .collect();
    let run = |mut e: Box<dyn Engine>| {
        let mut out = e.start(at, serde_json::json!({})).events;
        for l in &lines {
            out.extend(e.stream_line(l.as_bytes(), at).events);
        }
        out.extend(
            e.finish(Ending::Exited(0), Duration::from_secs(1), at)
                .events,
        );
        out.iter()
            .map(|e| (e.kind, e.visibility, e.native.tool_use_id.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        run(engine(&m, attempt.clone())),
        run(Box::new(Normalizer::new(attempt)))
    );
}

fn stand_in(stream: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let bin = t.path().join("bin");
    let work = t.path().join("work");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&work).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(bin.join("version.txt"), spike("01-version.txt")).unwrap();
    fs::write(bin.join("help.txt"), spike("02-help.txt")).unwrap();
    fs::write(bin.join("stream.jsonl"), stream).unwrap();
    let script = format!(
        "#!/bin/sh\nD={}\ncase \"$1\" in --version) cat \"$D/version.txt\"; exit 0;; --help) cat \"$D/help.txt\"; exit 0;; esac\ncat > /dev/null\ncat \"$D/stream.jsonl\"\n",
        bin.display()
    );
    let exe = bin.join("claude");
    fs::write(&exe, script).unwrap();
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
    (t, bin, work)
}

#[tokio::test]
async fn the_reference_extension_runs_a_recorded_delegation_through_the_sandbox_and_the_supervisor()
{
    let (t, bin, work) = stand_in(&spike("11-foreground.stdout.jsonl"));
    let m = manifest();
    let grant = Grant {
        workdir: work,
        search_dirs: vec![bin],
        parent_env: vec![
            (OsString::from("HOME"), OsString::from("/home/x")),
            (
                OsString::from("ANTHROPIC_API_KEY"),
                OsString::from("sk-never"),
            ),
        ],
        ceiling: Limits::default(),
        extra_programs: Vec::new(),
        os: "macos".into(),
        typed: Default::default(),
    };
    let mut p = prepare(&m, &grant, &[]).await.unwrap();
    p.launch.stdin = b"review the repo\n".to_vec();
    assert!(p.launch.args.iter().all(|a| a != "{prompt}"));
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
        .all(|e| e.provider_extension == "org.devshell.aibus.reference-cli"));
    let delegations = events
        .iter()
        .filter(|e| e.kind == EventKind::TaskDelegated)
        .count();
    assert_eq!(delegations, 2, "the lead delegated to two native subagents");
    assert!(events
        .iter()
        .filter(|e| e.kind == EventKind::TaskDelegated)
        .all(|e| e.visibility == Visibility::Observed));
    // Every metric carries a data quality, and the cost is an estimate, never a bill.
    let p = project(&events);
    assert!(p.agents.len() >= 3, "lead and two children");
}

/// `AIBUS_BLESS=1` rewrites the expected files; otherwise this only checks they exist.
#[test]
fn bless() {
    let bless = std::env::var_os("AIBUS_BLESS").is_some();
    for dir in [
        template_dir(),
        reference_dir(),
        repo().join("examples/extensions/codex"),
    ] {
        let pkg = package::read(&dir).unwrap();
        for f in &pkg.manifest.fixtures.files {
            let expected = dir.join(f.replace(".jsonl", ".expected.json"));
            if bless {
                let obs = aibus_extension::conformance::observe(&pkg, f, "exit:0").unwrap();
                fs::write(&expected, expected_json(&obs)).unwrap();
            }
            assert!(
                expected.is_file(),
                "{} is missing; run with AIBUS_BLESS=1",
                expected.display()
            );
        }
    }
}

/// The text of an `.expected.json` for what was observed (`fixture-expected-v1.json`).
fn expected_json(o: &aibus_extension::conformance::Observed) -> String {
    let kinds: Vec<serde_json::Value> = o
        .kinds
        .iter()
        .map(|k| serde_json::to_value(k).unwrap())
        .collect();
    let mut text = serde_json::to_string_pretty(&serde_json::json!({
        "ending": o.ending,
        "status": serde_json::to_value(o.status).unwrap(),
        "kinds": kinds,
    }))
    .unwrap();
    text.push('\n');
    text
}

/// #334: the reference extension is regenerated as schema 2 with the typed `--model` and `--agents`
/// the workflow compiler and the scheduler need, and nothing else about its argv changed.
#[test]
fn issue_334_the_reference_extension_is_schema_2_with_a_typed_model_and_agents() {
    use aibus_extension::manifest::ValueKind;
    let m = manifest();
    assert_eq!(m.schema_version, "2");
    assert_eq!(m.compat.protocol, ">=1.1.0, <2.0.0");
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
            ("--agents", ValueKind::AgentList)
        ]
    );
    assert!(!m
        .execution
        .argv
        .iter()
        .any(|a| a == "--model" || a == "--agents"));
}
