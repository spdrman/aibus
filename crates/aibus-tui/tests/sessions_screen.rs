//! #355: `/sessions` and `/tap` in the dashboard. The prompt turns the line into
//! `Command::Sessions`, the terminal loop answers through `sessions::apply` (the rig stands in
//! for it), and the screen shows the text `aibus sessions` prints.

mod common;

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use aibus_core::*;
use aibus_tui::sessions::{apply, spawn, Setup};
use aibus_tui::Theme;
use common::*;
use crossterm::event::KeyCode as K;

const S: &str = "22222222-2222-4222-8222-222222222222";

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/claude-code/tap-355")
        .join(name)
}

fn lister() -> Result<Vec<u8>, String> {
    std::fs::read(fixture("agents.json")).map_err(|e| e.to_string())
}

fn rig() -> Rig {
    Rig::new(session(), (120, 40), Theme::recording())
}

fn line(r: &mut Rig, state: &Path, text: &str) {
    r.type_text(text);
    for m in r.key(K::Enter) {
        if let Msg::Command(Command::Sessions(q)) = m {
            for a in apply(&q, state, &state.join("cfg"), &lister, &Redactor::new()) {
                r.app.step(a).unwrap();
            }
        }
    }
    r.settle();
}

#[test]
fn slash_sessions_and_slash_tap_become_sessions_commands() {
    let mut r = rig();
    r.type_text("/sessions");
    let sent = r.key(K::Enter);
    assert!(
        sent.iter()
            .any(|m| matches!(m, Msg::Command(Command::Sessions(SessionsQuery::List)))),
        "{sent:?}"
    );
    r.type_text("/tap refactor");
    let sent = r.key(K::Enter);
    assert!(
        sent.iter().any(|m| matches!(m, Msg::Command(Command::Sessions(SessionsQuery::Tap(s))) if s == "refactor")),
        "{sent:?}"
    );
}

#[test]
fn slash_sessions_shows_the_list_on_the_runs_screen() {
    let tmp = tempfile::tempdir().unwrap();
    let mut r = rig();
    line(&mut r, tmp.path(), "/sessions");
    let text = r.text();
    assert!(
        text.contains("refactor parser") && text.contains("watch"),
        "{text}"
    );
}

#[test]
fn slash_tap_shows_the_same_text_as_the_cli_and_says_watch_only() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("taps/claude");
    let mut t = aibus_tap::tap::Tap::open(&root, S, &Redactor::new()).unwrap();
    for l in std::fs::read(fixture("hooks.jsonl"))
        .unwrap()
        .split(|b| *b == b'\n')
    {
        if !l.is_empty() {
            t.ingest(l, SystemTime::now()).unwrap();
        }
    }
    drop(t);
    let mut r = rig();
    line(&mut r, tmp.path(), "/tap refactor parser");
    let text = r.text();
    assert!(text.contains("watch only"), "{text}");
    assert!(
        text.contains("refactor parser") && text.contains("plan (the definition graph"),
        "{text}"
    );
    r.key(K::Esc);
    line(&mut r, tmp.path(), "/tap no-such-session");
    assert!(!r.text().contains("watch only"));
}

#[test]
fn without_a_claude_config_directory_nothing_is_read_relative_to_the_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let msgs = apply(
        &SessionsQuery::List,
        tmp.path(),
        Path::new(""),
        &lister,
        &Redactor::new(),
    );
    assert!(
        matches!(msgs.as_slice(), [Msg::IoError { context, .. }] if context.contains("CLAUDE_CONFIG_DIR")),
        "{msgs:?}"
    );
    let setup = Setup {
        state: tmp.path().into(),
        config: None,
        redactor: Redactor::new(),
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    spawn(
        &SessionsQuery::List,
        &setup,
        std::sync::Arc::new(lister),
        tx,
    );
    let first = rx.blocking_recv().unwrap();
    assert!(matches!(first, Msg::IoError { .. }), "{first:?}");
}

#[test]
fn a_slow_claude_agents_json_does_not_hold_the_caller() {
    let tmp = tempfile::tempdir().unwrap();
    let setup = Setup {
        state: tmp.path().into(),
        config: Some(tmp.path().join("cfg")),
        redactor: Redactor::new(),
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let slow = || {
        std::thread::sleep(std::time::Duration::from_millis(800));
        lister()
    };
    let t = std::time::Instant::now();
    spawn(&SessionsQuery::List, &setup, std::sync::Arc::new(slow), tx);
    assert!(
        t.elapsed() < std::time::Duration::from_millis(300),
        "spawn waited for the CLI: {:?}",
        t.elapsed()
    );
    let first = rx.blocking_recv().unwrap();
    assert!(
        matches!(first, Msg::RunsLoaded { ref text, .. } if text.contains("refactor parser")),
        "{first:?}"
    );
}

#[test]
fn the_session_redactor_reaches_the_screen_text() {
    let tmp = tempfile::tempdir().unwrap();
    let r = Redactor::new();
    r.register(&AccountName("a".into()), "refactor");
    let msgs = apply(
        &SessionsQuery::List,
        tmp.path(),
        &tmp.path().join("cfg"),
        &lister,
        &r,
    );
    let text = msgs
        .iter()
        .find_map(|m| {
            if let Msg::RunsLoaded { text, .. } = m {
                Some(text.clone())
            } else {
                None
            }
        })
        .unwrap();
    assert!(!text.contains("refactor"), "{text}");
}
