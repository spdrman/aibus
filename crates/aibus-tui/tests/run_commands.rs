//! G5 (#124): the per-agent commands on the real screen. Each one is typed into the prompt of a
//! `Rig`, so the registry, the event layer, the reducer and the views are all in the loop.

mod common;

use aibus_core::state::value::{get, map, n, s};
use aibus_core::state::MODAL;
use aibus_graph::{Graph, Value};
use aibus_testkit::tui::{poke, run_id};
use aibus_tui::hit::HitId;
use aibus_tui::Theme;
use common::*;
use crossterm::event::{KeyCode as K, MouseEventKind as Mk};

/// A lead (run 1) and a running `grep` worker (run 2).
fn rig() -> Rig {
    let mut app = session();
    worker(&mut app, 2, "grep", "find the parser", Some(0.4), "running");
    Rig::new(app, (98, 46), Theme::recording())
}

fn modal(r: &Rig) -> Option<String> {
    r.app
        .graph
        .read_key(MODAL)
        .and_then(aibus_core::state::value::as_str)
        .map(str::to_owned)
}

fn type_line(r: &mut Rig, line: &str) -> Vec<String> {
    r.ui.prompt.set(line);
    names(&r.key(K::Enter))
}

fn usage(r: &mut Rig, seq: u128, input: i64, cost: f64) {
    let u = map([
        ("input_tokens", n(input)),
        ("output_tokens", n(300)),
        ("cache_read", n(50)),
        ("cache_write", n(10)),
        ("cost_usd", Value::F64(cost)),
    ]);
    poke(&mut r.app, &format!("model:run:{}:usage", run_id(seq)), u);
}

fn set_status(r: &mut Rig, seq: u128, status: &str) {
    poke(
        &mut r.app,
        &format!("model:run:{}:status", run_id(seq)),
        Value::Str(status.into()),
    );
}

#[test]
fn cancel_lead_asks_and_cancel_worker_does_not() {
    let mut r = rig();
    let sent = type_line(&mut r, "/cancel @lead");
    assert!(
        !sent.iter().any(|m| m.contains("Cancel {")),
        "the lead is not cancelled yet: {sent:?}"
    );
    assert_eq!(modal(&r), Some(format!("confirm:cancel:{}", run_id(1))));
    assert!(r.text().contains("Cancel the lead run?"), "{}", r.text());
    let by_key = names(&r.press('y'));
    assert!(
        by_key.contains(&format!("Command(Cancel {{ run: {:?} }})", run_id(1))),
        "{by_key:?}"
    );
    assert_eq!(modal(&r), None);
    // The button does the same as the key.
    type_line(&mut r, "/cancel @lead");
    let by_mouse = names(&r.click(HitId::Button("cancel_yes")));
    assert_eq!(by_key, by_mouse, "keyboard parity");
    // A worker goes at once.
    let mut r = rig();
    let sent = type_line(&mut r, "/cancel @grep");
    assert_eq!(
        sent,
        [format!("Command(Cancel {{ run: {:?} }})", run_id(2))]
    );
    assert_eq!(modal(&r), None);
}

#[test]
fn retry_is_only_offered_on_failed_or_partial() {
    let mut r = rig();
    assert!(type_line(&mut r, "/retry @grep").is_empty());
    assert_eq!(
        r.ui.prompt.notice.as_deref(),
        Some("/retry is not available: the run is running")
    );
    for status in ["failed", "partial"] {
        set_status(&mut r, 2, status);
        let sent = type_line(&mut r, "/retry @grep");
        assert_eq!(
            sent,
            [format!("Command(Retry {{ run: {:?} }})", run_id(2))],
            "{status}"
        );
        set_status(&mut r, 2, status);
    }
    set_status(&mut r, 2, "running");
    type_line(&mut r, "/resume @grep go on");
    assert_eq!(
        r.ui.prompt.notice.as_deref(),
        Some("/resume is not available: the run is running")
    );
}

