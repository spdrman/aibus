//! G4b (#161): `/login` for a provider whose definition carries a browser flow. The key step is
//! replaced by a modal that shows what the flow says (the address, the device code), `o` opens
//! the address through the platform opener, and `Esc` cancels the flow's token.

mod common;

use aibus_core::state::value::as_str;
use aibus_core::*;
use aibus_graph::Graph;
use aibus_tui::app::{run_job, AccountHook};
use aibus_tui::input::{AccountOp, Job};
use aibus_tui::Theme;
use common::*;
use crossterm::event::KeyCode as K;
use tokio::sync::mpsc;

const URL: &str = "https://openrouter.ai/auth?callback_url=http%3A%2F%2F127.0.0.1%3A3000%2Fcallback&code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM&code_challenge_method=S256";

fn rig() -> Rig {
    let mut r = Rig::new(session(), (98, 46), Theme::recording());
    r.input.oauth_providers = vec!["openrouter".into()];
    r
}

fn line(r: &mut Rig, text: &str) -> Vec<Msg> {
    r.type_text(text);
    r.key(K::Enter)
}

fn said(r: &mut Rig, text: &str) {
    let m = Msg::AccountPrompt {
        account: "or1".into(),
        line: text.into(),
    };
    r.input.observe(&m);
    r.app.step(m).unwrap();
    r.settle();
}

/// `/login openrouter`, named `or1`, up to the modal being open.
fn started() -> Rig {
    let mut r = rig();
    line(&mut r, "/login openrouter");
    line(&mut r, "or1");
    r
}

#[test]
fn a_provider_with_a_flow_gets_the_modal_instead_of_the_key_step() {
    let r = started();
    assert_eq!(
        r.jobs,
        [Job::Account(AccountOp::AddOauth {
            name: "or1".into(),
            provider: "openrouter".into()
        })]
    );
    let shown = r.text();
    assert!(
        shown.contains("log in to openrouter") && shown.contains("Esc"),
        "{shown}"
    );
    assert!(!shown.contains("openrouter key"), "no key field: {shown}");
    assert_eq!(
        r.app.graph.read_key("ctrl:modal").and_then(as_str),
        Some("accounts")
    );

    // A provider without a flow still asks for a key.
    let mut r = rig();
    line(&mut r, "/login anthropic");
    line(&mut r, "work2");
    assert!(
        r.jobs.is_empty() && r.text().contains("anthropic key"),
        "{}",
        r.text()
    );
}

#[test]
fn the_modal_shows_the_address_and_the_device_code() {
    let mut r = started();
    said(&mut r, &format!("open this address to log in:\n  {URL}"));
    let shown = r.text().replace('\n', "");
    // The address wraps over rows; its head and its tail are both on screen.
    assert!(
        shown.contains("https://openrouter.ai/auth?callback_url=") && shown.contains("S256"),
        "{shown}"
    );

    let mut r = started();
    said(
        &mut r,
        "open https://example.test/device and enter the code ABCD-1234",
    );
    assert!(
        r.text().contains("ABCD-1234") && r.text().contains("https://example.test/device"),
        "{}",
        r.text()
    );
}

#[test]
fn o_opens_the_address_and_only_an_http_one() {
    let mut r = started();
    r.jobs.clear();
    r.press('o');
    assert!(r.jobs.is_empty(), "nothing to open yet");

    said(&mut r, &format!("open this address to log in:\n  {URL}"));
    r.press('o');
    assert_eq!(r.jobs, [Job::Account(AccountOp::OpenUrl(URL.into()))]);

    for odd in [
        "open javascript:alert(1)",
        "open file:///etc/passwd",
        "open --version",
    ] {
        let mut r = started();
        r.jobs.clear();
        said(&mut r, odd);
        r.press('o');
        assert!(r.jobs.is_empty(), "{odd} is not an address: {:?}", r.jobs);
    }
}

#[test]
fn esc_cancels_the_flow_and_closes_the_modal() {
    let mut r = started();
    r.jobs.clear();
    r.key(K::Esc);
    assert_eq!(r.jobs, [Job::Account(AccountOp::CancelOauth)]);
    assert!(!r.text().contains("log in to openrouter"), "{}", r.text());
}

#[test]
fn the_end_of_the_flow_closes_the_modal() {
    let mut r = started();
    let done = Msg::AccountOpDone {
        op: "add".into(),
        account: "or1".into(),
        ok: true,
        text: "added or1".into(),
    };
    r.input.observe(&done);
    r.app.step(done).unwrap();
    r.settle();
    assert!(
        !r.text().contains("log in to openrouter") && r.text().contains("added or1"),
        "{}",
        r.text()
    );
    // Esc after that is an ordinary Esc, not a second cancel.
    r.jobs.clear();
    r.key(K::Esc);
    assert!(r.jobs.is_empty(), "{:?}", r.jobs);
}

#[test]
fn the_ops_reach_the_hook_and_the_prompt_text_is_not_logged() {
    let (tx, _rx) = mpsc::channel(8);
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    let mut hook: Option<AccountHook> = Some(Box::new(move |op, _| {
        log.lock().unwrap().push(format!("{op:?}"));
        String::new()
    }));
    let mut note = String::new();
    for op in [AccountOp::CancelOauth, AccountOp::OpenUrl(URL.into())] {
        run_job(Job::Account(op), None, &mut None, &mut hook, &mut note, &tx);
    }
    assert_eq!(seen.lock().unwrap().len(), 2);

    // What the flow said is the user's to read; the graph and the summaries only count it.
    let m = Msg::AccountPrompt {
        account: "or1".into(),
        line: URL.into(),
    };
    assert!(
        !format!("{:?}", m.summary()).contains("openrouter.ai"),
        "{:?}",
        m.summary()
    );
    let mut r = started();
    said(&mut r, URL);
    assert!(!format!("{:?}", r.app.graph.read_key("model:log")).contains("openrouter.ai"));
}
