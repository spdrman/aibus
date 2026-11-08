//! The dashboard frame: layout from the row budget (05 sections 1 and 6), the hit map built while
//! drawing (6.1), the help screen, and the draw gate that redraws only when `view:dirty` names a
//! widget, a piece of UI state changed, or the size changed (08 section 1).

use std::io;

use aibus_core::state::value::{as_i64, as_list, as_str, get};
use aibus_core::state::{
    scroll_key, FOCUS, LAYOUT, MODAL, RAIL_EXPANDED, SELECTED_CARD, SELECTED_LOG,
};
use aibus_core::Pane;
use aibus_graph::{Graph, GraphTime, Value};
use ratatui::backend::Backend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Clear, Widget};
use ratatui::Terminal;

use crate::glyphs;
use crate::hit::{z, HitId, HitKind, HitMap};
pub use crate::layout::{areas, Areas, Layout};
use crate::layout::{too_small, MIN_COLS, MIN_ROWS};
use crate::keys::Keymap;
use crate::theme::{Interact, Theme};
use crate::view::{
    CARDS, DIRTY, FORK_LAYER, HEADER, LEAD_BOX, LOG_LINES, RAIL, RETURN_BOX, STATUS_BAR,
};
pub use crate::widgets::Prompt;
use crate::widgets::{self, panel, put};

static NULL: Value = Value::Null;

/// Everything one frame reads: exactly the `view:` nodes, one per widget.
pub struct Views<'a> {
    header: &'a Value,
    rail: &'a Value,
    lead: &'a Value,
    fork: &'a Value,
    cards: &'a Value,
    ret: &'a Value,
    log: &'a Value,
    status: &'a Value,
}

impl<'a> Views<'a> {
    pub fn read<G: Graph>(g: &'a G) -> Self {
        let at = |key| g.read_key(key).unwrap_or(&NULL);
        Self {
            header: at(HEADER),
            rail: at(RAIL),
            lead: at(LEAD_BOX),
            fork: at(FORK_LAYER),
            cards: at(CARDS),
            ret: at(RETURN_BOX),
            log: at(LOG_LINES),
            status: at(STATUS_BAR),
        }
    }
}

pub fn pane_of(name: &str) -> Pane {
    match name {
        "advisor" => Pane::Advisor,
        "forks" => Pane::Forks,
        "cards" => Pane::Cards,
        "log" => Pane::Log,
        _ => Pane::Input,
    }
}

/// The `ctrl:` state a frame is drawn from: which pane has focus, what is selected, how far the
/// log is scrolled, and the modal on top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chrome {
    pub focus: Pane,
    pub selected_card: usize,
    pub selected_log: Option<usize>,
    pub scroll_log: usize,
    pub cards_x: usize,
    pub rail_open: bool,
    pub modal: Option<String>,
    pub layout: Layout,
}

impl Chrome {
    pub fn read<G: Graph>(g: &G) -> Self {
        let int = |key: &str| g.read_key(key).and_then(as_i64);
        let uint = |key: &str| int(key).map(|v| usize::try_from(v).unwrap_or(0));
        let size = |v: Option<&Value>, k: &str| {
            v.and_then(|l| get(l, k))
                .and_then(as_i64)
                .and_then(|n| u16::try_from(n).ok())
        };
        let layout = g.read_key(LAYOUT);
        Self {
            focus: g.read_key(FOCUS).and_then(as_str).map_or(Pane::Input, pane_of),
            selected_card: uint(SELECTED_CARD).unwrap_or(0),
            selected_log: uint(SELECTED_LOG),
            scroll_log: uint(&scroll_key("log")).unwrap_or(0),
            cards_x: uint(&scroll_key("cards:x")).unwrap_or(0),
            rail_open: matches!(g.read_key(RAIL_EXPANDED), Some(Value::Bool(true))),
            modal: g.read_key(MODAL).and_then(as_str).map(str::to_owned),
            layout: Layout { rail: size(layout, "rail"), log: size(layout, "log") },
        }
    }
}

