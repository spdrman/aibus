//! 08 section 1, soft: time to the first frame. Both numbers are logged and only a very loose
//! ceiling fails, because the budget is for a developer laptop and the runners are shared.
//! Run with `--nocapture` to see them.
//!
//! * warm: the config is loaded; build the app, register the views, draw the first frame at
//!   98x46. Target 150 ms.
//! * cold: the same plus a fresh SQLite index and discovery of 100 definition files. Target
//!   400 ms.

use std::time::{Duration, Instant};

use aibus_core::agent::discover::discover;
use aibus_core::persist::index::Index;
use aibus_core::{App, Msg};
use aibus_graph::mini::MiniGraph;
use aibus_tui::{Theme, Ui};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

const WARM_TARGET: Duration = Duration::from_millis(150);
const COLD_TARGET: Duration = Duration::from_millis(400);
/// Ten times the target: a miss this big is a bug, not a slow runner.
const CEILING: u32 = 10;

fn first_frame() -> App<MiniGraph> {
    let mut app = App::new(MiniGraph::new(), || 1_700_000_000_000);
    let keys = aibus_tui::register(&mut app.graph, app.store.clone());
    app.keys.extend(keys);
    app.set_session_view(
        Some("--model sonnet"),
        (Some("opus 5.5"), true),
        (None, true),
    )
    .unwrap();
    app.step(Msg::Resize { cols: 98, rows: 46 }).unwrap();
    let mut term = Terminal::new(TestBackend::new(98, 46)).unwrap();
    let mut ui = Ui::new(Theme::recording());
    ui.note_commit(&app.graph);
    assert!(
        ui.draw(&mut term, &app.graph).unwrap(),
        "the first frame draws"
    );
    app
}

fn definition(i: usize) -> String {
    format!(
        "---\nname: agent-{i:03}\ndescription: Definition number {i} for the startup measurement.\nrole: worker\ntools: read, grep, glob, ls\nmodel: haiku\nmaxTurns: 20\n---\n\nYou are agent {i}. Do the one thing you were asked and report back.\n"
    )
}

#[test]
fn first_frame_warm_and_cold() {
    let warm = (0..5)
        .map(|_| {
            let start = Instant::now();
            first_frame();
            start.elapsed()
        })
        .min()
        .unwrap();
    println!("first frame, warm (config loaded): {warm:?} (target {WARM_TARGET:?})");

    let dir = tempfile::tempdir().unwrap();
    let agents = dir.path().join(".aibus/agents");
    std::fs::create_dir_all(&agents).unwrap();
    for i in 0..100 {
        std::fs::write(agents.join(format!("agent-{i:03}.md")), definition(i)).unwrap();
    }
    let start = Instant::now();
    let found = discover(None, dir.path(), None, &[], &[]).unwrap();
    let discovered = start.elapsed();
    let _index = Index::open(&dir.path().join("state/index.sqlite")).unwrap();
    let opened = start.elapsed();
    first_frame();
    let cold = start.elapsed();
    assert!(
        found.agents.len() >= 100,
        "{} definitions found",
        found.agents.len()
    );
    println!(
        "first frame, cold (100 definitions, SQLite open): {cold:?} (target {COLD_TARGET:?}); discovery {discovered:?}, index open done at {opened:?}"
    );

    // The bundled SQLite is built without optimisation in a debug build, which is most of the
    // cold number there; the targets are for `--release`.
    let ceiling = if cfg!(debug_assertions) {
        CEILING * 5
    } else {
        CEILING
    };
    assert!(
        warm < WARM_TARGET * ceiling,
        "warm first frame took {warm:?}"
    );
    assert!(
        cold < COLD_TARGET * ceiling,
        "cold first frame took {cold:?}"
    );
}
