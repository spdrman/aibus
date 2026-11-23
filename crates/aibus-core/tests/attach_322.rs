//! #322 (Phase 1 spike): the attachment registry. An agent session that is already open attaches
//! to the live aibus session, pulls a step, and returns a result. Everything here is the rule
//! the screen and the socket rely on: opt-in, the keyboard's acceptance, one step at a time, a
//! deadline, a decline that is a result, nothing late accepted, and no loops.

use std::time::{Duration, Instant};

use aibus_core::attach::{Accepted, Attachments, Interrupt, Shown, StepState, MAX_LINEAGE};

fn pending_code(a: &mut Attachments, client: &str, now: Instant) -> String {
    let out = a
        .request_attach(client, None, &[], (Some(4123), Some("~/work/app")), now)
        .unwrap();
    assert_eq!(out["status"], "pending", "{out}");
    out["code"].as_str().unwrap().to_owned()
}

fn attached(now: Instant) -> Attachments {
    let mut a = Attachments::new(true, true);
    let code = pending_code(&mut a, "claude-code", now);
    a.accept(&code, now).unwrap();
    a
}

#[test]
fn attaching_needs_the_session_opt_in_and_the_keyboard() {
    let now = Instant::now();
    let mut off = Attachments::new(false, false);
    let why = off
        .request_attach("claude-code", None, &[], (None, None), now)
        .unwrap_err();
    assert!(why.contains("--control-attach"), "{why}");

    let mut a = Attachments::new(true, false);
    let code = pending_code(&mut a, "claude-code", now);
    assert_eq!(code.len(), 4);
    // Until the keyboard accepts, nothing is attached and no step can go.
    assert!(a.attached().is_none());
    assert!(a
        .dispatch("review", "look at it", Duration::from_secs(60), now)
        .unwrap_err()
        .contains("no attached session"));
    assert!(
        a.accept("ZZZZ", now).is_err(),
        "a wrong code accepts nothing"
    );
    assert!(a.attached().is_none());
    assert!(matches!(
        a.accept(&code.to_lowercase(), now),
        Ok(Accepted::Attached { .. })
    ));
    assert_eq!(a.attached().unwrap().1, "claude-code");
}

#[test]
fn an_unaccepted_request_times_out_and_changes_nothing() {
    let now = Instant::now();
    let mut a = Attachments::new(true, false);
    let code = pending_code(&mut a, "claude-code", now);
    let notes = a.expire(now + Duration::from_secs(61));
    assert!(notes.iter().any(|n| n.contains("timed out")), "{notes:?}");
    assert!(a.accept(&code, now + Duration::from_secs(62)).is_err());
    assert!(a.attached().is_none());
}

#[test]
fn a_denied_request_is_gone() {
    let now = Instant::now();
    let mut a = Attachments::new(true, false);
    let code = pending_code(&mut a, "claude-code", now);
    assert!(a.deny());
    assert!(a.accept(&code, now).is_err());
    assert!(!a.deny(), "nothing left to deny");
}

#[test]
fn one_attachment_at_a_time_and_a_second_client_is_refused() {
    let now = Instant::now();
    let mut a = attached(now);
    let why = a
        .request_attach("codex", None, &[], (None, None), now)
        .unwrap_err();
    assert!(why.contains("already attached"), "{why}");
    // The same client asking again just hears it is attached.
    let again = a
        .request_attach("claude-code", None, &[], (None, None), now)
        .unwrap();
    assert_eq!(again["status"], "attached");
}

#[test]
fn a_step_goes_out_through_a_pull_and_a_result_comes_back_with_evidence() {
    let now = Instant::now();
    let mut a = attached(now);
    let token = a
        .dispatch("review", "review the diff", Duration::from_secs(600), now)
        .unwrap();
    assert!(token.starts_with("st-"));
    assert_eq!(a.steps()[0].state, StepState::Queued);

    let pulled = a.next_step("claude-code", now).unwrap();
    assert_eq!(pulled["step"]["token"], token.as_str());
    assert_eq!(pulled["step"]["prompt"], "review the diff");
    assert_eq!(pulled["step"]["lineage"][0], a.lineage());
    assert_eq!(a.steps()[0].state, StepState::Delivered);
    // Nothing else is waiting.
    assert!(a.next_step("claude-code", now).unwrap()["step"].is_null());

    let done = a
        .step_result(
            "claude-code",
            &token,
            "done",
            "two findings",
            now + Duration::from_secs(3),
        )
        .unwrap();
    assert_eq!(a.steps()[0].state, StepState::Done);
    let ev = &done["evidence"];
    assert_eq!(ev["dispatched"]["visibility"], "observed");
    assert_eq!(ev["received"]["visibility"], "observed");
    assert_eq!(
        ev["started"]["visibility"], "unavailable",
        "the pull leg has no ack"
    );
    assert_eq!(ev["tool_calls"]["visibility"], "unavailable");
    assert_eq!(ev["summary"]["visibility"], "inferred");
    assert_eq!(ev["tokens"]["visibility"], "unavailable");
    assert_eq!(ev["tokens"]["quality"], "unknown");
    assert_eq!(ev["duration_ms"]["quality"], "derived");
}

