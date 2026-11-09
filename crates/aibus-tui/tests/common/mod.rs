#![allow(dead_code)]
//! A dashboard on a `TestBackend` driven by synthetic terminal events: what the real loop does,
//! minus the tty. Each event goes through the event layer, its messages are stepped, and the
//! frame is drawn again.

use std::time::{Duration, Instant};

use aibus_core::state::value::as_i64;
use aibus_core::{App, Msg};
use aibus_graph::mini::MiniGraph;
use aibus_graph::Graph;
use aibus_testkit::tui::{frame_of, Frame};
use aibus_tui::app::key_from_crossterm;
use aibus_tui::hit::HitId;
use aibus_tui::input::{Clip, Context, Input, Out};
use aibus_tui::{Theme, Ui};
use crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::backend::TestBackend;
use ratatui::buffer::Cell;
use ratatui::Terminal;

pub const LEFT: MouseButton = MouseButton::Left;

pub struct Rig {
    pub app: App<MiniGraph>,
    pub term: Terminal<TestBackend>,
    pub ui: Ui,
    pub input: Input,
    pub now: Instant,
    pub layout_done: bool,
    /// What menu picks asked to be copied.
    pub copied: Vec<Clip>,
    /// What keys and clicks asked the loop to do outside the graph.
    pub jobs: Vec<aibus_tui::input::Job>,
}

impl Rig {
    pub fn new(mut app: App<MiniGraph>, size: (u16, u16), theme: Theme) -> Self {
        app.step(Msg::Resize { cols: size.0, rows: size.1 }).unwrap();
        let mut rig = Self {
            app,
            term: Terminal::new(TestBackend::new(size.0, size.1)).unwrap(),
            ui: Ui::new(theme),
            input: Input::default(),
            now: Instant::now(),
            layout_done: false,
            copied: Vec::new(),
            jobs: Vec::new(),
        };
        rig.settle();
        rig
    }

    pub fn resize(&mut self, size: (u16, u16)) {
        self.app.step(Msg::Resize { cols: size.0, rows: size.1 }).unwrap();
        self.term.backend_mut().resize(size.0, size.1);
        self.settle();
    }

    /// What the loop does after every step: tell the UI about pointer state and the commit, draw.
    pub fn settle(&mut self) {
        self.input.settle(self.now);
        aibus_tui::app::sync(&mut self.ui, &self.input);
        self.ui.note_commit(&self.app.graph);
        self.ui.draw(&mut self.term, &self.app.graph).unwrap();
    }

    fn apply(&mut self, out: Out) -> Vec<Msg> {
        for m in out.msgs.iter().cloned() {
            self.app.step(m).unwrap();
        }
        self.layout_done |= out.layout_done;
        self.copied.extend(out.copy);
        self.jobs.extend(out.jobs);
        self.settle();
        out.msgs
    }

    /// Lets `ms` pass with the pointer still, and draws what that brings (a tooltip).
    pub fn advance(&mut self, ms: u64) {
        self.now += Duration::from_millis(ms);
        self.settle();
    }

    pub fn right_down(&mut self, col: u16, row: u16) -> Vec<Msg> {
        self.mouse(MouseEventKind::Down(MouseButton::Right), col, row)
    }

    pub fn mouse_with(&mut self, kind: MouseEventKind, col: u16, row: u16, mods: KeyModifiers) -> Vec<Msg> {
        let cx = Context::read(&self.app.graph);
        let ev = MouseEvent { kind, column: col, row, modifiers: mods };
        let out = self.input.mouse(ev, self.now, &self.ui.drawn, &cx, &mut self.ui.prompt);
        self.apply(out)
    }

    pub fn mouse(&mut self, kind: MouseEventKind, col: u16, row: u16) -> Vec<Msg> {
        self.mouse_with(kind, col, row, KeyModifiers::NONE)
    }

