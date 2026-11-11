//! C1.2: every panel of the recorded screen as a function of its `view:` node (05 section 2).
//! The views are read from the graph, the frames from a `TestBackend`; nothing here touches a tty.

mod common;

use std::collections::BTreeSet;

use aibus_core::state::value::as_list;
use aibus_core::*;
use aibus_graph::{Graph, Value};
use aibus_testkit::tui::{answer, replay_app, run_id};
use aibus_tui::hit::HitId;
use aibus_tui::view;
use aibus_tui::Theme;
use common::*;

const TH: Theme = Theme::recording();

fn rig(app: aibus_core::App<aibus_graph::mini::MiniGraph>, size: (u16, u16)) -> Rig {
    Rig::new(app, size, TH)
}

fn keys(v: &Value) -> BTreeSet<String> {
    match v {
        Value::Map(m) => m.keys().map(|k| k.to_string()).collect(),
        other => panic!("not a map: {other:?}"),
    }
}

fn want(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| (*s).to_owned()).collect()
}

fn view_of(app: &aibus_core::App<aibus_graph::mini::MiniGraph>, key: &str) -> Value {
    app.graph.read_key(key).unwrap_or_else(|| panic!("{key} is not registered")).clone()
}

// ---- the views ------------------------------------------------------------------------------

#[test]
fn the_views_have_the_shapes_of_05_section_2() {
    let mut app = replay_app();
    log_row(&mut app, 1_700_000_000_000, "router", "which file → src/auth.ts", vec![]);
    assert_eq!(
        keys(&view_of(&app, view::LEAD_BOX)),
        want(&["model_label", "label", "effort", "model_flag", "highlight"])
    );
    assert_eq!(
        keys(&view_of(&app, view::DELEGATE_ROW)),
        want(&["swarm_model_label", "connectors"])
    );
    assert_eq!(
        keys(&as_list(&view_of(&app, view::CARDS))[0]),
        want(&[
            "run_id", "name", "model_label", "progress", "label", "status", "spinner_phase",
            "border", "selected"
        ])
    );
    assert_eq!(
        keys(&view_of(&app, view::RETURN_BOX)),
        want(&["lead_model_label", "highlight", "pulse"])
    );
    assert_eq!(
        keys(&view_of(&app, view::RAIL)),
        want(&["title", "subtitle", "checkpoints", "advice", "typing_cursor", "calls", "tokens_read", "status"])
    );
    assert_eq!(
        keys(&view_of(&app, view::FORK_LAYER)),
        want(&["title", "forks", "rows", "any_split", "enabled"])
    );
    let line = as_list(&view_of(&app, view::LOG_LINES)).last().unwrap().clone();
    let k = keys(&line);
    for name in ["time", "actor", "actor_colour", "text", "p", "verdict"] {
        assert!(k.contains(name), "{name} is in {k:?}");
    }
    let status = keys(&view_of(&app, view::STATUS_BAR));
    for name in ["swarm", "subagents", "advisor", "router", "ledger", "alerts"] {
        assert!(status.contains(name), "{name} is in {status:?}");
    }
}

// ---- the lead box ---------------------------------------------------------------------------

#[test]
fn effort_is_blocks_low_1_medium_2_high_4_max_4_plus() {
    for (effort, shown) in [
        ("low", "effort ▮··· low"),
        ("medium", "effort ▮▮·· medium"),
        ("high", "effort ▮▮▮▮ high"),
        ("max", "effort ▮▮▮▮+ max"),
    ] {
        let mut app = session();
        meta(&mut app, 1, "effort", Value::Str(effort.into()));
        let r = rig(app, (98, 46));
        assert!(r.text().contains(shown), "{effort}: {}", r.text());
    }
}

#[test]
fn the_model_flag_is_dim_and_right_aligned_and_missing_on_the_default() {
    let r = rig(session(), (98, 46));
    let (x, y) = r.find("--model sonnet").expect("the flag is shown");
    assert_eq!(r.cell(x, y).fg, TH.dim_text);
    let lead = r.text().lines().nth(usize::from(y)).unwrap().to_owned();
    assert!(lead.trim_end().ends_with("--model sonnet │"), "{lead}");

    let mut app = session();
    app.set_session_view(None, (Some("opus 5.5"), true), (Some("jev"), true)).unwrap();
    assert!(!rig(app, (98, 46)).text().contains("--model sonnet │"), "the default shows no flag");
}

// ---- the cards ------------------------------------------------------------------------------

fn three_running() -> aibus_core::App<aibus_graph::mini::MiniGraph> {
    let mut app = session();
    worker(&mut app, 2, "search", "maps the repo", Some(0.71), "running");
    worker(&mut app, 3, "grep", "finds call sites", Some(0.35), "running");
    worker(&mut app, 4, "audit", "runs in a worktree", None, "running");
    app
}

#[test]
fn a_fraction_is_a_bar_and_a_percentage_and_an_unknown_one_a_bouncing_block() {
    let r = rig(three_running(), (98, 46));
    assert!(r.text().contains("71%") && r.text().contains("35%"), "{}", r.text());
    let blocks = |r: &Rig| {
        let card = r.rect(HitId::Card(2));
        let row = r.text().lines().nth(usize::from(card.y) + 2).unwrap().to_owned();
        row.chars().skip(usize::from(card.x) + 2).take(10).collect::<String>()
    };
    // The indeterminate card has a 2-cell block that moves one cell per tick and turns at the end.
    let mut seen = Vec::new();
    let mut r = r;
    for _ in 0..20 {
        let b = blocks(&r);
        assert_eq!(b.matches('█').count(), 2, "{b:?}");
        seen.push(b.find('█').unwrap());
        ticks(&mut r.app, 1);
        r.settle();
    }
    let steps: Vec<i64> = seen.windows(2).map(|w| w[1] as i64 - w[0] as i64).collect();
    assert!(steps.iter().all(|s| s.abs() == 1), "one cell per tick: {seen:?}");
    assert!(seen.contains(&0) && seen.contains(&8), "end to end: {seen:?}");
    assert!(steps.contains(&1) && steps.contains(&-1), "and back: {seen:?}");
}

