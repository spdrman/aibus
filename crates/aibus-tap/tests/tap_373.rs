//! #373 (phase 2 of #348): send guidance to a tapped Claude Code session over the inbox socket
//! the vendor documents. The line format for a sender that is not a Claude session is not
//! documented, so these tests pin what aibus controls (who may send, how often, what is
//! audited, what is refused), not what the CLI does with the line.

use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use aibus_core::Redactor;
use aibus_tap::agents::Session;
use aibus_tap::control::{self, Inbound};
use aibus_tap::Env;
use serde_json::Value;

const S: &str = "22222222-2222-4222-8222-222222222222";

fn write(p: &Path, s: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, s).unwrap();
}

fn env(tmp: &Path) -> Env {
    Env {
        state: tmp.join("state"),
        config: tmp.join("cfg"),
    }
}

fn session(sock: Option<PathBuf>) -> Session {
    Session {
        session_id: S.into(),
        job_id: None,
        name: "api".into(),
        cwd: "/nonexistent-project".into(),
        kind: "interactive".into(),
        started_ms: Some(0),
        state: "idle".into(),
        pid: Some(std::process::id()),
        version: Some("2.1.304".into()),
        socket: sock,
    }
}

/// A socket in a short path (unix socket paths are about 100 bytes at most).
fn listen() -> (tempfile::TempDir, PathBuf, UnixListener) {
    let d = tempfile::Builder::new()
        .prefix("t373")
        .tempdir_in("/tmp")
        .unwrap();
    let p = d.path().join("s.sock");
    let l = UnixListener::bind(&p).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
    (d, p, l)
}

fn read_line(l: UnixListener) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let (mut c, _) = l.accept().unwrap();
        let mut s = String::new();
        c.read_to_string(&mut s).unwrap();
        s
    })
}

fn yes(_: &str) -> bool {
    true
}

fn send(
    e: &Env,
    s: &Session,
    text: &str,
    now: i64,
    confirm: &mut dyn FnMut(&str) -> bool,
) -> Result<String, String> {
    control::send(e, s, text, &Redactor::new(), None, now, confirm)
}

// ---- crossSessionInbound, with the vendor's precedence ----

fn inbound(
    user: Option<&str>,
    project: Option<&str>,
    local: Option<&str>,
    managed: Option<&str>,
) -> Inbound {
    let t = tempfile::tempdir().unwrap();
    let cfg = t.path().join("cfg");
    let proj = t.path().join("proj");
    let val = |v: &str| format!("{{\"crossSessionInbound\": {v}}}");
    if let Some(v) = user {
        write(&cfg.join("settings.json"), &val(v));
    }
    if let Some(v) = project {
        write(&proj.join(".claude/settings.json"), &val(v));
    }
    if let Some(v) = local {
        write(&proj.join(".claude/settings.local.json"), &val(v));
    }
    let m = t.path().join("managed.json");
    if let Some(v) = managed {
        write(&m, &val(v));
    }
    control::inbound(&cfg, &proj, managed.map(|_| m.as_path()))
}

#[test]
fn inbound_defaults_to_unset() {
    assert_eq!(inbound(None, None, None, None), Inbound::Unset);
}

#[test]
fn inbound_reads_the_user_value() {
    assert_eq!(
        inbound(Some("\"accept\""), None, None, None),
        Inbound::Accept
    );
    assert_eq!(inbound(Some("\"hold\""), None, None, None), Inbound::Hold);
    assert_eq!(
        inbound(Some("\"refuse\""), None, None, None),
        Inbound::Refuse
    );
}

#[test]
fn managed_settings_win_over_user_settings() {
    assert_eq!(
        inbound(Some("\"accept\""), None, None, Some("\"hold\"")),
        Inbound::Hold
    );
}

#[test]
fn a_project_value_applies_only_when_stricter() {
    assert_eq!(
        inbound(Some("\"accept\""), Some("\"refuse\""), None, None),
        Inbound::Refuse
    );
    assert_eq!(
        inbound(Some("\"hold\""), Some("\"accept\""), None, None),
        Inbound::Hold
    );
    assert_eq!(
        inbound(None, Some("\"accept\""), None, None),
        Inbound::Unset
    );
    assert_eq!(
        inbound(None, Some("\"hold\""), Some("\"refuse\""), None),
        Inbound::Refuse
    );
}

