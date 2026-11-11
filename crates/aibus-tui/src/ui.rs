//! The dashboard frame: layout from the row budget (05 sections 1 and 6), the hit map built while
//! drawing (6.1), the help screen, and the draw gate that redraws only when `view:dirty` names a
//! widget, a piece of UI state changed, or the size changed (08 section 1).


use aibus_core::state::value::{as_i64, as_list, as_str, get};
use aibus_core::state::{
    scroll_key, FOCUS, LAYOUT, LOG_FILTER, LOG_FOLLOW, MENU, MODAL, RAIL_EXPANDED, SELECTED_CARD,
    SELECTED_LOG, TICK,
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
use crate::hit::{z, HitId, HitKind, HitMap, Segment};
pub use crate::layout::{areas, Areas, Layout};
use crate::layout::{too_small, MIN_COLS, MIN_ROWS};
use crate::keys::Keymap;
use crate::screens::{self, Entry, Placed};
use crate::theme::{Interact, Theme};
use crate::transcript::{self, Cells, Geom};
use crate::view::{
    CARDS, DELEGATE_ROW, DIRTY, FORK_LAYER, HEADER, LEAD_BOX, LOG_LINES, MENU_VIEW, RAIL,
    RETURN_BOX, RUN_TIPS, STATUS_BAR,
};
use crate::views::{ACCOUNTS_VIEW, AGENTS, COST, MODELS, PALETTE, PERMISSION, RUN_STATUS, TRANSCRIPT};
pub use crate::widgets::Prompt;
use crate::widgets::{self, panel, put};

static NULL: Value = Value::Null;

/// Everything one frame reads: exactly the `view:` nodes, one per widget.
pub struct Views<'a> {
    header: &'a Value,
    rail: &'a Value,
    pub(crate) lead: &'a Value,
    pub(crate) delegate: &'a Value,
    pub(crate) fork: &'a Value,
    pub(crate) cards: &'a Value,
    pub(crate) ret: &'a Value,
    log: &'a Value,
    status: &'a Value,
    tips: &'a Value,
    menu: &'a Value,
    permission: &'a Value,
    palette: &'a Value,
    agents: &'a Value,
    accounts: &'a Value,
    models: &'a Value,
    transcript: &'a Value,
    run_status: &'a Value,
    cost: &'a Value,
}

impl<'a> Views<'a> {
    pub fn read<G: Graph>(g: &'a G) -> Self {
        let at = |key| g.read_key(key).unwrap_or(&NULL);
        Self {
            header: at(HEADER),
            rail: at(RAIL),
            lead: at(LEAD_BOX),
            delegate: at(DELEGATE_ROW),
            fork: at(FORK_LAYER),
            cards: at(CARDS),
            ret: at(RETURN_BOX),
            log: at(LOG_LINES),
            status: at(STATUS_BAR),
            tips: at(RUN_TIPS),
            menu: at(MENU_VIEW),
            permission: at(PERMISSION),
            palette: at(PALETTE),
            agents: at(AGENTS),
            accounts: at(ACCOUNTS_VIEW),
            models: at(MODELS),
            transcript: at(TRANSCRIPT),
            run_status: at(RUN_STATUS),
            cost: at(COST),
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
    /// The animation counter the connectors' dots and the arrowhead are drawn from.
    pub tick: i64,
    /// The text after `/` in the log, and whether the log follows its newest row.
    pub filter: String,
    pub follow: bool,
    pub menu_open: bool,
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
            tick: int(TICK).unwrap_or(0),
            filter: g.read_key(LOG_FILTER).and_then(as_str).unwrap_or("").to_owned(),
            follow: !matches!(g.read_key(LOG_FOLLOW), Some(Value::Bool(false))),
            menu_open: !matches!(g.read_key(MENU), None | Some(Value::Null)),
        }
    }
}