#[test]
fn a_done_card_is_a_full_bar_and_a_green_done_that_stays() {
    let mut app = session();
    worker(&mut app, 2, "audit", "runs in a worktree", Some(1.0), "completed");
    let mut r = rig(app, (98, 46));
    assert!(r.text().contains("100%"));
    assert!(r.text().contains("██████████ 100%"), "{}", r.text());
    assert_eq!(r.cell_of("✓ done").fg, TH.ok);
    ticks(&mut r.app, 120);
    r.settle();
    assert!(r.text().contains("✓ done"), "done stays until it is dismissed or replaced");
}

#[test]
fn the_spinner_turns_per_tick_and_each_card_starts_somewhere_else() {
    let mut r = rig(three_running(), (98, 46));
    let glyph = |r: &Rig, card: usize| {
        let c = r.rect(HitId::Card(card));
        let row = r.text().lines().nth(usize::from(c.y) + 5).unwrap().to_owned();
        row.chars().nth(usize::from(c.x) + 2).unwrap()
    };
    let mut cycle = Vec::new();
    for _ in 0..4 {
        cycle.push(glyph(&r, 0));
        ticks(&mut r.app, 1);
        r.settle();
    }
    let mut sorted = cycle.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 4, "all four phases in four ticks: {cycle:?}");
    assert!(cycle.iter().all(|g| "◐◓◑◒".contains(*g)), "{cycle:?}");
    // The phase is seeded by the run id, so the three cards are not in step.
    let now: Vec<char> = (0..3).map(|i| glyph(&r, i)).collect();
    assert!(now[0] != now[1] || now[1] != now[2], "{now:?}");
}

#[test]
fn more_than_three_cards_scroll_with_edge_markers_and_h_l() {
    let mut app = three_running();
    worker(&mut app, 5, "lint", "checks style", Some(0.1), "running");
    worker(&mut app, 6, "docs", "writes docs", Some(0.2), "running");
    let mut r = rig(app, (98, 46));
    assert!(r.text().contains("2 more ▸"), "{}", r.text());
    assert!(!r.text().contains('◂'), "nothing to the left yet");
    assert_eq!(r.ui.drawn.cards, 5);
    r.key(crossterm::event::KeyCode::Tab);
    assert_eq!(r.int(aibus_core::state::SELECTED_CARD), Some(0));
    for _ in 0..3 {
        r.press('l');
    }
    // The window follows the selection: card 3 is last in view of 1..=3.
    assert!(r.text().contains("◂ 1 more"), "{}", r.text());
    assert!(r.text().contains("1 more ▸"), "{}", r.text());
    for _ in 0..4 {
        r.press('l');
    }
    assert!(r.text().contains("◂ 2 more"), "{}", r.text());
    assert!(!r.text().contains("more ▸"), "nothing to the right any more");
    for _ in 0..6 {
        r.press('h');
    }
    assert!(!r.text().contains('◂'), "back at the start");
}

#[test]
fn fewer_than_three_cards_are_centred() {
    let mut app = session();
    worker(&mut app, 2, "search", "maps the repo", Some(0.5), "running");
    let r = rig(app, (98, 46));
    let card = r.rect(HitId::Card(0));
    let centre = r.ui.drawn.areas.centre;
    let (left, right) = (card.x - centre.x, centre.right() - card.right());
    assert!(left.abs_diff(right) <= 1, "{left} left of it, {right} right of it");
    assert_eq!(card.width, 19, "19 columns wide");
}

#[test]
fn zero_cards_draw_no_card_row_and_no_markers() {
    let r = rig(session(), (98, 46));
    assert_eq!(r.ui.drawn.cards, 0);
    assert!(!r.text().contains("more"));
    assert!(!r.text().contains("running"));
}

// ---- connectors and the purple borders -------------------------------------------------------

fn dots(r: &Rig, fg: ratatui::style::Color) -> Vec<(u16, u16)> {
    let mut out = Vec::new();
    for y in 0..r.term.backend().buffer().area.height {
        for x in 0..r.term.backend().buffer().area.width {
            let c = r.cell(x, y);
            if c.symbol() == "●" && c.fg == fg {
                out.push((x, y));
            }
        }
    }
    out
}

#[test]
fn a_dot_travels_one_cell_per_tick_down_the_path_to_a_working_card() {
    // Only the leftmost of three cards is working, so the path to it is the long one.
    let mut app = session();
    worker(&mut app, 2, "search", "maps the repo", Some(0.5), "running");
    worker(&mut app, 3, "grep", "finds call sites", Some(1.0), "completed");
    worker(&mut app, 4, "audit", "runs in a worktree", Some(1.0), "completed");
    let mut r = rig(app, (98, 46));
    let mut at = Vec::new();
    for _ in 0..24 {
        let d = dots(&r, TH.swarm);
        assert_eq!(d.len(), 1, "one dot on one working card: {d:?}");
        at.push(d[0]);
        ticks(&mut r.app, 1);
        r.settle();
    }
    for w in at.windows(2) {
        let (a, b) = (w[0], w[1]);
        let step = a.0.abs_diff(b.0) + a.1.abs_diff(b.1);
        // One cell along the path, or the path starting over; a cell is also skipped when a
        // box is in the way, so allow a gap of two.
        assert!(step <= 2 || b.1 < a.1, "{a:?} -> {b:?}");
    }
    assert!(at.iter().collect::<BTreeSet<_>>().len() > 15, "it moves along the path: {at:?}");
    // The rest of the path is dotted.
    assert!(r.text().contains('·'));
}