#[test]
fn an_unrecognised_value_holds_in_user_settings_and_refuses_in_managed() {
    assert_eq!(
        inbound(Some("\"yes please\""), None, None, None),
        Inbound::Hold
    );
    assert_eq!(
        inbound(Some("\"accept\""), None, None, Some("\"nope\"")),
        Inbound::Refuse
    );
}

// ---- sending ----

#[test]
fn a_confirmed_send_writes_one_tagged_line_to_the_socket_and_audits_a_hash() {
    let t = tempfile::tempdir().unwrap();
    let e = env(t.path());
    let (_d, sock, l) = listen();
    let rx = read_line(l);
    let mut shown = String::new();
    let out = send(
        &e,
        &session(Some(sock)),
        "please stop and re-read the plan",
        1_000,
        &mut |p| {
            shown = p.to_owned();
            true
        },
    )
    .unwrap();
    assert!(out.contains("sent"), "{out}");
    assert!(out.contains("delivery unknown"), "{out}");
    assert!(
        shown.contains("please stop and re-read the plan"),
        "{shown}"
    );
    let line = rx.join().unwrap();
    assert!(
        line.ends_with('\n') && line.matches('\n').count() == 1,
        "{line:?}"
    );
    let v: Value = serde_json::from_str(line.trim()).unwrap();
    let text = v["text"].as_str().unwrap();
    assert!(text.starts_with("[aibus]"), "{text}");
    assert!(text.contains("please stop and re-read the plan"));

    let audit = fs::read_to_string(e.state.join("taps/control/audit.jsonl")).unwrap();
    let row: Value = serde_json::from_str(audit.lines().next().unwrap()).unwrap();
    assert_eq!(row["via"], "tap");
    assert_eq!(row["client"], "claude");
    assert_eq!(row["session"], S);
    assert_eq!(row["action"], "guidance");
    assert_eq!(row["outcome"], "sent");
    assert_eq!(row["sha256"].as_str().unwrap().len(), 64);
    assert!(
        !audit.contains("please stop"),
        "the audit never holds the text"
    );
}

#[test]
fn not_confirmed_sends_nothing_and_writes_no_audit_row() {
    let t = tempfile::tempdir().unwrap();
    let e = env(t.path());
    let (_d, sock, l) = listen();
    l.set_nonblocking(true).unwrap();
    let err = send(&e, &session(Some(sock)), "hello", 1_000, &mut |_| false).unwrap_err();
    assert!(err.contains("not confirmed"), "{err}");
    assert!(l.accept().is_err(), "nothing connected");
    assert!(!e.state.join("taps/control/audit.jsonl").exists());
}

#[test]
fn a_session_that_refuses_inbound_messages_gets_nothing() {
    let t = tempfile::tempdir().unwrap();
    let e = env(t.path());
    write(
        &e.config.join("settings.json"),
        r#"{"crossSessionInbound":"refuse"}"#,
    );
    let (_d, sock, l) = listen();
    l.set_nonblocking(true).unwrap();
    let err = send(&e, &session(Some(sock)), "hello", 1_000, &mut yes).unwrap_err();
    assert!(err.contains("refuse"), "{err}");
    assert!(err.contains("never changes"), "{err}");
    assert!(l.accept().is_err());
}

#[test]
fn a_session_that_holds_inbound_messages_is_told_to_approve_there() {
    let t = tempfile::tempdir().unwrap();
    let e = env(t.path());
    write(
        &e.config.join("settings.json"),
        r#"{"crossSessionInbound":"hold"}"#,
    );
    let (_d, sock, l) = listen();
    let rx = read_line(l);
    let out = send(&e, &session(Some(sock)), "hello", 1_000, &mut yes).unwrap();
    rx.join().unwrap();
    assert!(
        out.contains("held by the session, approve it there"),
        "{out}"
    );
}

#[test]
fn a_socket_other_users_could_write_to_is_refused() {
    let t = tempfile::tempdir().unwrap();
    let e = env(t.path());
    let (_d, sock, l) = listen();
    fs::set_permissions(&sock, fs::Permissions::from_mode(0o666)).unwrap();
    l.set_nonblocking(true).unwrap();
    let err = send(&e, &session(Some(sock)), "hello", 1_000, &mut yes).unwrap_err();
    assert!(err.contains("socket"), "{err}");
    assert!(l.accept().is_err());
}

