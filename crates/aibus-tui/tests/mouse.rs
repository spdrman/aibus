//! Synthetic mouse events through the event layer, the graph and a `TestBackend` (05 section
//! 6.1): both the messages and the frame are asserted.

mod common;

use std::time::Instant;

use aibus_core::state::{scroll_key, FOCUS, MODAL, SELECTED_CARD, SELECTED_LOG};
use aibus_core::*;
use aibus_graph::{Graph, Value};
use aibus_testkit::tui::{poke, replay_app};
use aibus_tui::app::{terminal_events, CtrlC};
use aibus_tui::hit::HitId;
use aibus_tui::input::{Input, DOUBLE_CLICK};
use aibus_tui::{Chrome, Layout, Theme, Ui};
use common::*;
use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};

fn rig(size: (u16, u16)) -> Rig {
    let mut app = replay_app();
    for i in 0..40 {
        app.step(Msg::IoError { context: format!("ctx {i}"), error: "e".into() }).unwrap();
    }
    Rig::new(app, size, Theme::recording())
}

fn focus(r: &Rig) -> String {
    match r.app.graph.read_key(FOCUS) {
        Some(Value::Str(s)) => s.to_string(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_click_on_a_card_focuses_cards_and_selects_that_run() {
    let mut r = rig((98, 46));
    assert_eq!(focus(&r), "input");
    let msgs = r.click(HitId::Card(1));
    assert_eq!(
        names(&msgs),
        ["Ui(Focus(Cards))", "Ui(Select(Cards, Index(1)))"],
    );
    assert_eq!(focus(&r), "cards");
    assert_eq!(r.int(SELECTED_CARD), Some(1));
    // The frame shows it: the marker in the title and the selected background.
    assert!(r.text().contains("▸ grep"), "{}", r.text());
    let (x, y) = r.centre(HitId::Card(1));
    assert_eq!(r.cell(x, y).bg, Theme::recording().selected);
    // And the focus ring is on that card's border.
    let card = r.ui.drawn.hits.find(HitId::Card(1)).unwrap().rect;
    assert_eq!(r.cell(card.x, card.y + 1).fg, Theme::recording().focus);
}

#[test]
fn a_click_on_a_log_row_selects_it() {
    let mut r = rig((98, 46));
    let msgs = r.click(HitId::LogRow(r.ui.drawn.log.total - 1));
    assert_eq!(msgs.len(), 2);
    assert_eq!(focus(&r), "log");
    assert_eq!(r.int(SELECTED_LOG), Some(r.ui.drawn.log.total as i64 - 1));
}

#[test]
fn a_double_click_inside_400_ms_opens_the_transcript_and_a_slow_one_does_not() {
    let mut r = rig((98, 46));
    let (c, row) = r.centre(HitId::Card(2));
    r.down(c, row);
    r.up(c, row);
    r.advance(300);
    let msgs = r.down(c, row);
    assert_eq!(
        names(&msgs),
        ["Ui(Focus(Cards))", "Ui(Select(Cards, Index(2)))", "Ui(Open(Cards))"],
    );
    let Some(Value::Str(modal)) = r.app.graph.read_key(MODAL) else { panic!("no modal") };
    assert_eq!(modal.to_string(), format!("transcript:{}", testkit_run(4)));

    // Close it, then click twice more than 400 ms apart.
    r.app.step(Msg::Command(Command::CloseModal)).unwrap();
    r.settle();
    r.up(c, row);
    r.advance(DOUBLE_CLICK.as_millis() as u64 + 100);
    let msgs = r.down(c, row);
    assert!(!names(&msgs).iter().any(|m| m.contains("Open")), "{msgs:?}");
    // The double click is spent: a third press right after is a single click again.
    r.advance(50);
    let msgs = r.down(c, row);
    assert!(names(&msgs).iter().any(|m| m.contains("Open")), "second of a new pair");
}

fn testkit_run(seq: u128) -> RunId {
    aibus_testkit::tui::run_id(seq)
}

#[test]
fn the_wheel_scrolls_the_pane_under_the_pointer_three_lines_a_notch() {
    let mut r = rig((98, 46));
    assert_eq!(focus(&r), "input", "the focused pane is not the one that scrolls");
    let before = r.text();
    let (c, row) = r.centre(HitId::LogRow(r.ui.drawn.log.total - 1));
    let msgs = r.mouse(MouseEventKind::ScrollUp, c, row);
    assert_eq!(names(&msgs), ["Ui(Scroll { pane: Log, lines: 3, sideways: false })"]);
    assert_eq!(r.int(&scroll_key("log")), Some(3));
    assert_ne!(r.text(), before, "the log moved");
    assert_eq!(focus(&r), "input");
    // Down is back towards the newest row, and never below it.
    r.mouse(MouseEventKind::ScrollDown, c, row);
    r.mouse(MouseEventKind::ScrollDown, c, row);
    assert_eq!(r.int(&scroll_key("log")), Some(0));
    assert_eq!(r.text(), before);

    // Over the cards the wheel is for the cards, with Shift sideways.
    let (c, row) = r.centre(HitId::Card(0));
    let msgs = r.mouse_with(MouseEventKind::ScrollDown, c, row, KeyModifiers::SHIFT);
    assert_eq!(names(&msgs), ["Ui(Scroll { pane: Cards, lines: -3, sideways: true })"]);
    let msgs = r.mouse(MouseEventKind::ScrollUp, c, row);
    assert_eq!(names(&msgs), ["Ui(Scroll { pane: Cards, lines: 3, sideways: false })"]);
}

#[test]
fn the_scrollbar_pages_on_the_track_and_follows_the_thumb() {
    let mut r = rig((98, 46));
    let log = r.ui.drawn.log;
    assert!(log.total > log.visible, "{log:?}");
    let thumb = r.ui.drawn.hits.find(HitId::Thumb(Pane::Log)).unwrap().rect;
    let track = r.ui.drawn.hits.find(HitId::Track(Pane::Log)).unwrap().rect;
    // Newest rows show, so the thumb sits at the bottom of its track.
    assert_eq!(thumb.bottom(), track.bottom());
    assert_eq!(r.cell(thumb.x, thumb.y).symbol(), "█");

    // A click above the thumb pages towards older rows by a screenful.
    let msgs = r.down(track.x, track.y);
    assert_eq!(
        names(&msgs),
        [format!("Ui(Scroll {{ pane: Log, lines: {}, sideways: false }})", log.visible)],
    );
    r.up(track.x, track.y);
    assert_eq!(r.int(&scroll_key("log")), Some(log.visible as i64));

    // Dragging the thumb to the top of the track shows the oldest rows.
    let thumb = r.ui.drawn.hits.find(HitId::Thumb(Pane::Log)).unwrap().rect;
    r.down(thumb.x, thumb.y);
    r.drag(thumb.x, track.y);
    r.up(thumb.x, track.y);
    assert_eq!(r.int(&scroll_key("log")), Some((log.total - log.visible) as i64));
    // And the whole way back down.
    let thumb = r.ui.drawn.hits.find(HitId::Thumb(Pane::Log)).unwrap().rect;
    assert_eq!(thumb.y, track.y);
    r.down(thumb.x, thumb.y);
    r.drag(thumb.x, track.bottom() - 1);
    r.up(thumb.x, track.bottom() - 1);
    assert_eq!(r.int(&scroll_key("log")), Some(0));
}

#[test]
fn the_border_between_the_rail_and_the_centre_drags_and_clamps() {
    let mut r = rig((120, 50));
    let handle = r.ui.drawn.hits.find(HitId::RailHandle).unwrap().rect;
    let rail_x = r.ui.drawn.areas.rail.x;
    assert_eq!(handle.x, rail_x + 22);
    let row = handle.y + 5;

    let msgs = r.down(handle.x, row);
    assert!(msgs.is_empty(), "a press on a handle only arms the drag");
    let msgs = r.drag(rail_x + 30, row);
    assert_eq!(names(&msgs), ["Ui(SetRailWidth(30))"]);
    assert_eq!(r.ui.drawn.areas.rail.width, 30);
    // The handle lights up while it is dragged: a bar in the focus colour on the border.
    let h = r.ui.drawn.hits.find(HitId::RailHandle).unwrap().rect;
    assert_eq!(h.x, rail_x + 30);
    assert_eq!(r.cell(h.x, row).symbol(), "│");
    assert_eq!(r.cell(h.x, row).fg, Theme::recording().focus);
    assert!(!r.layout_done);
    r.up(rail_x + 30, row);
    assert!(r.layout_done, "the sizes are final when the button goes up");
    assert_eq!(Chrome::read(&r.app.graph).layout, Layout { rail: Some(30), log: None });

    // Too far either way stops at the minimum and the maximum.
    r.down(rail_x + 30, row);
    assert_eq!(names(&r.drag(0, row)), ["Ui(SetRailWidth(22))"]);
    assert_eq!(names(&r.drag(119, row)), ["Ui(SetRailWidth(40))"]);
    r.up(119, row);
    assert_eq!(r.ui.drawn.areas.rail.width, 40);
    assert!(r.ui.drawn.areas.centre.width >= 40);
}

#[test]
fn the_border_above_the_log_drags_and_clamps() {
    let mut r = rig((98, 46));
    let log = r.ui.drawn.areas.log;
    assert_eq!(log.height, 6);
    r.down(10, log.y);
    let msgs = r.drag(10, log.y - 4);
    assert_eq!(names(&msgs), ["Ui(SetLogRows(10))"]);
    assert_eq!(r.ui.drawn.areas.log.height, 10);
    // Hovering it lights the border row.
    assert_eq!(r.cell(10, r.ui.drawn.areas.log.y).fg, Theme::recording().focus);
    r.up(10, log.y - 4);
    assert!(r.layout_done);
    // Dragging it down to nothing leaves the 3-row minimum, and up to the top keeps main rows.
    let y = r.ui.drawn.areas.log.y;
    r.down(10, y);
    assert_eq!(names(&r.drag(10, 45)), ["Ui(SetLogRows(5))"]);
    r.drag(10, 0);
    assert_eq!(r.ui.drawn.areas.centre.height, 14, "main keeps its minimum");
}

#[test]
fn dragged_sizes_survive_a_restart_through_ui_toml() {
    let mut r = rig((120, 50));
    let rail_x = r.ui.drawn.areas.rail.x;
    let handle = r.ui.drawn.hits.find(HitId::RailHandle).unwrap().rect;
    r.down(handle.x, handle.y);
    r.drag(rail_x + 28, handle.y);
    r.up(rail_x + 28, handle.y);
    let dir = std::env::temp_dir().join(format!("aibus-mouse-{}", std::process::id()));
    let file = dir.join("ui.toml");
    Chrome::read(&r.app.graph).layout.save(&file).unwrap();
    let saved = Layout::load(&file);
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(saved.rail, Some(28));

    // A fresh session applies it the way the loop does, as messages.
    let mut again = Rig::new(replay_app(), (120, 50), Theme::recording());
    again.app.step(Msg::Ui(UiIntent::SetRailWidth(saved.rail.unwrap()))).unwrap();
    again.settle();
    assert_eq!(again.ui.drawn.areas.rail.width, 28);
}

#[test]
fn hover_and_pressed_are_theme_tokens_on_the_element_under_the_pointer() {
    let th = Theme::recording();
    let mut r = rig((98, 46));
    // Card 0 starts selected, so the pointer goes to card 1.
    let card = r.ui.drawn.hits.find(HitId::Card(1)).unwrap().rect;
    let (x, y) = (card.x + 2, card.y + 2);
    assert_ne!(r.cell(x, y).bg, th.hover);
    r.hover(x, y);
    assert_eq!(r.input.pointer.hover, Some(HitId::Card(1)));
    assert_eq!(r.cell(x, y).bg, th.hover);
    // Moving off puts it back.
    r.hover(0, 0);
    assert_ne!(r.cell(x, y).bg, th.hover);
    // Pressed beats hover, and goes away on release.
    r.hover(x, y);
    r.down(x, y);
    assert_eq!(r.input.pointer.pressed, Some(HitId::Card(1)));
    assert_eq!(r.cell(x, y).bg, th.pressed);
    r.up(x, y);
    assert_eq!(r.input.pointer.pressed, None);
    // The press selected it, and a selected element under the pointer shows as selected.
    assert_eq!(r.cell(x, y).bg, th.selected);
}

#[test]
fn a_modal_keeps_every_click_from_the_dashboard_under_it() {
    let mut r = rig((98, 46));
    poke(&mut r.app, MODAL, Value::Str("permission".into()));
    r.settle();
    let backdrop = r.ui.drawn.hits.find(HitId::Backdrop).unwrap().rect;
    assert_eq!((backdrop.width, backdrop.height), (98, 46));
    let (c, row) = r.centre(HitId::Modal);
    let before = r.int(SELECTED_CARD);
    // Inside the box, on the backdrop over a card, over a log row, and in a corner.
    for (x, y) in [(c, row), (c, 20), (50, 44), (0, 0), (97, 45)] {
        let msgs = r.down(x, y);
        assert!(msgs.is_empty(), "({x}, {y}) reached the dashboard or closed the modal: {msgs:?}");
        r.up(x, y);
    }
    assert_eq!(r.int(SELECTED_CARD), before);
    assert_eq!(focus(&r), "input");
    // The wheel does not scroll what is behind it either.
    let msgs = r.mouse(MouseEventKind::ScrollUp, 50, 44);
    assert!(msgs.is_empty());

    // A dismissible one closes on an outside click.
    poke(&mut r.app, MODAL, Value::Str("palette".into()));
    r.settle();
    let msgs = r.down(0, 0);
    assert_eq!(names(&msgs), ["Command(CloseModal)"]);
    assert_eq!(r.app.graph.read_key(MODAL), Some(&Value::Null));
}

#[test]
fn the_rail_strip_opens_on_a_click_and_a_checkpoint_opens_its_advice() {
    let mut r = Rig::new(replay_app(), (80, 24), Theme::recording());
    assert_eq!(r.ui.drawn.areas.rail.width, 3);
    let msgs = r.click(HitId::RailStrip);
    assert!(names(&msgs).contains(&"Ui(ToggleRail)".to_owned()), "{msgs:?}");
    assert!(r.ui.drawn.areas.rail_open.is_some());
    assert!(r.text().contains("ADVISOR"), "{}", r.text());
    let msgs = r.click(HitId::Checkpoint(0));
    assert_eq!(names(&msgs), ["Ui(Focus(Advisor))", "Ui(Open(Advisor))"]);
    assert!(r.text().contains("last advice"), "{}", r.text());
    assert!(r.text().contains("plan skips auth"), "{}", r.text());
}

#[test]
fn with_the_mouse_off_pointer_events_do_nothing() {
    let events = |c: u16, rowc: u16| {
        vec![Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(LEFT),
            column: c,
            row: rowc,
            modifiers: KeyModifiers::NONE,
        })]
    };
    for (mouse, changed) in [(false, false), (true, true)] {
        let mut r = rig((98, 46));
        let (c, row) = r.centre(HitId::Card(1));
        let seq = r.int("eng:msg_seq");
        let mut ui = Ui::new(Theme::recording());
        ui.drawn = r.ui.drawn.clone();
        terminal_events(
            events(c, row),
            &mut r.app,
            &mut ui,
            &mut Input::default(),
            &mut CtrlC::default(),
            mouse,
            Instant::now(),
        )
        .unwrap();
        assert_eq!(r.int("eng:msg_seq") != seq, changed, "mouse = {mouse}");
        assert_eq!(r.int(SELECTED_CARD) == Some(1), changed, "mouse = {mouse}");
    }
}

/// Every mouse action has a key with the same meaning, and it takes the same path.
#[test]
fn every_mouse_action_has_a_keyboard_equivalent_with_the_same_message() {
    let select_card_1 = "Ui(Select(Cards, Index(1)))";
    // Click a card / `l` with the cards focused.
    let mut r = rig((98, 46));
    assert!(names(&r.click(HitId::Card(1))).contains(&select_card_1.to_owned()));
    let mut k = rig((98, 46));
    k.app.step(Msg::Ui(UiIntent::Focus(Pane::Cards))).unwrap();
    k.settle();
    assert_eq!(names(&k.press('l')), [select_card_1]);

    // Double click / Enter.
    assert_eq!(names(&k.key(crossterm::event::KeyCode::Enter)), ["Ui(Open(Cards))"]);

    // Click a pane / Tab.
    let mut k = rig((98, 46));
    assert_eq!(names(&k.key(crossterm::event::KeyCode::Tab)), ["Ui(FocusNext)"]);
    assert_eq!(focus(&k), "cards");

    // Click the strip / `a`.
    let mut k = Rig::new(replay_app(), (80, 24), Theme::recording());
    k.app.step(Msg::Ui(UiIntent::Focus(Pane::Cards))).unwrap();
    k.settle();
    assert_eq!(names(&k.press('a')), ["Ui(Focus(Advisor))", "Ui(ToggleRail)"]);

    // Click a checkpoint / Enter on the rail.
    assert_eq!(names(&k.key(crossterm::event::KeyCode::Enter)), ["Ui(Open(Advisor))"]);

    // The wheel / `k` and `j` in the log.
    let mut k = rig((98, 46));
    k.app.step(Msg::Ui(UiIntent::Focus(Pane::Log))).unwrap();
    k.settle();
    assert_eq!(names(&k.press('k')), ["Ui(Scroll { pane: Log, lines: 1, sideways: false })"]);
    assert_eq!(names(&k.press('j')), ["Ui(Scroll { pane: Log, lines: -1, sideways: false })"]);
}
