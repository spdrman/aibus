//! #360 live mode: one short turn on Codex, a preview and a recorded answer, and, only if the
//! answer is yes, one short turn on Claude Code that gets Codex's reply. Both are the official CLIs
//! the user is signed in to on this machine; aibus reads no credential. Ignored by default.
//! `AIBUS_HANDOFF_LIVE=1`, `AIBUS_HANDOFF_OUT=<dir>`, and `AIBUS_HANDOFF_CONSENT=granted` (anything
//! else is a decline). The consent here is the test operator's environment variable standing in for
//! the person the TUI would ask, and the record says so; it is not a user interface.
#![cfg(unix)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use aibus_core::workflow::compile::content_hash;
use aibus_core::workflow::handoff::{check_delivery, preview, record, Decision, Item, Transfer};
use aibus_core::Redactor;
use aibus_extension::manifest::Manifest;
use aibus_extension::sandbox::{engine, prepare, Grant, TypedValue};
use aibus_supervisor::{supervise, Limits};
use aibus_trace::{AttemptRef, AttemptStatus, PayloadPolicy, StoreLimits, TraceEvent, TraceStore};
use common::{reference_dir, repo};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn grant(work: &Path, typed: BTreeMap<String, TypedValue>) -> Grant {
    Grant {
        workdir: work.to_path_buf(),
        search_dirs: std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect(),
        parent_env: std::env::vars_os().collect(),
        ceiling: Limits {
            wall_time: Duration::from_secs(150),
            ..Limits::default()
        },
        extra_programs: Vec::new(),
        os: if cfg!(target_os = "macos") {
            "macos"
        } else {
            "linux"
        }
        .into(),
        typed,
    }
}

/// One attempt of `manifest_dir`'s extension: its trace events and how it ended.
async fn run(
    manifest_dir: &Path,
    work: &Path,
    typed: BTreeMap<String, TypedValue>,
    prompt: &str,
    trace_dir: &Path,
) -> (String, AttemptStatus, Vec<TraceEvent>) {
    let m =
        Manifest::parse(&fs::read_to_string(manifest_dir.join("aibus-extension.toml")).unwrap())
            .unwrap();
    let mut p = prepare(&m, &grant(work, typed), &[]).await.unwrap();
    p.launch.stdin = prompt.as_bytes().to_vec();
    let (mut store, _) = TraceStore::open(
        trace_dir,
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
    (m.id, out.status, TraceStore::read(trace_dir).unwrap().0)
}

#[tokio::test]
#[ignore = "live: starts the real codex and claude; set AIBUS_HANDOFF_LIVE=1 and run with --ignored"]
async fn live_codex_to_claude_with_a_recorded_consent() {
    if std::env::var_os("AIBUS_HANDOFF_LIVE").is_none() {
        eprintln!("AIBUS_HANDOFF_LIVE is not set; live mode skipped");
        return;
    }
    let out_dir = PathBuf::from(std::env::var_os("AIBUS_HANDOFF_OUT").expect("an output dir"));
    let (w1, w2) = (out_dir.join("work-codex"), out_dir.join("work-claude"));
    fs::create_dir_all(&w1).unwrap();
    fs::create_dir_all(&w2).unwrap();

    // Step one, on Codex. Its last message goes to a file the host names, so it never has to be
    // read out of a trace.
    let typed = BTreeMap::from([(
        "--output-last-message".to_owned(),
        TypedValue::Path("last.txt".into()),
    )]);
    let (id1, status1, events1) = run(
        &repo().join("examples/extensions/codex"),
        &w1,
        typed,
        "Reply with one short sentence about the number 7.\n",
        &out_dir.join("trace-codex"),
    )
    .await;
    assert_eq!(status1, AttemptStatus::Completed);
    let text = fs::read_to_string(w1.join("last.txt")).unwrap();

    // The handoff: what would cross, and the answer.
    let transfer = Transfer {
        from_step: "draft".into(),
        from_runtime: "cli.codex".into(),
        to_step: "review".into(),
        to_runtime: "cli.claude".into(),
        items: vec![Item {
            name: "prev.summary".into(),
            kind: "step_output".into(),
            sha256: content_hash(&text),
            bytes: text.len() as u64,
        }],
    };
    eprintln!("{}", preview(&transfer)["text"]);
    let granted = std::env::var("AIBUS_HANDOFF_CONSENT").is_ok_and(|v| v == "granted");
    let by = "test operator, AIBUS_HANDOFF_CONSENT in the environment";
    let decision = if granted {
        Decision::Granted { by }
    } else {
        Decision::Declined { by }
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let artifact = record(transfer.clone(), decision, &Uuid::now_v7().to_string(), now).unwrap();

    // Step two, on Claude Code, only with the yes.
    let mut second = Value::Null;
    if check_delivery(&artifact, &transfer.items).is_ok() {
        let typed = BTreeMap::from([("--model".to_owned(), TypedValue::Model("haiku".into()))]);
        let prompt = format!("React to this sentence in five words or fewer: {text}\n");
        let (id2, status2, events2) = run(
            &reference_dir(),
            &w2,
            typed,
            &prompt,
            &out_dir.join("trace-claude"),
        )
        .await;
        assert_eq!(status2, AttemptStatus::Completed);
        assert_ne!(id1, id2, "two distinct providers");
        second = json!({ "extension": id2, "status": status2, "events": events2.len() });
    }
    let report = json!({
        "handoff": artifact,
        "first": { "extension": id1, "status": status1, "events": events1.len() },
        "second": second,
        "note": "One live pair, n=1. The consent is a test operator's environment variable, not a person answering a prompt. No text is recorded, only hashes and sizes.",
    });
    fs::write(
        out_dir.join("handoff.json"),
        serde_json::to_string_pretty(&report).unwrap() + "\n",
    )
    .unwrap();
}