/// Where the pointer is resting or pressed, as ids in the last hit map. Not in the graph: it
/// changes with every mouse move.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pointer {
    pub hover: Option<HitId>,
    pub pressed: Option<HitId>,
}

/// The size of a scrollable list and how much of it shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Extent {
    pub total: usize,
    pub visible: usize,
}

/// What a frame leaves behind for the event layer to resolve pointer events against.
#[derive(Clone, Debug, Default)]
pub struct Drawn {
    pub hits: HitMap,
    pub areas: Areas,
    pub log: Extent,
    pub cards: usize,
}

/// One frame's inputs.
pub struct Scene<'a> {
    pub views: &'a Views<'a>,
    pub chrome: &'a Chrome,
    pub pointer: Pointer,
    pub theme: &'a Theme,
    pub keys: &'a Keymap,
    pub prompt: &'a Prompt,
}

fn connector(buf: &mut Buffer, area: Rect, glyph: &str, style: Style) {
    let x = area.x + area.width / 2;
    for y in area.y..area.bottom() {
        buf.set_string(x, y, glyph, style);
    }
}

/// The centre column: lead box, fork layer, delegate row, cards, return box. Items that do
/// not fit are dropped from the bottom up. Returns where the cards went.
fn centre(
    buf: &mut Buffer,
    area: Rect,
    v: &Views,
    th: &Theme,
    compact: bool,
    first_card: usize,
) -> Vec<(usize, Rect)> {
    const W: u16 = 40;
    let narrow = |r: Rect| Rect {
        x: r.x + r.width.saturating_sub(W) / 2,
        width: r.width.min(W),
        ..r
    };
    let roomy = area.height >= 26;
    let fork_h = if compact { 5 } else { 6 };
    let y = std::cell::Cell::new(area.y);
    let place = |h: u16, buf: &mut Buffer, draw: &mut dyn FnMut(&mut Buffer, Rect)| {
        let room = area.bottom().saturating_sub(y.get());
        let r = Rect { y: y.get(), height: h.min(room), ..area };
        if r.height > 0 {
            draw(buf, r);
        }
        y.set(y.get() + h);
    };
    let dim = Style::new().fg(th.dim);
    let mut cards = Vec::new();
    place(4, buf, &mut |b, r| widgets::lead_box(b, narrow(r), v.lead, th));
    if roomy {
        place(1, buf, &mut |b, r| connector(b, r, th.g("·"), dim));
    }
    place(fork_h, buf, &mut |b, r| widgets::fork_layer(b, narrow(r), v.fork, th, !compact));
    if roomy {
        place(1, buf, &mut |b, r| widgets::delegate_label(b, r, v.cards, th));
        place(2, buf, &mut |b, r| connector(b, r, "┼", dim));
    }
    let cards_h = area.bottom().saturating_sub(y.get()).min(7);
    place(cards_h, buf, &mut |b, r| {
        cards = widgets::cards(b, r, v.cards, th, first_card);
    });
    if roomy {
        place(2, buf, &mut |b, r| connector(b, r, "│", dim));
        place(3, buf, &mut |b, r| widgets::return_box(b, narrow(r), v.ret, th));
    }
    cards
}

/// A style patched onto the four edges of a rectangle.
fn ring(buf: &mut Buffer, r: Rect, style: Style) {
    if r.width == 0 || r.height == 0 {
        return;
    }
    for x in r.left()..r.right() {
        buf[(x, r.top())].set_style(style);
        buf[(x, r.bottom() - 1)].set_style(style);
    }
    for y in r.top()..r.bottom() {
        buf[(r.left(), y)].set_style(style);
        buf[(r.right() - 1, y)].set_style(style);
    }
}

fn centred(full: Rect, w: u16, h: u16) -> Rect {
    let (w, h) = (w.min(full.width), h.min(full.height));
    Rect {
        x: full.x + (full.width - w) / 2,
        y: full.y + (full.height - h) / 2,
        width: w,
        height: h,
    }
}

