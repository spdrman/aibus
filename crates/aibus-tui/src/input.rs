//! The event layer (05 section 6.1): a terminal key or mouse event becomes `Msg`s that go through
//! `reduce` like any other message. The hit map of the last frame says what a pointer event is
//! on; hover and pressed state live here, not in the graph, because they change with every move.
//!
//! The terminal itself is not touched here, so a test can feed synthetic events through this and
//! then draw the result into a `TestBackend`.

use std::time::{Duration, Instant};

use aibus_core::state::value::{as_i64, as_list, as_str, get};
use aibus_core::state::{run_key, PERMISSIONS_OPEN, RUNS_ORDER};
use aibus_core::{
    Command, Decision, KeyCode, KeyEvent, Msg, Pane, PermissionId, RunId, Selection, UiIntent,
};
use aibus_graph::Graph;
use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::hit::{HitId, HitKind, Segment};
use crate::keys::{Action, Ctx, Keymap};
use crate::layout::{log_for_drag, rail_for_drag};
use crate::ui::{Chrome, Drawn, Pointer};
use crate::view::{CARDS, LOG_LINES, MENU_VIEW};

/// Two presses on the same thing this close together are a double click.
pub const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// A pointer resting this long on something shows its tooltip.
pub const TOOLTIP_AFTER: Duration = Duration::from_millis(500);
/// Rows per wheel notch.
pub const WHEEL_LINES: i32 = 3;

/// The open context menu as the key map and the pointer need it.
#[derive(Clone, Debug)]
pub struct Menu {
    pub run: RunId,
    pub selected: usize,
    /// Each item's id and whether it applies to the run.
    pub items: Vec<(String, bool)>,
}

/// What a menu pick wants copied. The loop turns it into an OSC 52 sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Clip {
    Text(String),
    /// The result of this run.
    Result(RunId),
}

/// What the key map and the pointer need to know about the session, read from the graph.
#[derive(Clone, Debug)]
pub struct Context {
    pub chrome: Chrome,
    pub selected_run: Option<RunId>,
    pub lead_run: Option<RunId>,
    pub permission: Option<PermissionId>,
    /// The run of each card, in the order the cards are drawn.
    pub cards: Vec<Option<RunId>>,
    /// The run each log line belongs to, if any.
    pub log_runs: Vec<Option<RunId>>,
    pub menu: Option<Menu>,
}

impl Context {
    pub fn read<G: Graph>(g: &G) -> Self {
        let chrome = Chrome::read(g);
        let run_of = |v: &aibus_graph::Value, key: &str| {
            get(v, key).and_then(as_str).and_then(|id| id.parse::<RunId>().ok())
        };
        let card_views = g.read_key(CARDS).map_or(&[][..], as_list);
        let cards: Vec<Option<RunId>> = card_views.iter().map(|c| run_of(c, "run_id")).collect();
        let log_runs: Vec<Option<RunId>> =
            g.read_key(LOG_LINES).map_or(&[][..], as_list).iter().map(|l| run_of(l, "run")).collect();
        let menu = g.read_key(MENU_VIEW).filter(|m| !matches!(m, aibus_graph::Value::Null)).and_then(|m| {
            Some(Menu {
                run: run_of(m, "run_id")?,
                selected: usize::try_from(get(m, "selected").and_then(as_i64)?).ok()?,
                items: as_list(get(m, "items")?)
                    .iter()
                    .map(|i| {
                        (
                            get(i, "id").and_then(as_str).unwrap_or("").to_owned(),
                            matches!(get(i, "enabled"), Some(aibus_graph::Value::Bool(true))),
                        )
                    })
                    .collect(),
            })
        });
        let selected_run = cards.get(chrome.selected_card).copied().flatten();
        let lead_run = g
            .read_key(RUNS_ORDER)
            .map_or(&[][..], as_list)
            .iter()
            .filter_map(as_str)
            .find(|id| {
                g.read_key(&run_key(id, "meta"))
                    .and_then(|m| get(m, "role"))
                    .and_then(as_str)
                    == Some("lead")
            })
            .and_then(|id| id.parse().ok());
        let permission = g
            .read_key(PERMISSIONS_OPEN)
            .map_or(&[][..], as_list)
            .first()
            .and_then(|p| get(p, "id"))
            .and_then(as_str)
            .and_then(|id| id.parse().ok());
        Self { chrome, selected_run, lead_run, permission, cards, log_runs, menu }
    }

