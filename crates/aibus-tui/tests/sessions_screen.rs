//! #355: `/sessions` and `/tap` in the dashboard. The prompt turns the line into
//! `Command::Sessions`, the terminal loop answers through `sessions::apply` (the rig stands in
//! for it), and the screen shows the text `aibus sessions` prints.

mod common;

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use aibus_core::*;
use aibus_tui::sessions::apply;
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
            for a in apply(&q, state, &state.join("cfg"), &lister) {
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
    let mut t = aibus_tap::tap::Tap::open(&root, S).unwrap();
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