#[test]
fn a_step_is_for_the_attachment_that_holds_it_and_one_is_in_flight_at_a_time() {
    let now = Instant::now();
    let mut a = attached(now);
    let first = a
        .dispatch("one", "do one", Duration::from_secs(60), now)
        .unwrap();
    let busy = a
        .dispatch("two", "do two", Duration::from_secs(60), now)
        .unwrap_err();
    assert!(busy.contains("busy") && busy.contains(&first), "{busy}");
    // Only the attached client pulls and answers.
    assert!(a.next_step("other", now).is_err());
    a.next_step("claude-code", now).unwrap();
    assert!(a.step_result("other", &first, "done", "x", now).is_err());
    assert!(a
        .step_result("claude-code", "st-NOPE", "done", "x", now)
        .is_err());
}

#[test]
fn the_agent_may_decline_and_it_is_a_result_not_a_retry() {
    let now = Instant::now();
    let mut a = attached(now);
    let token = a
        .dispatch("review", "do it", Duration::from_secs(60), now)
        .unwrap();
    a.next_step("claude-code", now).unwrap();
    a.step_result("claude-code", &token, "declined", "not in this repo", now)
        .unwrap();
    assert_eq!(a.steps()[0].state, StepState::Declined);
    assert_eq!(a.steps()[0].summary, "not in this repo");
    // Never handed to the same session again.
    assert!(a.next_step("claude-code", now).unwrap()["step"].is_null());
    // A status outside the three is refused.
    let t2 = a
        .dispatch("again", "x", Duration::from_secs(60), now)
        .unwrap();
    a.next_step("claude-code", now).unwrap();
    assert!(a
        .step_result("claude-code", &t2, "approved", "x", now)
        .is_err());
}

#[test]
fn a_used_token_is_refused() {
    let now = Instant::now();
    let mut a = attached(now);
    let token = a
        .dispatch("review", "do it", Duration::from_secs(60), now)
        .unwrap();
    a.next_step("claude-code", now).unwrap();
    a.step_result("claude-code", &token, "done", "ok", now)
        .unwrap();
    let why = a
        .step_result("claude-code", &token, "done", "again", now)
        .unwrap_err();
    assert!(why.contains("already"), "{why}");
}

#[test]
fn a_deadline_times_the_step_out_and_a_late_result_is_refused() {
    let now = Instant::now();
    let mut a = attached(now);
    let token = a
        .dispatch("review", "do it", Duration::from_secs(30), now)
        .unwrap();
    a.next_step("claude-code", now).unwrap();
    let late = now + Duration::from_secs(31);
    let notes = a.expire(late);
    assert!(notes.iter().any(|n| n.contains("timed_out")), "{notes:?}");
    assert_eq!(a.steps()[0].state, StepState::TimedOut);
    let why = a
        .step_result("claude-code", &token, "done", "too late", late)
        .unwrap_err();
    assert!(why.contains("timed_out"), "{why}");
    // The attachment is free for the next step.
    a.dispatch("next", "go", Duration::from_secs(30), late)
        .unwrap();
}

#[test]
fn a_late_result_is_refused_even_when_nobody_ticked_the_clock() {
    let now = Instant::now();
    let mut a = attached(now);
    let token = a
        .dispatch("review", "do it", Duration::from_secs(30), now)
        .unwrap();
    a.next_step("claude-code", now).unwrap();
    let why = a
        .step_result(
            "claude-code",
            &token,
            "done",
            "x",
            now + Duration::from_secs(99),
        )
        .unwrap_err();
    assert!(why.contains("timed_out"), "{why}");
}

#[test]
fn cancel_marks_the_step_and_refuses_the_result_and_queues_an_interrupt() {
    let now = Instant::now();
    let mut a = attached(now);
    let token = a
        .dispatch("review", "do it", Duration::from_secs(60), now)
        .unwrap();
    a.next_step("claude-code", now).unwrap();
    let said = a.cancel(now).unwrap();
    assert!(said.contains("interrupt queued"), "{said}");
    assert_eq!(a.steps()[0].state, StepState::Cancelled);
    assert!(a
        .step_result("claude-code", &token, "done", "x", now)
        .unwrap_err()
        .contains("cancelled"));
    assert!(a.cancel(now).is_err(), "nothing in flight");
}

