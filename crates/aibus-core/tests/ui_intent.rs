//! `Msg::Ui` goes through `reduce` like any other message (05 section 6.1).

mod common;

use aibus_core::state::value::{as_i64, as_str, get};
use aibus_core::*;
use aibus_graph::Graph;
use common::*;

fn ui(a: &mut A, i: UiIntent) {
    a.step(Msg::Ui(i)).unwrap();
}

fn focus(a: &A) -> String {
    as_str(a.graph.read_key("ctrl:focus").unwrap()).unwrap().to_owned()
}

#[test]
fn tab_cycles_prompt_cards_log_rail_prompt_and_back() {
    let mut a = app();
    let mut seen = vec![focus(&a)];
    for _ in 0..4 {
        ui(&mut a, UiIntent::FocusNext);
        seen.push(focus(&a));
    }
    assert_eq!(seen, ["input", "cards", "log", "advisor", "input"]);
    ui(&mut a, UiIntent::FocusPrev);
    assert_eq!(focus(&a), "advisor");
}

#[test]
fn select_and_scroll_match_the_commands() {
    let mut a = app();
    ui(&mut a, UiIntent::Select(Pane::Cards, Selection::Index(2)));
    assert_eq!(as_i64(a.graph.read_key("ctrl:selected:card").unwrap()), Some(2));
    ui(&mut a, UiIntent::Select(Pane::Cards, Selection::Prev));
    assert_eq!(as_i64(a.graph.read_key("ctrl:selected:card").unwrap()), Some(1));
    // The log has no rows, so it cannot be scrolled; the card row can.
    ui(&mut a, UiIntent::Scroll { pane: Pane::Log, lines: 3, sideways: false });
    assert_eq!(as_i64(a.graph.read_key("ctrl:scroll:log").unwrap()), Some(0));
    ui(&mut a, UiIntent::Scroll { pane: Pane::Cards, lines: 2, sideways: true });
    assert_eq!(as_i64(a.graph.read_key("ctrl:scroll:cards:x").unwrap()), Some(2));
    ui(&mut a, UiIntent::Scroll { pane: Pane::Cards, lines: -9, sideways: true });
    assert_eq!(as_i64(a.graph.read_key("ctrl:scroll:cards:x").unwrap()), Some(0));
}

#[test]
fn the_log_scrolls_no_further_than_its_oldest_row() {
    let mut a = app();
    for i in 0..5 {
        a.step(Msg::IoError { context: format!("c{i}"), error: "e".into() }).unwrap();
    }
    ui(&mut a, UiIntent::Scroll { pane: Pane::Log, lines: 99, sideways: false });
    let rows = a.store.log_len() as i64;
    assert_eq!(as_i64(a.graph.read_key("ctrl:scroll:log").unwrap()), Some(rows - 1));
}

#[test]
fn open_on_cards_names_the_selected_workers_transcript() {
    let mut a = app();
    for (seq, role) in [(1, Role::Lead), (2, Role::Worker), (3, Role::Worker)] {
        a.step(Msg::RunStarted {
            run: rid(seq),
            agent: "search".parse().unwrap(),
            role,
            parent: None,
            model: ModelId("m".into()),
            account: AccountName("acct".into()),
        })
        .unwrap();
    }
    ui(&mut a, UiIntent::Select(Pane::Cards, Selection::Index(1)));
    ui(&mut a, UiIntent::Open(Pane::Cards));
    let modal = as_str(a.graph.read_key("ctrl:modal").unwrap()).unwrap().to_owned();
    assert_eq!(modal, format!("transcript:{}", rid(3)));
}

#[test]
fn rail_toggle_and_dragged_sizes_are_kept_and_clamped() {
    let mut a = app();
    ui(&mut a, UiIntent::ToggleRail);
    assert_eq!(a.graph.read_key("ctrl:rail:expanded"), Some(&aibus_graph::Value::Bool(true)));
    ui(&mut a, UiIntent::ToggleRail);
    assert_eq!(a.graph.read_key("ctrl:rail:expanded"), Some(&aibus_graph::Value::Bool(false)));
    ui(&mut a, UiIntent::SetRailWidth(3));
    ui(&mut a, UiIntent::SetLogRows(500));
    let layout = a.graph.read_key("ctrl:layout").unwrap();
    assert_eq!(as_i64(get(layout, "rail").unwrap()), Some(22));
    assert_eq!(as_i64(get(layout, "log").unwrap()), Some(80));
}

#[test]
fn a_ui_message_summary_names_the_intent_and_nothing_else() {
    let m = Msg::Ui(UiIntent::Open(Pane::Log));
    assert_eq!(m.name(), "Ui");
    let text = format!("{:?}", m.summary());
    assert!(text.contains("open"), "{text}");
}
