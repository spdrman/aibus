//! The Kimi Code, GitHub Copilot CLI, Cursor Agent and Qwen Code extensions (#291, A3.1). None of
//! the four CLIs was installed or run where this was written, so every stream is synthetic and
//! every claim about a real CLI is unverified. The tests check the manifests and the fixtures
//! against the extension contract and nothing more.
#![cfg(unix)]

mod common;

use std::fs;

use aibus_extension::conformance::{observe, run};
use aibus_extension::manifest::{Capability, Manifest};
use aibus_extension::negotiate::{negotiate, Verdict};
use aibus_extension::package;
use aibus_supervisor::cli;
use aibus_trace::{AttemptStatus, EventKind};
use common::repo;

/// (directory, id, executable)
const CASES: [(&str, &str, &str); 4] = [
    ("kimi", "org.devshell.aibus.kimi-cli", "kimi"),
    ("copilot", "org.devshell.aibus.copilot-cli", "copilot"),
    ("cursor", "org.devshell.aibus.cursor-cli", "cursor-agent"),
    ("qwen", "org.devshell.aibus.qwen-cli", "qwen"),
];

fn dir(d: &str) -> std::path::PathBuf {
    repo().join("examples/extensions").join(d)
}

#[test]
fn each_manifest_launches_only_the_official_cli_and_never_touches_credentials() {
    for (d, id, exe) in CASES {
        let m = Manifest::parse(&fs::read_to_string(dir(d).join("aibus-extension.toml")).unwrap())
            .unwrap_or_else(|e| panic!("{d}: {e:?}"));
        assert_eq!(m.schema_version, "2", "{d}");
        assert_eq!(m.id, id);
        assert_eq!(m.cli.executable_candidates, [exe], "{d}");
        assert!(m.permissions.spawn_only_declared_cli, "{d}");
        assert!(!m.permissions.read_provider_credentials, "{d}");
        assert!(!m.permissions.external_network_from_parser, "{d}");
        assert!(m.execution.env_allow.is_empty(), "{d}");
    }
}

#[test]
fn each_extension_passes_the_fixture_conformance_run() {
    for (d, _, _) in CASES {
        let pkg = package::read(&dir(d)).unwrap();
        let r = run(&pkg);
        assert!(r.passed(), "{d}: {:?}", r.checks);
        assert_eq!(pkg.manifest.fixtures.files.len(), 3, "{d}");
    }
}

#[test]
fn a_tool_use_and_its_result_share_the_tool_id() {
    for (d, _, _) in CASES {
        let pkg = package::read(&dir(d)).unwrap();
        let o = observe(&pkg, "fixtures/tool-run.jsonl", "exit:0").unwrap();
        let tools: Vec<_> = o
            .events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::ToolStarted | EventKind::ToolCompleted))
            .collect();
        assert_eq!(tools.len(), 2, "{d}");
        assert!(
            tools
                .iter()
                .all(|e| e.native.tool_use_id.as_deref() == Some("tool-0001")),
            "{d}"
        );
        assert!(!o.kinds.contains(&EventKind::GapDetected), "{d}");
    }
}

#[test]
fn a_crash_mid_stream_ends_the_attempt_failed() {
    for (d, _, _) in CASES {
        let pkg = package::read(&dir(d)).unwrap();
        let o = observe(&pkg, "fixtures/crash.jsonl", "exit:1").unwrap();
        assert_eq!(o.status, AttemptStatus::Failed, "{d}");
    }
}

#[test]
fn a_cancelled_run_is_not_reported_as_completed() {
    for (d, _, _) in CASES {
        let pkg = package::read(&dir(d)).unwrap();
        let o = observe(&pkg, "fixtures/basic.jsonl", "cancelled").unwrap();
        assert_eq!(o.status, AttemptStatus::Cancelled, "{d}");
    }
}

#[test]
fn what_the_cli_cannot_be_asked_stays_explicit() {
    for (d, _, _) in CASES {
        let m = Manifest::parse(&fs::read_to_string(dir(d).join("aibus-extension.toml")).unwrap())
            .unwrap();
        let probe = cli::parse_help("0.1.0\n", "      --output-format <string>\n").unwrap();
        let n = negotiate(&m, "macos", &probe).unwrap();
        for c in [
            Capability::OneShot,
            Capability::ToolEvents,
            Capability::Cancel,
        ] {
            assert!(n.supports(c), "{d} {c:?}");
        }
        for c in [
            Capability::NativeChildLineage,
            Capability::SessionResume,
            Capability::InteractiveApproval,
            Capability::WorkspaceControl,
            Capability::HookChannel,
        ] {
            assert!(
                matches!(&n.capabilities[&c], Verdict::Unsupported(why) if !why.is_empty()),
                "{d} {c:?}"
            );
        }
    }
}

#[test]
fn the_readmes_and_the_scorecard_say_nothing_was_recorded() {
    let card = fs::read_to_string(repo().join("docs/EXTENSION-SCORECARD.md")).unwrap();
    for (d, id, _) in CASES {
        let readme = fs::read_to_string(dir(d).join("README.md")).unwrap();
        assert!(readme.contains("Unverified"), "{d}");
        assert!(readme.contains("no stream was recorded"), "{d}");
        assert!(card.contains(id), "{d} is missing from the scorecard");
    }
    assert!(card.contains("unverified"));
}

#[test]
fn the_feasibility_register_has_a_dated_yes_or_no_for_xai_and_deepseek() {
    let reg = fs::read_to_string(repo().join("docs/PROVIDER-FEASIBILITY.md")).unwrap();
    for name in ["xAI/Grok", "DeepSeek"] {
        let row = reg
            .lines()
            .find(|l| l.starts_with('|') && l.contains(name))
            .unwrap_or_else(|| panic!("no row for {name}"));
        assert!(row.contains("2026-11-25"), "{row}");
        assert!(row.contains("| yes |"), "{row}");
        assert!(row.contains("`disabled`"), "{row}");
    }
    assert!(reg.contains("--profile headless"));
}