fn too_small_screen(buf: &mut Buffer, th: &Theme) {
    let full = buf.area;
    let lines = [
        "terminal too small".to_owned(),
        format!("need {MIN_COLS}x{MIN_ROWS}, have {}x{}", full.width, full.height),
    ];
    let fg = Style::new().fg(th.warn).add_modifier(Modifier::BOLD);
    for (i, line) in lines.iter().enumerate() {
        let w = line.chars().count() as u16;
        let x = full.width.saturating_sub(w) / 2;
        let y = (full.height / 2).saturating_sub(1) + i as u16;
        put(buf, full, x, y, line, if i == 0 { fg } else { Style::new().fg(th.dim_text) });
    }
}

/// The `?` screen: every action with its keys, two columns, built from the live key map.
fn help(buf: &mut Buffer, full: Rect, sc: &Scene) -> Rect {
    let th = sc.theme;
    let rows = sc.keys.help_rows();
    let per_col = rows.len().div_ceil(2).max(1);
    let area = centred(full, 76, per_col as u16 + 4);
    Clear.render(area, buf);
    let inner = panel(buf, area, "help · key map", th.focus, th);
    let half = inner.width / 2;
    for (i, row) in rows.iter().enumerate() {
        let (col, y) = ((i / per_col) as u16, (i % per_col) as u16);
        let cell = Rect {
            x: inner.x + col * half + u16::from(col > 0),
            width: half.saturating_sub(2),
            ..inner
        };
        let x = put(buf, cell, 0, y, &row.keys, Style::new().fg(th.swarm).add_modifier(Modifier::BOLD));
        put(buf, cell, x.max(11) + 1, y, row.text, Style::new().fg(th.fg));
    }
    put(buf, inner, 0, per_col as u16 + 1, "any key closes", Style::new().fg(th.dim_text));
    area
}

/// The advice modal: the last advice the rail shows, in full.
fn advice(buf: &mut Buffer, full: Rect, sc: &Scene) -> Rect {
    let th = sc.theme;
    let lines: Vec<&str> = as_list(get(sc.views.rail, "advice").unwrap_or(&NULL))
        .iter()
        .filter_map(as_str)
        .collect();
    let area = centred(full, 60, lines.len().max(1) as u16 + 4);
    Clear.render(area, buf);
    let inner = panel(buf, area, "last advice", th.advisor, th);
    if lines.is_empty() {
        put(buf, inner, 0, 0, "nothing yet", Style::new().fg(th.dim_text));
    }
    for (i, l) in lines.iter().enumerate() {
        put(buf, inner, 0, i as u16, &format!("{} {l}", th.g("»")), Style::new().fg(th.advisor));
    }
    put(buf, inner, 0, lines.len().max(1) as u16 + 1, "Esc closes", Style::new().fg(th.dim_text));
    area
}

/// A modal whose screen is not built yet: its name and how to leave.
fn placeholder(buf: &mut Buffer, full: Rect, th: &Theme, name: &str) -> Rect {
    let area = centred(full, 50, 5);
    Clear.render(area, buf);
    let inner = panel(buf, area, name, th.dim_text, th);
    put(buf, inner, 0, 0, "this screen is not built yet", Style::new().fg(th.dim_text));
    put(buf, inner, 0, 1, "Esc closes", Style::new().fg(th.dim_text));
    area
}

