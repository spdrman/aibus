//! C1.2, mouse acceptance (05 section 6.1): the widgets as pointer targets. Synthetic events go
//! through the event layer and the graph, and the frame is read back from a `TestBackend`.

mod common;

use std::time::Instant;

use aibus_core::state::SELECTED_CARD;
use aibus_core::*;
use aibus_graph::Graph;
use aibus_testkit::tui::run_id;
use aibus_tui::app::{terminal_events, CtrlC};
use aibus_tui::hit::{HitId, Segment};
use aibus_tui::input::{Clip, Input};
use aibus_tui::{Theme, Ui};
use common::*;
use crossterm::event::{Event, KeyCode, KeyModifiers, MouseEvent, MouseEventKind};

const TH: Theme = Theme::recording();

/// A lead and three workers: running, partial and done.
fn rig() -> Rig {
    let mut app = session();
    worker(&mut app, 2, "search", "maps the repo", Some(0.71), "running");
    worker(&mut app, 3, "grep", "finds call sites", Some(0.35), "partial");
    worker(&mut app, 4, "audit", "runs in a worktree", Some(1.0), "completed");
    Rig::new(app, (98, 46), TH)
}

const ITEMS: [&str; 6] = ["Open transcript", "Cancel", "Fork", "Resume", "Copy id", "Copy result"];

fn opened(r: &mut Rig, card: usize) {
    let (c, row) = r.centre(HitId::Card(card));
    r.right_down(c, row);
}

// ---- click -----------------------------------------------------------------------------------

#[test]
fn a_click_on_a_status_segment_does_what_its_key_would() {
    let table = [
        (Segment::Swarm, "Ui(Focus(Cards))"),
        (Segment::Subagents, "Ui(Focus(Cards))"),
        (Segment::Advisor, "Ui(Focus(Advisor))"),
        (Segment::Router, "Ui(Focus(Forks))"),
    ];
    for (seg, first) in table {
        let mut r = rig();
        let msgs = r.click(HitId::Status(seg));
        assert_eq!(names(&msgs)[0], first, "{seg:?}");
    }
    // The advisor segment opens the last advice, like Enter on the rail.
    let mut r = rig();
    r.app.step(Msg::AdvisorSpoke { checkpoint: Checkpoint::BeforePlan, advice: "plan skips auth".into(), tokens_read: 1 }).unwrap();
    r.settle();
    r.click(HitId::Status(Segment::Advisor));
    assert!(r.text().contains("» plan skips auth"), "{}", r.text());
}

#[test]
fn a_click_on_the_ledger_shows_all_of_it_and_a_click_elsewhere_hides_it() {
    let mut app = session();
    for (section, text) in [("goal", "ship the dashboard widgets with their tests"), ("now", "writing the mouse tests")] {
        app.step(Msg::Command(Command::LedgerUpdate { section: section.into(), text: text.into() })).unwrap();
    }
    let mut r = Rig::new(app, (110, 46), TH);
    assert!(!r.text().contains("goal: ship"), "cut to fit, so not whole");
    r.click(HitId::Status(Segment::Ledger));
    assert!(r.text().contains("goal: ship the dashboard widgets with their tests"), "{}", r.text());
    assert!(r.text().contains("now: writing the mouse tests"));
    r.down(2, 12);
    assert!(!r.text().contains("goal: ship"));
}

#[test]
fn a_double_click_on_a_log_row_of_a_run_opens_its_transcript() {
    let mut r = rig();
    let rows = aibus_tui::input::Context::read(&r.app.graph).log_runs;
    let at = rows.iter().position(|x| *x == Some(run_id(3))).expect("a row of the grep worker");
    let id = HitId::LogRow(at);
    let (c, row) = r.centre(id);
    r.down(c, row);
    r.up(c, row);
    r.advance(100);
    let msgs = r.down(c, row);
    assert!(names(&msgs).iter().any(|m| m.starts_with("Command(OpenTranscript(")), "{:?}", names(&msgs));
    assert_eq!(r.app.graph.read_key("ctrl:modal").cloned().map(|v| format!("{v:?}")).unwrap_or_default(), format!("Str(\"transcript:{}\")", run_id(3)));
}