#[test]
fn no_dot_travels_to_a_card_that_is_done() {
    let mut app = session();
    worker(&mut app, 2, "audit", "runs in a worktree", Some(1.0), "completed");
    let r = rig(app, (98, 46));
    assert!(dots(&r, TH.swarm).is_empty());
}

#[test]
fn the_arrowhead_pulses_while_a_result_is_in_flight_and_not_otherwise() {
    let head = |r: &Rig| r.cell_of("▾");
    // Quiet: steady.
    let mut r = rig(three_running(), (98, 46));
    let first = head(&r).fg;
    ticks(&mut r.app, 4);
    r.settle();
    assert_eq!(head(&r).fg, first, "steady while nothing is in flight");
    // A worker has just ended while the lead is still going.
    let mut app = three_running();
    poke_status(&mut app, 1, "running");
    meta(&mut app, 2, "ended_at", Value::I64(1_700_000_000_000 - 500));
    let mut r = rig(app, (98, 46));
    let (a, mut b) = (head(&r).fg, head(&r).fg);
    for _ in 0..8 {
        ticks(&mut r.app, 1);
        r.settle();
        b = if head(&r).fg != a { head(&r).fg } else { b };
    }
    assert_ne!(a, b, "the head blinks");
    assert!([TH.lead, TH.dim_text].contains(&a) && [TH.lead, TH.dim_text].contains(&b));
}

fn poke_status(app: &mut aibus_core::App<aibus_graph::mini::MiniGraph>, seq: u128, st: &str) {
    aibus_testkit::tui::poke(app, &format!("model:run:{}:status", run_id(seq)), Value::Str(st.into()));
}

#[test]
fn borders_turn_purple_for_the_checkpoint_they_belong_to() {
    let hit = |checkpoint: Checkpoint| {
        let mut app = session();
        worker(&mut app, 2, "search", "maps the repo", Some(0.5), "running");
        app.step(Msg::RunFailed {
            run: run_id(2),
            error: RunError { kind: RunErrorKind::Tool, message: "e".into(), retryable: true },
        })
        .unwrap();
        app.step(Msg::CheckpointHit { checkpoint, detail: String::new() }).unwrap();
        rig(app, (98, 46))
    };
    // before_plan: the lead box (its top border row carries the title).
    let r = hit(Checkpoint::BeforePlan);
    assert_eq!(r.cell_of("┌ Sonnet 5.5 · main session").fg, TH.advisor);
    assert_eq!(r.cell_of("┌ back to").fg, TH.lead);
    // same_error_x2: the failing worker's card.
    let r = hit(Checkpoint::SameErrorX2);
    let card = r.rect(HitId::Card(0));
    assert_eq!(r.cell(card.x, card.y).fg, TH.advisor);
    assert_eq!(r.cell_of("┌ Sonnet 5.5 · main session").fg, TH.lead);
    // before_done: the box that takes the results back.
    let r = hit(Checkpoint::BeforeDone);
    assert_eq!(r.cell_of("┌ back to").fg, TH.advisor);
    assert_eq!(r.cell(r.rect(HitId::Card(0)).x, r.rect(HitId::Card(0)).y).fg, TH.swarm);
}

#[test]
fn the_active_checkpoint_is_joined_to_its_box_by_a_path_with_a_travelling_dot() {
    let mut app = session();
    worker(&mut app, 2, "search", "maps the repo", Some(0.5), "running");
    app.step(Msg::CheckpointHit { checkpoint: Checkpoint::BeforePlan, detail: String::new() }).unwrap();
    let mut r = rig(app, (98, 46));
    let mut at = BTreeSet::new();
    for _ in 0..6 {
        let d = dots(&r, TH.advisor);
        assert_eq!(d.len(), 1, "{d:?}");
        at.insert(d[0]);
        ticks(&mut r.app, 1);
        r.settle();
    }
    assert!(at.len() >= 5, "{at:?}");
}

// ---- the rail --------------------------------------------------------------------------------

#[test]
fn the_rail_types_its_advice_one_character_per_tick_and_f31_is_reproducible() {
    let mut app = session();
    ticks(&mut app, 7);
    app.step(Msg::AdvisorSpoke {
        checkpoint: Checkpoint::SameErrorX2,
        advice: "wrong fixture".into(),
        tokens_read: 1000,
    })
    .unwrap();
    let mut r = rig(app, (98, 46));
    assert!(r.text().lines().any(|l| l.contains("│ »")), "starts as a bare »");
    assert!(!r.text().contains("wrong"), "nothing typed yet");
    ticks(&mut r.app, 10);
    r.settle();
    assert!(r.text().contains("» wrong fixt"), "{}", r.text());
    assert!(!r.text().contains("wrong fixtu"));
    ticks(&mut r.app, 1);
    r.settle();
    assert!(r.text().contains("» wrong fixtu"));
    ticks(&mut r.app, 30);
    r.settle();
    assert!(r.text().contains("» wrong fixture"));
}