/// Where the pointer is resting or pressed, as ids in the last hit map. Not in the graph: it
/// changes with every mouse move.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pointer {
    pub hover: Option<HitId>,
    pub pressed: Option<HitId>,
    /// The element whose tooltip is up: the one the pointer has rested on for 500 ms, or the
    /// selected one after `Space`.
    pub tip: Option<HitId>,
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
    /// The open transcript's geometry and the plain text of its rows.
    pub transcript: Option<Geom>,
    /// Each row of the prompt and the char index it starts at, and the rect of its popup rows.
    pub prompt_rows: Vec<(Rect, usize)>,
    /// The column the prompt's text starts at.
    pub prompt_indent: u16,
}

/// One frame's inputs.
pub struct Scene<'a> {
    pub views: &'a Views<'a>,
    pub chrome: &'a Chrome,
    pub pointer: Pointer,
    pub theme: &'a Theme,
    pub keys: &'a Keymap,
    pub prompt: &'a Prompt,
    /// The log's filter is being typed.
    pub filter_edit: bool,
    /// A text field open over the modal, if any.
    pub entry: Option<&'a Entry>,
    /// The cells selected in the transcript, and whether they were just copied.
    pub selection: Option<Cells>,
    pub copied: bool,
    /// What the last action on the agents or accounts screen said.
    pub note: &'a str,
}

/// Dotted cells in `dim`, with a brighter `●` at step `at` of the path when there is one. Only
/// blank cells are drawn on, so a connector never cuts through a box.
fn dotted(buf: &mut Buffer, path: &[(u16, u16)], at: Option<usize>, dim: Style, bright: Style, th: &Theme) {
    let full = buf.area;
    for (i, &(x, y)) in path.iter().enumerate() {
        // Blank cells only, except that a travelling dot may take the place of a plain one.
        let here = buf[(x, y)].symbol();
        let free = here == " " || (at == Some(i) && here == th.g("·"));
        if x >= full.right() || y >= full.bottom() || !free {
            continue;
        }
        if at == Some(i) {
            buf.set_string(x, y, th.g("●"), bright);
        } else {
            buf.set_string(x, y, th.g("·"), dim);
        }
    }
}

/// Where the centre column put things, for the connectors and the hit map.
pub(crate) struct Centre {
    pub(crate) cards: Vec<(usize, Rect)>,
    pub(crate) lead: Rect,
    pub(crate) ret: Rect,
}