// ---- the context menu ------------------------------------------------------------------------

#[test]
fn right_click_on_a_card_selects_it_and_opens_the_menu_with_all_six_items() {
    let mut r = rig();
    let (c, row) = r.centre(HitId::Card(1));
    let msgs = names(&r.right_down(c, row));
    assert_eq!(&msgs[..2], ["Ui(Focus(Cards))", "Ui(Select(Cards, Index(1)))"]);
    assert!(msgs[2].starts_with("Ui(OpenMenu { run: "), "{msgs:?}");
    assert_eq!(r.int(SELECTED_CARD), Some(1));
    for label in ITEMS {
        assert!(r.text().contains(label), "{label}: {}", r.text());
    }
    assert!(r.text().contains("┌ grep "), "titled with the agent");
}

#[test]
fn items_that_do_not_apply_are_shown_disabled_not_hidden() {
    // Card 0 is running: it can be cancelled, but not resumed, and has no result yet.
    let mut r = rig();
    opened(&mut r, 0);
    for label in ITEMS {
        assert!(r.text().contains(label), "{label} is still there");
    }
    for on in ["Open transcript", "Cancel", "Fork", "Copy id"] {
        assert_eq!(r.cell_of(on).fg, TH.fg, "{on}");
    }
    for off in ["Resume", "Copy result"] {
        assert_eq!(r.cell_of(off).fg, TH.dim_text, "{off}");
    }
    // A partial run is the other way round.
    let mut r = rig();
    opened(&mut r, 1);
    assert_eq!(r.cell_of("Cancel").fg, TH.dim_text);
    for on in ["Resume", "Copy result"] {
        assert_eq!(r.cell_of(on).fg, TH.fg, "{on}");
    }
}

#[test]
fn esc_closes_the_menu() {
    let mut r = rig();
    opened(&mut r, 0);
    assert!(r.text().contains("Open transcript"));
    assert_eq!(names(&r.key(KeyCode::Esc)), ["Ui(CloseMenu)"]);
    assert!(!r.text().contains("Open transcript"));
}

#[test]
fn a_click_outside_closes_the_menu_and_does_not_reach_what_is_behind() {
    let mut r = rig();
    opened(&mut r, 0);
    // The cell is a card (card 2), covered by the menu's backdrop.
    let (c, row) = r.centre(HitId::Card(2));
    let msgs = r.down(c, row);
    assert_eq!(names(&msgs), ["Ui(CloseMenu)"]);
    assert_eq!(r.int(SELECTED_CARD), Some(0), "card 2 was not selected");
    assert!(!r.text().contains("Open transcript"));
    // A second right click closes it too.
    opened(&mut r, 0);
    let (c, row) = r.centre(HitId::Card(2));
    assert_eq!(names(&r.right_down(c, row)), ["Ui(CloseMenu)"]);
}

#[test]
fn choosing_an_item_sends_its_command_and_closes_the_menu() {
    let mut r = rig();
    opened(&mut r, 0);
    let msgs = names(&r.click(HitId::MenuItem(1)));
    assert!(msgs[0].starts_with("Command(Cancel { run: "), "{msgs:?}");
    assert_eq!(msgs[1], "Ui(CloseMenu)");
    assert!(!r.text().contains("Open transcript"));

    let mut r = rig();
    opened(&mut r, 0);
    let msgs = names(&r.click(HitId::MenuItem(0)));
    assert!(msgs[0].starts_with("Command(OpenTranscript("), "{msgs:?}");
    assert_eq!(
        r.app.graph.read_key("ctrl:modal").cloned().map(|v| format!("{v:?}")),
        Some(format!("Str(\"transcript:{}\")", run_id(2)))
    );

    let mut r = rig();
    opened(&mut r, 1);
    let msgs = names(&r.click(HitId::MenuItem(3)));
    assert!(msgs[0].starts_with("Command(Resume { run: "), "{msgs:?}");
    let msgs = {
        opened(&mut r, 2);
        names(&r.click(HitId::MenuItem(2)))
    };
    assert!(msgs[0].starts_with("Command(Fork { prompt: "), "{msgs:?}");
}

