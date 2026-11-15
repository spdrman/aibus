//! #322 (Phase 1 spike): an open agent session attached to this aibus session. The attachment
//! and the first `session_present` are accepted at the keyboard (a typed `/attach accept <code>`,
//! which a client cannot send), a step goes out through a pull and comes back as a result, the
//! presented panel is inert and badged, and nothing here opens a screen with live keys.

use std::time::{Duration, Instant};

mod common;

use aibus_core::attach::Attachments;
use aibus_core::remote::{Ask, Remote};
use aibus_core::state::value::{as_str, get};
use aibus_graph::Graph;
use aibus_testkit::tui::run_id;
use aibus_tui::input::Context;
use aibus_tui::{remote, Theme};
use common::*;
use crossterm::event::KeyCode as K;
use serde_json::Value as Json;

const SIZE: (u16, u16) = (120, 40);

fn rig(attach: bool, present: bool) -> Rig {
    let mut r = Rig::new(session(), SIZE, Theme::recording());
    r.ui.attach = Attachments::new(attach, present);
    r
}

fn deliver(r: &mut Rig, event: Remote) {
    remote::handle(&mut r.app, &mut r.ui, event, false, &mut |app, m| {
        app.step(m)
    })
    .unwrap();
    r.settle();
}

fn call(r: &mut Rig, ask: Ask) -> Result<Json, String> {
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    deliver(
        r,
        Remote::Call {
            client: "editor".into(),
            ask,
            reply: tx,
        },
    );
    rx.try_recv().expect("every call is answered at once")
}

fn connect(r: &mut Rig) {
    deliver(
        r,
        Remote::Connected {
            client: "editor".into(),
        },
    );
}

fn type_line(r: &mut Rig, line: &str) -> Result<String, String> {
    let out = remote::keyboard(
        line,
        &mut r.app,
        &mut r.ui,
        Instant::now(),
        &mut |app, m| app.step(m),
    )
    .expect("an /attach line is the keyboard's");
    r.settle();
    out
}

fn attach_ask() -> Ask {
    Ask::Attach {
        label: None,
        lineage: vec![],
        pid: Some(4123),
        cwd: Some("~/work/app".into()),
    }
}