    pub fn down(&mut self, col: u16, row: u16) -> Vec<Msg> {
        self.mouse(MouseEventKind::Down(LEFT), col, row)
    }

    pub fn up(&mut self, col: u16, row: u16) -> Vec<Msg> {
        self.mouse(MouseEventKind::Up(LEFT), col, row)
    }

    pub fn drag(&mut self, col: u16, row: u16) -> Vec<Msg> {
        self.mouse(MouseEventKind::Drag(LEFT), col, row)
    }

    pub fn hover(&mut self, col: u16, row: u16) -> Vec<Msg> {
        self.mouse(MouseEventKind::Moved, col, row)
    }

    pub fn key_with(&mut self, code: KeyCode, mods: KeyModifiers) -> Vec<Msg> {
        let ev = key_from_crossterm(KeyEvent::new(code, mods)).unwrap();
        let cx = Context::read(&self.app.graph);
        let out = self.input.key(ev, &cx, &self.ui.keys, &self.ui.drawn, &mut self.ui.prompt);
        self.apply(out)
    }

    pub fn key(&mut self, code: KeyCode) -> Vec<Msg> {
        self.key_with(code, KeyModifiers::NONE)
    }

    pub fn press(&mut self, c: char) -> Vec<Msg> {
        let mods = if c.is_uppercase() { KeyModifiers::SHIFT } else { KeyModifiers::NONE };
        self.key_with(KeyCode::Char(c), mods)
    }

    /// The middle of a region of the last frame.
    pub fn centre(&self, id: HitId) -> (u16, u16) {
        let r = self.ui.drawn.hits.find(id).unwrap_or_else(|| panic!("{id:?} was not drawn")).rect;
        (r.x + r.width / 2, r.y + r.height / 2)
    }

    /// A press and release on a region. Returns the press's messages.
    pub fn click(&mut self, id: HitId) -> Vec<Msg> {
        let (c, r) = self.centre(id);
        let msgs = self.down(c, r);
        self.up(c, r);
        msgs
    }

    pub fn frame(&self) -> Frame {
        frame_of(self.term.backend().buffer())
    }

    pub fn cell(&self, x: u16, y: u16) -> Cell {
        self.term.backend().buffer()[(x, y)].clone()
    }

    pub fn int(&self, key: &str) -> Option<i64> {
        self.app.graph.read_key(key).and_then(as_i64)
    }

    pub fn text(&self) -> String {
        self.frame().text
    }
}

/// `Msg`s as their Debug text, which is how the tests compare them.
pub fn names(msgs: &[Msg]) -> Vec<String> {
    msgs.iter().map(|m| format!("{m:?}")).collect()
}

// ---- C1.2: sessions built for the widget tests ----------------------------------------------

use aibus_core::{Command, Isolation, Role};
use aibus_graph::Value;
use aibus_testkit::tui::{poke, run_id, start, NOW};
use ratatui::layout::Rect;

/// A session with the views registered, the router named `jev`, and a lead, but no workers.
pub fn session() -> App<MiniGraph> {
    let mut app = App::new(MiniGraph::new(), || NOW);
    let keys = aibus_tui::register(&mut app.graph, app.store.clone());
    app.keys.extend(keys);
    app.set_session_view(Some("--model sonnet"), (Some("opus 5.5"), true), (Some("jev"), true)).unwrap();
    app.step(Msg::SubmitPrompt { text: "edits + runs tests".into() }).unwrap();
    start(&mut app, 1, "lead", Role::Lead, "Sonnet 5.5");
    app
}

/// Worker `seq` (2 and up, in order) with its task, progress and status word.
pub fn worker(app: &mut App<MiniGraph>, seq: u128, agent: &str, task: &str, progress: Option<f64>, status: &str) {
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
    start(app, seq, agent, Role::Worker, "Haiku 5.5");
    if let Some(p) = progress {
        poke(app, &format!("model:run:{}:progress", run_id(seq)), Value::F64(p));
    }
    poke(app, &format!("model:run:{}:status", run_id(seq)), Value::Str(status.into()));
}

