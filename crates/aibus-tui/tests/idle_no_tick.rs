//! 08 section 1: an idle session schedules no `Msg::Tick`.

use std::time::Duration;

use aibus_core::state::value::as_i64;
use aibus_core::state::TICK;
use aibus_core::*;
use aibus_graph::mini::MiniGraph;
use aibus_graph::Graph;

const NOW: u64 = 1_700_000_000_000;

fn ticks(app: &App<MiniGraph>) -> i64 {
    app.graph.read_key(TICK).and_then(as_i64).unwrap()
}

#[tokio::test(start_paused = true)]
async fn no_tick_for_5_seconds_with_no_runs() {
    let mut app = App::new(MiniGraph::new(), || NOW);
    let keys = aibus_tui::register(&mut app.graph, app.store.clone());
    app.keys.extend(keys);
    app.start_ticker(30);
    let before = app.graph.now();
    let ran = tokio::time::timeout(Duration::from_secs(5), app.run()).await;
    assert!(ran.is_err(), "the loop waits for messages");
    assert_eq!(ticks(&app), 0);
    assert_eq!(app.graph.now(), before, "no message arrived, so no commit");
}

#[tokio::test(start_paused = true)]
async fn ticks_flow_while_a_run_is_running() {
    let mut app = App::new(MiniGraph::new(), || NOW);
    app.start_ticker(30);
    app.step(Msg::SubmitPrompt { text: "go".into() }).unwrap();
    app.step(Msg::RunStarted {
        run: RunId(ulid::Ulid::from_parts(NOW, 1)),
        agent: "lead".parse().unwrap(),
        role: Role::Lead,
        parent: None,
        model: ModelId("m".into()),
        account: AccountName("a".into()),
    })
    .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(1), app.run()).await;
    let n = ticks(&app);
    assert!(
        (25..=35).contains(&n),
        "about 30 ticks in one second, got {n}"
    );
}
