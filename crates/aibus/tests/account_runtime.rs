//! Issue #181: `account add` and `account test` through the real binary against a loopback
//! provider, so the runtime the production code builds for the network call is the one under
//! test. The unit tests build their own runtimes with `enable_all()` and cannot see a missing
//! `enable_io()`.
//!
//! The key comes from a file: `--key-stdin` always stores into the OS keychain, which a test must
//! not touch. `verify` is the same function on every path, and that is where the runtime is made.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use aibus_testkit::conformance::{FakeServer, Fixture};
use aibus_testkit::xdg_temp;
use common::{command, stderr};

const MODELS: &str = r#"{"object":"list","data":[{"id":"fake-model-1","object":"model"}]}"#;

/// A fake provider on its own thread (and runtime), so the binary under test can be run with the
/// blocking `Command` here. `answers` models replies are queued up front.
fn provider(answers: usize) -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let server = FakeServer::start().await;
            for _ in 0..answers {
                server.script(Fixture::http(
                    200,
                    &[("content-type", "application/json")],
                    MODELS,
                ));
            }
            tx.send(server.uri()).unwrap();
            std::future::pending::<()>().await;
        });
    });
    rx.recv().unwrap()
}

#[test]
fn account_add_and_test_reach_the_provider_without_a_tokio_panic() {
    let uri = provider(8);
    let home = xdg_temp();
    let h = home.path();
    let keyfile = h.join("smoke.key");
    fs::write(&keyfile, "sk-fake-0123456789abcdef\n").unwrap();
    fs::set_permissions(&keyfile, fs::Permissions::from_mode(0o600)).unwrap();

    let add = command(h)
        .args([
            "account",
            "add",
            "smoke",
            "--provider",
            "openai-compatible",
            "--base-url",
        ])
        .arg(format!("{uri}/v1"))
        .arg("--file")
        .arg(&keyfile)
        .output()
        .unwrap();
    assert!(!stderr(&add).contains("IO is disabled"), "{}", stderr(&add));
    assert_eq!(add.status.code(), Some(0), "{}", stderr(&add));

    let test = command(h)
        .args(["account", "test", "smoke", "--json"])
        .output()
        .unwrap();
    assert!(
        !stderr(&test).contains("IO is disabled"),
        "{}",
        stderr(&test)
    );
    assert_eq!(test.status.code(), Some(0), "{}", stderr(&test));
    let report: serde_json::Value = serde_json::from_slice(&test.stdout).unwrap();
    assert_eq!(report["ok"], true, "{report}");
    assert_eq!(report["models"][0], "fake-model-1");
}