#[test]
fn a_disabled_item_does_nothing_and_the_menu_stays_open() {
    let mut r = rig();
    opened(&mut r, 0);
    let (x, y) = r.find("Resume").unwrap();
    let msgs = r.down(x + 1, y);
    r.up(x + 1, y);
    assert!(msgs.is_empty(), "{:?}", names(&msgs));
    assert!(r.text().contains("Open transcript"), "still open");
    // And hovering it does not light it up.
    r.hover(x + 1, y);
    assert_ne!(r.cell(x + 1, y).bg, TH.hover);
}

#[test]
fn copy_id_and_copy_result_ask_for_the_clipboard() {
    let mut r = rig();
    opened(&mut r, 2);
    r.click(HitId::MenuItem(4));
    assert_eq!(r.copied, [Clip::Text(run_id(4).to_string())]);
    opened(&mut r, 2);
    r.click(HitId::MenuItem(5));
    assert_eq!(r.copied[1], Clip::Result(run_id(4)));
}

#[test]
fn with_the_mouse_off_the_menu_is_reached_by_m_and_chosen_with_the_keys() {
    let mut r = rig();
    r.key(KeyCode::Tab);
    assert_eq!(r.int(SELECTED_CARD), Some(0));
    let msgs = names(&r.press('m'));
    assert!(msgs[0].starts_with("Ui(OpenMenu { run: "), "{msgs:?}");
    assert!(r.text().contains("Open transcript"));
    // Down skips what does not apply: Cancel, Fork, then past Resume to Copy id.
    assert_eq!(names(&r.key(KeyCode::Down)), ["Ui(MenuSelect(1))"]);
    assert_eq!(names(&r.key(KeyCode::Down)), ["Ui(MenuSelect(2))"]);
    assert_eq!(names(&r.press('j')), ["Ui(MenuSelect(4))"]);
    assert_eq!(names(&r.press('k')), ["Ui(MenuSelect(2))"]);
    r.press('j');
    let msgs = names(&r.key(KeyCode::Enter));
    assert_eq!(msgs, ["Ui(CloseMenu)"], "Copy id has no message of its own");
    assert_eq!(r.copied, [Clip::Text(run_id(2).to_string())]);
    // Esc backs out.
    r.press('m');
    assert_eq!(names(&r.key(KeyCode::Esc)), ["Ui(CloseMenu)"]);
}

#[test]
fn the_menu_key_works_in_the_log_too_and_the_pointer_events_are_ignored_when_the_mouse_is_off() {
    let mut r = rig();
    let rows = aibus_tui::input::Context::read(&r.app.graph).log_runs;
    let at = rows.iter().rposition(Option::is_some).unwrap();
    r.click(HitId::LogRow(at));
    let msgs = names(&r.press('m'));
    assert!(msgs[0].starts_with("Ui(OpenMenu"), "{msgs:?}");

    let mut ui = Ui::new(TH);
    ui.drawn = r.ui.drawn.clone();
    let (c, row) = r.centre(HitId::Card(1));
    let event = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(crossterm::event::MouseButton::Right),
        column: c,
        row,
        modifiers: KeyModifiers::NONE,
    });
    let mut off = rig();
    let before = off.int("eng:msg_seq");
    terminal_events(vec![event], &mut off.app, &mut ui, &mut Input::default(), &mut CtrlC::default(), false, Instant::now(), &mut |app, m| app.step(m)).unwrap();
    assert_eq!(off.int("eng:msg_seq"), before);
}

// ---- hover and tooltips ----------------------------------------------------------------------

#[test]
fn the_element_under_the_pointer_takes_the_hover_background() {
    let mut r = rig();
    let (c, row) = r.centre(HitId::Card(2));
    assert_ne!(r.cell(c, row).bg, TH.hover);
    r.hover(c, row);
    assert_eq!(r.cell(c, row).bg, TH.hover);
    let (c1, row1) = r.centre(HitId::Card(1));
    r.hover(c1, row1);
    assert_ne!(r.cell(c, row).bg, TH.hover, "it moved");
    assert_eq!(r.cell(c1, row1).bg, TH.hover);
    // The selected card keeps its own background under the pointer.
    let (c0, row0) = r.centre(HitId::Card(0));
    r.hover(c0, row0);
    assert_eq!(r.cell(c0, row0).bg, TH.selected);
}

