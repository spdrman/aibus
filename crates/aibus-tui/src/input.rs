//! The event layer (05 section 6.1): a terminal key or mouse event becomes `Msg`s that go through
//! `reduce` like any other message. The hit map of the last frame says what a pointer event is
//! on; hover and pressed state live here, not in the graph, because they change with every move.
//!
//! Every button and row a mouse can reach has a key that does the same thing, and both go
//! through the same function (`Input::screen` for the screens, `Input::act` for the dashboard
//! and the transcript), so the two cannot drift apart.
//!
//! The terminal itself is not touched here, so a test can feed synthetic events through this and
//! then draw the result into a `TestBackend`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use aibus_core::state::value::{as_i64, as_list, as_str, get};
use aibus_core::state::{run_key, PERMISSIONS_OPEN, RUNS_ORDER};
use aibus_core::{
    Command, Decision, KeyCode, KeyEvent, Msg, Outcome, Pane, PermissionId, RunId, Screen, Selection, TrOp, UiIntent,
};
use aibus_graph::{Graph, Value};
use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::hit::{HitId, HitKind, Segment};
use crate::keys::{Action, Ctx, Keymap};
use crate::layout::{log_for_drag, rail_for_drag};
use crate::prompt::{send, Prompt, Sent};
use crate::screens::{Entry, EntryKind};
use crate::transcript::Cells;
use crate::ui::{Chrome, Drawn, Pointer};
use crate::view::{CARDS, LOG_LINES, MENU_VIEW};
use crate::views::{ACCOUNTS_VIEW, AGENTS, PALETTE, PERMISSION, TRANSCRIPT};

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

/// What the open transcript allows.
#[derive(Clone, Debug)]
pub struct TrCx {
    pub run: RunId,
    pub live: bool,
    pub retryable: bool,
    pub partial: bool,
    pub follow: bool,
    pub stepped: bool,
    pub height: usize,
}

/// What the key map and the pointer need to know about the session, read from the graph.
#[derive(Clone, Debug)]
pub struct Context {
    pub chrome: Chrome,
    pub selected_run: Option<RunId>,
    pub lead_run: Option<RunId>,
    pub permission: Option<PermissionId>,
    pub permission_summary: String,
    /// The run of each card, in the order the cards are drawn.
    pub cards: Vec<Option<RunId>>,
    /// The run each log line belongs to, if any.
    pub log_runs: Vec<Option<RunId>>,
    pub menu: Option<Menu>,
    pub transcript: Option<TrCx>,
    /// The ids of the palette's rows and which one is lit.
    pub palette: Vec<String>,
    pub palette_text: String,
    pub screen_sel: usize,
    /// The agents screen's rows: name and the file it lives in, when it has one.
    pub agent_rows: Vec<(String, Option<PathBuf>)>,
    pub account_rows: Vec<String>,
    /// Names for the prompt's completion.
    pub agents: Vec<String>,
    /// Some run has finished: quitting asks how it went.
    pub any_runs: bool,
}