#[test]
fn status_modal_matches_the_graph() {
    let mut r = rig();
    usage(&mut r, 2, 1200, 0.042);
    type_line(&mut r, "/status @grep");
    assert_eq!(modal(&r), Some(format!("status:{}", run_id(2))));
    let view = r.app.graph.read_key("view:run_status").unwrap().clone();
    let row = |k: &str| {
        let rows = aibus_core::state::value::as_list(get(&view, "rows").unwrap()).to_vec();
        let hit = rows
            .iter()
            .find(|x| get(x, "k").and_then(aibus_core::state::value::as_str) == Some(k))
            .unwrap()
            .clone();
        get(&hit, "v")
            .and_then(aibus_core::state::value::as_str)
            .unwrap()
            .to_owned()
    };
    assert_eq!(row("agent"), "grep");
    assert_eq!(row("role"), "worker");
    assert_eq!(row("status"), "running");
    assert_eq!(row("input tokens"), "1200");
    assert_eq!(row("output tokens"), "300");
    assert_eq!(row("cache read"), "50");
    assert_eq!(row("cache write"), "10");
    assert_eq!(row("cost"), "$0.0420");
    let text = r.text();
    for want in ["agent", "grep", "1200", "$0.0420", "elapsed"] {
        assert!(text.contains(want), "{want}: {text}");
    }
    r.key(K::Esc);
    assert_eq!(modal(&r), None);
    // The seats have a modal of their own.
    type_line(&mut r, "/status @advisor");
    assert_eq!(modal(&r).as_deref(), Some("status:advisor"));
    assert!(r.text().contains("checkpoints"), "{}", r.text());
}

#[test]
fn cost_table_total_equals_the_status_bar() {
    let mut r = rig();
    usage(&mut r, 1, 5000, 0.01);
    usage(&mut r, 2, 1200, 0.042);
    type_line(&mut r, "/cost");
    assert_eq!(modal(&r).as_deref(), Some("cost"));
    let table = r.app.graph.read_key("view:cost").unwrap().clone();
    let bar = r.app.graph.read_key("view:status_bar").unwrap().clone();
    assert_eq!(
        get(&table, "total"),
        get(&bar, "cost"),
        "one sum feeds both"
    );
    assert_eq!(get(&table, "total"), Some(&s("$0.0520")));
    let rows = aibus_core::state::value::as_list(get(&table, "rows").unwrap()).to_vec();
    assert_eq!(
        get(&rows[0], "agent"),
        Some(&s("grep")),
        "dearest run first"
    );
    assert!(
        r.text().contains("session total") && r.text().contains("$0.0520"),
        "{}",
        r.text()
    );
    r.key(K::Esc);
    // `$` is a dashboard key, so it is typing while the prompt has focus: leave the prompt first.
    r.key(K::Tab);
    r.press('$');
    assert_eq!(
        modal(&r).as_deref(),
        Some("cost"),
        "the key opens the same table"
    );
}

#[test]
fn menu_has_status_and_cost_and_the_old_items_are_unchanged() {
    let mut r = rig();
    let (c, row) = r.centre(HitId::Card(0));
    r.mouse(Mk::Down(crossterm::event::MouseButton::Right), c, row);
    let menu = r.app.graph.read_key("view:menu").unwrap().clone();
    let ids: Vec<String> = aibus_core::state::value::as_list(get(&menu, "items").unwrap())
        .iter()
        .map(|i| {
            get(i, "id")
                .and_then(aibus_core::state::value::as_str)
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        ids,
        [
            "open",
            "cancel",
            "fork",
            "resume",
            "copy_id",
            "copy_result",
            "status",
            "cost"
        ]
    );
    let labels = [
        "Open transcript",
        "Cancel",
        "Fork",
        "Resume",
        "Copy id",
        "Copy result",
        "Status",
        "Cost",
    ];
    let ys: Vec<u16> = labels
        .iter()
        .map(|l| r.find(l).unwrap_or_else(|| panic!("{l}: {}", r.text())).1)
        .collect();
    assert!(
        ys.windows(2).all(|w| w[1] == w[0] + 1),
        "one row each, in order: {ys:?}"
    );
    let sent = names(&r.click(HitId::MenuItem(6)));
    assert!(
        sent.iter()
            .any(|m| m.starts_with("Command(OpenScreen(RunStatus(")),
        "{sent:?}"
    );
}