#[test]
fn tokens_read_is_in_thousands_up_to_1020k() {
    let mut app = session();
    for k in [122_000, 898_000] {
        app.step(Msg::AdvisorSpoke { checkpoint: Checkpoint::BeforePlan, advice: "x".into(), tokens_read: k })
            .unwrap();
    }
    ticks(&mut app, 5);
    let r = rig(app, (98, 46));
    assert!(r.text().lines().any(|l| l.contains("tokens read") && l.contains("1020k")), "{}", r.text());
    let r2 = rig(replay_app(), (98, 46));
    assert!(r2.text().lines().any(|l| l.contains("tokens read") && l.contains("122k")), "{}", r2.text());
}

#[test]
fn the_subtitle_follows_the_advisor_on_call_reading_advising_off() {
    let sub = |r: &Rig| {
        ["advisor · on call", "advisor · reading", "advisor · advising", "advisor · off"]
            .into_iter()
            .find(|s| r.text().contains(s))
            .map(str::to_owned)
    };
    // Each stage is a fresh session taken that far.
    let stage = |n: u8| {
        let mut app = session();
        if n >= 1 {
            app.step(Msg::CheckpointHit { checkpoint: Checkpoint::BeforePlan, detail: String::new() }).unwrap();
        }
        if n >= 2 {
            app.step(Msg::AdvisorSpoke { checkpoint: Checkpoint::BeforePlan, advice: "plan skips auth".into(), tokens_read: 1 })
                .unwrap();
        }
        if n >= 3 {
            ticks(&mut app, 40);
        }
        rig(app, (98, 46))
    };
    assert_eq!(sub(&rig(session(), (98, 46))).as_deref(), Some("advisor · on call"));
    assert_eq!(sub(&stage(1)).as_deref(), Some("advisor · reading"));
    assert_eq!(sub(&stage(2)).as_deref(), Some("advisor · advising"));
    assert_eq!(sub(&stage(3)).as_deref(), Some("advisor · on call"));
    let mut off = session();
    off.set_session_view(None, (Some("opus 5.5"), false), (Some("jev"), true)).unwrap();
    let r = rig(off, (98, 46));
    assert_eq!(sub(&r).as_deref(), Some("advisor · off"));
    assert!(r.text().contains("opus [off]"), "the status bar says so too");
}

#[test]
fn the_active_diamond_is_filled_and_its_row_highlighted() {
    let r = rig(replay_app(), (98, 46));
    let (x, y) = r.find("◆ before a plan").expect("the active checkpoint is a filled diamond");
    assert_eq!(r.cell(x, y).bg, TH.highlight);
    assert!(r.text().contains("◇ same error x2") && r.text().contains("◇ before done"));
    let (x, y) = r.find("◇ same error x2").unwrap();
    assert_eq!(r.cell(x, y).bg, TH.panel);
}

#[test]
fn enter_on_the_rail_shows_the_full_advice_even_while_it_is_still_typing() {
    let mut app = session();
    app.step(Msg::AdvisorSpoke {
        checkpoint: Checkpoint::BeforePlan,
        advice: "split the work in three PRs".into(),
        tokens_read: 1,
    })
    .unwrap();
    let mut r = rig(app, (98, 46));
    assert!(!r.text().contains("three PRs"), "still typing");
    r.click(HitId::Pane(Pane::Advisor));
    r.key(crossterm::event::KeyCode::Enter);
    assert!(r.text().contains("» split the work in three PRs"), "{}", r.text());
}

// ---- the fork layer --------------------------------------------------------------------------

fn fork_row(r: &Rig, label: &str) -> (String, Vec<ratatui::buffer::Cell>) {
    let (x, y) = r.find(label).unwrap_or_else(|| panic!("{label}: {}", r.text()));
    let cells: Vec<_> = (x + 15..x + 25).map(|c| r.cell(c, y)).collect();
    (cells.iter().map(|c| c.symbol().to_owned()).collect(), cells)
}

#[test]
fn bars_are_ten_cells_filled_to_round_p_times_ten_with_a_half_cell() {
    let mut app = session();
    answer(&mut app, 1, "which_file", 0.94, Verdict::Sharp);
    answer(&mut app, 2, "which_tool", 0.5, Verdict::Sharp);
    answer(&mut app, 3, "retry_or_stop", 0.58, Verdict::Split);
    let r = rig(app, (98, 46));
    let (bar, cells) = fork_row(&r, "which file");
    assert_eq!(bar.matches('█').count(), 9);
    assert!(bar.contains('▌'), "0.94 is 9 and a half: {bar:?}");
    assert!(cells.iter().all(|c| c.fg == TH.router));
    let (bar, _) = fork_row(&r, "which tool");
    assert_eq!((bar.matches('█').count(), bar.contains('▌')), (5, false), "{bar:?}");
    // A split row: orange bar, the rest in split_rest.
    let (bar, cells) = fork_row(&r, "retry or stop");
    assert_eq!(bar.chars().filter(|c| *c == '█').count(), 10, "6 filled and 4 remainder: {bar:?}");
    assert!(cells[..6].iter().all(|c| c.fg == TH.split));
    assert!(cells[6..].iter().all(|c| c.fg == TH.split_rest));
    assert!(r.text().contains("0.94 sharp") && r.text().contains("0.58 split"));
}