#[test]
fn a_symlink_is_not_followed_to_a_socket() {
    let t = tempfile::tempdir().unwrap();
    let e = env(t.path());
    let (d, sock, l) = listen();
    let link = d.path().join("link.sock");
    std::os::unix::fs::symlink(&sock, &link).unwrap();
    l.set_nonblocking(true).unwrap();
    assert!(send(&e, &session(Some(link)), "hello", 1_000, &mut yes).is_err());
    assert!(l.accept().is_err());
}

#[test]
fn a_session_with_no_socket_or_no_longer_there_cannot_be_sent_to() {
    let t = tempfile::tempdir().unwrap();
    let e = env(t.path());
    let err = send(&e, &session(None), "hello", 1_000, &mut yes).unwrap_err();
    assert!(err.contains("inbox socket"), "{err}");
    let mut gone = session(None);
    gone.state = "gone".into();
    assert!(send(&e, &gone, "hello", 1_000, &mut yes)
        .unwrap_err()
        .contains("gone"));
}

#[test]
fn text_is_capped_and_never_empty() {
    let t = tempfile::tempdir().unwrap();
    let e = env(t.path());
    let (_d, sock, _l) = listen();
    let s = session(Some(sock));
    assert!(send(&e, &s, "   ", 1, &mut yes).is_err());
    let big = "x".repeat(control::MAX_TEXT + 1);
    assert!(send(&e, &s, &big, 1, &mut yes)
        .unwrap_err()
        .contains("8 KiB"));
}

#[test]
fn secrets_are_redacted_before_the_text_leaves() {
    let t = tempfile::tempdir().unwrap();
    let e = env(t.path());
    let (_d, sock, l) = listen();
    let rx = read_line(l);
    let mut shown = String::new();
    send(
        &e,
        &session(Some(sock)),
        "use sk-ant-abcdefghijklmnopqrstuvwxyz0123 for it",
        1,
        &mut |p| {
            shown = p.to_owned();
            true
        },
    )
    .unwrap();
    let line = rx.join().unwrap();
    assert!(!line.contains("abcdefghijklmnopqrstuv"), "{line}");
    assert!(!shown.contains("abcdefghijklmnopqrstuv"), "{shown}");
}

#[test]
fn one_controller_per_session() {
    let t = tempfile::tempdir().unwrap();
    let e = env(t.path());
    let (_d, sock, l) = listen();
    l.set_nonblocking(true).unwrap();
    // The parent process is alive and is not us: a controller that holds the lock.
    let held = format!("{}\n", std::os::unix::process::parent_id());
    write(&e.state.join(format!("taps/control/{S}.lock")), &held);
    let err = send(&e, &session(Some(sock)), "hello", 1_000, &mut yes).unwrap_err();
    assert!(err.contains("another controller"), "{err}");
    assert!(l.accept().is_err());
}

#[test]
fn a_dead_controllers_lock_is_taken_over_and_released_after_the_send() {
    let t = tempfile::tempdir().unwrap();
    let e = env(t.path());
    let (_d, sock, l) = listen();
    let rx = read_line(l);
    let lock = e.state.join(format!("taps/control/{S}.lock"));
    write(&lock, "2147483646\n");
    send(&e, &session(Some(sock)), "hello", 1_000, &mut yes).unwrap();
    rx.join().unwrap();
    assert!(!lock.exists(), "the lock is released when the send is done");
}

#[test]
fn more_than_ten_sends_a_minute_are_refused() {
    let t = tempfile::tempdir().unwrap();
    let e = env(t.path());
    let rows: String = (0..10)
        .map(|i| {
            format!(
                "{{\"ts_ms\":{},\"via\":\"tap\",\"outcome\":\"sent\"}}\n",
                100_000 + i
            )
        })
        .collect();
    write(&e.state.join("taps/control/audit.jsonl"), &rows);
    let (_d, sock, l) = listen();
    l.set_nonblocking(true).unwrap();
    let s = session(Some(sock));
    let err = send(&e, &s, "hello", 110_000, &mut yes).unwrap_err();
    assert!(err.contains("10 a minute"), "{err}");
    assert!(l.accept().is_err());
    // A minute later the old rows no longer count.
    l.set_nonblocking(false).unwrap();
    let rx = read_line(l);
    send(&e, &s, "hello", 200_000, &mut yes).unwrap();
    rx.join().unwrap();
}