#[test]
fn a_session_that_goes_away_fails_its_steps_as_unavailable_and_is_removed() {
    let now = Instant::now();
    let mut a = attached(now);
    a.dispatch("review", "do it", Duration::from_secs(60), now)
        .unwrap();
    a.next_step("claude-code", now).unwrap();
    let notes = a.detach("claude-code");
    assert!(!notes.is_empty());
    assert!(a.attached().is_none());
    assert_eq!(a.steps()[0].state, StepState::Unavailable);
    assert!(a.dispatch("x", "y", Duration::from_secs(60), now).is_err());
}

#[test]
fn a_step_in_flight_stops_its_own_session_from_starting_work() {
    let now = Instant::now();
    let mut a = attached(now);
    assert!(a.in_flight("claude-code").is_none());
    let token = a
        .dispatch("review", "do it", Duration::from_secs(60), now)
        .unwrap();
    let held = a
        .in_flight("claude-code")
        .expect("queued counts as in flight");
    assert_eq!(held.token, token);
    assert!(a.in_flight("someone else").is_none());
    a.next_step("claude-code", now).unwrap();
    a.step_result("claude-code", &token, "done", "ok", now)
        .unwrap();
    assert!(a.in_flight("claude-code").is_none());
}

#[test]
fn a_session_that_descends_from_this_one_cannot_attach_back() {
    let now = Instant::now();
    let mut a = Attachments::new(true, false);
    let me = a.lineage().to_owned();
    let why = a
        .request_attach("claude-code", None, &[me], (None, None), now)
        .unwrap_err();
    assert!(why.contains("would loop"), "{why}");
    // Any lineage at the bound is refused as well, whoever's it is: the depth is bounded.
    let deep: Vec<String> = (0..MAX_LINEAGE).map(|i| format!("lin-other{i}")).collect();
    let why = a
        .request_attach("claude-code", None, &deep, (None, None), now)
        .unwrap_err();
    assert!(why.contains("would loop") && why.contains("depth"), "{why}");
    assert!(a
        .request_attach("claude-code", None, &[], (None, None), now)
        .is_ok());
}

#[test]
fn the_first_present_asks_the_keyboard_and_later_ones_show_at_once() {
    let now = Instant::now();
    let mut off = Attachments::new(true, false);
    assert!(off
        .present("claude-code", "t", "m", &[], &[], now)
        .unwrap_err()
        .contains("--control-present"));

    let mut a = Attachments::new(false, true);
    let Shown::Pending { code } = a
        .present("claude-code", "plan", "three steps", &[], &[], now)
        .unwrap()
    else {
        panic!("the first use is confirmed first")
    };
    let Accepted::Present(p) = a.accept(&code, now).unwrap() else {
        panic!("accepting shows the held panel")
    };
    assert_eq!(
        (p.client.as_str(), p.title.as_str(), p.message.as_str()),
        ("claude-code", "plan", "three steps")
    );
    assert!(matches!(
        a.present("claude-code", "again", "m", &[], &[], now)
            .unwrap(),
        Shown::Now(_)
    ));
    // A client that reconnects is asked again.
    a.detach("claude-code");
    assert!(matches!(
        a.present("claude-code", "x", "m", &[], &[], now).unwrap(),
        Shown::Pending { .. }
    ));
}

#[test]
fn a_panel_is_plain_capped_text_with_no_control_characters() {
    let now = Instant::now();
    let mut a = Attachments::new(false, true);
    let Shown::Pending { code } = a.present("c", "t", "m", &[], &[], now).unwrap() else {
        panic!()
    };
    a.accept(&code, now).unwrap();
    let evil = "ok\x1b[2J\x07\u{202e}done\nline two";
    let cols = vec!["a\x1b[31m".to_owned()];
    let rows = vec![vec![evil.to_owned()]; 100];
    let Shown::Now(p) = a
        .present(
            "c",
            &"T".repeat(500),
            &"m".repeat(10_000),
            &cols,
            &rows,
            now,
        )
        .unwrap()
    else {
        panic!()
    };
    for text in std::iter::once(&p.title)
        .chain(&p.columns)
        .chain(p.rows.iter().flatten())
    {
        assert!(
            !text.chars().any(|c| c.is_control() || c == '\u{202e}'),
            "{text:?}"
        );
    }
    assert!(p
        .message
        .chars()
        .filter(|c| *c != '\n')
        .all(|c| !c.is_control()));
    assert!(p.title.chars().count() <= 80 && p.message.len() <= 4096 && p.rows.len() <= 30);
    assert!(p.rows[0][0].starts_with("ok") && p.rows[0][0].contains("done"));
}

// #346: step_ack, tool-call correlation and the interrupt, on the pull leg.

fn pulled(now: Instant) -> (Attachments, String) {
    let mut a = attached(now);
    let token = a
        .dispatch("review", "do it", Duration::from_secs(60), now)
        .unwrap();
    a.next_step("claude-code", now).unwrap();
    (a, token)
}