    /// The run of the log line Enter and the menu key mean: the selected one, else the newest.
    pub fn log_run(&self) -> Option<RunId> {
        let at = self.chrome.selected_log.unwrap_or(self.log_runs.len().saturating_sub(1));
        self.log_runs.get(at).copied().flatten()
    }

    /// Which set of keys is live.
    pub fn keys(&self) -> Ctx {
        if self.menu.is_some() && self.chrome.modal.is_none() {
            return Ctx::Menu;
        }
        match self.chrome.modal.as_deref() {
            Some("permission") => Ctx::Permission,
            Some(m) if m.starts_with("transcript:") => Ctx::Transcript,
            Some(_) => Ctx::Modal,
            None => match self.chrome.focus {
                Pane::Cards => Ctx::Cards,
                Pane::Log => Ctx::Log,
                Pane::Advisor => Ctx::Rail,
                Pane::Input | Pane::Forks => Ctx::Prompt,
            },
        }
    }
}

/// What handling one event produced.
#[derive(Debug, Default)]
pub struct Out {
    pub msgs: Vec<Msg>,
    /// Pointer or prompt state changed, so the frame needs drawing even without a commit.
    pub redraw: bool,
    /// A drag of a border ended: the sizes are final and worth saving.
    pub layout_done: bool,
    /// A menu pick asked for this to be copied.
    pub copy: Option<Clip>,
}

#[derive(Clone, Copy, Debug)]
enum Drag {
    Rail,
    Log,
    /// The scrollbar thumb: where the press was and where the thumb's top was then.
    Thumb { row: u16, top: u16 },
}

#[derive(Debug, Default)]
pub struct Input {
    pub pointer: Pointer,
    /// The log's filter is being typed: keys go to it until Enter or Esc.
    pub filter_edit: bool,
    last_click: Option<(HitId, Instant)>,
    drag: Option<Drag>,
    /// What the pointer rests on and since when, for the tooltip.
    resting: Option<(HitId, Instant)>,
    /// The tooltip `Space` or a click on the ledger asked for.
    pinned: Option<HitId>,
    /// A tooltip was up when the last key came in, so `Space` closes it instead of reopening.
    tip_was_up: bool,
}

/// Only these have anything to say in a tooltip.
fn tippable(id: HitId) -> bool {
    matches!(
        id,
        HitId::Card(_) | HitId::LogRow(_) | HitId::Status(Segment::Ledger | Segment::Alert)
    )
}

fn ui(i: UiIntent) -> Msg {
    Msg::Ui(i)
}

/// The pane a wheel turn belongs to: the one under the pointer.
fn pane_under(id: HitId) -> Option<Pane> {
    match id {
        HitId::Pane(p) | HitId::Track(p) | HitId::Thumb(p) => Some(p),
        HitId::Card(_) => Some(Pane::Cards),
        HitId::LogRow(_) | HitId::LogHandle => Some(Pane::Log),
        HitId::Checkpoint(_) | HitId::RailStrip | HitId::RailHandle => Some(Pane::Advisor),
        HitId::Status(_) | HitId::Backdrop | HitId::Modal | HitId::MenuBox | HitId::MenuItem(_) => None,
    }
}

impl Input {
    /// Settles the tooltip for time `now`: the pinned one, else the one the pointer has rested
    /// on for 500 ms. Returns whether it changed (so the frame needs drawing).
    pub fn settle(&mut self, now: Instant) -> bool {
        let rested = self
            .resting
            .filter(|(id, at)| tippable(*id) && now.saturating_duration_since(*at) >= TOOLTIP_AFTER)
            .map(|(id, _)| id);
        let tip = self.pinned.or(rested);
        let changed = tip != self.pointer.tip;
        self.pointer.tip = tip;
        changed
    }