#[test]
fn a_half_cell_on_a_split_row_sits_on_the_remainder_colour() {
    let mut app = session();
    answer(&mut app, 1, "which_file", 0.5, Verdict::Split);
    answer(&mut app, 2, "which_tool", 0.55, Verdict::Split);
    let r = rig(app, (98, 46));
    let (_, cells) = fork_row(&r, "which tool");
    assert_eq!(cells[5].symbol(), "▌");
    assert_eq!((cells[5].fg, cells[5].bg), (TH.split, TH.split_rest));
}

#[test]
fn the_footer_names_the_lead_and_lights_up_only_when_a_row_is_split() {
    let mut app = session();
    answer(&mut app, 1, "which_file", 0.94, Verdict::Sharp);
    let r = rig(app, (98, 46));
    assert!(r.text().contains("sharp → runs in code  split → Sonnet"), "{}", r.text());
    assert_eq!(r.cell_of("split → Sonnet").fg, TH.dim_text);
    let mut app = session();
    answer(&mut app, 1, "which_file", 0.94, Verdict::Sharp);
    answer(&mut app, 2, "retry_or_stop", 0.58, Verdict::Split);
    let r = rig(app, (98, 46));
    let split = r.cell_of("split → Sonnet");
    assert_eq!((split.fg, split.bg), (TH.split, TH.highlight));
}

#[test]
fn the_footer_is_there_from_30_rows_up_and_gone_below() {
    for (rows, footer) in [(24, false), (29, false), (30, true), (46, true)] {
        let r = rig(replay_app(), (98, rows));
        assert_eq!(r.text().contains("runs in code"), footer, "{rows} rows");
    }
}

#[test]
fn the_counter_has_thousands_separators_and_counts_every_answer() {
    let mut app = session();
    answer(&mut app, 1313, "which_file", 0.9, Verdict::Sharp);
    let mut r = rig(app, (98, 46));
    assert!(r.text().contains("forks 1,313"), "{}", r.text());
    answer(&mut r.app, 1314, "which_tool", 0.9, Verdict::Sharp);
    r.settle();
    assert!(r.text().contains("forks 1,314") && r.text().contains("jev [ 1,314 forks]"));
    answer(&mut r.app, 1315, "which_tool", 0.9, Verdict::Sharp);
    r.settle();
    assert!(r.text().contains("forks 1,315"));
}

#[test]
fn the_title_is_the_router_label_and_off_says_router_off() {
    let r = rig(session(), (98, 46));
    assert!(r.text().contains("JEV · fork layer"), "{}", r.text());
    let mut app = session();
    app.set_session_view(Some("--model sonnet"), (Some("opus 5.5"), true), (None, true)).unwrap();
    assert!(rig(app, (98, 46)).text().contains("ROUTER · fork layer"));
    let mut app = session();
    app.set_session_view(None, (Some("opus 5.5"), true), (Some("jev"), false)).unwrap();
    let r = rig(app, (98, 46));
    assert!(r.text().contains("router · off"), "{}", r.text());
    assert!(!r.text().contains("fork layer"));
}

// ---- the session log -------------------------------------------------------------------------

fn spec_log() -> aibus_core::App<aibus_graph::mini::MiniGraph> {
    use aibus_core::state::value::s;
    let mut app = session();
    let t = 1_700_000_000_000u64;
    let sharp = |p: f64| vec![("p", Value::F64(p)), ("verdict", s("sharp"))];
    log_row(&mut app, t, "router", "which file → src/auth.ts", sharp(0.94));
    log_row(&mut app, t + 2000, "advisor", "before a plan · reading full session", vec![]);
    log_row(&mut app, t + 3000, "router", "which tool → edit", sharp(0.94));
    log_row(&mut app, t + 5000, "audit", "1 issue flagged", vec![]);
    log_row(&mut app, t + 6000, "search", "mapped 4 modules", vec![]);
    log_row(&mut app, t + 7000, "grep", "4 call sites found", vec![]);
    log_row(&mut app, t + 8000, "search", "read 23 files", vec![]);
    log_row(&mut app, t + 9000, "router", "retry or stop → retry", vec![("p", Value::F64(0.58)), ("verdict", s("split"))]);
    log_row(&mut app, t + 10000, "advisor", "same error x2 · reading full session", vec![]);
    log_row(&mut app, t + 11000, "advisor", "before done · advice sent, back to sonnet", vec![]);
    app
}

#[test]
fn the_newest_row_is_at_the_bottom_and_older_rows_scroll_off_the_top() {
    let r = rig(spec_log(), (98, 46));
    let log = r.ui.drawn.areas.log;
    let rows: Vec<String> = r.text().lines().skip(usize::from(log.y) + 1).take(4).map(str::to_owned).collect();
    assert!(rows[3].contains("before done · advice sent, back to sonnet"), "{rows:#?}");
    assert!(rows[0].contains("retry or stop → retry") || rows[0].contains("read 23 files"), "{rows:#?}");
    assert!(!r.text().contains("which file → src/auth.ts"), "scrolled off the top");
    assert_eq!(r.ui.drawn.log.visible, 4);
}