pub fn ticks(app: &mut App<MiniGraph>, n: u32) {
    for _ in 0..n {
        app.step(Msg::Tick).unwrap();
    }
}

/// One more key of the run's meta, e.g. `effort`.
pub fn meta(app: &mut App<MiniGraph>, seq: u128, key: &str, v: Value) {
    let node = format!("model:run:{}:meta", run_id(seq));
    let old = app.graph.read_key(&node).unwrap().clone();
    poke(app, &node, aibus_core::state::value::with(&old, key, v));
}

/// A log row as the reducer would write it, for the lines the spec shows.
pub fn log_row(app: &mut App<MiniGraph>, at: u64, actor: &str, text: &str, extra: Vec<(&str, Value)>) {
    use aibus_core::state::value::{map_of, n, s};
    let mut fields = vec![("at", n(at)), ("level", s("info")), ("actor", s(actor)), ("run", Value::Null), ("text", s(text))];
    fields.extend(extra);
    let h = app.store.log_push(map_of(fields));
    poke(app, "model:log", Value::Handle(h));
}

impl Rig {
    /// Where `needle` first appears in the frame, as (column, row).
    pub fn find(&self, needle: &str) -> Option<(u16, u16)> {
        let n: Vec<char> = needle.chars().collect();
        for (y, line) in self.text().lines().enumerate() {
            let cs: Vec<char> = line.chars().collect();
            if let Some(x) = cs.windows(n.len()).position(|w| w == n.as_slice()) {
                return Some((x as u16, y as u16));
            }
        }
        None
    }

    /// The cell where `needle` starts.
    pub fn cell_of(&self, needle: &str) -> Cell {
        let (x, y) = self.find(needle).unwrap_or_else(|| panic!("`{needle}` is not on screen:\n{}", self.text()));
        self.cell(x, y)
    }

    pub fn rect(&self, id: HitId) -> Rect {
        self.ui.drawn.hits.find(id).unwrap_or_else(|| panic!("{id:?} was not drawn")).rect
    }
}

/// The part of a frame inside `area`: its text rows, and the style runs of those rows cut to it.
pub fn crop(frame: &Frame, area: Rect) -> (String, String) {
    let (x0, x1) = (usize::from(area.x), usize::from(area.right()));
    let text: String = frame
        .text
        .lines()
        .enumerate()
        .filter(|(y, _)| (area.y..area.bottom()).contains(&(*y as u16)))
        .map(|(_, l)| format!("{}\n", l.chars().skip(x0).take(x1 - x0).collect::<String>().trim_end()))
        .collect();
    let mut styles = String::new();
    for line in frame.styles.lines() {
        let (row, rest) = line.split_at(2);
        let y: u16 = row.parse().unwrap();
        if !(area.y..area.bottom()).contains(&y) {
            continue;
        }
        let rest = rest.trim_start();
        let (span, style) = rest.split_once(' ').unwrap();
        let (a, b) = span.split_once("..").unwrap();
        let (a, b) = (a.parse::<usize>().unwrap().max(x0), b.parse::<usize>().unwrap().min(x1));
        if a < b {
            styles.push_str(&format!("{:02} {:02}..{:02} {style}\n", y - area.y, a - x0, b - x0));
        }
    }
    (text, styles)
}

/// Compares a cropped frame with `tests/snapshots/<name>.txt` and `<name>.styles.txt`.
pub fn assert_crop(frame: &Frame, area: Rect, name: &str, styles: bool) {
    let (text, st) = crop(frame, area);
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/snapshots");
    aibus_testkit::tui::check_snapshot(dir, name, ".txt", &text);
    if styles {
        aibus_testkit::tui::check_snapshot(dir, name, ".styles.txt", &st);
    }
}