    /// How long until a tooltip is due, when the pointer is resting on something that has one.
    pub fn tip_due(&self, now: Instant) -> Option<Duration> {
        let (id, at) = self.resting?;
        if !tippable(id) || self.pointer.tip.is_some() {
            return None;
        }
        Some(TOOLTIP_AFTER.saturating_sub(now.saturating_duration_since(at)))
    }

    pub fn mouse(&mut self, ev: MouseEvent, now: Instant, drawn: &Drawn, cx: &Context) -> Out {
        let mut out = Out::default();
        let (col, row) = (ev.column, ev.row);
        let hit = drawn.hits.at(col, row).map(|r| (r.id, r.kind));
        let modal = cx.chrome.modal.as_deref();
        let hover = hit.map(|h| h.0);
        let set_hover = |input: &mut Self, out: &mut Out| {
            if input.pointer.hover != hover {
                input.pointer.hover = hover;
                // Resting starts over; a tooltip that came from resting goes away.
                input.resting = hover.map(|id| (id, now));
                out.redraw = true;
            }
        };
        match ev.kind {
            MouseEventKind::Moved => set_hover(self, &mut out),
            MouseEventKind::Down(MouseButton::Left) => {
                set_hover(self, &mut out);
                // A press ends a tooltip; resting starts over from here.
                self.resting = hover.map(|id| (id, now));
                self.pinned = None;
                self.press(hit, row, now, drawn, cx, &mut out);
            }
            MouseEventKind::Down(MouseButton::Right) => {
                set_hover(self, &mut out);
                self.resting = hover.map(|id| (id, now));
                self.pinned = None;
                self.context(hit, (col, row), cx, &mut out);
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                set_hover(self, &mut out);
                if modal.is_none() {
                    self.dragged(col, row, drawn, cx, &mut out);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                set_hover(self, &mut out);
                if self.pointer.pressed.take().is_some() {
                    out.redraw = true;
                }
                if matches!(self.drag.take(), Some(Drag::Rail | Drag::Log)) {
                    out.layout_done = true;
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let sideways = ev.modifiers.contains(KeyModifiers::SHIFT);
                self.wheel(hit, ev.kind == MouseEventKind::ScrollUp, sideways, modal, &mut out);
            }
            MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => {
                self.wheel(hit, ev.kind == MouseEventKind::ScrollLeft, true, modal, &mut out);
            }
            _ => {}
        }
        out.redraw |= self.settle(now);
        out
    }

    /// Right click: on a run's card or row it selects it and opens the menu; with a menu open it
    /// closes it.
    fn context(&mut self, hit: Option<(HitId, HitKind)>, (col, row): (u16, u16), cx: &Context, out: &mut Out) {
        if cx.menu.is_some() {
            out.msgs.push(ui(UiIntent::CloseMenu));
            return;
        }
        if cx.chrome.modal.is_some() {
            return;
        }
        let (pane, i, run) = match hit.map(|h| h.0) {
            Some(HitId::Card(i)) => (Pane::Cards, i, cx.cards.get(i).copied().flatten()),
            Some(HitId::LogRow(i)) => (Pane::Log, i, cx.log_runs.get(i).copied().flatten()),
            _ => return,
        };
        out.msgs.push(ui(UiIntent::Focus(pane)));
        out.msgs.push(ui(UiIntent::Select(pane, Selection::Index(i as i64))));
        if let Some(run) = run {
            out.msgs.push(ui(UiIntent::OpenMenu { run, col, row }));
        }
    }

    /// What choosing item `i` of the menu does, then the menu closes. An item that does not apply
    /// does nothing and the menu stays.
    fn pick(menu: &Menu, i: usize, out: &mut Out) {
        let Some((item, true)) = menu.items.get(i).map(|(id, on)| (id.as_str(), *on)) else {
            return;
        };
        let run = menu.run;
        match item {
            "open" => out.msgs.push(Msg::Command(Command::OpenTranscript(run))),
            "cancel" => out.msgs.push(Msg::Command(Command::Cancel { run })),
            "fork" => out.msgs.push(Msg::Command(Command::Fork {
                prompt: format!("continue the work of run {run}"),
                parent: Some(run),
            })),
            "resume" => out.msgs.push(Msg::Command(Command::Resume { run, message: "continue".into() })),
            "copy_id" => out.copy = Some(Clip::Text(run.to_string())),
            "copy_result" => out.copy = Some(Clip::Result(run)),
            _ => {}
        }
        out.msgs.push(ui(UiIntent::CloseMenu));
    }

    fn wheel(&self, hit: Option<(HitId, HitKind)>, up: bool, sideways: bool, modal: Option<&str>, out: &mut Out) {
        if modal.is_some() {
            return;
        }
        let Some(pane) = hit.and_then(|h| pane_under(h.0)) else { return };
        let lines = if up { WHEEL_LINES } else { -WHEEL_LINES };
        out.msgs.push(ui(UiIntent::Scroll { pane, lines, sideways }));
    }

    fn press(
        &mut self,
        hit: Option<(HitId, HitKind)>,
        row: u16,
        now: Instant,
        drawn: &Drawn,
        cx: &Context,
        out: &mut Out,
    ) {
        let Some((id, _)) = hit else { return };
        // An open context menu takes the click: an item picks, its border does nothing, and
        // anywhere else closes it without the click reaching what is behind.
        if let Some(menu) = &cx.menu {
            match id {
                HitId::MenuItem(i) => {
                    self.pointer.pressed = Some(id);
                    out.redraw = true;
                    Self::pick(menu, i, out);
                }
                HitId::MenuBox => {}
                _ => out.msgs.push(ui(UiIntent::CloseMenu)),
            }
            return;
        }
        // A modal's backdrop catches the click so it never reaches the dashboard. Only a
        // dismissible one closes on it: an outside click must not be an unintended deny.
        if matches!(id, HitId::Backdrop) {
            if cx.chrome.modal.as_deref() != Some("permission") {
                out.msgs.push(Msg::Command(Command::CloseModal));
            }
            return;
        }
        if matches!(id, HitId::Modal) {
            // The help screen closes on any key, and so on any click.
            if cx.chrome.modal.as_deref() == Some("help") {
                out.msgs.push(Msg::Command(Command::CloseModal));
            }
            return;
        }
        self.pointer.pressed = Some(id);
        out.redraw = true;
        let double = self
            .last_click
            .is_some_and(|(last, at)| last == id && now.saturating_duration_since(at) <= DOUBLE_CLICK);
        self.last_click = if double { None } else { Some((id, now)) };
        match id {
            HitId::Pane(p) => out.msgs.push(ui(UiIntent::Focus(p))),
            HitId::Card(i) => {
                out.msgs.push(ui(UiIntent::Focus(Pane::Cards)));
                out.msgs.push(ui(UiIntent::Select(Pane::Cards, Selection::Index(i as i64))));
                if double {
                    out.msgs.push(ui(UiIntent::Open(Pane::Cards)));
                }
            }
            HitId::LogRow(i) => {
                out.msgs.push(ui(UiIntent::Focus(Pane::Log)));
                out.msgs.push(ui(UiIntent::Select(Pane::Log, Selection::Index(i as i64))));
                if let (true, Some(run)) = (double, cx.log_runs.get(i).copied().flatten()) {
                    out.msgs.push(Msg::Command(Command::OpenTranscript(run)));
                }
            }
            HitId::Status(seg) => match seg {
                Segment::Swarm | Segment::Subagents => out.msgs.push(ui(UiIntent::Focus(Pane::Cards))),
                Segment::Advisor => {
                    out.msgs.push(ui(UiIntent::Focus(Pane::Advisor)));
                    out.msgs.push(ui(UiIntent::Open(Pane::Advisor)));
                }
                Segment::Router => out.msgs.push(ui(UiIntent::Focus(Pane::Forks))),
                // The ledger line is cut to fit: a click shows it whole, as a tooltip.
                Segment::Ledger => self.pinned = Some(id),
                // The permission screen itself arrives with the modal (C1.3).
                Segment::Alert => {}
            },
            HitId::Checkpoint(_) => {
                out.msgs.push(ui(UiIntent::Focus(Pane::Advisor)));
                out.msgs.push(ui(UiIntent::Open(Pane::Advisor)));
            }
            HitId::RailStrip => {
                out.msgs.push(ui(UiIntent::Focus(Pane::Advisor)));
                out.msgs.push(ui(UiIntent::ToggleRail));
            }
            HitId::RailHandle => self.drag = Some(Drag::Rail),
            HitId::LogHandle => self.drag = Some(Drag::Log),
            HitId::Track(pane) => {
                let thumb = drawn.hits.find(HitId::Thumb(pane)).map(|r| r.rect);
                let page = i32::try_from(drawn.log.visible).unwrap_or(i32::MAX).max(1);
                if let Some(t) = thumb {
                    // Above the thumb is towards older rows.
                    let lines = if row < t.y { page } else { -page };
                    out.msgs.push(ui(UiIntent::Scroll { pane, lines, sideways: false }));
                }
            }
            HitId::Thumb(_) => {
                let top = drawn.hits.find(id).map_or(row, |r| r.rect.y);
                self.drag = Some(Drag::Thumb { row, top });
            }
            HitId::Backdrop | HitId::Modal | HitId::MenuBox | HitId::MenuItem(_) => {}
        }
    }

    fn dragged(&mut self, col: u16, row: u16, drawn: &Drawn, cx: &Context, out: &mut Out) {
        let Some(drag) = self.drag else { return };
        let cols = drawn.areas.status.width;
        match drag {
            Drag::Rail => {
                let w = rail_for_drag(&drawn.areas, col, cols);
                out.msgs.push(ui(UiIntent::SetRailWidth(w)));
            }
            Drag::Log => {
                let h = log_for_drag(&drawn.areas, row);
                out.msgs.push(ui(UiIntent::SetLogRows(h)));
            }
            Drag::Thumb { row: down, top } => {
                let Some(track) = drawn.hits.find(HitId::Track(Pane::Log)).map(|r| r.rect) else {
                    return;
                };
                let thumb_h = drawn.hits.find(HitId::Thumb(Pane::Log)).map_or(1, |r| r.rect.height);
                let (total, visible) = (drawn.log.total, drawn.log.visible);
                let max_scroll = total.saturating_sub(visible);
                let travel = usize::from(track.height.saturating_sub(thumb_h));
                if max_scroll == 0 || travel == 0 {
                    return;
                }
                let wanted = i64::from(top) - i64::from(track.y) + i64::from(row) - i64::from(down);
                let from_top = usize::try_from(wanted.clamp(0, travel as i64)).unwrap_or(0);
                let target = max_scroll - from_top * max_scroll / travel;
                let lines = target as i64 - cx.chrome.scroll_log.min(max_scroll) as i64;
                if lines != 0 {
                    out.msgs.push(ui(UiIntent::Scroll {
                        pane: Pane::Log,
                        lines: i32::try_from(lines).unwrap_or(0),
                        sideways: false,
                    }));
                }
            }
        }
    }

    /// A key press. `prompt` is the text being typed, edited in place.
    pub fn key(
        &mut self,
        ev: KeyEvent,
        cx: &Context,
        keys: &Keymap,
        drawn: &Drawn,
        prompt: &mut String,
    ) -> Out {
        let mut out = Out::default();
        // A key ends any tooltip, whether the pointer or `Space` brought it up.
        self.tip_was_up = self.pinned.take().is_some() || self.pointer.tip.is_some();
        self.resting = None;
        if self.tip_was_up {
            self.pointer.tip = None;
            out.redraw = true;
        }
        // The help screen closes on any key.
        if cx.chrome.modal.as_deref() == Some("help") {
            out.msgs.push(Msg::Command(Command::CloseModal));
            return out;
        }
        if self.filter_edit {
            // Typing after `/`: Enter keeps the filter, Esc drops it.
            let mut text = cx.chrome.filter.clone();
            match ev.code {
                KeyCode::Enter => self.filter_edit = false,
                KeyCode::Esc => {
                    self.filter_edit = false;
                    out.msgs.push(Msg::Command(Command::LogFilter(String::new())));
                }
                KeyCode::Backspace => {
                    text.pop();
                    out.msgs.push(Msg::Command(Command::LogFilter(text)));
                }
                KeyCode::Char(c) if !ev.ctrl && !ev.alt => {
                    text.push(c);
                    out.msgs.push(Msg::Command(Command::LogFilter(text)));
                }
                _ => {}
            }
            out.redraw = true;
            return out;
        }
        let ctx = cx.keys();
        match keys.lookup(ev, ctx) {
            Some(action) => self.act(action, cx, drawn, prompt, &mut out),
            None if ctx == Ctx::Prompt && !ev.ctrl && !ev.alt => match ev.code {
                KeyCode::Char(c) => {
                    prompt.push(c);
                    out.redraw = true;
                }
                KeyCode::Backspace => {
                    out.redraw = prompt.pop().is_some();
                }
                _ => {}
            },
            None => {}
        }
        // Only a pinned tooltip survives a key; one the pointer earned is gone until it rests again.
        if self.pointer.tip != self.pinned {
            self.pointer.tip = self.pinned;
            out.redraw = true;
        }
        out
    }

    fn act(&mut self, action: Action, cx: &Context, drawn: &Drawn, prompt: &mut String, out: &mut Out) {
        use Action::*;
        let focus = cx.chrome.focus;
        let cmd = |c: Command| Msg::Command(c);
        let step = |by: i64| {
            let last = drawn.cards.saturating_sub(1) as i64;
            let to = (cx.chrome.selected_card as i64 + by).clamp(0, last.max(0));
            ui(UiIntent::Select(Pane::Cards, Selection::Index(to)))
        };
        match action {
            FocusNext => out.msgs.push(ui(UiIntent::FocusNext)),
            FocusPrev => out.msgs.push(ui(UiIntent::FocusPrev)),
            Submit => {
                if !prompt.trim().is_empty() {
                    out.msgs.push(Msg::SubmitPrompt { text: std::mem::take(prompt) });
                    out.redraw = true;
                }
            }
            ClearPrompt => {
                out.redraw = !prompt.is_empty();
                prompt.clear();
            }
            OpenTranscript if focus == Pane::Log => {
                if let Some(run) = cx.log_run() {
                    out.msgs.push(cmd(Command::OpenTranscript(run)));
                }
            }
            OpenTranscript => out.msgs.push(ui(UiIntent::Open(focus))),
            ShowAdvice => out.msgs.push(ui(UiIntent::Open(Pane::Advisor))),
            MoveUp | MoveDown if cx.menu.is_some() => {
                // The next item that applies, wrapping round; disabled ones are skipped.
                if let Some(menu) = &cx.menu {
                    let n = menu.items.len();
                    let by = if action == MoveUp { n - 1 } else { 1 };
                    let next = (1..=n)
                        .map(|k| (menu.selected + k * by) % n)
                        .find(|i| menu.items[*i].1);
                    if let Some(i) = next {
                        out.msgs.push(ui(UiIntent::MenuSelect(i as i64)));
                    }
                }
            }
            MoveUp | MoveDown => {
                let up = action == MoveUp;
                match focus {
                    Pane::Cards => out.msgs.push(step(if up { -1 } else { 1 })),
                    pane => out.msgs.push(ui(UiIntent::Scroll {
                        pane,
                        lines: if up { 1 } else { -1 },
                        sideways: false,
                    })),
                }
            }
            CardPrev => out.msgs.push(step(-1)),
            CardNext => out.msgs.push(step(1)),
            StopRun | CancelRun => {
                if let Some(run) = cx.selected_run {
                    out.msgs.push(cmd(Command::Cancel { run }));
                }
            }
            RetryRun => {
                if let Some(run) = cx.selected_run {
                    out.msgs.push(cmd(Command::Retry { run }));
                }
            }
            ToggleBackground | BackgroundWait => {
                if let Some(run) = cx.selected_run.or(cx.lead_run) {
                    out.msgs.push(cmd(Command::Background { run }));
                }
            }
            OpenLeadTranscript => {
                if let Some(run) = cx.lead_run {
                    out.msgs.push(cmd(Command::OpenTranscript(run)));
                }
            }
            FocusRail => {
                out.msgs.push(ui(UiIntent::Focus(Pane::Advisor)));
                // On a narrow terminal `a` also opens the full rail over the strip.
                if drawn.areas.rail_open.is_none() && drawn.areas.rail.width < crate::layout::RAIL_FULL {
                    out.msgs.push(ui(UiIntent::ToggleRail));
                }
            }
            Allow | Deny | AllowAlways => {
                if let Some(id) = cx.permission {
                    let decision = match action {
                        Allow => Decision::Allow,
                        Deny => Decision::Deny,
                        _ => Decision::AllowSession,
                    };
                    out.msgs.push(cmd(Command::Permission { id, decision }));
                }
            }
            OpenPalette => out.msgs.push(cmd(Command::OpenPalette)),
            OpenHelp => out.msgs.push(cmd(Command::OpenHelp)),
            CloseModal if cx.menu.is_some() => out.msgs.push(ui(UiIntent::CloseMenu)),
            CloseModal => out.msgs.push(cmd(Command::CloseModal)),
            PickMenuItem => {
                if let Some(menu) = &cx.menu {
                    Self::pick(menu, menu.selected, out);
                }
            }
            OpenMenu => {
                // At the selected card or row, one cell in.
                let (id, run) = match focus {
                    Pane::Cards => (HitId::Card(cx.chrome.selected_card), cx.selected_run),
                    _ => {
                        let at = cx.chrome.selected_log.unwrap_or(cx.log_runs.len().saturating_sub(1));
                        (HitId::LogRow(at), cx.log_run())
                    }
                };
                if let (Some(r), Some(run)) = (drawn.hits.find(id), run) {
                    out.msgs.push(ui(UiIntent::OpenMenu { run, col: r.rect.x + 1, row: r.rect.y + 1 }));
                }
            }
            ShowTooltip => {
                // Pins the tooltip of the selection; pressing it again closes it.
                let id = match focus {
                    Pane::Cards => Some(HitId::Card(cx.chrome.selected_card)),
                    Pane::Log => Some(HitId::LogRow(cx.chrome.selected_log.unwrap_or(cx.log_runs.len().saturating_sub(1)))),
                    _ => None,
                };
                if let (Some(id), false) = (id, self.tip_was_up) {
                    self.pinned = Some(id);
                }
                out.redraw = true;
            }
            Filter if focus == Pane::Log => {
                self.filter_edit = true;
                out.redraw = true;
            }
            ToggleFollow if focus == Pane::Log => out.msgs.push(ui(UiIntent::ToggleFollow)),
            Quit => out.msgs.push(cmd(Command::Quit { force: false })),
            // Their screens and prompts land with the transcript, agents and accounts work.
            MessageRun | ResumeRun | JumpPermission | EditInput | Filter | ToggleFollow | TimeBack
            | TimeForward | OpenAgents | OpenAccounts => {}
        }
    }
}

/// Keeps the last of each run of move events and each run of drag events, so a burst of them
/// costs one pass (the loop handles one per frame).
pub fn coalesce(events: Vec<Event>) -> Vec<Event> {
    let kind_of = |e: &Event| match e {
        Event::Mouse(m) => match m.kind {
            MouseEventKind::Moved => Some(0),
            MouseEventKind::Drag(b) => Some(1 + b as u8),
            _ => None,
        },
        _ => None,
    };
    let mut out = Vec::with_capacity(events.len());
    let mut it = events.into_iter().peekable();
    while let Some(e) = it.next() {
        if kind_of(&e).is_some() && it.peek().map(kind_of) == Some(kind_of(&e)) {
            continue;
        }
        out.push(e);
    }
    out
}

/// How much of the mouse the terminal is asked to report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseMode {
    Off,
    /// Presses, releases, drags and the wheel.
    Clicks,
    /// Those and plain moves, for hover.
    Hover,
}

/// What the loop wants: nothing when the mouse is off or unsupported, move events only while
/// something on screen reacts to hover.
pub fn mouse_mode(enabled: bool, supported: bool, hoverable: bool) -> MouseMode {
    match (enabled && supported, hoverable) {
        (false, _) => MouseMode::Off,
        (true, false) => MouseMode::Clicks,
        (true, true) => MouseMode::Hover,
    }
}

/// The escape sequences that take the terminal from one mode to the other: 1000 presses, 1002
/// drags, 1003 every move, 1006 the SGR encoding that works past column 223.
pub fn mouse_switch(from: MouseMode, to: MouseMode) -> String {
    let mut s = String::new();
    if from == to {
        return s;
    }
    if to == MouseMode::Off {
        return "\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?1006l".into();
    }
    if from == MouseMode::Off {
        s.push_str("\x1b[?1000h\x1b[?1002h\x1b[?1006h");
    }
    s.push_str(if to == MouseMode::Hover { "\x1b[?1003h" } else { "\x1b[?1003l" });
    s
}

/// A linux console and a dumb terminal have no mouse reporting to ask for.
pub fn mouse_supported(term: Option<&str>) -> bool {
    !matches!(term, Some("dumb" | "linux" | ""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Event {
        Event::Mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE })
    }

    #[test]
    fn bursts_of_moves_and_drags_collapse_to_the_last() {
        let m = |c| mouse(MouseEventKind::Moved, c, 0);
        let d = |c| mouse(MouseEventKind::Drag(MouseButton::Left), c, 0);
        let down = mouse(MouseEventKind::Down(MouseButton::Left), 1, 1);
        let got = coalesce(vec![m(1), m(2), m(3), down.clone(), d(4), d(5), m(6), d(7)]);
        let cols: Vec<u16> = got
            .iter()
            .map(|e| match e {
                Event::Mouse(m) => m.column,
                _ => 0,
            })
            .collect();
        // The three moves become the third, the press stays, the two drags become the second,
        // and a drag after a move is a different kind of event.
        assert_eq!(cols, [3, 1, 5, 6, 7]);
        let key = Event::Key(crossterm::event::KeyEvent {
            code: crossterm::event::KeyCode::Char('a'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });
        assert_eq!(coalesce(vec![key.clone(), key]).len(), 2);
    }

    #[test]
    fn mouse_mode_follows_the_opt_outs_and_hover() {
        assert_eq!(mouse_mode(false, true, true), MouseMode::Off);
        assert_eq!(mouse_mode(true, false, true), MouseMode::Off);
        assert_eq!(mouse_mode(true, true, false), MouseMode::Clicks);
        assert_eq!(mouse_mode(true, true, true), MouseMode::Hover);
    }

    #[test]
    fn move_reporting_is_switched_on_and_off_on_its_own() {
        use MouseMode::*;
        assert_eq!(mouse_switch(Off, Off), "");
        assert!(mouse_switch(Off, Clicks).contains("?1000h"));
        assert!(!mouse_switch(Off, Clicks).contains("?1003h"));
        assert!(mouse_switch(Off, Hover).contains("?1003h"));
        assert_eq!(mouse_switch(Hover, Clicks), "\x1b[?1003l");
        assert_eq!(mouse_switch(Clicks, Hover), "\x1b[?1003h");
        let off = mouse_switch(Hover, Off);
        assert!(off.contains("?1000l") && off.contains("?1003l") && off.contains("?1006l"));
    }

    #[test]
    fn terminals_without_mouse_reporting_are_recognised() {
        assert!(!mouse_supported(Some("dumb")));
        assert!(!mouse_supported(Some("linux")));
        assert!(mouse_supported(Some("xterm-256color")));
        assert!(mouse_supported(None));
    }
}
