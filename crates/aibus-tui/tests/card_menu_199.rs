//! #199: a right click on a card has to open the card menu every time. Found in a real pty at
//! 98x46 with eight workers streaming: ten of twenty right clicks opened it, strictly every other
//! one. The streaming was a red herring... a right click that lands while a menu is still open
//! used to only close that menu, so the next one found it closed and opened it again.
//!
//! The events here go through `terminal_events` in batches, like the loop does, with worker
//! output and ticks stepped between the press and the release.

mod common;

use std::time::Instant;

use aibus_core::state::SELECTED_CARD;
use aibus_core::*;
use aibus_provider::StreamChunk;
use aibus_testkit::tui::run_id;
use aibus_tui::app::{terminal_events, CtrlC};
use aibus_tui::hit::HitId;
use aibus_tui::Theme;
use common::*;
use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

/// A lead and eight running workers, `w0` to `w7`, like the mock's EIGHT scenario.
fn rig8() -> Rig {
    let mut app = session();
    for i in 0..8u128 {
        worker(&mut app, 2 + i, &format!("w{i}"), "work", Some(0.3), "running");
    }
    Rig::new(app, (98, 46), Theme::recording())
}

fn event(kind: MouseEventKind, (column, row): (u16, u16)) -> Event {
    Event::Mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE })
}

/// One pass of the loop for these events: handled as one batch, then the frame.
fn batch(r: &mut Rig, events: Vec<Event>) {
    let step = &mut |app: &mut App<aibus_graph::mini::MiniGraph>, m: Msg| app.step(m);
    terminal_events(events, &mut r.app, &mut r.ui, &mut r.input, &mut CtrlC::default(), true, Instant::now(), step).unwrap();
    r.settle();
}

/// What the workers do while the pointer is down: output on every run, then a tick.
fn streaming(r: &mut Rig) {
    for i in 0..8 {
        let chunks = vec![StreamChunk::TextDelta("some more output ".into())];
        r.app.step(Msg::Stream { run: run_id(2 + i), chunks }).unwrap();
        r.settle();
    }
    r.app.step(Msg::Tick).unwrap();
    r.settle();
}

/// A right click: press, the workers stream, release.
fn right_click(r: &mut Rig, at: (u16, u16)) {
    batch(r, vec![event(MouseEventKind::Down(MouseButton::Right), at)]);
    streaming(r);
    batch(r, vec![event(MouseEventKind::Up(MouseButton::Right), at)]);
}

/// Whether the menu on screen is the one for worker `card`: its title sits right above the first
/// item (the card row has the same titles, so the text alone says nothing).
fn menu_for(r: &Rig, card: u128) -> bool {
    let Some((x, y)) = r.find("Open transcript") else { return false };
    let Some(title) = y.checked_sub(1).and_then(|row| r.text().lines().nth(usize::from(row)).map(str::to_owned)) else { return false };
    let near: String = title.chars().skip(usize::from(x).saturating_sub(8)).take(24).collect();
    near.contains(&format!("┌ w{card} ")) && r.text().contains("Copy id")
}

#[test]
fn twenty_right_clicks_in_a_row_open_the_menu_twenty_times_without_an_escape_between() {
    let mut r = rig8();
    let at: Vec<(u16, u16)> = (0..3).map(|i| r.centre(HitId::Card(i))).collect();
    for trial in 0..20 {
        let card = trial % 3;
        right_click(&mut r, at[card]);
        assert!(menu_for(&r, card as u128), "trial {trial}: no menu for card {card}\n{}", r.text());
        assert_eq!(r.int(SELECTED_CARD), Some(card as i64), "trial {trial}");
        streaming(&mut r);
        assert!(menu_for(&r, card as u128), "trial {trial}: the menu went away by itself\n{}", r.text());
    }
}

#[test]
fn a_right_click_on_another_card_moves_the_menu_there() {
    let mut r = rig8();
    let (first, other) = (r.centre(HitId::Card(0)), r.centre(HitId::Card(2)));
    right_click(&mut r, first);
    assert!(menu_for(&r, 0));
    right_click(&mut r, other);
    assert!(menu_for(&r, 2), "{}", r.text());
    assert!(!menu_for(&r, 0), "one menu at a time");
    assert_eq!(r.int(SELECTED_CARD), Some(2));
}

#[test]
fn a_right_click_on_a_log_row_while_a_menu_is_open_moves_the_menu_to_that_run() {
    let mut r = rig8();
    let at = r.centre(HitId::Card(0));
    right_click(&mut r, at);
    // The last rows of the log are drawn: take one that belongs to a worker.
    let runs = aibus_tui::input::Context::read(&r.app.graph).log_runs;
    let (row, worker) = (0..runs.len())
        .rev()
        .filter(|i| r.ui.drawn.hits.find(HitId::LogRow(*i)).is_some())
        .find_map(|i| (0..8u128).find(|k| runs[i] == Some(run_id(2 + k))).map(|k| (i, k)))
        .expect("a drawn log row of a worker");
    let at = r.centre(HitId::LogRow(row));
    right_click(&mut r, at);
    assert!(menu_for(&r, worker), "{}", r.text());
}

#[test]
fn a_right_click_beside_the_menu_that_is_on_no_card_still_just_closes_it() {
    let mut r = rig8();
    let at = r.centre(HitId::Card(0));
    right_click(&mut r, at);
    // The header row is neither a card nor a log row.
    right_click(&mut r, (60, 0));
    assert!(!r.text().contains("Open transcript"), "{}", r.text());
}

#[test]
fn a_right_click_on_the_menu_acts_on_the_card_that_is_under_it() {
    let mut r = rig8();
    let at = r.centre(HitId::Card(0));
    right_click(&mut r, at);
    // The menu opens at the pointer and is 21 cells wide, so it covers part of card 1.
    let under_menu = (55, 24);
    assert!(matches!(r.ui.drawn.hits.at(under_menu.0, under_menu.1).map(|h| h.id), Some(HitId::MenuBox | HitId::MenuItem(_))));
    right_click(&mut r, under_menu);
    assert!(menu_for(&r, 1), "{}", r.text());
    assert_eq!(r.int(SELECTED_CARD), Some(1));
}

#[test]
fn a_left_click_beside_the_menu_closes_it_and_leaves_the_selection_alone() {
    let mut r = rig8();
    let at = r.centre(HitId::Card(0));
    right_click(&mut r, at);
    let other = r.centre(HitId::Card(2));
    batch(&mut r, vec![event(MouseEventKind::Down(MouseButton::Left), other), event(MouseEventKind::Up(MouseButton::Left), other)]);
    assert!(!r.text().contains("Open transcript"));
    assert_eq!(r.int(SELECTED_CARD), Some(0));
}
