use aibus_core::state::value::{as_list, as_str};
use aibus_core::*;
use aibus_graph::mini::MiniGraph;
use aibus_graph::{Graph, Value};
use aibus_core::state::ANIMATING;
use aibus_testkit::tui::{poke, replay_app, run_id};
use aibus_tui::view::{DIRTY, WIDGETS};
use aibus_tui::{Theme, Ui};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn dirty(app: &App<MiniGraph>) -> Vec<String> {
    as_list(app.graph.read_key(DIRTY).unwrap())
        .iter()
        .filter_map(as_str)
        .map(str::to_owned)
        .collect()
}

fn key(c: char) -> Msg {
    Msg::Key(KeyEvent { code: KeyCode::Char(c), ctrl: false, alt: false, shift: false })
}

#[test]
fn the_view_nodes_are_registered() {
    let app = replay_app();
    for (name, node) in WIDGETS {
        assert!(node.starts_with("view:"), "{name} reads {node}");
        assert!(app.graph.node(node).is_some(), "{node} is not registered");
    }
    for node in [
        "view:lead_box",
        "view:cards",
        "view:fork_layer",
        "view:status_bar",
        "view:dirty",
    ] {
        assert!(app.graph.node(node).is_some(), "{node}");
    }
}

#[test]
fn dirty_names_the_widgets_a_commit_changed() {
    let mut app = replay_app();
    app.step(key('x')).unwrap();
    assert!(dirty(&app).is_empty(), "a key press changes no widget: {:?}", dirty(&app));

    app.step(Msg::RouterAnswered {
        fork: ForkId(1400),
        question: RouterQuestion {
            kind: "which_tool".into(),
            context: String::new(),
            choices: Vec::new(),
        },
        answer: RouterAnswer {
            choice: String::new(),
            p: 0.5,
            verdict: Verdict::Split,
            latency: std::time::Duration::ZERO,
            source: PSource::Logprob,
        },
    })
    .unwrap();
    let d = dirty(&app);
    assert!(d.contains(&"fork_layer".to_owned()), "{d:?}");
    assert!(d.contains(&"status_bar".to_owned()), "the fork count is in the status bar: {d:?}");
    assert!(!d.contains(&"cards".to_owned()), "{d:?}");
}

#[test]
fn zero_draws_over_100_idle_commits() {
    let mut app = replay_app();
    let mut term = Terminal::new(TestBackend::new(98, 46)).unwrap();
    let mut ui = Ui::new(Theme::recording());
    ui.note_commit(&app.graph);
    assert!(ui.draw(&mut term, &app.graph).unwrap(), "the first frame draws");
    let start = app.graph.now();
    for i in 0..100 {
        let msg = match i % 3 {
            0 => key('j'),
            1 => Msg::Persisted,
            _ => Msg::Mouse(MouseEvent { kind: MouseKind::Move, col: 1, row: 1 }),
        };
        app.step(msg).unwrap();
        ui.note_commit(&app.graph);
        assert!(!ui.draw(&mut term, &app.graph).unwrap(), "commit {i} drew");
    }
    assert_eq!(app.graph.now(), start + 100, "each message was a commit");
    assert_eq!(ui.draws, 1);
}

#[test]
fn a_dirty_widget_draws_once_and_a_resize_draws() {
    let mut app = replay_app();
    let mut term = Terminal::new(TestBackend::new(98, 46)).unwrap();
    let mut ui = Ui::new(Theme::recording());
    ui.note_commit(&app.graph);
    ui.draw(&mut term, &app.graph).unwrap();

    app.step(Msg::Tick).unwrap();
    ui.note_commit(&app.graph);
    assert!(ui.draw(&mut term, &app.graph).unwrap(), "a tick moves the spinner of a running card");
    assert!(!ui.draw(&mut term, &app.graph).unwrap(), "nothing new, nothing drawn");

    // Two commits before the next draw: the dirty one is not lost to the clean one after it.
    app.step(Msg::Tick).unwrap();
    ui.note_commit(&app.graph);
    app.step(Msg::Persisted).unwrap();
    ui.note_commit(&app.graph);
    assert!(ui.draw(&mut term, &app.graph).unwrap());

    term.backend_mut().resize(80, 24);
    assert!(ui.draw(&mut term, &app.graph).unwrap(), "resize redraws");
    assert_eq!(ui.draws, 4);
}

/// The flag that keeps the 30 Hz ticker going is the same whichever way the column is drawn: it
/// is up while a card runs, and down once the last one has stopped (run with and without
/// `--features flow-diagram`).
#[test]
fn animating_clears_when_the_last_run_stops() {
    let mut app = replay_app();
    assert_eq!(app.graph.read_key(ANIMATING), Some(&Value::Bool(true)), "three runs are going");
    for seq in 1..=3 {
        poke(&mut app, &format!("model:run:{}:status", run_id(seq)), Value::Str("completed".into()));
    }
    assert_eq!(app.graph.read_key(ANIMATING), Some(&Value::Bool(false)), "every run is done");
}
