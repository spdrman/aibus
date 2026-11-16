//! The Gemini CLI extension (#358, A2.2b): the official `gemini` CLI as an extension on the A2.1
//! contract. No Gemini CLI was installed where this was written, so nothing here was recorded from
//! a run: the streams are built from the event types in `fixtures/gemini/types-2026-11-15.ts.txt`
//! (the CLI's own source, main branch) with synthetic values, and every claim about the real CLI is
//! unverified. The tests say what they check against that source and nothing more.
#![cfg(unix)]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use aibus_core::Redactor;
use aibus_extension::conformance::observe;
use aibus_extension::manifest::{Capability, Manifest, ValueKind};
use aibus_extension::negotiate::{negotiate, Verdict};
use aibus_extension::package;
use aibus_extension::sandbox::{engine, prepare, Grant};
use aibus_supervisor::{cli, supervise, Limits};
use aibus_trace::{
    AttemptRef, AttemptStatus, EventKind, PayloadPolicy, StoreLimits, TraceStore, Visibility,
};
use common::repo;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn gemini_dir() -> PathBuf {
    repo().join("examples/extensions/gemini")
}

fn manifest() -> Manifest {
    Manifest::parse(&fs::read_to_string(gemini_dir().join("aibus-extension.toml")).unwrap())
        .unwrap()
}

fn observed(fixture: &str, ending: &str) -> aibus_extension::conformance::Observed {
    let pkg = package::read(&gemini_dir()).unwrap();
    observe(&pkg, &format!("fixtures/{fixture}.jsonl"), ending).unwrap()
}

#[test]
fn the_manifest_is_a_schema_2_extension_for_the_official_gemini_cli() {
    let m = manifest();
    assert_eq!(m.schema_version, "2");
    assert_eq!(m.id, "org.devshell.aibus.gemini-cli");
    assert_eq!(m.provider, "Google");
    assert_eq!(m.cli.executable_candidates, ["gemini"]);
    assert!(m
        .execution
        .argv
        .windows(2)
        .any(|w| w == ["--output-format", "stream-json"]));
    let typed: Vec<_> = m
        .execution
        .typed
        .iter()
        .map(|t| (t.flag.as_str(), t.kind))
        .collect();
    assert_eq!(typed, [("--model", ValueKind::ModelId)]);
    // The CLI signs in on its own first run and has no documented login or status subcommand, so
    // none is claimed: the capability is declared unsupported and the auth argv is empty.
    assert!(m.auth.login_argv.is_empty() && m.auth.status_argv.is_empty());
}

#[test]
fn what_the_cli_cannot_be_asked_stays_explicit() {
    let m = manifest();
    let help =
        "  -m, --model <string>\n      --output-format <string>\n      --approval-mode <string>\n";
    let probe = cli::parse_help("0.1.0\n", help).unwrap();
    let n = negotiate(&m, "macos", &probe).unwrap();
    for c in [
        Capability::OneShot,
        Capability::StructuredEvents,
        Capability::ToolEvents,
        Capability::UsageReport,
        Capability::Headless,
        Capability::Cancel,
    ] {
        assert!(n.supports(c), "{c:?}");
    }
    for c in [
        Capability::LoginStatus,
        Capability::NativeChildLineage,
        Capability::SessionResume,
        Capability::InteractiveApproval,
        Capability::WorkspaceControl,
        Capability::HookChannel,
    ] {
        assert!(
            matches!(&n.capabilities[&c], Verdict::Unsupported(why) if !why.is_empty()),
            "{c:?}"
        );
    }
}

#[test]
fn an_assistant_message_and_the_reported_usage_are_mapped_and_the_prompt_echo_is_not() {
    use EventKind::*;
    let o = observed("basic", "exit:0");
    assert_eq!(
        o.kinds,
        [
            AttemptStarted,
            TraceWarning, // init
            TraceWarning, // the user's own message echoed back
            MessageChunk,
            UsageReported,
            AttemptCompleted
        ]
    );
    let usage = o.events.iter().find(|e| e.kind == UsageReported).unwrap();
    let metrics = usage.data["metrics"].as_array().unwrap();
    let get = |n: &str| metrics.iter().find(|m| m["name"] == n).unwrap();
    assert_eq!(get("input_tokens")["value"], 1200.0);
    assert_eq!(get("output_tokens")["value"], 5.0);
    assert_eq!(get("input_tokens")["quality"], "reported");
    assert_eq!(usage.visibility, Visibility::Observed);
    let msg = o.events.iter().find(|e| e.kind == MessageChunk).unwrap();
    assert_eq!(msg.native.native_type.as_deref(), Some("message/assistant"));
}

