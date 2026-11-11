//! G4: a key typed into `/login` leaves no trace in the session directory. The key itself never
//! becomes a message (it travels in a `Secret` to the worker), so the one way it could reach disk
//! is inside the words of an outcome, a provider's error that echoes it back. That text goes
//! through the redactor before it is sent, and the writer redacts what it records as well.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use aibus_core::config::Persist;
use aibus_core::persist::session::SessionWriter;
use aibus_core::{AccountName, AccountView, App, Command, Msg, Redactor, Screen};
use aibus_graph::mini::MiniGraph;
use aibus_testkit::xdg_temp;

const KEY: &str = "sk-login-SECRET-0123456789abcdef";

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        if e.path().is_dir() {
            out.extend(walk(&e.path()));
        } else {
            out.push(e.path());
        }
    }
    out
}

#[test]
fn no_login_secret_in_any_session_artefact() {
    let dir = xdg_temp();
    let redactor = Redactor::new();
    redactor.register(&AccountName("work".into()), KEY);
    let mut app = App::new(MiniGraph::new(), || 5_000);
    app.record_commits();
    let mut w = SessionWriter::open(dir.path(), redactor.clone(), &Persist::default(), 64 * 1024).unwrap();
    let msgs = [
        Msg::Command(Command::OpenScreen(Screen::Providers)),
        Msg::Command(Command::OpenScreen(Screen::Accounts)),
        // A provider that echoes the key in its refusal: the worker redacts the line first...
        Msg::AccountOpDone {
            op: "add".into(),
            account: "work".into(),
            ok: false,
            text: redactor.redact(&format!("failed: 401 invalid key {KEY}")).into_owned(),
        },
        // ...and the writer would catch it if a line slipped through unredacted.
        Msg::AccountOpDone { op: "add".into(), account: "work".into(), ok: false, text: format!("failed: {KEY}") },
        Msg::AccountsReloaded {
            accounts: vec![AccountView {
                name: AccountName("work".into()),
                provider: "anthropic".into(),
                label: "work".into(),
                default_model: None,
                key_ref: "keyring:aibus/work".into(),
                models: vec![],
            }],
        },
    ];
    for msg in msgs {
        w.msg(&msg, SystemTime::now()).unwrap();
        app.step(msg).unwrap();
        w.after_step(&mut app).unwrap();
    }
    w.finish(&mut app).unwrap();
    let files = walk(dir.path());
    assert!(!files.is_empty());
    for file in files {
        let raw = String::from_utf8_lossy(&std::fs::read(&file).unwrap()).into_owned();
        assert!(!raw.contains("SECRET-0123456789"), "{}", file.display());
    }
}
