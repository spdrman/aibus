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
use aibus_tui::input::{Context, Input, Out};
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
        self.ui.pointer = self.input.pointer;
        self.ui.note_commit(&self.app.graph);
        self.ui.draw(&mut self.term, &self.app.graph).unwrap();
    }

    fn apply(&mut self, out: Out) -> Vec<Msg> {
        for m in out.msgs.iter().cloned() {
            self.app.step(m).unwrap();
        }
        self.layout_done |= out.layout_done;
        self.settle();
        out.msgs
    }

    pub fn advance(&mut self, ms: u64) {
        self.now += Duration::from_millis(ms);
    }

    pub fn mouse_with(&mut self, kind: MouseEventKind, col: u16, row: u16, mods: KeyModifiers) -> Vec<Msg> {
        let cx = Context::read(&self.app.graph);
        let ev = MouseEvent { kind, column: col, row, modifiers: mods };
        let out = self.input.mouse(ev, self.now, &self.ui.drawn, &cx);
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
        let out = self.input.key(ev, &cx, &self.ui.keys, &self.ui.drawn, &mut self.ui.prompt.text);
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
