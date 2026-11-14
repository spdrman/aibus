//! #274 end to end: the real `aibus mcp` binary reaching a live session's control endpoint. The
//! endpoint is the one a terminal session opens (`aibus_control::Endpoint`), with a stand-in for
//! the terminal loop behind it that answers what reaches it, so the socket, the token, the
//! handshake and the choice of session are all the shipped ones.

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aibus_control::{private_dir, runtime_dir, Endpoint};
use aibus_core::remote::Remote;
use aibus_core::Redactor;
use aibus_testkit::xdg_temp;
use common::mcp_client::{mcp_command, McpClient, Protocol};
use serde_json::json;
use tokio::sync::mpsc;

type Seen = Arc<Mutex<Vec<String>>>;

fn run_dir(home: &Path) -> PathBuf {
    home.join("run")
}

/// A live session `id` in `home`'s runtime directory, answered by a stand-in loop.
fn session(rt: &tokio::runtime::Runtime, home: &Path, id: &str, seen: Seen) -> Endpoint {
    let dir = runtime_dir(Some(run_dir(home).as_os_str()), home);
    private_dir(&dir).unwrap();
    let info = json!({"id": id, "cwd": "~/app", "lead": "lead", "model": "sonnet", "started": "2026-11-14T10:00:00Z"});
    let mut ep = Endpoint::open(&dir, id, &info).unwrap();
    let (tx, mut rx) = mpsc::channel::<Remote>(16);
    rt.spawn(async move {
        while let Some(event) = rx.recv().await {
            let line = match event {
                Remote::Connected { client } => format!("connected {client}"),
                Remote::Disconnected { client } => format!("disconnected {client}"),
                Remote::Call { client, ask, reply } => {
                    let said = format!("{ask:?}");
                    let _ = reply.send(Ok(json!({"lead": {"agent": "lead"}, "ask": said})));
                    format!("call {client} {said}")
                }
            };
            seen.lock().unwrap().push(line);
        }
    });
    let _in = rt.enter();
    ep.serve(tx, Redactor::new()).unwrap();
    ep
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn client(home: &Path, p: Protocol, args: &[&str]) -> McpClient {
    let mut cmd = mcp_command(home, args);
    cmd.env("XDG_RUNTIME_DIR", run_dir(home));
    let mut c = McpClient::spawn(cmd, p, home);
    if p == Protocol::V2025_11_25 {
        c.initialize_legacy().expect("initialize");
    }
    c
}

fn tool_names(c: &mut McpClient) -> Vec<String> {
    c.list_tools()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect()
}

fn wait_for(seen: &Seen, what: &str) -> bool {
    for _ in 0..150 {
        if seen.lock().unwrap().iter().any(|l| l.contains(what)) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn aibus_mcp_drives_a_live_session_through_its_socket() {
    for p in Protocol::BOTH {
        let home = xdg_temp();
        let rt = runtime();
        let seen: Seen = Arc::default();
        let ep = session(&rt, home.path(), "01JCE2E0000000000000000000", seen.clone());
        let mut c = client(
            home.path(),
            p,
            &[
                "--allow",
                "session-status",
                "--allow",
                "session-send",
                "--confirm",
                "never",
            ],
        );
        let names = tool_names(&mut c);
        assert!(
            names.contains(&"session_status".into()) && names.contains(&"session_send".into()),
            "{names:?}"
        );
        assert!(!names.contains(&"session_command".into()), "{names:?}");

        let status = c.call("session_status", json!({})).expect("session_status");
        assert_ne!(status["isError"], true, "{p:?}: {status}");
        assert_eq!(
            status["structuredContent"]["lead"]["agent"], "lead",
            "{status}"
        );
        let sent = c
            .call("session_send", json!({"text": "fix the login bug"}))
            .expect("session_send");
        assert_ne!(sent["isError"], true, "{p:?}: {sent}");
        // Not listed, so not callable.
        let off = c
            .call("session_command", json!({"line": "/model haiku"}))
            .unwrap_err();
        assert_eq!(off.code, -32602);

        assert!(
            wait_for(&seen, "fix the login bug"),
            "{:?}",
            seen.lock().unwrap()
        );
        let (done, _) = c.finish();
        assert!(done.status.success(), "{p:?}: {}", done.stderr);
        assert!(
            wait_for(&seen, "disconnected"),
            "the session hears the client go: {:?}",
            seen.lock().unwrap()
        );
        let seen = seen.lock().unwrap().clone();
        assert!(seen[0].starts_with("connected "), "{seen:?}");
        assert_eq!(
            seen.iter().filter(|l| l.starts_with("connected")).count(),
            1,
            "{seen:?}"
        );
        drop(ep);
    }
}

#[test]
fn with_several_live_sessions_and_no_id_the_server_refuses_and_lists_them() {
    let home = xdg_temp();
    let rt = runtime();
    let seen: Seen = Arc::default();
    let _a = session(&rt, home.path(), "01JCTWO0000000000000000000", seen.clone());
    let _b = session(&rt, home.path(), "01JCTWO0000000000000000001", seen.clone());
    let mut c = client(
        home.path(),
        Protocol::V2026_07_28,
        &["--allow", "session-status", "--allow", "session-list"],
    );
    let listed = c.call("session_list", json!({})).expect("session_list");
    let ids: Vec<&str> = listed["structuredContent"]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["id"].as_str())
        .collect();
    assert_eq!(ids.len(), 2, "{listed}");
    let refused = c
        .call("session_status", json!({}))
        .expect("an isError, not a protocol error");
    assert_eq!(refused["isError"], true, "{refused}");
    let said = refused["content"][0]["text"].as_str().unwrap();
    assert!(
        said.contains("01JCTWO0000000000000000000") && said.contains("01JCTWO0000000000000000001"),
        "{said}"
    );
    c.finish();
    assert!(
        !seen
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("connected")),
        "nothing connected"
    );

    let mut c = client(
        home.path(),
        Protocol::V2026_07_28,
        &[
            "--allow",
            "session-status",
            "--session",
            "01JCTWO0000000000000000001",
        ],
    );
    let status = c.call("session_status", json!({})).expect("session_status");
    assert_ne!(status["isError"], true, "{status}");
    c.finish();
}

#[test]
fn the_session_tools_are_not_there_by_default_and_session_needs_one() {
    let home = xdg_temp();
    let mut c = client(home.path(), Protocol::V2026_07_28, &[]);
    let names = tool_names(&mut c);
    assert!(
        !names.iter().any(|n| n.starts_with("session_")),
        "{names:?}"
    );
    c.finish();
    let out = common::aibus(
        home.path(),
        &["mcp", "--session", "01JCX00000000000000000000A"],
    );
    assert_eq!(out.status.code(), Some(2), "{}", common::stderr(&out));
    let out = common::aibus(home.path(), &["mcp", "--allow", "session-everything"]);
    assert_eq!(out.status.code(), Some(2), "{}", common::stderr(&out));
}

#[test]
fn every_session_flag_is_on_the_help_text() {
    let home = xdg_temp();
    let help = String::from_utf8(common::aibus(home.path(), &["mcp", "--help"]).stdout).unwrap();
    for flag in [
        "session-list",
        "session-status",
        "session-send",
        "session-command",
        "session-cancel",
        "session-transcript",
        "permission",
        "--session",
    ] {
        assert!(
            help.contains(flag),
            "{flag} is not in `aibus mcp --help`:\n{help}"
        );
    }
    let top = String::from_utf8(common::aibus(home.path(), &["--help"]).stdout).unwrap();
    for flag in ["--no-control", "--control-permission"] {
        assert!(top.contains(flag), "{flag} is not in `aibus --help`");
    }
}
