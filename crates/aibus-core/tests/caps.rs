//! 08 section 3: the limits that are filled rather than read off a constant. The per-run
//! channels (1024 chunks, 64 mails) are in `run_actor.rs` next to the run they belong to, and
//! the hook capture (1 MiB, 3 MiB of output) in `hooks.rs`. Here: the loop's channel and the
//! `bash` tool's default cap.

use std::path::Path;
use std::sync::Arc;

use aibus_core::tools::Bash;
use aibus_core::*;
use aibus_graph::mini::MiniGraph;
use serde_json::json;
use aibus_testkit::TempDir;
use tokio::sync::mpsc::error::TrySendError;
use tokio_util::sync::CancellationToken;

#[test]
fn the_loop_channel_takes_4096_messages_and_then_says_full() {
    let app = App::new(MiniGraph::new(), || 0);
    let tx = app.sender();
    for n in 0..4096 {
        tx.try_send(Msg::Tick).unwrap_or_else(|e| panic!("message {n} should fit: {e}"));
    }
    assert!(matches!(tx.try_send(Msg::Tick), Err(TrySendError::Full(_))));
}

/// `tools.bash_max_output_bytes` is 256 KiB by default; 588,894 bytes of `seq` output is cut to
/// its tail behind one marker.
#[tokio::test]
async fn bash_output_over_256_kib_keeps_its_tail_with_a_head_marker() {
    let dir = TempDir::new().unwrap();
    let env = ["PATH", "HOME", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| ((*k).to_owned(), v)))
        .collect();
    let bash = Arc::new(Bash::new(60, Vec::new(), env, dir.path().to_path_buf(), "sess-1".into()));
    let ex = ToolExecutor::new(ToolRegistry::standard(bash));
    let eff = EffectiveTools { allow: vec!["bash".parse().unwrap()], deny: vec![] };
    let cx = ToolCx {
        run: RunId::new(),
        cwd: Path::new(dir.path()).to_path_buf(),
        cancel: CancellationToken::new(),
        limits: ToolLimits::default(),
        tx: tokio::sync::mpsc::channel(1).0,
        memory: None,
    };
    assert_eq!(cx.limits.bash_max_output_bytes, 256 * 1024);

    let out = ex
        .call_gated(&eff, &cx, PermissionMode::Bypass, "bash", json!({"command": "seq 1 100000"}), |_| async {
            Decision::Deny
        })
        .await
        .unwrap();
    assert!(out.truncated);
    assert!(out.content.len() <= 256 * 1024, "{}", out.content.len());
    assert!(out.content.len() > 200 * 1024, "the cap is filled, not just hit: {}", out.content.len());
    assert!(out.content.starts_with("[... "));
    assert!(out.content.trim_end().ends_with("100000"));
    assert_eq!(out.content.matches("bytes truncated").count(), 1);
}
