//! C1.3: the frame snapshot suite (10 section 6). f01, f02, f03, f04, f09 and f21 at 98x46 from
//! one session, style snapshots of the cards, fork rows and status bar in f02 and f04, the
//! 80x24 collapsed rail, 120x50 with the ledger, `ascii`, `mono`, the permission modal, the
//! transcript collapsed and expanded, and the agents screen. Every frame is deterministic: the
//! animation comes from the injected tick counter.

mod common;

use aibus_core::state::value::{map, s};
use aibus_core::*;
use aibus_graph::Value;
use aibus_testkit::tui::{answer, poke, render_in, replay_app, run_id};
use aibus_testkit::{assert_frame, assert_styles};
use aibus_tui::Theme;
use common::*;

/// The session of the recording, near f02, with the advisor idle.
fn base() -> aibus_core::App<aibus_graph::mini::MiniGraph> {
    let mut app = replay_app();
    // `replay_app` ends with the advisor reading `before_plan`; each frame sets its own.
    app.step(Msg::AdvisorSpoke {
        checkpoint: Checkpoint::BeforePlan,
        advice: "split in 3 PRs".into(),
        tokens_read: 91_000,
    })
    .unwrap();
    for _ in 0..40 {
        app.step(Msg::Tick).unwrap();
    }
    app
}

fn status(
    app: &mut aibus_core::App<aibus_graph::mini::MiniGraph>,
    seq: u128,
    st: &str,
    p: Option<f64>,
) {
    poke(
        app,
        &format!("model:run:{}:status", run_id(seq)),
        Value::Str(st.into()),
    );
    if let Some(p) = p {
        poke(
            app,
            &format!("model:run:{}:progress", run_id(seq)),
            Value::F64(p),
        );
    }
}

fn frame(name: &str, mut app: aibus_core::App<aibus_graph::mini::MiniGraph>, styles: bool) {
    let f = render_in(&mut app, (98, 46), 0, Theme::recording());
    assert_frame!(f, name);
    if styles {
        assert_styles!(f, name);
    }
}

/// f01: `before_plan` is active and no router row is split.
#[test]
fn f01_before_plan_no_split() {
    let mut app = base();
    answer(&mut app, 1315, "retry_or_stop", 0.88, Verdict::Sharp);
    app.step(Msg::CheckpointHit {
        checkpoint: Checkpoint::BeforePlan,
        detail: String::new(),
    })
    .unwrap();
    frame("c13_f01", app, false);
}

/// f02: the same plan with a split row; the style snapshot covers cards, fork rows and the bar.
#[test]
fn f02_split_row() {
    let mut app = base();
    app.step(Msg::CheckpointHit {
        checkpoint: Checkpoint::BeforePlan,
        detail: String::new(),
    })
    .unwrap();
    frame("c13_f02", app, true);
}

/// f03: search finished.
#[test]
fn f03_search_done() {
    let mut app = base();
    status(&mut app, 2, "completed", Some(1.0));
    frame("c13_f03", app, false);
}

/// f04: `same_error_x2` with the search card's border in the advisor colour.
#[test]
fn f04_same_error_twice_purple_search_card() {
    let mut app = base();
    poke(
        &mut app,
        &format!("model:run:{}:error", run_id(2)),
        map([
            ("kind", s("provider")),
            ("message", s("boom")),
            ("retryable", Value::Bool(true)),
        ]),
    );
    status(&mut app, 2, "failed", None);
    app.step(Msg::CheckpointHit {
        checkpoint: Checkpoint::SameErrorX2,
        detail: String::new(),
    })
    .unwrap();
    frame("c13_f04", app, true);
}

/// f09: `before_done`, the return box in the advisor colour.
#[test]
fn f09_before_done_purple_return_box() {
    let mut app = base();
    for seq in 2..=4 {
        status(&mut app, seq, "completed", Some(1.0));
    }
    app.step(Msg::CheckpointHit {
        checkpoint: Checkpoint::BeforeDone,
        detail: String::new(),
    })
    .unwrap();
    frame("c13_f09", app, false);
}

/// f21: the audit card done and the others still going.
#[test]
fn f21_audit_done() {
    frame("c13_f21", base(), false);
}

#[test]
fn collapsed_rail_at_80x24_and_the_ledger_at_120x50() {
    let mut app = base();
    assert_frame!(
        render_in(&mut app, (80, 24), 0, Theme::recording()),
        "c13_80x24_rail_collapsed"
    );
    app.step(Msg::Command(Command::LedgerUpdate {
        section: "goal".into(),
        text: "ship the auth fix".into(),
    }))
    .unwrap();
    app.step(Msg::Command(Command::LedgerUpdate {
        section: "now".into(),
        text: "tests".into(),
    }))
    .unwrap();
    assert_frame!(
        render_in(&mut app, (120, 50), 0, Theme::recording()),
        "c13_120x50_ledger"
    );
}

#[test]
fn ascii_and_mono() {
    let mut ascii = Theme::recording();
    ascii.ascii = true;
    let mut app = base();
    let f = render_in(&mut app, (98, 46), 0, ascii);
    assert!(f.text.is_ascii(), "ascii = true draws nothing but ASCII");
    assert_frame!(f, "c13_ascii");
    let mut app = base();
    let f = render_in(&mut app, (98, 46), 0, Theme::mono());
    assert_frame!(f, "c13_mono");
    assert_styles!(f, "c13_mono");
}

fn rig_with_tools(theme: Theme) -> Rig {
    Rig::new(tool_app(), (98, 46), theme)
}

#[test]
fn transcript_collapsed_and_expanded() {
    let mut r = rig_with_tools(Theme::recording());
    let text = r.text();
    assert!(
        text.contains("▸ bash  cargo test  (1.2 s, 4 B)")
            || text.contains("▸ bash  cargo test  (1.2 s,"),
        "{text}"
    );
    assert!(text.contains("⌁ compacted at 167k"), "{text}");
    assert!(
        text.contains("lead · sonnet · acct"),
        "the account is in the header: {text}"
    );
    assert_frame!(r.frame(), "c13_transcript_collapsed");
    r.key(crossterm::event::KeyCode::Tab);
    r.key(crossterm::event::KeyCode::Enter);
    assert!(r.text().contains("▾ bash"), "{}", r.text());
    assert!(r.text().contains("test result: ok"), "{}", r.text());
    assert_frame!(r.frame(), "c13_transcript_expanded");
}

#[test]
fn permission_modal() {
    let mut app = session();
    ask(&mut app, 1, "lead", 7, "bash: cargo test -p server auth::");
    let r = Rig::new(app, (98, 46), Theme::recording());
    assert_frame!(r.frame(), "c13_permission");
    assert_styles!(r.frame(), "c13_permission");
}

#[test]
fn agents_screen() {
    let mut app = session();
    let defs = common::defs(&["search", "grep", "audit"]);
    app.step(Msg::AgentsReloaded {
        defs,
        warnings: vec![],
    })
    .unwrap();
    app.step(Msg::Command(Command::OpenScreen(Screen::Agents)))
        .unwrap();
    let r = Rig::new(app, (98, 46), Theme::recording());
    assert_frame!(r.frame(), "c13_agents");
}