impl Context {
    pub fn read<G: Graph>(g: &G) -> Self {
        let chrome = Chrome::read(g);
        let run_of = |v: &Value, key: &str| get(v, key).and_then(as_str).and_then(|id| id.parse::<RunId>().ok());
        let card_views = g.read_key(CARDS).map_or(&[][..], as_list);
        let cards: Vec<Option<RunId>> = card_views.iter().map(|c| run_of(c, "run_id")).collect();
        let log_runs: Vec<Option<RunId>> =
            g.read_key(LOG_LINES).map_or(&[][..], as_list).iter().map(|l| run_of(l, "run")).collect();
        let menu = g.read_key(MENU_VIEW).filter(|m| !matches!(m, Value::Null)).and_then(|m| {
            Some(Menu {
                run: run_of(m, "run_id")?,
                selected: usize::try_from(get(m, "selected").and_then(as_i64)?).ok()?,
                items: as_list(get(m, "items")?)
                    .iter()
                    .map(|i| {
                        (
                            get(i, "id").and_then(as_str).unwrap_or("").to_owned(),
                            matches!(get(i, "enabled"), Some(Value::Bool(true))),
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
            .find(|id| g.read_key(&run_key(id, "meta")).and_then(|m| get(m, "role")).and_then(as_str) == Some("lead"))
            .and_then(|id| id.parse().ok());
        let permission = g
            .read_key(PERMISSIONS_OPEN)
            .map_or(&[][..], as_list)
            .first()
            .and_then(|p| get(p, "id"))
            .and_then(as_str)
            .and_then(|id| id.parse().ok());
        let view = |key: &str| g.read_key(key).filter(|v| !matches!(v, Value::Null));
        let transcript = view(TRANSCRIPT).and_then(|t| {
            let flag = |k: &str| matches!(get(t, k), Some(Value::Bool(true)));
            Some(TrCx {
                run: run_of(t, "run")?,
                live: flag("live"),
                retryable: flag("retryable"),
                partial: flag("partial"),
                follow: flag("follow"),
                stepped: !matches!(get(t, "at"), None | Some(Value::Null)),
                height: get(t, "height").and_then(as_i64).unwrap_or(10).max(1) as usize,
            })
        });
        let palette_view = view(PALETTE);
        let palette = palette_view
            .and_then(|p| get(p, "items"))
            .map_or(&[][..], as_list)
            .iter()
            .filter_map(|i| get(i, "id").and_then(as_str).map(str::to_owned))
            .collect();
        let palette_text = palette_view.and_then(|p| get(p, "text")).and_then(as_str).unwrap_or("").to_owned();
        let screen_sel = [PALETTE, AGENTS, ACCOUNTS_VIEW]
            .iter()
            .find_map(|k| view(k).and_then(|v| get(v, "sel")).and_then(as_i64))
            .unwrap_or(0)
            .max(0) as usize;
        let agent_rows = view(AGENTS)
            .and_then(|a| get(a, "rows"))
            .map_or(&[][..], as_list)
            .iter()
            .map(|r| {
                (
                    get(r, "name").and_then(as_str).unwrap_or("").to_owned(),
                    get(r, "path").and_then(as_str).map(PathBuf::from),
                )
            })
            .collect();
        let account_rows = view(ACCOUNTS_VIEW)
            .and_then(|a| get(a, "rows"))
            .map_or(&[][..], as_list)
            .iter()
            .map(|r| get(r, "name").and_then(as_str).unwrap_or("").to_owned())
            .collect();
        let agents = g
            .read_key(aibus_core::state::AGENTS_DEFS)
            .map_or(&[][..], as_list)
            .iter()
            .filter_map(|d| get(d, "name").and_then(as_str).map(str::to_owned))
            .collect();
        let any_runs = !g.read_key(RUNS_ORDER).map_or(&[][..], as_list).is_empty();
        let permission_summary = view(PERMISSION).and_then(|p| get(p, "summary")).and_then(as_str).unwrap_or("").to_owned();
        Self {
            chrome,
            selected_run,
            lead_run,
            permission,
            permission_summary,
            cards,
            log_runs,
            menu,
            transcript,
            palette,
            palette_text,
            screen_sel,
            agent_rows,
            account_rows,
            agents,
            any_runs,
        }
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
            Some("palette") => Ctx::Palette,
            Some("agents") => Ctx::Agents,
            Some("accounts") => Ctx::Accounts,
            Some("outcome") => Ctx::Outcome,
            Some("confirm:clear") => Ctx::Confirm,
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

/// Something the terminal loop does outside the graph because a key or a click asked for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Job {
    /// Open this file in `$EDITOR`.
    EditFile(PathBuf),
    /// Write a template for a new agent and open it.
    NewAgent(String),
    /// Run the agent validator over the definition and report.
    Validate { name: String, path: Option<PathBuf> },
    Account(AccountOp),
}

#[derive(Clone, PartialEq, Eq)]
pub enum AccountOp {
    Test(String),
    /// The key was typed into a masked field and goes straight to the keychain.
    Add { name: String, provider: String, key: String },
    Disable(String),
}

// A key must not reach a log through Debug.
impl std::fmt::Debug for AccountOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AccountOp::Add { name, provider, .. } => write!(f, "Add {name} {provider} <hidden>"),
            AccountOp::Test(n) => write!(f, "Test {n}"),
            AccountOp::Disable(n) => write!(f, "Disable {n}"),
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
    /// A menu pick or `Ctrl+C` on a selection asked for this to be copied.
    pub copy: Option<Clip>,
    pub jobs: Vec<Job>,
}

#[derive(Clone, Copy, Debug)]
enum Drag {
    Rail,
    Log,
    /// The scrollbar thumb: where the press was and where the thumb's top was then.
    Thumb { row: u16, top: u16 },
    /// The transcript's scrollbar thumb.
    TrThumb { row: u16, top: u16 },
    /// Selecting transcript text.
    Text,
    /// Selecting prompt text.
    Prompt,
}

/// The screens' own actions: a key and a button with the same meaning both come here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scr {
    Close,
    Run,
    AgentNew,
    AgentEdit,
    AgentValidate,
    AgentDelegate,
    AcctTest,
    AcctAdd,
    AcctDisable,
    Outcome(Outcome),
    QuitNow,
    ClearYes,
    SendEntry,
    CancelEntry,
}

fn scr_of(id: &str) -> Option<Scr> {
    Some(match id {
        "cancel" | "close" => Scr::Close,
        "run" => Scr::Run,
        "new" => Scr::AgentNew,
        "edit" => Scr::AgentEdit,
        "validate" => Scr::AgentValidate,
        "delegate" => Scr::AgentDelegate,
        "test" => Scr::AcctTest,
        "add" => Scr::AcctAdd,
        "disable" => Scr::AcctDisable,
        "outcome:1" => Scr::Outcome(Outcome::Succeeded),
        "outcome:2" => Scr::Outcome(Outcome::PartialPlus),
        "outcome:3" => Scr::Outcome(Outcome::PartialMinus),
        "outcome:4" => Scr::Outcome(Outcome::Failed),
        "quit_now" => Scr::QuitNow,
        "clear_yes" => Scr::ClearYes,
        "send_entry" | "send_edit" => Scr::SendEntry,
        "cancel_entry" => Scr::CancelEntry,
        _ => return None,
    })
}

#[derive(Debug, Default)]
pub struct Input {
    pub pointer: Pointer,
    /// The log's filter is being typed: keys go to it until Enter or Esc.
    pub filter_edit: bool,
    /// A text field open over a modal.
    pub entry: Option<Entry>,
    /// What the last action on the agents or accounts screen said.
    pub note: String,
    /// Transcript text under the mouse: anchor and head as (row, col) in the body.
    sel: Option<((u16, u16), (u16, u16))>,
    pub copied: bool,
    last_click: Option<(HitId, Instant, u8)>,
    drag: Option<Drag>,
    /// The prompt's selection started here (a char index).
    prompt_anchor: Option<usize>,
    /// What the pointer rests on and since when, for the tooltip.
    resting: Option<(HitId, Instant)>,
    /// The tooltip `Space` or a click on the ledger asked for.
    pinned: Option<HitId>,
    /// A tooltip was up when the last key came in, so `Space` closes it instead of reopening.
    tip_was_up: bool,
    /// A press landed on a tool row and has not moved: the release toggles the call.
    tool_press: Option<usize>,
}

/// Only these have anything to say in a tooltip.
fn tippable(id: HitId) -> bool {
    matches!(id, HitId::Card(_) | HitId::LogRow(_) | HitId::Status(Segment::Ledger | Segment::Alert))
}

fn ui(i: UiIntent) -> Msg {
    Msg::Ui(i)
}

fn tr(op: TrOp) -> Msg {
    Msg::Ui(UiIntent::Transcript(op))
}

/// The pane a wheel turn belongs to: the one under the pointer.
fn pane_under(id: HitId) -> Option<Pane> {
    match id {
        HitId::Pane(p) | HitId::Track(p) | HitId::Thumb(p) => Some(p),
        HitId::Card(_) => Some(Pane::Cards),
        HitId::LogRow(_) | HitId::LogHandle => Some(Pane::Log),
        HitId::Checkpoint(_) | HitId::RailStrip | HitId::RailHandle => Some(Pane::Advisor),
        HitId::PromptRow(_) => Some(Pane::Input),
        _ => None,
    }
}

/// The word at char `col` of `line`, as an inclusive range of columns. Letters, digits and the
/// characters of a path make a word; anything else is a word of one.
pub fn word_at(line: &str, col: usize) -> (usize, usize) {
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return (0, 0);
    }
    let col = col.min(chars.len() - 1);
    let part = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '/' | '.' | '~');
    if !part(chars[col]) {
        return (col, col);
    }
    let mut a = col;
    while a > 0 && part(chars[a - 1]) {
        a -= 1;
    }
    let mut b = col;
    while b + 1 < chars.len() && part(chars[b + 1]) {
        b += 1;
    }
    (a, b)
}

/// The text of the cells `sel` covers in `lines`.
pub fn selected_text(lines: &[String], sel: Cells) -> String {
    let ((r0, c0), (r1, c1)) = sel;
    let mut out = Vec::new();
    for row in r0..=r1 {
        let Some(line) = lines.get(usize::from(row)) else { break };
        let chars: Vec<char> = line.chars().collect();
        let from = if row == r0 { usize::from(c0) } else { 0 };
        let to = if row == r1 { usize::from(c1) + 1 } else { chars.len() };
        out.push(chars.iter().skip(from).take(to.saturating_sub(from)).collect::<String>().trim_end().to_owned());
    }
    out.join("\n")
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

    /// The cells selected in the transcript, in reading order.
    pub fn selection(&self) -> Option<Cells> {
        let (a, b) = self.sel?;
        (a != b).then(|| if a <= b { (a, b) } else { (b, a) })
    }

    /// Whether `Ctrl+C` copies instead of cancelling.
    pub fn has_selection(&self, prompt: &Prompt) -> bool {
        self.selection().is_some() || prompt.selection().is_some()
    }

    fn copy_selection(&mut self, drawn: &Drawn, prompt: &Prompt, out: &mut Out) -> bool {
        let text = match (self.selection(), &drawn.transcript) {
            (Some(sel), Some(g)) => Some(selected_text(&g.lines, sel)),
            _ => prompt.selected_text(),
        };
        let Some(text) = text.filter(|t| !t.is_empty()) else { return false };
        out.copy = Some(Clip::Text(text));
        self.copied = true;
        out.redraw = true;
        true
    }

    pub fn mouse(&mut self, ev: MouseEvent, now: Instant, drawn: &Drawn, cx: &Context, prompt: &mut Prompt) -> Out {
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
                self.press(hit, (col, row), now, drawn, cx, prompt, &mut out);
            }
            MouseEventKind::Down(MouseButton::Right) => {
                set_hover(self, &mut out);
                self.resting = hover.map(|id| (id, now));
                self.pinned = None;
                self.context(hit, (col, row), cx, &mut out);
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                set_hover(self, &mut out);
                self.dragged(col, row, drawn, cx, prompt, &mut out);
            }
            MouseEventKind::Up(MouseButton::Left) => {
                set_hover(self, &mut out);
                if self.pointer.pressed.take().is_some() {
                    out.redraw = true;
                }
                if matches!(self.drag.take(), Some(Drag::Rail | Drag::Log)) {
                    out.layout_done = true;
                }
                // A click on a tool row (no drag) opens or closes the call.
                if let Some(o) = self.tool_press.take() {
                    if self.selection().is_none() {
                        out.msgs.push(tr(TrOp::Tool(Selection::Index(o as i64))));
                        out.msgs.push(tr(TrOp::Toggle));
                    }
                }
                self.prompt_anchor = None;
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let sideways = ev.modifiers.contains(KeyModifiers::SHIFT);
                self.wheel(hit, ev.kind == MouseEventKind::ScrollUp, sideways, cx, prompt, &mut out);
            }
            MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => {
                self.wheel(hit, ev.kind == MouseEventKind::ScrollLeft, true, cx, prompt, &mut out);
            }
            _ => {}
        }
        let _ = modal;
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
                run: None,
            })),
            "resume" => out.msgs.push(Msg::Command(Command::Resume { run, message: "continue".into(), parent: None })),
            "copy_id" => out.copy = Some(Clip::Text(run.to_string())),
            "copy_result" => out.copy = Some(Clip::Result(run)),
            _ => {}
        }
        out.msgs.push(ui(UiIntent::CloseMenu));
    }

    fn wheel(&self, hit: Option<(HitId, HitKind)>, up: bool, sideways: bool, cx: &Context, prompt: &mut Prompt, out: &mut Out) {
        let notch = if up { WHEEL_LINES } else { -WHEEL_LINES };
        match cx.keys() {
            Ctx::Transcript => return out.msgs.push(tr(TrOp::Scroll(notch))),
            Ctx::Palette | Ctx::Agents | Ctx::Accounts => {
                return out.msgs.push(ui(UiIntent::Pick(if up { Selection::Prev } else { Selection::Next })));
            }
            _ if cx.chrome.modal.is_some() => return,
            _ => {}
        }
        let Some(pane) = hit.and_then(|h| pane_under(h.0)) else { return };
        if pane == Pane::Input {
            // A long prompt scrolls under the wheel: the cursor moves a row, the view follows it.
            if up {
                prompt.up();
            } else {
                prompt.down();
            }
            out.redraw = true;
            return;
        }
        out.msgs.push(ui(UiIntent::Scroll { pane, lines: notch, sideways }));
    }

    #[allow(clippy::too_many_arguments)]
    fn press(
        &mut self,
        hit: Option<(HitId, HitKind)>,
        (col, row): (u16, u16),
        now: Instant,
        drawn: &Drawn,
        cx: &Context,
        prompt: &mut Prompt,
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
            if !matches!(cx.chrome.modal.as_deref(), Some("permission" | "outcome" | "confirm:clear")) {
                self.entry = None;
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
        let (double, count) = match self.last_click {
            Some((last, at, n)) if last == id && now.saturating_duration_since(at) <= DOUBLE_CLICK => (true, (n + 1).min(3)),
            _ => (false, 1),
        };
        self.last_click = if count >= 3 { None } else { Some((id, now, count)) };
        // Anything that is not text drops the selection.
        if !matches!(id, HitId::TrRow(_) | HitId::CopyHint | HitId::PromptRow(_)) && self.sel.take().is_some() {
            out.redraw = true;
        }
        match id {
            HitId::Pane(p) => out.msgs.push(ui(UiIntent::Focus(p))),
            HitId::PromptRow(i) => {
                out.msgs.push(ui(UiIntent::Focus(Pane::Input)));
                if let Some((rect, start)) = drawn.prompt_rows.get(i) {
                    let line = prompt.lines().get(prompt.pos(*start).0).map_or(0, |l| l.chars().count());
                    let x = usize::from(col.saturating_sub(rect.x + drawn.prompt_indent));
                    let at = start + x.min(line);
                    if double {
                        let (a, b) = word_at(prompt.lines().get(prompt.pos(*start).0).copied().unwrap_or(""), x);
                        prompt.go(start + a, false);
                        prompt.go(start + b + 1, true);
                    } else {
                        prompt.go(at, false);
                        self.prompt_anchor = Some(at);
                        self.drag = Some(Drag::Prompt);
                    }
                }
            }
            HitId::PopupItem(i) => {
                if let Some(p) = &mut prompt.popup {
                    p.sel = i;
                }
                prompt.accept_popup();
            }
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
                Segment::Alert => out.msgs.push(ui(UiIntent::ShowPermission)),
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
            HitId::TrTrack => {
                if let (Some(t), Some(g)) = (cx.transcript.as_ref(), drawn.transcript.as_ref()) {
                    let thumb = g.track.map(|(_, t)| t);
                    if let Some(th) = thumb {
                        let page = i32::try_from(t.height.saturating_sub(1)).unwrap_or(10).max(1);
                        out.msgs.push(tr(TrOp::Scroll(if row < th.y { page } else { -page })));
                    }
                }
            }
            HitId::TrThumb => {
                let top = drawn.hits.find(id).map_or(row, |r| r.rect.y);
                self.drag = Some(Drag::TrThumb { row, top });
            }
            HitId::JumpLatest => out.msgs.push(tr(TrOp::Bottom)),
            HitId::CopyHint => {
                self.copy_selection(drawn, prompt, out);
            }
            HitId::TrRow(i) => {
                let Some(g) = &drawn.transcript else { return };
                let c = col.saturating_sub(g.body.x);
                let line = g.lines.get(i).map_or("", String::as_str);
                self.copied = false;
                self.tool_press = None;
                match count {
                    3 => self.sel = Some(((i as u16, 0), (i as u16, u16::try_from(line.chars().count().saturating_sub(1)).unwrap_or(0)))),
                    2 => {
                        let (a, b) = word_at(line, usize::from(c));
                        self.sel = Some(((i as u16, a as u16), (i as u16, b as u16)));
                    }
                    _ => {
                        self.sel = Some(((i as u16, c), (i as u16, c)));
                        self.drag = Some(Drag::Text);
                        if let Some(o) = g.tools.get(i).copied().flatten() {
                            self.tool_press = Some(o);
                        }
                    }
                }
            }
            HitId::ListRow(i) => {
                out.msgs.push(ui(UiIntent::Pick(Selection::Index(i as i64))));
                if double {
                    // The row's default action: run it, edit it, test it.
                    let default = match cx.keys() {
                        Ctx::Palette => Some(Scr::Run),
                        Ctx::Agents => Some(Scr::AgentEdit),
                        Ctx::Accounts => Some(Scr::AcctTest),
                        _ => None,
                    };
                    if let Some(a) = default {
                        let cx2 = Context { screen_sel: i, ..cx.clone() };
                        self.screen(a, &cx2, prompt, out);
                    }
                }
            }
            HitId::Button(name) => self.button(name, cx, drawn, prompt, out),
            HitId::Backdrop | HitId::Modal | HitId::MenuBox | HitId::MenuItem(_) => {}
        }
    }

    /// A click on a button does what its key does.
    fn button(&mut self, name: &str, cx: &Context, drawn: &Drawn, prompt: &mut Prompt, out: &mut Out) {
        let action = match (cx.keys(), name) {
            (Ctx::Permission, "allow") => Some(Action::Allow),
            (Ctx::Permission, "always") => Some(Action::AllowAlways),
            (Ctx::Permission, "deny") => Some(Action::Deny),
            (Ctx::Permission, "edit") => Some(Action::EditInput),
            (Ctx::Permission | Ctx::Transcript, "cancel" | "close") => Some(Action::CloseModal),
            (Ctx::Transcript, "toggle") => Some(Action::OpenTranscript),
            (Ctx::Transcript, "older") => Some(Action::TimeBack),
            (Ctx::Transcript, "newer") => Some(Action::TimeForward),
            (Ctx::Transcript, "retry") => Some(Action::RetryRun),
            (Ctx::Transcript, "stop") => Some(Action::StopRun),
            (Ctx::Transcript, "compact") => Some(Action::CompactRun),
            (Ctx::Transcript, "message") => Some(Action::MessageRun),
            (Ctx::Transcript, "resume") => Some(Action::ResumeRun),
            _ => None,
        };
        if let Some(a) = action {
            self.act(a, cx, drawn, prompt, out);
        } else if let Some(a) = scr_of(name) {
            self.screen(a, cx, prompt, out);
        }
    }

    fn dragged(&mut self, col: u16, row: u16, drawn: &Drawn, cx: &Context, prompt: &mut Prompt, out: &mut Out) {
        let Some(drag) = self.drag else { return };
        let cols = drawn.areas.status.width;
        match drag {
            Drag::Rail => out.msgs.push(ui(UiIntent::SetRailWidth(rail_for_drag(&drawn.areas, col, cols)))),
            Drag::Log => out.msgs.push(ui(UiIntent::SetLogRows(log_for_drag(&drawn.areas, row)))),
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
                    out.msgs.push(ui(UiIntent::Scroll { pane: Pane::Log, lines: i32::try_from(lines).unwrap_or(0), sideways: false }));
                }
            }
            Drag::TrThumb { row: down, top } => {
                let (Some(g), Some(t)) = (drawn.transcript.as_ref(), cx.transcript.as_ref()) else { return };
                let Some((track, thumb)) = g.track else { return };
                let travel = usize::from(track.height.saturating_sub(thumb.height));
                let wanted = i64::from(top) - i64::from(track.y) + i64::from(row) - i64::from(down);
                if travel == 0 {
                    return;
                }
                let from_top = usize::try_from(wanted.clamp(0, travel as i64)).unwrap_or(0);
                // Rows above the thumb's travel are rows above the view: scroll by the difference
                // between where the thumb is now and where the drag wants it.
                let total = g.total;
                let max_top = total.saturating_sub(t.height);
                let target = from_top * max_top / travel;
                let now = usize::from(thumb.y - track.y) * max_top / travel;
                let lines = now as i64 - target as i64;
                if lines != 0 {
                    out.msgs.push(tr(TrOp::Scroll(i32::try_from(lines).unwrap_or(0))));
                }
            }
            Drag::Text => {
                if let (Some(g), Some((anchor, _))) = (&drawn.transcript, self.sel) {
                    let r = row.clamp(g.body.y, g.body.bottom().saturating_sub(1)) - g.body.y;
                    let c = col.clamp(g.body.x, g.body.right().saturating_sub(1)) - g.body.x;
                    self.sel = Some((anchor, (r, c)));
                    self.tool_press = None;
                    out.redraw = true;
                }
            }
            Drag::Prompt => {
                if let Some(anchor) = self.prompt_anchor {
                    let hit = drawn.hits.at(col, row.clamp(drawn.areas.prompt.y, drawn.areas.prompt.bottom().saturating_sub(1)));
                    if let Some(HitId::PromptRow(i)) = hit.map(|h| h.id) {
                        if let Some((rect, start)) = drawn.prompt_rows.get(i) {
                            let line = prompt.lines().get(prompt.pos(*start).0).map_or(0, |l| l.chars().count());
                            let x = usize::from(col.saturating_sub(rect.x + drawn.prompt_indent));
                            prompt.anchor = Some(anchor);
                            prompt.cursor = start + x.min(line);
                            out.redraw = true;
                        }
                    }
                }
            }
        }
    }

    /// Bracketed paste: into the field that has the keyboard. A dropped file path is inserted as
    /// a path and is never submitted.
    pub fn paste(&mut self, text: &str, cx: &Context, prompt: &mut Prompt) -> Out {
        let mut out = Out { redraw: true, ..Out::default() };
        if let Some(e) = &mut self.entry {
            e.text.push_str(text.lines().next().unwrap_or(""));
        } else if cx.chrome.modal.is_none() {
            prompt.paste(text);
            out.msgs.push(ui(UiIntent::Focus(Pane::Input)));
        }
        out
    }

    /// A key press. `prompt` is the prompt line, edited in place.
    pub fn key(&mut self, ev: KeyEvent, cx: &Context, keys: &Keymap, drawn: &Drawn, prompt: &mut Prompt) -> Out {
        let mut out = Out::default();
        // A key ends any tooltip, whether the pointer or `Space` brought it up.
        self.tip_was_up = self.pinned.take().is_some() || self.pointer.tip.is_some();
        self.resting = None;
        if self.tip_was_up {
            self.pointer.tip = None;
            out.redraw = true;
        }
        // Ctrl+C with something selected copies it; without, it keeps its meaning.
        if ev.ctrl && ev.code == KeyCode::Char('c') && self.copy_selection(drawn, prompt, &mut out) {
            return out;
        }
        if ev.code != KeyCode::Char('c') || !ev.ctrl {
            self.copied = false;
            if self.sel.take().is_some() {
                out.redraw = true;
            }
        }
        // The help screen closes on any key.
        if cx.chrome.modal.as_deref() == Some("help") {
            out.msgs.push(Msg::Command(Command::CloseModal));
            return out;
        }
        if self.entry.is_some() {
            self.entry_key(ev, cx, prompt, &mut out);
            out.redraw = true;
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
        if let (Ctx::Transcript, KeyCode::PageUp | KeyCode::PageDown) = (ctx, ev.code) {
            let page = cx.transcript.as_ref().map_or(10, |t| i32::try_from(t.height.saturating_sub(1)).unwrap_or(10).max(1));
            out.msgs.push(tr(TrOp::Scroll(if ev.code == KeyCode::PageUp { page } else { -page })));
            return out;
        }
        match ctx {
            Ctx::Prompt => self.prompt_key(ev, cx, keys, drawn, prompt, &mut out),
            Ctx::Palette | Ctx::Agents | Ctx::Accounts | Ctx::Outcome | Ctx::Confirm => {
                self.screen_key(ev, ctx, cx, prompt, &mut out);
            }
            _ => {
                if let Some(action) = keys.lookup(ev, ctx) {
                    self.act(action, cx, drawn, prompt, &mut out);
                }
            }
        }
        // Only a pinned tooltip survives a key; one the pointer earned is gone until it rests again.
        if self.pointer.tip != self.pinned {
            self.pointer.tip = self.pinned;
            out.redraw = true;
        }
        out
    }

    /// The prompt has the keyboard: editing, history, completion, Enter. Keys with a meaning of
    /// their own (the global ones) still go through the key map when they are not text.
    fn prompt_key(&mut self, ev: KeyEvent, cx: &Context, keys: &Keymap, drawn: &Drawn, prompt: &mut Prompt, out: &mut Out) {
        let shift = ev.shift;
        prompt.notice = None;
        match (ev.code, ev.ctrl, ev.alt) {
            (KeyCode::Enter, _, true) => prompt.newline(),
            (KeyCode::Enter, ..) if prompt.accept_popup() => {}
            (KeyCode::Enter, ..) => {
                if let Some(line) = prompt.take() {
                    match send(&line, &cx.agents, &[]) {
                        Sent::Msgs(m) => out.msgs.extend(m),
                        Sent::Bad(why) => prompt.notice = Some(why),
                    }
                }
            }
            (KeyCode::Tab, ..) => {
                let (_, found) = prompt.candidates(&cx.agents, &[]);
                if prompt.popup.is_some() || !found.is_empty() {
                    if shift {
                        prompt.shift_tab();
                    } else {
                        prompt.tab(&cx.agents, &[]);
                    }
                } else if let Some(action) = keys.lookup(ev, Ctx::Prompt) {
                    self.act(action, cx, drawn, prompt, out);
                }
            }
            (KeyCode::Esc, ..) => {
                prompt.escape();
            }
            (KeyCode::Backspace, ..) => prompt.backspace(),
            (KeyCode::Left, ..) => prompt.left(shift),
            (KeyCode::Right, ..) => prompt.right(shift),
            (KeyCode::Up, ..) => prompt.up(),
            (KeyCode::Down, ..) => prompt.down(),
            (KeyCode::Char('a'), true, _) => prompt.home(shift),
            (KeyCode::Char('e'), true, _) => prompt.end(shift),
            (KeyCode::Char('u'), true, _) => {
                prompt.escape();
            }
            (KeyCode::Char(c), false, false) => prompt.insert(c),
            _ => {
                if let Some(action) = keys.lookup(ev, Ctx::Prompt) {
                    self.act(action, cx, drawn, prompt, out);
                }
            }
        }
        out.redraw = true;
    }

    /// Keys of the screens that have their own letters (named on their buttons).
    fn screen_key(&mut self, ev: KeyEvent, ctx: Ctx, cx: &Context, prompt: &mut Prompt, out: &mut Out) {
        let c = match ev.code {
            KeyCode::Char(c) if !ev.ctrl && !ev.alt => Some(c),
            _ => None,
        };
        if ev.code == KeyCode::Esc {
            return self.screen(Scr::Close, cx, prompt, out);
        }
        match ctx {
            Ctx::Palette => match ev.code {
                KeyCode::Enter => self.screen(Scr::Run, cx, prompt, out),
                KeyCode::Up => out.msgs.push(ui(UiIntent::Pick(Selection::Prev))),
                KeyCode::Down => out.msgs.push(ui(UiIntent::Pick(Selection::Next))),
                KeyCode::Backspace => {
                    let mut t = cx.palette_text.clone();
                    t.pop();
                    out.msgs.push(Msg::Command(Command::PaletteText(t)));
                }
                _ => {
                    if let Some(c) = c {
                        out.msgs.push(Msg::Command(Command::PaletteText(format!("{}{c}", cx.palette_text))));
                    }
                }
            },
            Ctx::Agents | Ctx::Accounts => {
                let a = match (ctx, ev.code, c) {
                    (_, KeyCode::Up, _) | (_, _, Some('k')) => return out.msgs.push(ui(UiIntent::Pick(Selection::Prev))),
                    (_, KeyCode::Down, _) | (_, _, Some('j')) => return out.msgs.push(ui(UiIntent::Pick(Selection::Next))),
                    (Ctx::Agents, KeyCode::Enter, _) | (Ctx::Agents, _, Some('e')) => Scr::AgentEdit,
                    (Ctx::Agents, _, Some('n')) => Scr::AgentNew,
                    (Ctx::Agents, _, Some('v')) => Scr::AgentValidate,
                    (Ctx::Agents, _, Some('d')) => Scr::AgentDelegate,
                    (Ctx::Accounts, KeyCode::Enter, _) | (Ctx::Accounts, _, Some('t')) => Scr::AcctTest,
                    (Ctx::Accounts, _, Some('a')) => Scr::AcctAdd,
                    (Ctx::Accounts, _, Some('d')) => Scr::AcctDisable,
                    _ => return,
                };
                self.screen(a, cx, prompt, out);
            }
            Ctx::Outcome => {
                let a = match c {
                    Some('1') => Scr::Outcome(Outcome::Succeeded),
                    Some('2') => Scr::Outcome(Outcome::PartialPlus),
                    Some('3') => Scr::Outcome(Outcome::PartialMinus),
                    Some('4') => Scr::Outcome(Outcome::Failed),
                    Some('!') => Scr::QuitNow,
                    _ => return,
                };
                self.screen(a, cx, prompt, out);
            }
            Ctx::Confirm => match c {
                Some('y') => self.screen(Scr::ClearYes, cx, prompt, out),
                Some('n') => self.screen(Scr::Close, cx, prompt, out),
                _ => {}
            },
            _ => {}
        }
    }

    /// What a screen's action does. A key and a button with the same meaning both end up here.
    fn screen(&mut self, a: Scr, cx: &Context, prompt: &mut Prompt, out: &mut Out) {
        let cmd = |c: Command| Msg::Command(c);
        let sel = cx.screen_sel;
        match a {
            Scr::Close => {
                self.entry = None;
                self.note.clear();
                out.msgs.push(cmd(Command::CloseModal));
            }
            Scr::Run => {
                let Some(id) = cx.palette.get(sel) else { return };
                let lead = cx.lead_run;
                let close = cmd(Command::CloseModal);
                match id.split_once(':') {
                    Some(("cmd", "agents")) => out.msgs.push(cmd(Command::OpenScreen(Screen::Agents))),
                    Some(("cmd", "accounts")) => out.msgs.push(cmd(Command::OpenScreen(Screen::Accounts))),
                    Some(("cmd", "help")) => out.msgs.push(cmd(Command::OpenHelp)),
                    Some(("cmd", "lead")) => match lead {
                        Some(run) => out.msgs.push(cmd(Command::OpenTranscript(run))),
                        None => out.msgs.push(close),
                    },
                    Some(("cmd", "permission")) => out.msgs.push(ui(UiIntent::ShowPermission)),
                    Some(("cmd", "handoff")) => out.msgs.extend([close, cmd(Command::CreateHandoff { outcome: None })]),
                    Some(("cmd", "clear")) => out.msgs.push(cmd(Command::OpenScreen(Screen::ConfirmClear))),
                    Some(("cmd", "rail")) => out.msgs.extend([close, ui(UiIntent::Focus(Pane::Advisor)), ui(UiIntent::ToggleRail)]),
                    Some(("cmd", "compact")) => {
                        out.msgs.push(close);
                        if let Some(run) = lead {
                            out.msgs.push(cmd(Command::Compact { run }));
                        }
                    }
                    Some(("cmd", "quit")) => out.msgs.extend([close, cmd(Command::Quit { force: false })]),
                    Some(("agent", name)) => {
                        out.msgs.extend([close, ui(UiIntent::Focus(Pane::Input))]);
                        prompt.set(&format!("@agent-{name} "));
                    }
                    Some(("run", run)) => {
                        if let Ok(run) = run.parse() {
                            out.msgs.push(cmd(Command::OpenTranscript(run)));
                        }
                    }
                    _ => {}
                }
            }
            Scr::AgentNew => {
                self.entry = Some(Entry { kind: EntryKind::AgentName, label: "new agent name".into(), text: String::new(), secret: false });
            }
            Scr::AgentEdit => match cx.agent_rows.get(sel) {
                Some((_, Some(path))) => out.jobs.push(Job::EditFile(path.clone())),
                Some((name, None)) => self.note = format!("{name} is built in: copy it with n to change it"),
                None => {}
            },
            Scr::AgentValidate => {
                if let Some((name, path)) = cx.agent_rows.get(sel) {
                    out.jobs.push(Job::Validate { name: name.clone(), path: path.clone() });
                }
            }
            Scr::AgentDelegate => {
                if let Some((name, _)) = cx.agent_rows.get(sel) {
                    out.msgs.extend([cmd(Command::CloseModal), ui(UiIntent::Focus(Pane::Input))]);
                    prompt.set(&format!("/delegate {name} "));
                }
            }
            Scr::AcctTest => {
                if let Some(name) = cx.account_rows.get(sel) {
                    self.note = format!("testing {name}...");
                    out.jobs.push(Job::Account(AccountOp::Test(name.clone())));
                }
            }
            Scr::AcctAdd => {
                self.entry = Some(Entry { kind: EntryKind::AccountName, label: "account name".into(), text: String::new(), secret: false });
            }
            Scr::AcctDisable => {
                if let Some(name) = cx.account_rows.get(sel) {
                    out.jobs.push(Job::Account(AccountOp::Disable(name.clone())));
                }
            }
            Scr::Outcome(o) => {
                out.msgs.extend([cmd(Command::CreateHandoff { outcome: Some(o) }), cmd(Command::Quit { force: false })]);
            }
            Scr::QuitNow => out.msgs.push(cmd(Command::Quit { force: true })),
            Scr::ClearYes => out.msgs.extend([cmd(Command::Clear), cmd(Command::CloseModal)]),
            Scr::SendEntry => self.finish_entry(cx, prompt, out),
            Scr::CancelEntry => self.entry = None,
        }
        out.redraw = true;
    }

    /// Keys typed into an open text field.
    fn entry_key(&mut self, ev: KeyEvent, cx: &Context, prompt: &mut Prompt, out: &mut Out) {
        let Some(e) = &mut self.entry else { return };
        match ev.code {
            KeyCode::Esc => self.entry = None,
            KeyCode::Enter => self.finish_entry(cx, prompt, out),
            KeyCode::Backspace => {
                e.text.pop();
            }
            KeyCode::Char(c) if !ev.ctrl && !ev.alt => e.text.push(c),
            _ => {}
        }
    }

    /// Enter in a text field: the next step of adding an account, or the result.
    fn finish_entry(&mut self, cx: &Context, _prompt: &mut Prompt, out: &mut Out) {
        let Some(e) = self.entry.take() else { return };
        let text = e.text.trim().to_owned();
        let cmd = |c: Command| Msg::Command(c);
        match e.kind {
            EntryKind::EditInput => {
                // The run has not been told: the edited call goes back as a message and this one
                // is denied, so the run proposes it again (see the PR for why).
                if let Some(id) = cx.permission {
                    out.msgs.push(cmd(Command::Permission { id, decision: Decision::Deny }));
                    if let Some(run) = self.edit_run(cx) {
                        out.msgs.push(cmd(Command::SendMessage { run, text: format!("Run this instead: {text}") }));
                    }
                }
            }
            EntryKind::Message(run) if !text.is_empty() => out.msgs.push(cmd(Command::SendMessage { run, text })),
            EntryKind::Resume(run) if !text.is_empty() => out.msgs.push(cmd(Command::Resume { run, message: text, parent: None })),
            EntryKind::AgentName if !text.is_empty() => out.jobs.push(Job::NewAgent(text)),
            EntryKind::AccountName if !text.is_empty() => {
                self.entry = Some(Entry { kind: EntryKind::AccountProvider(text), label: "provider (anthropic, openai, ...)".into(), text: String::new(), secret: false });
            }
            EntryKind::AccountProvider(name) if !text.is_empty() => {
                self.entry = Some(Entry { kind: EntryKind::AccountKey(name, text), label: "key".into(), text: String::new(), secret: true });
            }
            EntryKind::AccountKey(name, provider) if !e.text.is_empty() => {
                out.jobs.push(Job::Account(AccountOp::Add { name, provider, key: e.text }));
            }
            _ => {}
        }
    }

    fn edit_run(&self, cx: &Context) -> Option<RunId> {
        cx.lead_run
    }

    /// An action of the key map, from a key or a button.
    fn act(&mut self, action: Action, cx: &Context, drawn: &Drawn, prompt: &mut Prompt, out: &mut Out) {
        use Action::*;
        let focus = cx.chrome.focus;
        let cmd = |c: Command| Msg::Command(c);
        let step = |by: i64| {
            let last = drawn.cards.saturating_sub(1) as i64;
            let to = (cx.chrome.selected_card as i64 + by).clamp(0, last.max(0));
            ui(UiIntent::Select(Pane::Cards, Selection::Index(to)))
        };
        let in_transcript = cx.keys() == Ctx::Transcript;
        match action {
            FocusNext => out.msgs.push(ui(UiIntent::FocusNext)),
            FocusPrev => out.msgs.push(ui(UiIntent::FocusPrev)),
            Submit => {
                if let Some(line) = prompt.take() {
                    match send(&line, &cx.agents, &[]) {
                        Sent::Msgs(m) => out.msgs.extend(m),
                        Sent::Bad(why) => prompt.notice = Some(why),
                    }
                    out.redraw = true;
                }
            }
            ClearPrompt => {
                out.redraw = !prompt.is_empty();
                prompt.escape();
            }
            OpenTranscript if in_transcript => out.msgs.push(tr(TrOp::Toggle)),
            OpenTranscript if focus == Pane::Log => {
                if let Some(run) = cx.log_run() {
                    out.msgs.push(cmd(Command::OpenTranscript(run)));
                }
            }
            OpenTranscript => out.msgs.push(ui(UiIntent::Open(focus))),
            ShowAdvice => out.msgs.push(ui(UiIntent::Open(Pane::Advisor))),
            MoveUp | MoveDown if in_transcript => {
                out.msgs.push(tr(TrOp::Scroll(if action == MoveUp { 1 } else { -1 })));
            }
            NextTool => out.msgs.push(tr(TrOp::Tool(Selection::Next))),
            PrevTool => out.msgs.push(tr(TrOp::Tool(Selection::Prev))),
            MoveUp | MoveDown if cx.menu.is_some() => {
                // The next item that applies, wrapping round; disabled ones are skipped.
                if let Some(menu) = &cx.menu {
                    let n = menu.items.len();
                    let by = if action == MoveUp { n - 1 } else { 1 };
                    let next = (1..=n).map(|k| (menu.selected + k * by) % n).find(|i| menu.items[*i].1);
                    if let Some(i) = next {
                        out.msgs.push(ui(UiIntent::MenuSelect(i as i64)));
                    }
                }
            }
            MoveUp | MoveDown => {
                let up = action == MoveUp;
                match focus {
                    Pane::Cards => out.msgs.push(step(if up { -1 } else { 1 })),
                    pane => out.msgs.push(ui(UiIntent::Scroll { pane, lines: if up { 1 } else { -1 }, sideways: false })),
                }
            }
            CardPrev => out.msgs.push(step(-1)),
            CardNext => out.msgs.push(step(1)),
            StopRun | CancelRun => {
                let run = if in_transcript { cx.transcript.as_ref().map(|t| t.run) } else { cx.selected_run };
                if let Some(run) = run {
                    out.msgs.push(cmd(Command::Cancel { run }));
                }
            }
            RetryRun => {
                let run = if in_transcript {
                    cx.transcript.as_ref().filter(|t| t.retryable).map(|t| t.run)
                } else {
                    cx.selected_run
                };
                if let Some(run) = run {
                    out.msgs.push(cmd(Command::Retry { run }));
                }
            }
            CompactRun => {
                if let Some(t) = cx.transcript.as_ref().filter(|t| t.live) {
                    out.msgs.push(cmd(Command::Compact { run: t.run }));
                }
            }
            MessageRun => {
                let run = if in_transcript { cx.transcript.as_ref().map(|t| t.run) } else { cx.selected_run };
                if let Some(run) = run {
                    self.entry = Some(Entry { kind: EntryKind::Message(run), label: "message to the run".into(), text: String::new(), secret: false });
                }
            }
            ResumeRun => {
                let run = if in_transcript { cx.transcript.as_ref().filter(|t| t.partial).map(|t| t.run) } else { cx.selected_run };
                if let Some(run) = run {
                    self.entry = Some(Entry { kind: EntryKind::Resume(run), label: "resume with".into(), text: "continue".into(), secret: false });
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
            JumpPermission => out.msgs.push(ui(UiIntent::ShowPermission)),
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
            EditInput => {
                if cx.permission.is_some() {
                    self.entry = Some(Entry {
                        kind: EntryKind::EditInput,
                        label: "run instead".into(),
                        text: cx.permission_summary.split_once(": ").map_or(cx.permission_summary.clone(), |(_, rest)| rest.to_owned()),
                        secret: false,
                    });
                }
            }
            OpenPalette => out.msgs.push(cmd(Command::OpenPalette)),
            OpenHelp => out.msgs.push(cmd(Command::OpenHelp)),
            OpenAgents => out.msgs.push(cmd(Command::OpenScreen(Screen::Agents))),
            OpenAccounts => out.msgs.push(cmd(Command::OpenScreen(Screen::Accounts))),
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
            Filter if focus == Pane::Log && !in_transcript => {
                self.filter_edit = true;
                out.redraw = true;
            }
            ToggleFollow if in_transcript => {
                let stepped = cx.transcript.as_ref().is_some_and(|t| t.stepped);
                out.msgs.push(tr(if stepped { TrOp::Live } else { TrOp::Bottom }));
            }
            ToggleFollow if focus == Pane::Log => out.msgs.push(ui(UiIntent::ToggleFollow)),
            TimeBack if in_transcript => out.msgs.push(tr(TrOp::Back)),
            TimeForward if in_transcript => out.msgs.push(tr(TrOp::Forward)),
            Quit => out.msgs.push(cmd(Command::Quit { force: false })),
            // The dashboard has no time to step through yet.
            Filter | ToggleFollow | TimeBack | TimeForward => {}
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
