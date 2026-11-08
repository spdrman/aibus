//! The dashboard frame: layout from the row budget (05 sections 1 and 6) and the draw gate that
//! redraws only when `view:dirty` names a widget or the size changed (08 section 1).

use std::io;

use aibus_core::state::value::{as_list, as_str};
use aibus_graph::{Graph, GraphTime, Value};
use ratatui::backend::Backend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::Terminal;

use crate::theme::Theme;
use crate::view::{
    CARDS, DIRTY, FORK_LAYER, HEADER, LEAD_BOX, LOG_LINES, RAIL, RETURN_BOX, STATUS_BAR,
};
use crate::widgets;

static NULL: Value = Value::Null;

/// The rectangles of one frame.
pub struct Areas {
    pub header: Rect,
    pub legend: Rect,
    pub rail: Rect,
    pub centre: Rect,
    pub log: Rect,
    pub prompt: Rect,
    pub status: Rect,
}

/// Row budget at 46 rows: header 2, legend 1, main 30, log 6, prompt 1, status 1, and five
/// spacing rows. Taller terminals give the extra rows to the log; shorter ones lose spacing
/// first, then main rows. Under 30 rows the log keeps 3 rows. Under 98 columns the rail is a
/// 3-column strip.
pub fn areas(full: Rect) -> Areas {
    let rows = full.height;
    let base_log = if rows < 30 { 5 } else { 6 };
    let log_h = base_log + rows.saturating_sub(46);
    let gaps = rows.saturating_sub(5 + base_log + 30).min(5);
    let main_h = rows.saturating_sub(5 + log_h + gaps);
    let gap = |i: u16| u16::from(gaps > i);
    // Gap order: under the legend, under main, under the log, under the header, over status.
    let mut y = full.y;
    let mut take = |h: u16, after: u16| {
        let r = Rect { y, height: h, x: full.x, width: full.width };
        y += h + after;
        r
    };
    let header = take(2, gap(3));
    let legend = take(1, gap(0));
    let main = take(main_h, gap(1));
    let log = take(log_h, gap(2));
    let prompt = take(1, gap(4));
    let status = take(1, 0);
    let rail_w = if full.width >= 98 { 22 } else { 3 };
    let rail = Rect { x: main.x + 1, width: rail_w, ..main };
    let cx = rail.right() + 2;
    let centre = Rect {
        x: cx,
        width: main.right().saturating_sub(cx + 1),
        ..main
    };
    let fit = |r: Rect| r.intersection(full);
    Areas {
        header: fit(header),
        legend: fit(legend),
        rail: fit(rail),
        centre: fit(centre),
        log: fit(log),
        prompt: fit(prompt),
        status: fit(status),
    }
}

fn connector(buf: &mut Buffer, area: Rect, glyph: &str, style: Style) {
    let x = area.x + area.width / 2;
    for y in area.y..area.bottom() {
        buf.set_string(x, y, glyph, style);
    }
}

/// The centre column: lead box, fork layer, delegate row, cards, return box. Items that do
/// not fit are dropped from the bottom up.
fn centre(buf: &mut Buffer, area: Rect, v: &Views, th: &Theme) {
    const W: u16 = 40;
    let narrow = |r: Rect| Rect {
        x: r.x + r.width.saturating_sub(W) / 2,
        width: r.width.min(W),
        ..r
    };
    let roomy = area.height >= 26;
    let fork_h = if area.height >= 30 { 6 } else { 5 };
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
    place(4, buf, &mut |b, r| widgets::lead_box(b, narrow(r), v.lead, th));
    if roomy {
        place(1, buf, &mut |b, r| connector(b, r, "·", dim));
    }
    place(fork_h, buf, &mut |b, r| widgets::fork_layer(b, narrow(r), v.fork, th));
    if roomy {
        place(1, buf, &mut |b, r| widgets::delegate_label(b, r, v.cards, th));
        place(2, buf, &mut |b, r| connector(b, r, "┼", dim));
    }
    let cards_h = area.bottom().saturating_sub(y.get()).min(7);
    place(cards_h, buf, &mut |b, r| widgets::cards(b, r, v.cards, th));
    if roomy {
        place(2, buf, &mut |b, r| connector(b, r, "│", dim));
        place(3, buf, &mut |b, r| widgets::return_box(b, narrow(r), v.ret, th));
    }
}

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

/// Draws one frame into `buf`.
pub fn render(buf: &mut Buffer, v: &Views, th: &Theme) {
    let full = buf.area;
    buf.set_style(full, Style::new().bg(th.bg).fg(th.fg));
    let a = areas(full);
    widgets::header(buf, a.header, v.header, th);
    widgets::legend(buf, a.legend, v.header, th);
    widgets::rail(buf, a.rail, v.rail, th);
    centre(buf, a.centre, v, th);
    widgets::session_log(buf, a.log, v.log, th);
    widgets::prompt(buf, a.prompt, th);
    widgets::status_bar(buf, a.status, v.status, th);
}

/// Decides when to draw. Call `note_commit` after every step and `draw` when ready.
pub struct Ui {
    pub theme: Theme,
    seen: GraphTime,
    pending: bool,
    size: Option<(u16, u16)>,
    /// Frames drawn so far.
    pub draws: u64,
}

impl Ui {
    pub fn new(theme: Theme) -> Self {
        Self { theme, seen: 0, pending: false, size: None, draws: 0 }
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

    /// Draws if a widget is dirty, the size changed, or nothing was drawn yet. Returns whether it did.
    pub fn draw<G: Graph, B: Backend>(&mut self, term: &mut Terminal<B>, g: &G) -> io::Result<bool> {
        let size = term.size()?;
        let now = (size.width, size.height);
        if !self.pending && self.size == Some(now) {
            return Ok(false);
        }
        let views = Views::read(g);
        let theme = self.theme;
        term.draw(|f| render(f.buffer_mut(), &views, &theme))?;
        self.pending = false;
        self.size = Some(now);
        self.draws += 1;
        Ok(true)
    }
}
