//! TUI harness: a replay fixture, `render` onto a `TestBackend`, and `assert_frame!`.
//!
//! Animation frames are deterministic because `render` takes the tick counter and steps the
//! app's own `Msg::Tick` up to it.

use std::fmt::Write as _;
use std::path::Path;

use aibus_core::state::value::as_i64;
use aibus_core::state::TICK;
use aibus_core::*;
use aibus_graph::mini::MiniGraph;
use aibus_graph::{Graph, Value};
use aibus_provider::StreamChunk;
use aibus_tui::{Theme, Ui};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;
use ulid::Ulid;

/// The wall clock of every fixture, in milliseconds.
pub const NOW: u64 = 1_700_000_000_000;

/// One rendered frame as text, plus the same cells as style runs.
pub struct Frame {
    pub text: String,
    pub styles: String,
}

fn colour(c: Color) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Reset => "-".to_owned(),
        other => format!("{other:?}"),
    }
}

/// Rows of cell symbols with trailing blanks trimmed, and per row the runs of equal style.
pub fn frame_of(buf: &Buffer) -> Frame {
    let (mut text, mut styles) = (String::new(), String::new());
    for y in 0..buf.area.height {
        let mut row = String::new();
        let mut run: Option<(u16, String)> = None;
        for x in 0..buf.area.width {
            let cell = &buf[(x, y)];
            row.push_str(cell.symbol());
            let mods = cell.modifier;
            let key = format!(
                "fg={} bg={}{}",
                colour(cell.fg),
                colour(cell.bg),
                if mods == Modifier::empty() { String::new() } else { format!(" {mods:?}") }
            );
            match &run {
                Some((_, k)) if *k == key => {}
                _ => {
                    if let Some((from, k)) = run.take() {
                        let _ = writeln!(styles, "{y:02} {from:02}..{x:02} {k}");
                    }
                    run = Some((x, key));
                }
            }
        }
        if let Some((from, k)) = run {
            let _ = writeln!(styles, "{y:02} {from:02}..{:02} {k}", buf.area.width);
        }
        text.push_str(row.trim_end());
        text.push('\n');
    }
    Frame { text, styles }
}

/// Renders the app's views at `size` (cols, rows) with the tick counter at `tick`.
/// The app must have the `aibus_tui` views registered (`replay_app` does).
pub fn render<G: Graph>(app: &mut App<G>, size: (u16, u16), tick: u64) -> Frame {
    app.step(Msg::Resize { cols: size.0, rows: size.1 }).unwrap();
    while app.graph.read_key(TICK).and_then(as_i64).unwrap_or(0) < tick as i64 {
        app.step(Msg::Tick).unwrap();
    }
    let mut term = Terminal::new(TestBackend::new(size.0, size.1)).unwrap();
    let mut ui = Ui::new(Theme::recording());
    ui.draw(&mut term, &app.graph).unwrap();
    frame_of(term.backend().buffer())
}

