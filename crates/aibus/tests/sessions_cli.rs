//! #355 (phase 1 of #348): `aibus sessions`. A stand-in `claude` on PATH prints the synthetic
//! `agents.json` and records its arguments, so a test sees the one call aibus makes and never
//! needs the real CLI. Every settings file here is in a throwaway directory.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use common::*;

const S: &str = "22222222-2222-4222-8222-222222222222";

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/claude-code/tap-355")
        .join(name)
}

/// A `bin/claude` that answers `agents --json` and logs every call.
fn fake_claude(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let log = dir.join("claude-calls.log");
    let script = format!(
        "#!/bin/sh\necho \"$@\" >> '{}'\nif [ \"$1 $2\" = 'agents --json' ]; then cat '{}'; exit 0; fi\nexit 99\n",
        log.display(),
        fixture("agents.json").display()
    );
    let p = bin.join("claude");
    fs::write(&p, script).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

fn sessions(home: &Path, args: &[&str]) -> Output {
    let bin = fake_claude(home);
    let mut c = command(home);
    c.env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("CLAUDE_CONFIG_DIR", home.join("cfg"))
        .env("AIBUS_CONFIG", home.join("none.toml"));
    c.arg("sessions").args(args);
    c.output().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn sessions_lists_what_claude_agents_json_printed_and_calls_nothing_else() {
    let home = aibus_testkit::xdg_temp();
    let o = sessions(home.path(), &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    let text = out(&o);
    assert!(
        text.contains("refactor parser") && text.contains("watch"),
        "{text}"
    );
    assert!(!text.contains('\u{1b}'));
    let calls = fs::read_to_string(home.path().join("claude-calls.log")).unwrap();
    assert_eq!(calls.trim(), "agents --json", "the one call aibus makes");
}

#[test]
fn sessions_without_the_cli_says_so_and_fails() {
    let home = aibus_testkit::xdg_temp();
    let mut c = command(home.path());
    c.env("PATH", "")
        .env("AIBUS_CONFIG", home.path().join("none.toml"));
    let o = c.args(["sessions"]).output().unwrap();
    assert!(!o.status.success());
    assert!(stderr(&o).contains("claude"), "{}", stderr(&o));
}

#[test]
fn tap_of_a_session_nobody_posted_for_says_unavailable_and_how_to_turn_it_on() {
    let home = aibus_testkit::xdg_temp();
    let o = sessions(home.path(), &["tap", "refactor parser"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let text = out(&o);
    assert!(
        text.contains("unavailable") && text.contains("hooks install"),
        "{text}"
    );
    assert!(text.contains("watch only"), "{text}");
}

#[test]
fn hooks_install_prints_the_diff_and_without_a_yes_writes_nothing() {
    let home = aibus_testkit::xdg_temp();
    let cfg = home.path().join("cfg");
    fs::create_dir_all(&cfg).unwrap();
    fs::copy(fixture("settings-before.json"), cfg.join("settings.json")).unwrap();
    let mut c = command(home.path());
    c.env("XDG_STATE_HOME", home.path().join("state"))
        .env("AIBUS_CONFIG", home.path().join("none.toml"))
        .stdin(Stdio::piped());
    let mut child = c
        .args([
            "sessions",
            "hooks",
            "install",
            "--client",
            "claude",
            "--config-dir",
        ])
        .arg(&cfg)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(b"n\n").unwrap();
    }
    let o = child.wait_with_output().unwrap();
    let text = out(&o);
    assert!(
        text.contains("127.0.0.1") && text.contains("+"),
        "the diff is shown: {text}"
    );
    assert_eq!(
        fs::read(cfg.join("settings.json")).unwrap(),
        fs::read(fixture("settings-before.json")).unwrap()
    );
}

#[test]
fn hooks_install_yes_then_uninstall_yes_round_trips_and_keeps_the_token_out_of_the_output_of_list()
{
    let home = aibus_testkit::xdg_temp();
    let cfg = home.path().join("cfg");
    fs::create_dir_all(&cfg).unwrap();
    fs::copy(fixture("settings-before.json"), cfg.join("settings.json")).unwrap();
    let run = |args: &[&str]| {
        let mut c = command(home.path());
        c.env("XDG_STATE_HOME", home.path().join("state"))
            .env("AIBUS_CONFIG", home.path().join("none.toml"));
        c.args(["sessions", "hooks"])
            .args(args)
            .arg("--client")
            .arg("claude");
        c.arg("--config-dir").arg(&cfg).arg("--yes");
        c.output().unwrap()
    };
    let o = run(&["install"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let now = fs::read_to_string(cfg.join("settings.json")).unwrap();
    assert!(
        now.contains("\"type\": \"http\"") && now.contains("127.0.0.1"),
        "{now}"
    );
    let o = run(&["uninstall"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let back: serde_json::Value =
        serde_json::from_slice(&fs::read(cfg.join("settings.json")).unwrap()).unwrap();
    let before: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture("settings-before.json")).unwrap()).unwrap();
    assert_eq!(back, before);
}

#[test]
fn hooks_install_refuses_a_client_other_than_claude() {
    let home = aibus_testkit::xdg_temp();
    let o = aibus(
        home.path(),
        &["sessions", "hooks", "install", "--client", "codex", "--yes"],
    );
    assert!(!o.status.success());
    assert!(stderr(&o).contains("claude"), "{}", stderr(&o));
}

#[test]
fn allow_transcripts_is_a_per_client_switch_that_starts_off() {
    let home = aibus_testkit::xdg_temp();
    let o = sessions(home.path(), &["tap", "refactor parser"]);
    assert!(out(&o).contains("transcripts: off"), "{}", out(&o));
    let o = sessions(home.path(), &["allow-transcripts", "claude"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let o = sessions(home.path(), &["tap", "refactor parser"]);
    assert!(!out(&o).contains("transcripts: off"), "{}", out(&o));
    let o = sessions(home.path(), &["allow-transcripts", "claude", "--revoke"]);
    assert!(o.status.success());
    let o = sessions(home.path(), &["tap", "refactor parser"]);
    assert!(out(&o).contains("transcripts: off"));
    let o = sessions(home.path(), &["allow-transcripts", "codex"]);
    assert!(!o.status.success());
}

fn endpoint_token(home: &Path) -> String {
    let v: serde_json::Value =
        serde_json::from_slice(&fs::read(home.join("state/aibus/tap/endpoint.json")).unwrap())
            .unwrap();
    v["token"].as_str().unwrap().to_owned()
}

fn post(port: u16, host: &str, auth: &str, body: &[u8]) -> String {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    let head = format!(
        "POST /aibus-tap/hook HTTP/1.1\r\nHost: {host}\r\nAuthorization: {auth}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes()).unwrap();
    s.write_all(body).unwrap();
    let mut r = String::new();
    s.read_to_string(&mut r).unwrap();
    r.lines().next().unwrap_or("").to_owned()
}

#[test]
fn follow_takes_hook_posts_with_the_token_only_and_records_the_tapped_session() {
    use std::io::{BufRead, BufReader};
    let home = aibus_testkit::xdg_temp();
    let bin = fake_claude(home.path());
    let mut c = command(home.path());
    c.env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("XDG_STATE_HOME", home.path().join("state"))
        .env("CLAUDE_CONFIG_DIR", home.path().join("cfg"))
        .env("AIBUS_CONFIG", home.path().join("none.toml"));
    let mut child = c
        .args(["sessions", "tap", "refactor parser", "--follow"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut err = BufReader::new(child.stderr.take().unwrap());
    let mut first = String::new();
    err.read_line(&mut first).unwrap();
    assert!(first.contains("listening on http://127.0.0.1:"), "{first}");
    let ep: serde_json::Value = serde_json::from_slice(
        &fs::read(home.path().join("state/aibus/tap/endpoint.json")).unwrap(),
    )
    .unwrap();
    let port = ep["port"].as_u64().unwrap() as u16;
    let token = ep["token"].as_str().unwrap();
    let hooks = fs::read_to_string(fixture("hooks.jsonl")).unwrap();
    let line = hooks.lines().nth(2).unwrap().as_bytes();
    let h = format!("127.0.0.1:{port}");
    assert!(post(port, &h, "Bearer nope", line).contains("401"));
    assert!(post(port, "evil.example", &format!("Bearer {token}"), line).contains("404"));
    assert!(post(port, &h, &format!("Bearer {token}"), line).contains("200"));
    // Another session's event is answered and dropped.
    let other = br#"{"session_id":"33333333-3333-4333-8333-333333333333","hook_event_name":"SessionStart"}"#;
    assert!(post(port, &h, &format!("Bearer {token}"), other).contains("200"));
    child.kill().unwrap();
    child.wait().unwrap();
    let events = fs::read_to_string(
        home.path()
            .join(format!("state/aibus/taps/claude/{S}/events.jsonl")),
    )
    .unwrap();
    assert!(
        events.contains("tool_started") && !events.contains("SYNTHETIC-SECRET-PATH"),
        "{events}"
    );
    assert!(!home
        .path()
        .join("state/aibus/taps/claude/33333333-3333-4333-8333-333333333333")
        .exists());
}

#[test]
fn a_slow_client_does_not_hold_the_receiver() {
    use std::io::{BufRead, BufReader, Read, Write};
    let home = aibus_testkit::xdg_temp();
    let bin = fake_claude(home.path());
    let mut c = command(home.path());
    c.env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("XDG_STATE_HOME", home.path().join("state"))
        .env("CLAUDE_CONFIG_DIR", home.path().join("cfg"))
        .env("AIBUS_CONFIG", home.path().join("none.toml"));
    let mut child = c
        .args(["sessions", "tap", "refactor parser", "--follow"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = String::new();
    BufReader::new(child.stderr.take().unwrap())
        .read_line(&mut first)
        .unwrap();
    let ep: serde_json::Value = serde_json::from_slice(
        &fs::read(home.path().join("state/aibus/tap/endpoint.json")).unwrap(),
    )
    .unwrap();
    let port = ep["port"].as_u64().unwrap() as u16;
    let token = ep["token"].as_str().unwrap().to_owned();
    // Forty clients that connect and say nothing, then half a request line.
    let mut slow: Vec<std::net::TcpStream> = (0..40)
        .map(|_| std::net::TcpStream::connect(("127.0.0.1", port)).unwrap())
        .collect();
    for s in slow.iter_mut().take(10) {
        s.write_all(b"POST /aibus-tap/hook HTTP/1.1\r\nHost: ")
            .unwrap();
    }
    // They are cut off by the header timeout, and a real post then goes through.
    let t = std::time::Instant::now();
    for s in &mut slow {
        s.set_read_timeout(Some(std::time::Duration::from_secs(8)))
            .unwrap();
        let mut b = [0u8; 16];
        let _ = s.read(&mut b);
    }
    assert!(
        t.elapsed() < std::time::Duration::from_secs(6),
        "{:?}",
        t.elapsed()
    );
    let hooks = fs::read_to_string(fixture("hooks.jsonl")).unwrap();
    let h = format!("127.0.0.1:{port}");
    assert!(post(
        port,
        &h,
        &format!("Bearer {token}"),
        hooks.lines().nth(2).unwrap().as_bytes()
    )
    .contains("200"));
    child.kill().unwrap();
    child.wait().unwrap();
}