#[test]
fn the_time_is_dim_the_actor_has_its_colour_and_router_rows_carry_p_and_the_verdict() {
    let mut r = rig(spec_log(), (98, 46));
    r.app.step(Msg::Ui(UiIntent::Scroll { pane: Pane::Log, lines: 6, sideways: false })).unwrap();
    r.settle();
    assert!(r.text().contains("p=0.94"), "{}", r.text());
    assert!(r.text().contains("sharp → code"));
    assert_eq!(r.cell_of("jev").fg, TH.router);
    assert_eq!(r.cell_of("opus 5.5").fg, TH.advisor);
    assert_eq!(r.cell_of("audit").fg, TH.swarm);
    assert_eq!(r.cell_of("sharp → code").fg, TH.router);
    let (x, y) = r.find("22:13").unwrap();
    assert_eq!(r.cell(x, y).fg, TH.dim_text);
    // A split row says where it went, in orange.
    r.app.step(Msg::Ui(UiIntent::Scroll { pane: Pane::Log, lines: -9, sideways: false })).unwrap();
    r.app.step(Msg::Ui(UiIntent::Scroll { pane: Pane::Log, lines: 2, sideways: false })).unwrap();
    r.settle();
    assert!(r.text().contains("p=0.58") && r.text().contains("split → sonnet 5.5"), "{}", r.text());
    assert_eq!(r.cell_of("split → sonnet 5.5").fg, TH.split);
}

#[test]
fn log_rows_is_configurable_with_a_minimum_of_three() {
    let mut r = rig(spec_log(), (98, 46));
    for (asked, rows) in [(5u16, 3usize), (8, 6), (2, 3), (6, 4)] {
        r.app.step(Msg::Ui(UiIntent::SetLogRows(asked))).unwrap();
        r.settle();
        assert_eq!(r.ui.drawn.log.visible, rows, "asked for a box of {asked}");
    }
}

#[test]
fn j_and_k_scroll_the_log_and_f_follows_again() {
    let mut r = rig(spec_log(), (98, 46));
    r.key(crossterm::event::KeyCode::Tab);
    r.key(crossterm::event::KeyCode::Tab);
    assert_eq!(r.int("ctrl:scroll:log"), Some(0));
    assert!(!r.text().contains("paused"));
    for _ in 0..3 {
        r.press('k');
    }
    assert_eq!(r.int("ctrl:scroll:log"), Some(3), "k goes towards older rows");
    assert!(r.text().contains("paused"), "scrolled away: not following");
    assert!(!r.text().contains("before done · advice sent"));
    r.press('j');
    assert_eq!(r.int("ctrl:scroll:log"), Some(2));
    r.press('f');
    assert_eq!(r.int("ctrl:scroll:log"), Some(0), "f jumps to the newest and follows");
    assert!(!r.text().contains("paused") && r.text().contains("before done · advice sent"));
    r.press('f');
    assert!(r.text().contains("paused"), "f again holds the place");
}

#[test]
fn slash_filters_by_actor_or_text_and_escape_drops_it() {
    let mut r = rig(spec_log(), (98, 46));
    r.key(crossterm::event::KeyCode::Tab);
    r.key(crossterm::event::KeyCode::Tab);
    let all = r.ui.drawn.log.total;
    r.press('/');
    for c in "opus".chars() {
        r.press(c);
    }
    assert!(r.text().contains("session log · /opus▌"), "{}", r.text());
    let shown = r.ui.drawn.log.total;
    assert_eq!(shown, 3, "the three advisor rows");
    // Enter keeps it; typing goes back to the keys.
    r.key(crossterm::event::KeyCode::Enter);
    assert!(r.text().contains("session log · /opus"), "{}", r.text());
    assert!(!r.text().contains("/opus▌"));
    // By text.
    r.press('/');
    r.key(crossterm::event::KeyCode::Backspace);
    r.key(crossterm::event::KeyCode::Backspace);
    r.key(crossterm::event::KeyCode::Backspace);
    r.key(crossterm::event::KeyCode::Backspace);
    for c in "files".chars() {
        r.press(c);
    }
    assert_eq!(r.ui.drawn.log.total, 1, "{}", r.text());
    r.key(crossterm::event::KeyCode::Esc);
    assert_eq!(r.ui.drawn.log.total, all, "Esc clears the filter");
}

#[test]
fn enter_on_a_row_of_a_run_opens_that_runs_transcript() {
    let mut app = session();
    worker(&mut app, 2, "search", "maps the repo", Some(0.5), "running");
    let mut r = rig(app, (98, 46));
    // The row that says the worker started names its run.
    let rows = aibus_tui::input::Context::read(&r.app.graph).log_runs;
    let at = rows.iter().position(|x| *x == Some(run_id(2))).expect("a row of the worker");
    r.click(HitId::LogRow(at));
    r.key(crossterm::event::KeyCode::Enter);
    let modal = r.app.graph.read_key("ctrl:modal").cloned();
    assert_eq!(modal, Some(Value::Str(format!("transcript:{}", run_id(2)).into())));
}

// ---- the status bar --------------------------------------------------------------------------

fn status(r: &Rig) -> String {
    r.text().lines().last().unwrap().to_owned()
}

#[test]
fn the_status_bar_has_four_segments() {
    let mut app = three_running();
    answer(&mut app, 1314, "which_file", 0.9, Verdict::Sharp);
    let r = rig(app, (98, 46));
    let s = status(&r);
    assert!(s.contains("haiku [swarming]"), "{s}");
    assert!(s.contains("subagents [3/3]"));
    assert!(s.contains("opus [on call]"));
    assert!(s.contains("jev [ 1,314 forks]"));
    assert_eq!(r.cell_of("haiku [swarming]").fg, TH.swarm);
    assert_eq!(r.cell_of("subagents [3/3]").fg, TH.fg);
    assert_eq!(r.cell_of("opus [on call]").fg, TH.advisor);
    assert_eq!(r.cell_of("jev [").fg, TH.router);
    let mut app = session();
    worker(&mut app, 2, "audit", "runs in a worktree", Some(1.0), "completed");
    assert!(status(&rig(app, (98, 46))).contains("haiku [idle]"));
}

