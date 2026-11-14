//! G4: `/login`, `/logout` and `/accounts` inside a session. The provider is picked from a list,
//! the key is typed into a masked field and is a `Secret` from the first keystroke, the verify
//! runs off the UI loop, and nothing the key touches is a message, a frame, a history entry or a
//! log line.

mod common;

use std::time::{Duration, Instant};

use aibus_core::state::value::{as_list, as_str, get};
use aibus_core::*;
use aibus_graph::Graph;
use aibus_tui::app::{run_job, AccountHook};
use aibus_tui::input::{AccountOp, Job};
use aibus_tui::Theme;
use common::*;
use crossterm::event::KeyCode as K;
use tokio::sync::mpsc;

const KEY: &str = "sk-test-LOGIN-0123456789abcdef";

fn app() -> App<aibus_graph::mini::MiniGraph> {
    let mut app = session();
    app.step(Msg::AccountsReloaded {
        accounts: vec![AccountView {
            name: AccountName("work".into()),
            provider: "anthropic".into(),
            label: "work".into(),
            default_model: None,
            key_ref: "keyring:aibus/work".into(),
            models: vec![],
        }],
    })
    .unwrap();
    app
}

fn rig() -> Rig {
    Rig::new(app(), (98, 46), Theme::recording())
}

fn modal(r: &Rig) -> Option<String> {
    r.app
        .graph
        .read_key("ctrl:modal")
        .and_then(as_str)
        .map(str::to_owned)
}

fn line(r: &mut Rig, text: &str) -> Vec<Msg> {
    r.type_text(text);
    r.key(K::Enter)
}

fn provider_rows(r: &Rig) -> Vec<String> {
    let v = r.app.graph.read_key("view:providers").unwrap();
    get(v, "rows")
        .map(as_list)
        .unwrap_or(&[])
        .iter()
        .filter_map(|row| get(row, "id").and_then(as_str))
        .map(str::to_owned)
        .collect()
}

/// Walks the flow with the keyboard up to the key being typed: returns the rig with the key
/// field open and `KEY` typed into it.
fn typed(name: &str) -> Rig {
    let mut r = rig();
    line(&mut r, "/login anthropic");
    line(&mut r, name);
    r.type_text(KEY);
    r
}

#[test]
fn login_opens_the_provider_picker_and_the_masked_entry() {
    let mut r = rig();
    line(&mut r, "/login");
    assert_eq!(modal(&r).as_deref(), Some("providers"));
    let ids = provider_rows(&r);
    assert!(
        ids.iter().any(|i| i == "anthropic") && ids.iter().any(|i| i == "openrouter"),
        "{ids:?}"
    );
    assert!(
        r.text().contains("log in to a provider") && r.text().contains("anthropic"),
        "{}",
        r.text()
    );

    // Enter on the highlighted row goes to the name entry of that provider, on the Accounts screen.
    let at = ids.iter().position(|i| i == "anthropic").unwrap();
    for _ in 0..at {
        r.key(K::Down);
    }
    r.key(K::Enter);
    assert_eq!(modal(&r).as_deref(), Some("accounts"));
    assert!(r.text().contains("anthropic account name"), "{}", r.text());
    line(&mut r, "work2");
    assert!(
        r.text().contains("anthropic key"),
        "the key is asked for next: {}",
        r.text()
    );

    // `/login anthropic` skips the picker.
    let mut r = rig();
    line(&mut r, "/login anthropic");
    assert_eq!(modal(&r).as_deref(), Some("accounts"));
    assert!(r.text().contains("anthropic account name"));
    // An unknown provider is a notice, not a screen.
    let mut r = rig();
    line(&mut r, "/login nosuchvendor");
    assert_eq!(modal(&r), None);
    assert!(r
        .ui
        .prompt
        .notice
        .as_deref()
        .is_some_and(|n| n.contains("nosuchvendor")));

    // Every character of the key shows as a dot.
    let r = typed("work2");
    let shown = r.text();
    assert!(
        !shown.contains(KEY) && !shown.contains("sk-test"),
        "{shown}"
    );
    assert!(shown.contains(&"•".repeat(KEY.chars().count())), "{shown}");
}

#[test]
fn the_entry_holds_a_secret_and_the_op_carries_it() {
    let mut r = typed("work2");
    r.key(K::Enter);
    let [Job::Account(AccountOp::Add {
        name,
        provider,
        key,
    })] = &r.jobs[..]
    else {
        panic!("{:?}", r.jobs)
    };
    assert_eq!((name.as_str(), provider.as_str()), ("work2", "anthropic"));
    // The type is the point: `Secret<String>` has no Debug of the text and no Clone.
    let secret: &aibus_provider::Secret<String> = key;
    assert_eq!(secret.expose(), KEY);
    assert_eq!(
        format!("{:?}", r.jobs),
        "[Account(Add work2 anthropic <hidden>)]"
    );
    assert!(r.text().contains("verifying work2..."), "{}", r.text());
}

#[test]
fn an_empty_key_is_not_sent_and_leaves_the_field_open() {
    let mut r = rig();
    line(&mut r, "/login anthropic");
    line(&mut r, "work2");
    r.key(K::Enter);
    assert!(r.jobs.is_empty());
    assert!(r.text().contains("anthropic key"));
}