/// Compares `got` with `<dir>/<name><ext>`; `AIBUS_UPDATE_SNAPSHOTS=1` writes it instead.
pub fn check_snapshot(dir: &str, name: &str, ext: &str, got: &str) {
    let path = Path::new(dir).join(format!("{name}{ext}"));
    if std::env::var_os("AIBUS_UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(&path, got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("no snapshot {}; run with AIBUS_UPDATE_SNAPSHOTS=1", path.display()));
    assert!(
        want == got,
        "snapshot {} differs\n--- want\n{want}\n--- got\n{got}",
        path.display()
    );
}

/// `tests/snapshots/<name>.txt` of the calling crate holds the frame's text.
#[macro_export]
macro_rules! assert_frame {
    ($frame:expr, $name:expr) => {
        $crate::tui::check_snapshot(
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/snapshots"),
            $name,
            ".txt",
            &$frame.text,
        )
    };
}

/// `tests/snapshots/<name>.styles.txt` holds fg, bg and modifier runs per row.
#[macro_export]
macro_rules! assert_styles {
    ($frame:expr, $name:expr) => {
        $crate::tui::check_snapshot(
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/snapshots"),
            $name,
            ".styles.txt",
            &$frame.styles,
        )
    };
}

/// The id the nth run of a fresh fixture gets (the core derives it from the clock and a counter).
pub fn run_id(seq: u128) -> RunId {
    RunId(Ulid::from_parts(NOW, seq))
}

pub fn poke<G: Graph>(app: &mut App<G>, key: &str, v: Value) {
    let node = app.graph.node(key).unwrap_or_else(|| panic!("{key} not registered"));
    app.graph.commit("poke", &[(node, v)]).unwrap();
}

fn start<G: Graph>(app: &mut App<G>, seq: u128, agent: &str, role: Role, model: &str) {
    app.step(Msg::RunStarted {
        run: run_id(seq),
        agent: agent.parse().unwrap(),
        role,
        parent: None,
        model: ModelId(model.into()),
        account: AccountName("acct".into()),
    })
    .unwrap();
}

fn answer<G: Graph>(app: &mut App<G>, fork: u64, kind: &str, p: f64, verdict: Verdict) {
    app.step(Msg::RouterAnswered {
        fork: ForkId(fork),
        question: RouterQuestion { kind: kind.into(), prompt: String::new() },
        answer: RouterAnswer { choice: String::new(), p, verdict },
    })
    .unwrap();
}

/// A session near f02 of the recording: a lead, three workers (71 %, 35 % and done), three
/// router rows with one split, and the advisor reading at `before_plan`.
pub fn replay_app() -> App<MiniGraph> {
    let mut app = App::new(MiniGraph::new(), || NOW);
    let keys = aibus_tui::register(&mut app.graph, app.store.clone());
    app.keys.extend(keys);
    app.step(Msg::SubmitPrompt { text: "edits + runs tests".into() }).unwrap();
    start(&mut app, 1, "lead", Role::Lead, "Sonnet 5.5");
    for (seq, agent, task) in [
        (2, "search", "maps the repo"),
        (3, "grep", "finds call sites"),
        (4, "audit", "runs in a worktree"),
    ] {
        app.step(Msg::Command(Command::Delegate {
            agent: agent.parse().unwrap(),
            prompt: task.into(),
            background: false,
            isolation: Isolation::None,
            parent: None,
            run: None,
            model: None,
            account: None,
        }))
        .unwrap();
        start(&mut app, seq, agent, Role::Worker, "Haiku 5.5");
    }
    for (seq, p) in [(2, 0.71), (3, 0.35), (4, 1.0)] {
        poke(&mut app, &format!("model:run:{}:progress", run_id(seq)), Value::F64(p));
    }
    poke(&mut app, &format!("model:run:{}:status", run_id(4)), Value::Str("completed".into()));
    answer(&mut app, 1312, "which_file", 0.94, Verdict::Sharp);
    answer(&mut app, 1313, "which_tool", 0.94, Verdict::Sharp);
    answer(&mut app, 1314, "retry_or_stop", 0.58, Verdict::Split);
    app.step(Msg::AdvisorSpoke {
        checkpoint: Checkpoint::BeforePlan,
        advice: "plan skips auth".into(),
        tokens_read: 122_000,
    })
    .unwrap();
    app.step(Msg::CheckpointHit { checkpoint: Checkpoint::BeforePlan, detail: String::new() })
        .unwrap();
    app
}

/// The A1.3 load on top of `replay_app`: eight runs, a 100,000-line transcript on the lead,
/// 500 lines on every other run and 2,000 log rows.
pub fn heavy_app() -> App<MiniGraph> {
    let mut app = replay_app();
    for seq in 5..=8 {
        app.step(Msg::Command(Command::Delegate {
            agent: "extra".parse().unwrap(),
            prompt: "more work".into(),
            background: false,
            isolation: Isolation::None,
            parent: None,
            run: None,
            model: None,
            account: None,
        }))
        .unwrap();
        start(&mut app, seq, "extra", Role::Worker, "Haiku 5.5");
    }
    let stream = |seq: u128, text: String| Msg::Stream {
        run: run_id(seq),
        chunks: vec![StreamChunk::TextDelta(text)],
    };
    let line = |i: usize| {
        format!("line {i:06} the quick brown fox jumps over the lazy dog and keeps going a while\n")
    };
    let mut chunk = String::new();
    for i in 0..100_000 {
        chunk.push_str(&line(i));
        if chunk.len() >= 4096 {
            app.step(stream(1, std::mem::take(&mut chunk))).unwrap();
        }
    }
    app.step(stream(1, chunk)).unwrap();
    for seq in 2..=8 {
        app.step(stream(seq, line(0).repeat(500))).unwrap();
    }
    for i in 0..2000 {
        app.step(Msg::IoError { context: format!("ctx {i}"), error: "e".into() }).unwrap();
    }
    app
}