/// The checkpoint that is active now and where it was drawn on the rail, with the box it is
/// about: the lead box, the failing worker's card or the return box. A dotted path with
/// travelling dots joins them.
fn advisor_link(buf: &mut Buffer, rail: Rect, marks: &[Rect], c: &Centre, v: &Views, th: &Theme, tick: i64) {
    let checkpoints = as_list(get(v.rail, "checkpoints").unwrap_or(&NULL));
    let Some(i) = checkpoints.iter().position(|m| matches!(get(m, "active"), Some(Value::Bool(true)))) else {
        return;
    };
    let target = match i {
        0 => Some(c.lead),
        1 => as_list(v.cards)
            .iter()
            .position(|k| get(k, "border").and_then(as_str) == Some("advisor"))
            .and_then(|n| c.cards.iter().find(|(k, _)| *k == n))
            .map(|(_, r)| *r),
        _ => Some(c.ret),
    };
    let (Some(target), Some(from)) = (target, marks.get(i)) else { return };
    if target.width == 0 || from.width == 0 {
        return;
    }
    let (gx, y0, y1) = (rail.right(), from.y, target.y + target.height / 2);
    let mut path: Vec<(u16, u16)> = vec![(gx, y0)];
    if y1 >= y0 {
        path.extend((y0 + 1..=y1).map(|y| (gx, y)));
    } else {
        path.extend((y1..y0).rev().map(|y| (gx, y)));
    }
    path.extend((gx + 1..target.x).map(|x| (x, y1)));
    let len = i64::try_from(path.len()).unwrap_or(1).max(1);
    let at = usize::try_from(tick.rem_euclid(len)).unwrap_or(0);
    dotted(buf, &path, Some(at), Style::new().fg(th.dim), Style::new().fg(th.advisor), th);
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

/// What a tooltip says about the element under `id`: the parts of it a card or row cuts off.
fn tip_lines(id: HitId, v: &Views) -> Option<Vec<String>> {
    let word = |x: &Value, k: &str| get(x, k).and_then(as_str).unwrap_or("").to_owned();
    let of_run = |run: &str| -> Vec<String> {
        let Some(t) = as_list(v.tips).iter().find(|t| get(t, "run_id").and_then(as_str) == Some(run)) else {
            return Vec::new();
        };
        let mut out = vec![word(t, "name")];
        let description = word(t, "description");
        if !description.is_empty() {
            out.push(description);
        }
        out.push(format!("model {}  account {}", word(t, "model"), word(t, "account")));
        out.push(format!("cost {}", word(t, "cost")));
        out.push(format!("run {run}"));
        out
    };
    match id {
        HitId::Card(i) => {
            let card = as_list(v.cards).get(i)?;
            let mut out = vec![word(card, "label")];
            out.extend(of_run(&word(card, "run_id")));
            Some(out)
        }
        HitId::LogRow(i) => {
            let line = as_list(v.log).get(i)?;
            let mut out = vec![format!("{}  {}", word(line, "time"), word(line, "actor")), word(line, "text")];
            out.extend(of_run(&word(line, "run")));
            Some(out)
        }
        HitId::Status(Segment::Ledger) => {
            let l = get(v.status, "ledger")?;
            Some(vec![format!("goal: {}", word(l, "goal")), format!("now: {}", word(l, "now"))])
        }
        HitId::Status(Segment::Alert) => {
            let a = as_list(get(v.status, "alerts")?).iter().find(|a| get(a, "kind").and_then(as_str) == Some("permission"))?;
            Some(vec![word(a, "text")])
        }
        _ => None,
    }
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
    // A prompt of several rows and the hint under it take rows from the dashboard, as long as
    // what is left is still a dashboard.
    let want_hint = sc.prompt.hints || sc.prompt.notice.is_some();
    let wanted = sc.prompt.rows().saturating_sub(1) + u16::from(want_hint);
    let extra = if full.height.saturating_sub(wanted) >= MIN_ROWS { wanted } else { 0 };
    let mut a = areas(Rect { height: full.height - extra, ..full }, &c.layout, c.rail_open);
    let hint = extra > 0 && want_hint;
    a.prompt.height = if extra == 0 { 1 } else { sc.prompt.rows() };
    let hint_area = Rect { y: a.prompt.bottom(), height: u16::from(hint), ..a.prompt };
    a.status.y += extra;
    widgets::header(buf, a.header, v.header, th);
    widgets::legend(buf, a.legend, v.header, th);
    let marks = widgets::rail(buf, a.rail, v.rail, th);
    let cen = crate::diagram::centre(buf, a.centre, v, th, (a.compact, c.cards_x, c.tick));
    advisor_link(buf, a.rail, &marks, &cen, v, th, c.tick);
    let lead_label = get(v.lead, "model_label").and_then(as_str).unwrap_or("");
    let filter = (c.filter.as_str(), sc.filter_edit, c.follow);
    let log = widgets::session_log(buf, a.log, v.log, th, c.scroll_log, filter, lead_label);
    let prompt_rows = widgets::prompt(buf, a.prompt, th, sc.prompt, c.focus == Pane::Input, c.tick);
    if hint {
        widgets::hint_row(buf, hint_area, th, sc.prompt.notice.as_deref());
    }
    let segments = widgets::status_bar(buf, a.status, v.status, th);
    // Drawn over the centre column, so what is under it goes first.
    let open_marks = a.rail_open.map(|r| {
        Clear.render(r, buf);
        widgets::rail(buf, r, v.rail, th)
    });

    // The hit map, bottom layer first.
    let mut hits = HitMap::default();
    let strip = a.rail.width < crate::layout::RAIL_FULL;
    hits.add(Rect { height: a.prompt.height + u16::from(hint), ..a.prompt }, HitId::Pane(Pane::Input), HitKind::Pane, z::PANE);
    for (i, (r, _)) in prompt_rows.iter().enumerate() {
        hits.add(*r, HitId::PromptRow(i), HitKind::Pane, z::ITEM);
    }
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
    for (i, r) in &cen.cards {
        hits.add(*r, HitId::Card(*i), HitKind::Item, z::ITEM);
    }
    for (seg, r) in &segments {
        hits.add(*r, HitId::Status(*seg), HitKind::Item, z::ITEM);
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

    // The completion popup floats above the prompt.
    if modal.is_none() {
        if let Some(p) = sc.prompt.popup.as_ref().filter(|p| !p.items.is_empty()) {
            let (rect, rows) = widgets::popup(buf, full, a.prompt, &p.items, p.sel, th);
            hits.add(rect, HitId::Modal, HitKind::Modal, z::OVERLAY + 2);
            for (i, r) in rows {
                hits.add(r, HitId::PopupItem(i), HitKind::Button, z::OVERLAY + 3);
            }
        }
    }

    // The context menu, or else the tooltip: layers over the frame that move nothing.
    if modal.is_none() && !matches!(v.menu, Value::Null) {
        hits.add(full, HitId::Backdrop, HitKind::Modal, z::BACKDROP);
        let item = |id: Option<HitId>| match id {
            Some(HitId::MenuItem(i)) => Some(i),
            _ => None,
        };
        let (rect, rows) = widgets::menu(buf, full, v.menu, th, (item(sc.pointer.hover), item(sc.pointer.pressed)));
        hits.add(rect, HitId::MenuBox, HitKind::Modal, z::MODAL);
        for (i, r) in rows.iter().enumerate() {
            let enabled = as_list(get(v.menu, "items").unwrap_or(&NULL))
                .get(i)
                .is_some_and(|m| matches!(get(m, "enabled"), Some(Value::Bool(true))));
            if enabled {
                hits.add(*r, HitId::MenuItem(i), HitKind::Item, z::MODAL + 1);
            } else {
                // A disabled item swallows the click and does nothing.
                hits.add(*r, HitId::MenuBox, HitKind::Modal, z::MODAL + 1);
            }
        }
    } else if modal.is_none() {
        let shown = sc.pointer.tip.and_then(|id| Some((hits.find(id)?.rect, tip_lines(id, v)?)));
        if let Some((anchor, lines)) = shown {
            widgets::tooltip(buf, full, anchor, &lines, th);
        }
    }

    let mut transcript_geom = None;
    if let Some(name) = modal {
        hits.add(full, HitId::Backdrop, HitKind::Modal, z::BACKDROP);
        let cwd = sc.prompt.cwd.as_str();
        let placed: Placed = match name {
            "help" => Placed { area: help(buf, full, sc), hits: Vec::new() },
            "advice" => Placed { area: advice(buf, full, sc), hits: Vec::new() },
            "permission" if !matches!(v.permission, Value::Null) => {
                screens::permission(buf, full, v.permission, th, &sc.pointer, sc.entry, cwd)
            }
            "palette" => screens::palette(buf, full, v.palette, th, &sc.pointer),
            "agents" => screens::agents(buf, full, v.agents, th, &sc.pointer, sc.entry, sc.note),
            "accounts" => screens::accounts(buf, full, v.accounts, th, &sc.pointer, sc.entry, sc.note),
            m if m.starts_with("models:") => screens::models(buf, full, v.models, th, &sc.pointer, sc.entry, sc.note),
            "outcome" => screens::outcome(buf, full, th, &sc.pointer),
            "confirm:clear" => screens::confirm(buf, full, th, &sc.pointer, "clear", "Clear the session log?", ("clear_yes", "Clear")),
            c if c.starts_with("confirm:cancel:") => {
                screens::confirm(buf, full, th, &sc.pointer, "cancel", "Cancel the lead run?", ("cancel_yes", "Cancel the lead"))
            }
            t if t.starts_with("status:") => screens::run_status(buf, full, v.run_status, th, &sc.pointer),
            "cost" => screens::cost(buf, full, v.cost, th, &sc.pointer),
            t if t.starts_with("transcript:") => {
                let g = transcript::draw(buf, full, v.transcript, th, &sc.pointer, sc.selection, sc.entry, sc.copied);
                for (id, r, kind) in &g.hits {
                    let layer = if matches!(id, HitId::TrRow(_)) { z::MODAL + 1 } else { z::MODAL + 2 };
                    hits.add(*r, *id, *kind, layer);
                }
                let area = g.area;
                transcript_geom = Some(g);
                Placed { area, hits: Vec::new() }
            }
            other => Placed { area: placeholder(buf, full, th, other), hits: Vec::new() },
        };
        hits.add(placed.area, HitId::Modal, HitKind::Modal, z::MODAL);
        for (id, r) in placed.hits {
            let kind = if matches!(id, HitId::ListRow(_)) { HitKind::Item } else { HitKind::Button };
            hits.add(r, id, kind, z::MODAL + 1);
        }
    }
    if th.ascii {
        glyphs::sweep(buf);
    }
    Drawn {
        hits,
        areas: a,
        log: Extent { total: log.total, visible },
        cards: as_list(v.cards).len(),
        transcript: transcript_geom,
        prompt_rows,
        prompt_indent: u16::try_from(sc.prompt.cwd.chars().count() + 4).unwrap_or(0),
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
    /// The log's filter is being typed (`/`), so its title shows a cursor.
    pub filter_edit: bool,
    /// The last frame's geometry: what pointer events resolve against.
    pub drawn: Drawn,
    /// A text field open over the modal.
    pub entry: Option<Entry>,
    /// The transcript cells selected with the mouse, and whether they were just copied.
    pub selection: Option<Cells>,
    pub copied: bool,
    pub note: String,
    /// Ticks reach the screen (a run is running, advice is typing). Off, the tick counter only
    /// matters through the prompt's cursor, so a frame is drawn when that blinks and no oftener.
    pub animating: bool,
    seen: GraphTime,
    pending: bool,
    size: Option<(u16, u16)>,
    last: Option<State>,
    /// Frames drawn so far.
    pub draws: u64,
}

/// What a frame was drawn from, to tell whether the next one would differ.
#[derive(PartialEq)]
struct State {
    chrome: Chrome,
    pointer: Pointer,
    prompt: Prompt,
    theme: Theme,
    filter_edit: bool,
    entry: Option<Entry>,
    selection: Option<Cells>,
    copied: bool,
    note: String,
}

impl Ui {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            keys: Keymap::default(),
            prompt: Prompt::default(),
            pointer: Pointer::default(),
            filter_edit: false,
            drawn: Drawn::default(),
            entry: None,
            selection: None,
            copied: false,
            note: String::new(),
            animating: true,
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
    pub fn draw<G: Graph, B: Backend>(&mut self, term: &mut Terminal<B>, g: &G) -> Result<bool, B::Error> {
        let size = term.size()?;
        let now = (size.width, size.height);
        let chrome = Chrome::read(g);
        let mut state = State {
            chrome: chrome.clone(),
            pointer: self.pointer,
            prompt: self.prompt.clone(),
            theme: self.theme,
            filter_edit: self.filter_edit,
            entry: self.entry.clone(),
            selection: self.selection,
            copied: self.copied,
            note: self.note.clone(),
        };
        if !self.animating {
            state.chrome.tick = chrome.tick / crate::prompt::BLINK_TICKS;
        }
        if !self.pending && self.size == Some(now) && self.last.as_ref() == Some(&state) {
            return Ok(false);
        }
        let views = Views::read(g);
        let scene = Scene {
            views: &views,
            chrome: &chrome,
            pointer: self.pointer,
            theme: &self.theme,
            keys: &self.keys,
            prompt: &self.prompt,
            filter_edit: self.filter_edit,
            entry: self.entry.as_ref(),
            selection: self.selection,
            copied: self.copied,
            note: &self.note,
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