#[test]
fn a_tool_use_and_its_result_share_the_tool_id() {
    use EventKind::*;
    let o = observed("tool-run", "exit:0");
    let tools: Vec<_> = o
        .events
        .iter()
        .filter(|e| matches!(e.kind, ToolStarted | ToolCompleted))
        .collect();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].kind, ToolStarted);
    assert_eq!(tools[1].kind, ToolCompleted);
    assert!(tools
        .iter()
        .all(|e| e.native.tool_use_id.as_deref() == Some("tool-0001")));
    assert!(!o.kinds.contains(&GapDetected));
}

#[test]
fn a_tool_result_with_status_error_is_tool_failed() {
    let o = observed("tool-failed", "exit:0");
    assert!(o.kinds.contains(&EventKind::ToolFailed));
    assert!(!o.kinds.contains(&EventKind::ToolCompleted));
    let f = o
        .events
        .iter()
        .find(|e| e.kind == EventKind::ToolFailed)
        .unwrap();
    assert_eq!(f.native.native_type.as_deref(), Some("tool_result/error"));
}

#[test]
fn an_error_result_ends_the_attempt_failed_and_the_message_text_stays_out() {
    let o = observed("result-error", "exit:1");
    assert_eq!(o.status, AttemptStatus::Failed);
    assert!(!serde_json::to_string(&o.events)
        .unwrap()
        .contains("synthetic API failure"));
}

#[test]
fn every_fixture_meets_the_event_contract() {
    let pkg = package::read(&gemini_dir()).unwrap();
    let r = aibus_extension::conformance::run(&pkg);
    assert!(r.passed(), "{:?}", r.checks);
    assert_eq!(pkg.manifest.fixtures.files.len(), 4);
}

/// The package says plainly that nothing was recorded, and the live report says why.
#[test]
fn the_package_and_the_live_report_say_nothing_was_recorded() {
    let readme = fs::read_to_string(gemini_dir().join("README.md")).unwrap();
    assert!(readme.to_lowercase().contains("unverified"));
    assert!(readme.contains("no stream was recorded"));
    let report: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(repo().join("fixtures/gemini/live-report-2026-11-15.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(report["status"], "not_on_path");
}

#[tokio::test]
async fn the_extension_runs_a_documented_stream_through_the_sandbox_and_the_supervisor() {
    // The stand-in's `--version` and `--help` are synthetic, not Gemini's real text.
    let t = tempfile::tempdir().unwrap();
    let bin = t.path().join("bin");
    let work = t.path().join("work");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&work).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    fs::copy(
        gemini_dir().join("fixtures/tool-run.jsonl"),
        bin.join("stream.jsonl"),
    )
    .unwrap();
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in --version) echo 0.1.0; exit 0;; --help) printf '  -m, --model <string>\\n      --output-format <string>\\n'; exit 0;; esac\ncat > /dev/null\ncat {}/stream.jsonl\n",
        bin.display()
    );
    let exe = bin.join("gemini");
    fs::write(&exe, script).unwrap();
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
    let m = manifest();
    let grant = Grant {
        workdir: work,
        search_dirs: vec![bin],
        parent_env: vec![
            ("HOME".into(), "/home/x".into()),
            ("GEMINI_API_KEY".into(), "never".into()),
        ],
        ceiling: Limits::default(),
        extra_programs: Vec::new(),
        os: "macos".into(),
        typed: Default::default(),
    };
    let mut p = prepare(&m, &grant, &[]).await.unwrap();
    p.launch.stdin = b"read notes\n".to_vec();
    assert!(p.launch.env.iter().all(|(k, _)| k != "GEMINI_API_KEY"));
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
        .all(|e| e.provider_extension == "org.devshell.aibus.gemini-cli"));
    assert!(events.iter().any(|e| e.kind == EventKind::ToolCompleted));
}