#[test]
fn the_dashboard_keeps_ticking_during_the_verify() {
    let mut r = rig();
    line(&mut r, "/login anthropic");
    line(&mut r, "slow");
    r.type_text(KEY);
    r.key(K::Enter);
    let job = r.jobs.remove(0);

    // The hook is what the binary installs: it returns the line to show and finishes later.
    let (tx, mut rx) = mpsc::channel(8);
    let mut hook: Option<AccountHook> = Some(Box::new(|op, tx| {
        let AccountOp::Add { name, .. } = &op else {
            panic!()
        };
        let (shown, name) = (format!("verifying {name}..."), name.clone());
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            let _ = tx.blocking_send(Msg::AccountOpDone {
                op: "add".into(),
                account: name.clone(),
                ok: true,
                text: format!("added {name}"),
            });
        });
        shown
    }));
    let mut note = String::new();
    let started = Instant::now();
    run_job(job, None, &mut None, &mut hook, &mut note, &tx);
    assert!(
        started.elapsed() < Duration::from_millis(150),
        "the job returns before the verify is over"
    );
    assert_eq!(note, "verifying slow...");

    // Meanwhile the loop goes on: ticks advance and a frame renders with the note on screen.
    r.input.note = note;
    r.app
        .step(Msg::Command(Command::OpenScreen(Screen::Accounts)))
        .unwrap();
    let before = r.int("eng:tick").unwrap();
    ticks(&mut r.app, 5);
    r.settle();
    assert_eq!(r.int("eng:tick").unwrap(), before + 5);
    assert!(r.text().contains("verifying slow..."), "{}", r.text());
    assert!(rx.try_recv().is_err(), "the verify is still running");

    // Then the answer arrives as a message and replaces the note.
    let done = rx.blocking_recv().unwrap();
    r.input.observe(&done);
    r.app.step(done).unwrap();
    r.settle();
    assert!(
        r.text().contains("added slow") && !r.text().contains("verifying slow"),
        "{}",
        r.text()
    );
}

#[test]
fn inline_key_is_refused_and_redacted_and_not_in_history() {
    let mut r = rig();
    let msgs = line(&mut r, &format!("/login anthropic {KEY}"));
    assert!(msgs.is_empty(), "nothing reaches the graph: {msgs:?}");
    assert!(r.jobs.is_empty() && modal(&r).is_none());
    let notice = r.ui.prompt.notice.clone().expect("a notice says why");
    assert!(
        notice.contains("anthropic") && notice.contains("[redacted]"),
        "{notice}"
    );
    assert!(
        !notice.contains("sk-test") && !notice.contains(KEY),
        "{notice}"
    );
    assert!(
        r.ui.prompt.history.iter().all(|h| !h.contains("sk-test")),
        "{:?}",
        r.ui.prompt.history
    );
    r.key(K::Up);
    assert!(
        !r.text().contains("sk-test"),
        "the up arrow does not bring it back: {}",
        r.text()
    );
    assert!(!r.text().contains(KEY));
}

#[test]
fn logout_asks_then_removes() {
    let mut r = rig();
    line(&mut r, "/logout work");
    assert_eq!(modal(&r).as_deref(), Some("confirm:logout:work"));
    assert!(r.text().contains("Log out of work?"), "{}", r.text());
    r.press('n');
    assert!(
        modal(&r).is_none() && r.jobs.is_empty(),
        "no changes the mind"
    );

    line(&mut r, "/logout work");
    r.press('y');
    assert_eq!(r.jobs, [Job::Account(AccountOp::Remove("work".into()))]);

    // An account that is not there is a notice.
    let mut r = rig();
    line(&mut r, "/logout nobody");
    assert!(modal(&r).is_none());
    assert!(r
        .ui
        .prompt
        .notice
        .as_deref()
        .is_some_and(|n| n.contains("nobody")));
}

#[test]
fn the_accounts_screen_add_key_opens_the_picker() {
    let mut r = rig();
    line(&mut r, "/accounts");
    r.press('a');
    assert_eq!(modal(&r).as_deref(), Some("providers"));
}

/// Everything the flow leaves behind in the terminal's own state: the messages it produced, the
/// frames, the prompt history and the graph. None of it holds the key.
#[test]
fn no_login_secret_in_any_session_artefact() {
    let mut r = rig();
    let mut seen: Vec<String> = Vec::new();
    seen.extend(names(&line(&mut r, "/login")));
    seen.push(r.text());
    r.key(K::Esc);
    seen.extend(names(&line(&mut r, "/login anthropic")));
    seen.extend(names(&line(&mut r, "work2")));
    r.type_text(KEY);
    seen.push(r.text());
    seen.extend(names(&r.key(K::Enter)));
    seen.push(r.text());
    seen.extend(names(&line(&mut r, &format!("/login anthropic {KEY}"))));
    seen.push(format!("{:?}", r.ui.prompt.history));
    seen.push(format!("{:?}", r.jobs));
    seen.push(format!("{:?}", r.input.entry));
    // The whole graph, as a snapshot would write it.
    seen.push(format!("{:?}", r.app.graph.snapshot()));
    for text in &seen {
        assert!(!text.contains("LOGIN-0123456789"), "the key leaked: {text}");
    }
}