fn log_text(r: &Rig) -> String {
    r.app
        .store
        .log_rows(200)
        .iter()
        .filter_map(|row| get(row, "text").and_then(as_str).map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The code the screen shows (the last `code XXXX` of the log): the agent is never given it.
fn screen_code(r: &Rig) -> String {
    let log = log_text(r);
    let at = log.rfind("code ").expect("a code on screen") + 5;
    log[at..at + 4].to_owned()
}

fn attached(r: &mut Rig) {
    connect(r);
    let out = call(r, attach_ask()).unwrap();
    assert!(
        out.get("code").is_none(),
        "the agent is not given the code: {out}"
    );
    let code = screen_code(r);
    type_line(r, &format!("/attach accept {code}")).unwrap();
}

fn present_ask(title: &str) -> Ask {
    Ask::Present {
        title: title.into(),
        message: "three steps".into(),
        columns: vec!["step".into()],
        rows: vec![vec!["one".into()]],
    }
}

#[test]
fn nothing_attaches_without_the_session_opt_in() {
    let mut r = rig(false, false);
    connect(&mut r);
    let why = call(&mut r, attach_ask()).unwrap_err();
    assert!(why.contains("--control-attach"), "{why}");
    let why = call(&mut r, present_ask("t")).unwrap_err();
    assert!(why.contains("--control-present"), "{why}");
}

#[test]
fn an_attach_request_shows_a_code_and_only_the_keyboard_can_accept_it() {
    let mut r = rig(true, false);
    connect(&mut r);
    let out = call(&mut r, attach_ask()).unwrap();
    assert!(
        out.get("code").is_none(),
        "the agent is not given the code: {out}"
    );
    let code = screen_code(&r);
    let log = log_text(&r);
    assert!(
        log.contains("wants to attach") && log.contains(&code) && log.contains("4123"),
        "{log}"
    );
    assert!(!r.text().contains("attached"), "pending is not attached");
    // A client cannot answer its own request, by any door it has.
    let refused = call(
        &mut r,
        Ask::Command {
            line: format!("/attach accept {code}"),
        },
    )
    .unwrap_err();
    assert!(refused.contains("no command /attach"), "{refused}");
    assert!(r.ui.attach.attached().is_none());
    // A wrong code from the keyboard changes nothing; the right one attaches.
    assert!(type_line(&mut r, "/attach accept ZZZZ").is_err());
    assert!(r.ui.attach.attached().is_none());
    type_line(&mut r, &format!("/attach accept {code}")).unwrap();
    assert!(r.ui.attach.attached().is_some());
    assert!(
        r.text().contains("attached"),
        "the status bar says so:\n{}",
        r.text()
    );
}

#[test]
fn a_step_goes_out_and_the_result_comes_back_tagged_in_the_log() {
    let mut r = rig(true, false);
    attached(&mut r);
    let said = type_line(&mut r, "/attach step review the diff in ~/work/app").unwrap();
    assert!(said.contains("st-"), "{said}");
    let step = call(&mut r, Ask::NextStep).unwrap();
    let token = step["step"]["token"].as_str().unwrap().to_owned();
    assert!(step["step"]["prompt"]
        .as_str()
        .unwrap()
        .contains("review the diff"));
    let done = call(
        &mut r,
        Ask::StepResult {
            token: token.clone(),
            status: "done".into(),
            summary: "two findings".into(),
        },
    )
    .unwrap();
    assert_eq!(done["evidence"]["summary"]["visibility"], "inferred");
    let log = log_text(&r);
    assert!(
        log.contains("attached (editor)") && log.contains("two findings"),
        "{log}"
    );
    assert!(r
        .app
        .store
        .log_rows(200)
        .iter()
        .any(|row| get(row, "via").and_then(as_str) == Some("mcp")));
    // A second use of the token is refused and the refusal is on screen.
    assert!(call(
        &mut r,
        Ask::StepResult {
            token,
            status: "done".into(),
            summary: "again".into()
        }
    )
    .is_err());
}

#[test]
fn a_step_in_flight_stops_the_attached_client_from_prompting_the_lead() {
    let mut r = rig(true, false);
    attached(&mut r);
    type_line(&mut r, "/attach step review").unwrap();
    call(&mut r, Ask::NextStep).unwrap();
    let before = r.app.store.log_rows(200).len();
    for ask in [
        Ask::Send {
            text: "hello".into(),
        },
        Ask::Command {
            line: "/model haiku".into(),
        },
    ] {
        let why = call(&mut r, ask).unwrap_err();
        assert!(why.contains("would loop (step st-"), "{why}");
    }
    assert!(
        r.app.store.log_rows(200).len() > before,
        "the refusals are rows too"
    );
    assert!(
        as_str_runs(&r).is_empty(),
        "no run was started on the client's behalf"
    );
}

fn as_str_runs(r: &Rig) -> Vec<String> {
    // The rig's session has one lead run (seq 1); a client line would have added another.
    use aibus_core::state::value::as_list;
    use aibus_core::state::RUNS_ORDER;
    as_list(r.app.graph.read_key(RUNS_ORDER).unwrap())
        .iter()
        .filter_map(as_str)
        .filter(|id| *id != run_id(1).to_string())
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_session_that_disconnects_mid_step_makes_the_step_unavailable() {
    let mut r = rig(true, false);
    attached(&mut r);
    type_line(&mut r, "/attach step review").unwrap();
    call(&mut r, Ask::NextStep).unwrap();
    deliver(
        &mut r,
        Remote::Disconnected {
            client: "editor".into(),
        },
    );
    assert!(log_text(&r).contains("unavailable"), "{}", log_text(&r));
    assert!(r.ui.attach.attached().is_none());
    assert!(
        r.ui.remote.is_none(),
        "the status bar no longer names the client"
    );
}

#[test]
fn a_step_can_be_cancelled_from_the_keyboard_and_a_late_result_is_refused() {
    let mut r = rig(true, false);
    attached(&mut r);
    type_line(&mut r, "/attach step review").unwrap();
    let step = call(&mut r, Ask::NextStep).unwrap();
    let token = step["step"]["token"].as_str().unwrap().to_owned();
    let said = type_line(&mut r, "/attach cancel").unwrap();
    assert!(said.contains("session not interruptible"), "{said}");
    let why = call(
        &mut r,
        Ask::StepResult {
            token,
            status: "done".into(),
            summary: "late".into(),
        },
    )
    .unwrap_err();
    assert!(why.contains("cancelled"), "{why}");
}

#[test]
fn the_first_present_waits_for_the_keyboard_then_shows_an_inert_badged_panel() {
    let mut r = rig(false, true);
    connect(&mut r);
    let focus = r.app.graph.read_key(aibus_core::state::FOCUS).cloned();
    let out = call(&mut r, present_ask("the plan")).unwrap();
    assert_eq!(out["status"], "pending");
    assert!(
        out.get("code").is_none(),
        "the agent is not given the code: {out}"
    );
    assert!(
        !r.text().contains("the plan"),
        "nothing is drawn before the keyboard says yes"
    );
    let code = screen_code(&r);
    type_line(&mut r, &format!("/attach accept {code}")).unwrap();
    let screen = r.text();
    assert!(
        screen.contains("the plan")
            && screen.contains("three steps")
            && screen.contains("mcp editor"),
        "{screen}"
    );
    // It took no focus and opened no screen.
    assert_eq!(
        r.app.graph.read_key(aibus_core::state::FOCUS).cloned(),
        focus
    );
    assert!(Context::read(&r.app.graph).chrome.modal.is_none());
    // Esc closes it.
    let ev = crossterm::event::Event::Key(crossterm::event::KeyEvent::from(K::Esc));
    let mut input = aibus_tui::input::Input::default();
    let mut ctrl_c = aibus_tui::app::CtrlC::default();
    aibus_tui::app::terminal_events(
        vec![ev],
        &mut r.app,
        &mut r.ui,
        &mut input,
        &mut ctrl_c,
        false,
        Instant::now(),
        &mut |app, m| app.step(m),
    )
    .unwrap();
    r.settle();
    assert!(!r.text().contains("the plan"), "{}", r.text());
    // Later ones show at once.
    let again = call(&mut r, present_ask("second")).unwrap();
    assert_eq!(again["status"], "shown");
    assert!(r.text().contains("second"));
}

#[test]
fn a_panel_never_opens_over_what_the_user_is_doing() {
    let mut r = rig(false, true);
    connect(&mut r);
    let _ = call(&mut r, present_ask("first")).unwrap();
    let code = screen_code(&r);
    type_line(&mut r, &format!("/attach accept {code}")).unwrap();
    r.ui.presented = None;
    r.type_text("half a prompt");
    let why = call(&mut r, present_ask("busy one")).unwrap_err();
    assert!(why.contains("busy") && why.contains("typing"), "{why}");
    assert!(!r.text().contains("busy one"));
}

#[test]
fn a_typed_attach_line_never_reaches_the_lead_and_a_bad_one_says_why() {
    let mut r = rig(true, false);
    let before = as_str_runs(&r);
    assert!(type_line(&mut r, "/attach accept").is_err());
    assert!(type_line(&mut r, "/attach step go")
        .unwrap_err()
        .contains("no attached session"));
    assert!(type_line(&mut r, "/attach bogus").is_err());
    assert_eq!(as_str_runs(&r), before);
    assert!(
        remote::keyboard(
            "/model haiku",
            &mut r.app,
            &mut r.ui,
            Instant::now(),
            &mut |app, m| app.step(m)
        )
        .is_none(),
        "other lines take the prompt's path"
    );
    let _ = Duration::ZERO;
}

#[test]
fn a_result_with_escape_and_bidi_characters_is_clean_in_the_stored_log() {
    let mut r = rig(true, false);
    attached(&mut r);
    type_line(&mut r, "/attach step go").unwrap();
    let step = call(&mut r, Ask::NextStep).unwrap();
    let token = step["step"]["token"].as_str().unwrap().to_owned();
    call(
        &mut r,
        Ask::StepResult {
            token,
            status: "done".into(),
            summary: "ok\x1b[2J\u{202e}evil\x07".into(),
        },
    )
    .unwrap();
    let log = log_text(&r);
    assert!(log.contains("okevil") || log.contains("ok[2J"), "{log:?}");
    assert!(
        !log.chars()
            .any(|c| c == '\x1b' || c == '\x07' || c == '\u{202e}'),
        "{log:?}"
    );
}