#[test]
fn the_ledger_shows_from_100_columns_and_the_alerts_beside_it() {
    let ledger = |cols| {
        let mut app = session();
        app.step(Msg::Command(Command::LedgerUpdate { section: "goal".into(), text: "ship".into() })).unwrap();
        app.step(Msg::Command(Command::LedgerUpdate { section: "now".into(), text: "test".into() })).unwrap();
        status(&rig(app, (cols, 46)))
    };
    assert!(!ledger(98).contains("ledger"));
    assert!(ledger(110).contains("ledger: ship · now: test"), "{}", ledger(110));
    assert!(ledger(120).contains("ledger: ship · now: test"));
}

#[test]
fn a_waiting_permission_a_bypass_and_memory_have_their_own_segments() {
    let mut app = session();
    app.enable_bypass().unwrap();
    app.step(Msg::PermissionNeeded {
        request: PermissionRequest {
            id: aibus_core::PermissionId(ulid::Ulid::from_parts(1, 2)),
            agent: "search".parse().unwrap(),
            run: run_id(1),
            call: aibus_core::ToolCallId(ulid::Ulid::from_parts(1, 3)),
            tool: "bash".into(),
            summary: "cargo test".into(),
        },
    })
    .unwrap();
    app.step(Msg::Ui(UiIntent::Mem(212))).unwrap();
    let r = rig(app, (120, 46));
    let s = status(&r);
    assert!(s.contains("! 1 permission waiting (p)"), "{s}");
    assert!(s.contains("BYPASS") && s.contains("mem 212M"), "{s}");
    assert_eq!(r.cell_of("BYPASS").fg, TH.error);
    assert!(r.cell_of("BYPASS").modifier.contains(ratatui::style::Modifier::BOLD));
    assert_eq!(r.cell_of("! 1 permission").fg, TH.warn);
    // Without `-v` there is no memory sampled, so no segment.
    let quiet = rig(session(), (120, 46));
    assert!(!status(&quiet).contains("mem") && !status(&quiet).contains("BYPASS"));
}

// ---- the same frames, as the issue lists them ------------------------------------------------

#[test]
fn per_widget_snapshots_at_98x46() {
    let snap = |name: &str, mut app: aibus_core::App<aibus_graph::mini::MiniGraph>, ticks_to: u32, pick: &dyn Fn(&Rig) -> ratatui::layout::Rect, styles: bool| {
        common::ticks(&mut app, ticks_to);
        let r = rig(app, (98, 46));
        assert_crop(&r.frame(), pick(&r), name, styles);
    };
    let a = |r: &Rig| r.ui.drawn.areas;
    // The label, the paths down to the cards, the cards, their markers and the merge row.
    let cards = |r: &Rig| {
        let c = a(r).centre;
        let top = r.ui.drawn.hits.find(HitId::Card(0)).map_or(c.y + 12, |h| h.rect.y - 3);
        ratatui::layout::Rect { x: c.x, y: top, width: c.width, height: 12 }
    };
    let rail = |r: &Rig| a(r).rail;
    let fork = |r: &Rig| {
        let c = a(r).centre;
        ratatui::layout::Rect { x: c.x + (c.width - 40) / 2, y: c.y + 6, width: 40, height: 6 }
    };
    let log = |r: &Rig| a(r).log;
    let statusbar = |r: &Rig| a(r).status;

    // Cards: three running, one done, five with the row scrolled, none.
    snap("w_cards_three_running", three_running(), 3, &cards, true);
    let mut done = session();
    worker(&mut done, 2, "audit", "runs in a worktree", Some(1.0), "completed");
    snap("w_cards_one_done", done, 3, &cards, true);
    let mut five = three_running();
    worker(&mut five, 5, "lint", "checks style", Some(0.1), "running");
    worker(&mut five, 6, "docs", "writes docs", Some(0.2), "running");
    five.step(Msg::Ui(UiIntent::Select(Pane::Cards, Selection::Index(4)))).unwrap();
    snap("w_cards_five_scrolled", five, 3, &cards, false);
    snap("w_cards_zero", session(), 3, &cards, false);

    // The rail in the states of f01, f04 and f09, and collapsed.
    let advice = |app: &mut aibus_core::App<aibus_graph::mini::MiniGraph>, text: &str, read: u64, cp: Checkpoint| {
        app.step(Msg::AdvisorSpoke { checkpoint: cp.clone(), advice: text.into(), tokens_read: read }).unwrap();
        ticks(app, 40);
        app.step(Msg::CheckpointHit { checkpoint: cp, detail: String::new() }).unwrap();
    };
    let mut f01 = session();
    advice(&mut f01, "plan skips auth", 122_000, Checkpoint::BeforePlan);
    snap("w_rail_f01", f01, 0, &rail, true);
    let mut f04 = session();
    advice(&mut f04, "plan skips auth", 122_000, Checkpoint::BeforePlan);
    advice(&mut f04, "split in 3 PRs", 91_000, Checkpoint::SameErrorX2);
    snap("w_rail_f04", f04, 0, &rail, false);
    let mut f09 = session();
    for (t, k, cp) in [
        ("plan skips auth", 122_000, Checkpoint::BeforePlan),
        ("split in 3 PRs", 91_000, Checkpoint::SameErrorX2),
        ("wrong fixture", 92_000, Checkpoint::BeforeDone),
    ] {
        advice(&mut f09, t, k, cp);
    }
    snap("w_rail_f09", f09, 0, &rail, false);
    let strip = |app: aibus_core::App<aibus_graph::mini::MiniGraph>| {
        let r = Rig::new(app, (80, 24), TH);
        assert_crop(&r.frame(), r.ui.drawn.areas.rail, "w_rail_collapsed", false);
    };
    let mut collapsed = session();
    advice(&mut collapsed, "plan skips auth", 122_000, Checkpoint::BeforePlan);
    strip(collapsed);

    // The fork layer in f01 (all sharp), f02 (one split), f17 (two split) and off.
    let forks = |split: &[bool; 3]| {
        let mut app = session();
        for (i, (kind, p, p_split)) in [("which_file", 0.94, 0.5), ("which_tool", 0.94, 0.5), ("retry_or_stop", 0.88, 0.58)]
            .iter()
            .enumerate()
        {
            let v = if split[i] { Verdict::Split } else { Verdict::Sharp };
            answer(&mut app, 1312 + i as u64, kind, if split[i] { *p_split } else { *p }, v);
        }
        app
    };
    snap("w_fork_f01", forks(&[false, false, false]), 3, &fork, false);
    snap("w_fork_f02", forks(&[false, false, true]), 3, &fork, true);
    snap("w_fork_f17", forks(&[true, false, true]), 3, &fork, false);
    let mut off = forks(&[false; 3]);
    off.set_session_view(None, (Some("opus 5.5"), true), (Some("jev"), false)).unwrap();
    snap("w_fork_off", off, 3, &fork, false);

    // The log with the ten lines of 05 section 2.7, tall enough to hold them.
    let mut tall = spec_log();
    tall.step(Msg::Ui(UiIntent::SetLogRows(12))).unwrap();
    snap("w_log_ten_lines", tall, 0, &log, false);

    // The status bar at 98 and 120 columns.
    let mut bar = three_running();
    answer(&mut bar, 1314, "which_file", 0.9, Verdict::Sharp);
    snap("w_status_98", bar, 0, &statusbar, true);
    let mut bar = three_running();
    answer(&mut bar, 1314, "which_file", 0.9, Verdict::Sharp);
    let r = Rig::new(bar, (120, 46), TH);
    assert_crop(&r.frame(), r.ui.drawn.areas.status, "w_status_120", false);
}