/// Draws one frame into `buf` and returns the hit map and geometry it was built from.
pub fn render(buf: &mut Buffer, sc: &Scene) -> Drawn {
    let (th, c, v) = (sc.theme, sc.chrome, sc.views);
    let full = buf.area;
    buf.set_style(full, Style::new().bg(th.bg).fg(th.fg));
    if too_small(full) {
        too_small_screen(buf, th);
        if th.ascii {
            glyphs::sweep(buf);
        }
        return Drawn::default();
    }
    let a = areas(full, &c.layout, c.rail_open);
    widgets::header(buf, a.header, v.header, th);
    widgets::legend(buf, a.legend, v.header, th);
    let marks = widgets::rail(buf, a.rail, v.rail, th);
    let cards = centre(buf, a.centre, v, th, a.compact, c.cards_x);
    let log = widgets::session_log(buf, a.log, v.log, th, c.scroll_log);
    widgets::prompt(buf, a.prompt, th, sc.prompt, c.focus == Pane::Input);
    widgets::status_bar(buf, a.status, v.status, th);
    // Drawn over the centre column, so what is under it goes first.
    let open_marks = a.rail_open.map(|r| {
        Clear.render(r, buf);
        widgets::rail(buf, r, v.rail, th)
    });

    // The hit map, bottom layer first.
    let mut hits = HitMap::default();
    let strip = a.rail.width < crate::layout::RAIL_FULL;
    hits.add(a.prompt, HitId::Pane(Pane::Input), HitKind::Pane, z::PANE);
    hits.add(a.centre, HitId::Pane(Pane::Cards), HitKind::Pane, z::PANE);
    hits.add(a.log, HitId::Pane(Pane::Log), HitKind::Pane, z::PANE);
    hits.add(a.rail, HitId::Pane(Pane::Advisor), HitKind::Pane, z::PANE);
    if strip {
        hits.add(a.rail, HitId::RailStrip, HitKind::Item, z::ITEM);
    }
    let (marks, mark_z) = match (&open_marks, a.rail_open) {
        (Some(m), Some(r)) => {
            hits.add(r, HitId::Pane(Pane::Advisor), HitKind::Pane, z::OVERLAY);
            (m, z::OVERLAY + 1)
        }
        _ => (&marks, z::ITEM + 1),
    };
    for (i, r) in marks.iter().enumerate() {
        hits.add(*r, HitId::Checkpoint(i), HitKind::Item, mark_z);
    }
    for (i, r) in &cards {
        hits.add(*r, HitId::Card(*i), HitKind::Item, z::ITEM);
    }
    for k in 0..log.shown {
        let row = Rect { y: log.inner.y + k as u16, height: 1, ..log.inner };
        hits.add(row, HitId::LogRow(log.first + k), HitKind::Item, z::ITEM);
    }
    if let Some(h) = a.rail_handle {
        hits.add(h, HitId::RailHandle, HitKind::Handle, z::HANDLE);
    }
    hits.add(a.log_handle, HitId::LogHandle, HitKind::Handle, z::HANDLE);

    // The scrollbar sits on the log's right border: a track you click to page, a thumb you drag.
    let visible = usize::from(log.inner.height);
    if log.total > visible && visible > 0 {
        let x = a.log.right() - 1;
        let max_scroll = log.total - visible;
        let held = c.scroll_log.min(max_scroll);
        let len = (visible * visible / log.total).clamp(1, visible);
        let top = (max_scroll - held) * (visible - len) / max_scroll;
        let track = Rect { x, y: log.inner.y, width: 1, height: log.inner.height };
        let thumb = Rect { y: track.y + top as u16, height: len as u16, ..track };
        hits.add(track, HitId::Track(Pane::Log), HitKind::Track, z::TRACK);
        hits.add(thumb, HitId::Thumb(Pane::Log), HitKind::Thumb, z::THUMB);
        let lit = [sc.pointer.hover, sc.pointer.pressed].contains(&Some(HitId::Thumb(Pane::Log)));
        let colour = if lit { th.focus } else { th.dim_text };
        for y in thumb.top()..thumb.bottom() {
            buf.set_string(x, y, th.g("█"), Style::new().fg(colour).bg(th.panel));
        }
    }

    // Interaction states, as theme tokens. A modal covers all of it.
    let modal = c.modal.as_deref();
    let is_selected = |id: HitId| match id {
        HitId::Card(i) => i == c.selected_card,
        HitId::LogRow(i) => c.selected_log == Some(i),
        _ => false,
    };
    if modal.is_none() {
        for r in hits.regions().iter().filter(|r| r.kind == HitKind::Item) {
            if is_selected(r.id) {
                buf.set_style(r.rect, th.interact(Interact::Selected));
            }
        }
        for (id, state) in [(sc.pointer.hover, Interact::Hover), (sc.pointer.pressed, Interact::Pressed)] {
            let Some(r) = id.and_then(|id| hits.find(id)) else { continue };
            if r.kind == HitKind::Item && (state == Interact::Pressed || !is_selected(r.id)) {
                buf.set_style(r.rect, th.interact(state));
            }
        }
        match c.focus {
            Pane::Cards => {
                if let Some(r) = hits.find(HitId::Card(c.selected_card)) {
                    ring(buf, r.rect, th.focused());
                }
            }
            Pane::Log => ring(buf, a.log, th.focused()),
            Pane::Advisor if !strip || a.rail_open.is_some() => {
                ring(buf, a.rail_open.unwrap_or(a.rail), th.focused());
            }
            _ => {}
        }
        for id in [sc.pointer.hover, sc.pointer.pressed].into_iter().flatten() {
            match id {
                HitId::RailHandle => {
                    if let Some(r) = hits.find(id) {
                        for y in r.rect.top()..r.rect.bottom() {
                            buf.set_string(r.rect.x, y, "│", th.focused());
                        }
                    }
                }
                HitId::LogHandle => ring_top(buf, a.log_handle, th.focused()),
                _ => {}
            }
        }
    }

    if let Some(name) = modal {
        hits.add(full, HitId::Backdrop, HitKind::Modal, z::BACKDROP);
        let area = match name {
            "help" => help(buf, full, sc),
            "advice" => advice(buf, full, sc),
            other => placeholder(buf, full, th, other),
        };
        hits.add(area, HitId::Modal, HitKind::Modal, z::MODAL);
    }
    if th.ascii {
        glyphs::sweep(buf);
    }
    Drawn {
        hits,
        areas: a,
        log: Extent { total: log.total, visible },
        cards: as_list(v.cards).len(),
    }
}