#[test]
fn a_step_ack_marks_the_step_started_once_and_shows_in_the_evidence() {
    let now = Instant::now();
    let (mut a, token) = pulled(now);
    assert_eq!(a.step_ack("claude-code", &token, now).unwrap()["state"], "started");
    assert!(a
        .step_ack("claude-code", &token, now)
        .unwrap_err()
        .contains("already acknowledged"));
    let out = a
        .step_result("claude-code", &token, "done", "ok", now + Duration::from_secs(2))
        .unwrap();
    assert_eq!(out["evidence"]["started"]["visibility"], "observed");
    assert_eq!(out["evidence"]["started"]["source"], "step_ack");
}

#[test]
fn a_step_ack_needs_the_holder_a_known_token_and_a_pulled_step() {
    let now = Instant::now();
    let mut a = attached(now);
    let token = a.dispatch("review", "x", Duration::from_secs(60), now).unwrap();
    assert!(a.step_ack("claude-code", &token, now).unwrap_err().contains("pull it"));
    a.next_step("claude-code", now).unwrap();
    assert!(a.step_ack("someone-else", &token, now).unwrap_err().contains("refused"));
    assert!(a.step_ack("claude-code", "st-nope", now).unwrap_err().contains("no step"));
    let mut off = Attachments::new(true, false);
    assert!(off.step_ack("claude-code", &token, now).unwrap_err().contains("not attached"));
}

#[test]
fn reported_tool_calls_are_correlated_by_token_capped_and_cleaned() {
    let now = Instant::now();
    let (mut a, token) = pulled(now);
    a.tool_call("claude-code", &token, "Read", now).unwrap();
    a.tool_call("claude-code", &token, "Ba\u{1b}[31msh", now).unwrap();
    for _ in 0..150 {
        a.tool_call("claude-code", &token, "Grep", now).unwrap();
    }
    assert!(a.tool_call("claude-code", "st-nope", "Read", now).unwrap_err().contains("no step"));
    let out = a.step_result("claude-code", &token, "done", "ok", now).unwrap();
    let ev = &out["evidence"]["tool_calls"];
    assert_eq!(ev["visibility"], "correlated", "{ev}");
    assert_eq!(ev["count"], 152);
    assert_eq!(ev["names"].as_array().unwrap().len(), 100, "names are capped, the count is not");
    assert_eq!(ev["names"][1], "Ba[31msh", "control characters are dropped");
    // A step that ended takes no more reports.
    assert!(a.tool_call("claude-code", &token, "Read", now).unwrap_err().contains("done"));
}

#[test]
fn a_step_with_no_reported_calls_says_unavailable_not_zero() {
    let now = Instant::now();
    let (mut a, token) = pulled(now);
    let out = a.step_result("claude-code", &token, "done", "ok", now).unwrap();
    assert_eq!(out["evidence"]["tool_calls"]["visibility"], "unavailable");
    assert_eq!(out["evidence"]["started"]["visibility"], "unavailable");
}

#[test]
fn a_cancel_after_the_pull_is_an_interrupt_the_session_sees_once() {
    let now = Instant::now();
    let (mut a, token) = pulled(now);
    a.step_ack("claude-code", &token, now).unwrap();
    assert_eq!(a.steps()[0].interrupt, Interrupt::None);
    a.cancel(now).unwrap();
    assert_eq!(a.steps()[0].interrupt, Interrupt::Pending);
    let seen = a.tool_call("claude-code", &token, "Read", now).unwrap();
    assert_eq!(seen["interrupt"], true, "{seen}");
    assert_eq!(a.steps()[0].interrupt, Interrupt::Seen);
    // Shown once; after that it is an ordinary refusal, and a result is still refused.
    assert!(a.tool_call("claude-code", &token, "Read", now).unwrap_err().contains("cancelled"));
    assert!(a.step_result("claude-code", &token, "done", "x", now).unwrap_err().contains("cancelled"));
}

#[test]
fn an_interrupt_can_also_arrive_with_next_step() {
    let now = Instant::now();
    let (mut a, token) = pulled(now);
    a.cancel(now).unwrap();
    let out = a.next_step("claude-code", now).unwrap();
    assert_eq!(out["interrupt"]["token"], token.as_str());
    assert!(a.next_step("claude-code", now).unwrap()["interrupt"].is_null());
}

#[test]
fn a_cancel_before_the_pull_has_nothing_to_interrupt() {
    let now = Instant::now();
    let mut a = attached(now);
    a.dispatch("review", "x", Duration::from_secs(60), now).unwrap();
    let said = a.cancel(now).unwrap();
    assert!(said.contains("before the session pulled it"), "{said}");
    assert_eq!(a.steps()[0].interrupt, Interrupt::None);
    assert!(a.next_step("claude-code", now).unwrap()["step"].is_null());
}