#[test]
fn x_dismisses_a_finished_card_and_stops_a_running_one() {
    let mut app = session();
    worker(&mut app, 2, "audit", "runs in a worktree", Some(1.0), "completed");
    worker(&mut app, 3, "grep", "finds call sites", Some(0.4), "running");
    let mut r = rig(app, (98, 46));
    r.key(crossterm::event::KeyCode::Tab);
    assert_eq!(r.ui.drawn.cards, 2);
    assert!(r.text().contains("✓ done"));
    // Card 0 is finished: `x` takes its card away and the running one stays.
    let msgs = names(&r.press('x'));
    assert!(msgs[0].starts_with("Command(Cancel { run: "), "{msgs:?}");
    assert_eq!(r.ui.drawn.cards, 1);
    assert!(!r.text().contains("✓ done"));
    assert_eq!(r.int(aibus_core::state::SELECTED_CARD), Some(0));
    // The running one is asked to stop and stays on screen until it ends.
    r.press('x');
    assert_eq!(r.ui.drawn.cards, 1);
}

#[test]
fn a_new_run_of_the_same_agent_replaces_a_finished_card() {
    let mut app = session();
    worker(&mut app, 2, "audit", "runs in a worktree", Some(1.0), "completed");
    let r = rig(app, (98, 46));
    assert!(r.text().contains("✓ done"));
    let mut app = session();
    worker(&mut app, 2, "audit", "runs in a worktree", Some(1.0), "completed");
    worker(&mut app, 3, "audit", "runs it again", Some(0.2), "running");
    let r = rig(app, (98, 46));
    assert_eq!(r.ui.drawn.cards, 1, "one card for one agent");
    assert!(r.text().contains("runs it again") && !r.text().contains("✓ done"));
    // A still-running card is not replaced: two runs at once are two cards.
    let mut app = session();
    worker(&mut app, 2, "audit", "runs in a worktree", Some(0.5), "running");
    worker(&mut app, 3, "audit", "runs it again", Some(0.2), "running");
    assert_eq!(rig(app, (98, 46)).ui.drawn.cards, 2);
}

/// The card rects, and with them the hit map, come from `widgets::card_rects` whichever way the
/// column is drawn, so a run with `--features flow-diagram` and one without agree on them.
#[test]
fn the_card_hit_rects_are_the_ones_the_window_gives_for_1_3_and_8_cards() {
    for size in [(98, 46), (98, 29)] {
        for n in [1usize, 3, 8] {
            let mut app = session();
            for k in 0..n {
                worker(&mut app, 2 + k as u128, &format!("w{k}"), "does a thing", Some(0.5), "running");
            }
            let r = rig(app, size);
            let centre = r.ui.drawn.areas.centre;
            let want = aibus_tui::widgets::card_rects(centre, n, 0);
            assert_eq!(want.len(), n.min(3));
            for (i, w) in want {
                let got = r.rect(HitId::Card(i));
                assert_eq!((got.x, got.width), (w.x, w.width), "{size:?} {n} cards, card {i}");
                assert_eq!(got.height, 7, "{size:?} {n} cards, card {i}");
            }
            assert!(r.ui.drawn.hits.find(HitId::Card(3)).is_none(), "only three are in the window");
        }
    }
}