#[test]
fn a_tooltip_comes_after_500_ms_at_rest_and_changes_nothing_underneath() {
    let mut r = rig();
    let before = r.frame();
    let regions_before = format!("{:?}", r.ui.drawn.hits.regions());
    let (c, row) = r.centre(HitId::Card(1));
    r.hover(c, row);
    let hovered = r.frame();
    r.advance(499);
    assert_eq!(r.frame().text, hovered.text, "not yet");
    r.advance(1);
    let tip = r.frame();
    for line in [format!("run {}", run_id(3)), "cost $0.0000".to_owned(), "model Haiku 5.5  account acct".to_owned(), "finds call sites".to_owned()] {
        assert!(tip.text.contains(&line), "{line}\n{}", tip.text);
    }
    // It is a layer: the hit map and the geometry are the same, and the cells it did not
    // cover are the cells of the frame without it.
    assert_eq!(format!("{:?}", r.ui.drawn.hits.regions()), regions_before);
    let (a, b): (Vec<_>, Vec<_>) = (hovered.text.lines().collect(), tip.text.lines().collect());
    let changed: Vec<usize> = (0..a.len()).filter(|i| a[*i] != b[*i]).collect();
    let card = r.rect(HitId::Card(1));
    assert!(changed.first().is_some_and(|y| *y as u16 >= card.bottom()), "below the card: {changed:?}");
    assert!(changed.len() <= 7, "{changed:?}");
    assert_eq!(before.text.lines().take(usize::from(card.y)).collect::<Vec<_>>(), b.iter().copied().take(usize::from(card.y)).collect::<Vec<_>>());
    // Moving away takes it down.
    r.hover(0, 0);
    assert!(!r.text().contains("cost $"));
}

#[test]
fn a_press_ends_a_tooltip_and_it_is_earned_again_by_resting() {
    let mut r = rig();
    let (c, row) = r.centre(HitId::Card(0));
    r.hover(c, row);
    r.advance(500);
    assert!(r.text().contains("cost $"));
    r.down(c, row);
    assert!(!r.text().contains("cost $"));
    r.up(c, row);
    r.advance(500);
    assert!(r.text().contains("cost $"));
}

#[test]
fn space_shows_the_tooltip_of_the_selection_and_any_key_hides_it() {
    let mut r = rig();
    r.key(KeyCode::Tab);
    r.press(' ');
    assert!(r.text().contains(&format!("run {}", run_id(2))), "{}", r.text());
    // Space again closes it.
    r.press(' ');
    assert!(!r.text().contains("cost $"));
    r.press(' ');
    r.press('l');
    assert!(!r.text().contains("cost $"), "any other key hides it");
}

#[test]
fn nothing_but_cards_rows_and_the_cut_off_segments_have_tooltips() {
    let mut r = rig();
    let a = r.ui.drawn.areas;
    r.hover(a.header.x + 40, a.header.y);
    r.advance(1000);
    assert!(!r.text().contains("cost $"));
    // The corner of the centre column is a pane, not a card.
    r.hover(a.centre.x, a.centre.y);
    assert_eq!(r.ui.drawn.hits.at(a.centre.x, a.centre.y).map(|h| h.id), Some(HitId::Pane(Pane::Cards)));
    r.advance(1000);
    assert!(!r.text().contains("cost $"));
}

#[test]
fn a_pressed_card_shows_the_pressed_background_until_the_button_is_up() {
    let mut r = rig();
    let (c, row) = r.centre(HitId::Card(2));
    r.down(c, row);
    assert_eq!(r.cell(c, row).bg, TH.pressed);
    r.up(c, row);
    assert_ne!(r.cell(c, row).bg, TH.pressed);
}