fn ring_top(buf: &mut Buffer, r: Rect, style: Style) {
    for x in r.left()..r.right() {
        buf[(x, r.top())].set_style(style);
    }
}

/// Decides when to draw. Call `note_commit` after every step and `draw` when ready.
pub struct Ui {
    pub theme: Theme,
    pub keys: Keymap,
    pub prompt: Prompt,
    pub pointer: Pointer,
    /// The last frame's geometry: what pointer events resolve against.
    pub drawn: Drawn,
    seen: GraphTime,
    pending: bool,
    size: Option<(u16, u16)>,
    last: Option<(Chrome, Pointer, Prompt, Theme)>,
    /// Frames drawn so far.
    pub draws: u64,
}

impl Ui {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            keys: Keymap::default(),
            prompt: Prompt::default(),
            pointer: Pointer::default(),
            drawn: Drawn::default(),
            seen: 0,
            pending: false,
            size: None,
            last: None,
            draws: 0,
        }
    }

    /// Looks at `view:dirty` once per new commit and remembers that a widget changed.
    pub fn note_commit<G: Graph>(&mut self, g: &G) {
        if g.now() == self.seen {
            return;
        }
        self.seen = g.now();
        let dirty = g.read_key(DIRTY).map_or(&[][..], as_list);
        self.pending |= dirty.iter().filter_map(as_str).next().is_some();
    }

    /// Draws if a widget is dirty, the size, the UI state, the pointer or the prompt changed, or
    /// nothing was drawn yet. Returns whether it did.
    pub fn draw<G: Graph, B: Backend>(&mut self, term: &mut Terminal<B>, g: &G) -> io::Result<bool> {
        let size = term.size()?;
        let now = (size.width, size.height);
        let chrome = Chrome::read(g);
        let state = (chrome, self.pointer, self.prompt.clone(), self.theme);
        if !self.pending && self.size == Some(now) && self.last.as_ref() == Some(&state) {
            return Ok(false);
        }
        let views = Views::read(g);
        let scene = Scene {
            views: &views,
            chrome: &state.0,
            pointer: self.pointer,
            theme: &self.theme,
            keys: &self.keys,
            prompt: &self.prompt,
        };
        let mut drawn = None;
        term.draw(|f| drawn = Some(render(f.buffer_mut(), &scene)))?;
        self.drawn = drawn.unwrap_or_default();
        self.pending = false;
        self.size = Some(now);
        self.last = Some(state);
        self.draws += 1;
        Ok(true)
    }
}
