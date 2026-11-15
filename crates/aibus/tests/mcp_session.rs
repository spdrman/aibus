//! #274 end to end: the real `aibus mcp` binary reaching a live session's control endpoint. The
//! endpoint is the one a terminal session opens (`aibus_control::Endpoint`), with a stand-in for
//! the terminal loop behind it that answers what reaches it, so the socket, the token, the
//! handshake and the choice of session are all the shipped ones.

mod common;

use std::path::Path;
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

/// A short `XDG_RUNTIME_DIR`: the throwaway home's path is too long for a socket on macOS (#320).
fn short_run() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ab")
        .tempdir_in("/tmp")
        .unwrap()
}

/// A live session `id` in `home`'s runtime directory, answered by a stand-in loop.
fn session(rt: &Rt, run: &Path, id: &str, seen: Seen) -> Endpoint {
    let dir = runtime_dir(Some(run.as_os_str()), run, run);
    private_dir(&dir).unwrap();
    let info = json!({"id": id, "cwd": "~/app", "lead": "lead", "model": "sonnet", "started": "2026-11-14T10:00:00Z"});
    let mut ep = Endpoint::open(&dir, id, &info).unwrap();
    let (tx, mut rx) = mpsc::channel::<Remote>(16);
    rt.handle.spawn(async move {
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
    let _in = rt.handle.enter();
    ep.serve(tx, Redactor::new()).unwrap();
    ep
}

/// A current-thread runtime driven on a thread of its own until the returned guard drops; the
/// test thread meanwhile blocks on the `aibus mcp` child.
struct Rt {
    handle: tokio::runtime::Handle,
    _stop: tokio::sync::oneshot::Sender<()>,
}

fn runtime() -> Rt {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let handle = rt.handle().clone();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    std::thread::spawn(move || {
        rt.block_on(async {
            let _ = stopped.await;
        })
    });
    Rt {
        handle,
        _stop: stop,
    }
}

fn client(home: &Path, run: &Path, p: Protocol, args: &[&str]) -> McpClient {
    let mut cmd = mcp_command(home, args);
    cmd.env("XDG_RUNTIME_DIR", run);
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
        let run = short_run();
        let rt = runtime();
        let seen: Seen = Arc::default();
        let ep = session(&rt, run.path(), "01JCE2E0000000000000000000", seen.clone());
        let mut c = client(
            home.path(),
            run.path(),
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
    let run = short_run();
    let rt = runtime();
    let seen: Seen = Arc::default();
    let _a = session(&rt, run.path(), "01JCTWO0000000000000000000", seen.clone());
    let _b = session(&rt, run.path(), "01JCTWO0000000000000000001", seen.clone());
    let mut c = client(
        home.path(),
        run.path(),
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
        run.path(),
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
    let run = short_run();
    let mut c = client(home.path(), run.path(), Protocol::V2026_07_28, &[]);
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

/// Review of #276: the four tools that change a live session go through `--confirm elicit` on
/// the real call path, and reading does not; nothing reaches the session before the answer.
#[test]
fn a_client_that_can_be_asked_confirms_a_change_by_retyping_the_tail_of_the_id() {
    let home = xdg_temp();
    let run = short_run();
    let rt = runtime();
    let seen: Seen = Arc::default();
    let id = "01JCASK0000000000000000XYZ";
    let _ep = session(&rt, run.path(), id, seen.clone());
    let mut cmd = mcp_command(
        home.path(),
        &["--allow", "session-send", "--allow", "session-status"],
    );
    cmd.env("XDG_RUNTIME_DIR", run.path());
    let mut c = McpClient::spawn(cmd, Protocol::V2026_07_28, home.path());
    c.capabilities = json!({ "elicitation": { "form": {} } });
    let read = c.call("session_status", json!({})).unwrap();
    assert_ne!(read["resultType"], "input_required", "{read}");
    let first = c.call("session_send", json!({"text": "fix it"})).unwrap();
    assert_eq!(first["resultType"], "input_required", "{first}");
    assert!(first.to_string().contains(&id[id.len() - 8..]), "{first}");
    assert!(
        !seen.lock().unwrap().iter().any(|l| l.contains("fix it")),
        "sent before the answer"
    );
    let state = first["requestState"].as_str().unwrap().to_owned();
    let done = c
        .request(
            "tools/call",
            json!({
                "name": "session_send", "arguments": {"text": "fix it"}, "requestState": state,
                "inputResponses": { "confirm": { "action": "accept", "content": { "name": &id[id.len() - 8..] } } },
            }),
        )
        .unwrap();
    assert_ne!(done["isError"], true, "{done}");
    assert!(wait_for(&seen, "fix it"), "{:?}", seen.lock().unwrap());
    c.finish();
}

/// #320: no `XDG_RUNTIME_DIR` and a macOS-length `TMPDIR`: the session and `aibus mcp` both end
/// up in the short fallback directory, so `session_list` sees the session.
#[test]
fn issue_320_aibus_mcp_finds_a_session_with_a_macos_length_tmpdir_and_no_xdg() {
    let home = xdg_temp();
    let tmpdir = "/var/folders/zz/0123456789abcdefghijklmnopqrstuvwxyz0/T";
    assert!(tmpdir.len() >= 49);
    let rt = runtime();
    let id = "01JCT320000000000000000000";
    let dir = runtime_dir(None, Path::new(tmpdir), Path::new("/tmp"));
    private_dir(&dir).unwrap();
    let info = json!({"id": id, "cwd": "~/app", "lead": "lead", "model": "sonnet", "started": "2026-11-14T10:00:00Z"});
    let ep = Endpoint::open(&dir, id, &info).unwrap();
    let (tx, _rx) = mpsc::channel::<Remote>(16);
    {
        let _in = rt.handle.enter();
        let mut ep = ep;
        ep.serve(tx, Redactor::new()).unwrap();
        let mut cmd = mcp_command(home.path(), &["--allow", "session-list"]);
        cmd.env_remove("XDG_RUNTIME_DIR").env("TMPDIR", tmpdir);
        let mut c = McpClient::spawn(cmd, Protocol::V2026_07_28, home.path());
        let listed = c.call("session_list", json!({})).expect("session_list");
        let ids: Vec<&str> = listed["structuredContent"]["sessions"]
            .as_array()
            .expect("sessions")
            .iter()
            .filter_map(|s| s["id"].as_str())
            .collect();
        assert!(ids.contains(&id), "{ids:?} in {}", dir.display());
        c.finish();
    }
}