#[test]
fn the_session_list_reads_the_socket_path_from_the_local_session_file() {
    let t = tempfile::tempdir().unwrap();
    let cfg = t.path().join("cfg");
    let sock = "/tmp/cc-socks-1/9.sock";
    write(
        &cfg.join("sessions/1.json"),
        &format!(
            "{{\"sessionId\":\"{S}\",\"pid\":{},\"messagingSocketPath\":\"uds:{sock}\"}}",
            std::process::id()
        ),
    );
    let json = format!("[{{\"sessionId\":\"{S}\",\"name\":\"a\",\"cwd\":\"/x\",\"kind\":\"interactive\",\"status\":\"idle\",\"pid\":{}}}]", std::process::id());
    let mut l = aibus_tap::agents::parse_agents(json.as_bytes()).unwrap();
    aibus_tap::agents::merge_local(&mut l, &cfg.join("sessions"));
    assert_eq!(l[0].socket.as_deref(), Some(Path::new(sock)));
}

#[test]
fn the_list_and_the_tap_view_say_what_can_be_done() {
    let l = aibus_tap::agents::render_list(&[session(Some("/tmp/x.sock".into()))], &[], 1_000);
    assert!(l.contains("watch + send"), "{l}");
    let l = aibus_tap::agents::render_list(&[session(None)], &[], 1_000);
    assert!(!l.contains("watch + send"), "{l}");
    let c = control::status_lines(&session(Some("/tmp/x.sock".into())), Inbound::Unset);
    assert!(c.contains("send guidance"), "{c}");
    assert!(c.contains("cancel: not available for this client"), "{c}");
    assert!(c.contains("never answers a permission prompt"), "{c}");
    let c = control::status_lines(&session(Some("/tmp/x.sock".into())), Inbound::Refuse);
    assert!(c.contains("watch only"), "{c}");
}

// ---- text the terminal would interpret (review of #386) ----

#[test]
fn text_a_terminal_would_interpret_is_refused_before_anything_is_shown_or_written() {
    for bad in [
        "stop \u{1b}[2K\u{1b}[8m hidden",
        "looks fine\rBut this overwrites it",
        "c1 \u{9b}2J",
        "bidi \u{202e}txt.exe",
    ] {
        let t = tempfile::tempdir().unwrap();
        let e = env(t.path());
        let (_d, sock, l) = listen();
        l.set_nonblocking(true).unwrap();
        let mut asked = false;
        let err = send(&e, &session(Some(sock)), bad, 1_000, &mut |_| {
            asked = true;
            true
        })
        .unwrap_err();
        assert!(err.contains("control"), "{bad:?}: {err}");
        assert!(!asked, "{bad:?} reached the prompt");
        assert!(l.accept().is_err(), "{bad:?} connected");
        assert!(!e.state.join("taps/control/audit.jsonl").exists());
        assert!(!e.state.join(format!("taps/control/{S}.lock")).exists());
    }
}

#[test]
fn a_newline_is_fine_in_guidance() {
    let t = tempfile::tempdir().unwrap();
    let e = env(t.path());
    let (_d, sock, l) = listen();
    let rx = read_line(l);
    send(&e, &session(Some(sock)), "one\ntwo", 1_000, &mut yes).unwrap();
    let line = rx.join().unwrap();
    assert_eq!(line.matches('\n').count(), 1, "{line:?}");
}

#[test]
fn nine_sends_in_the_minute_are_still_allowed() {
    let t = tempfile::tempdir().unwrap();
    let e = env(t.path());
    let rows: String = (0..9)
        .map(|i| {
            format!(
                "{{\"ts_ms\":{},\"via\":\"tap\",\"outcome\":\"sent\"}}\n",
                100_000 + i
            )
        })
        .collect();
    write(&e.state.join("taps/control/audit.jsonl"), &rows);
    let (_d, sock, l) = listen();
    let rx = read_line(l);
    send(&e, &session(Some(sock)), "hello", 110_000, &mut yes).unwrap();
    rx.join().unwrap();
}
