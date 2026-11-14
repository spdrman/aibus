//! The centre column drawn through rataflow (D1.1).
//!
//! This is the only file in the workspace that names `rataflow`. A fresh locked `Flow` is built
//! every frame from the same row budget the classic `ui::centre` uses, with world coordinates
//! equal to screen cells (zoom 1, no pan), so nothing here is state that can drift from the graph.

use std::cell::Cell;

use aibus_core::state::value::{as_i64, as_list, as_str, get};
use aibus_graph::Value;
use rataflow::{
    Edge, EdgeContent, EdgePathContext, EdgeRenderContext, Flow, Handle, HandlePosition, Node,
    NodeContent, NodeRenderContext, Palette, Path, Position, Theme as FlowTheme,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Widget;

use crate::theme::Theme;
use crate::ui::{Centre, Views};
use crate::widgets::{self, CARD_W};

/// Our theme as rataflow's palette. The one place a `Color` of ours reaches rataflow, and only
/// copies: no colour literal here. `muted` is the edge and border colour, so it takes `dim_text`
/// (clears 3:1 on `bg` and `panel`); `dim` is the sampled rule colour and stays decorative.
pub fn palette(th: &Theme) -> Palette {
    Palette {
        canvas_bg: th.bg,
        surface: th.panel,
        muted: th.dim_text,
        subtle: th.dim,
        accent: th.focus,
        text: th.fg,
        success: th.ok,
        error: th.error,
    }
}

/// What a node draws. Each variant calls the widget the classic path calls, so the boxes are
/// cell for cell the same.
#[derive(Debug)]
enum Piece<'a> {
    Lead(&'a Value),
    Fork {
        v: &'a Value,
        footer: bool,
        lead: String,
    },
    Card(&'a Value),
    Return(&'a Value),
}

/// A piece and the theme it draws with.
#[derive(Debug)]
struct Boxed<'a> {
    piece: Piece<'a>,
    th: &'a Theme,
}

impl NodeContent for Boxed<'_> {
    fn render(&self, ctx: &NodeRenderContext, buf: &mut Buffer) {
        let (th, area) = (self.th, ctx.area);
        // The classic frame starts with this fill; a node's scratch buffer does not.
        buf.set_style(area, Style::new().bg(th.bg).fg(th.fg));
        match &self.piece {
            Piece::Lead(v) => widgets::lead_box(buf, area, v, th),
            Piece::Fork { v, footer, lead } => widgets::fork_layer(buf, area, v, th, *footer, lead),
            Piece::Card(v) => widgets::card(buf, area, v, th),
            Piece::Return(v) => widgets::return_box(buf, area, v, th),
        }
    }
}

/// A node at `at` (world cells, relative to the centre area's corner), `size` cells big. No
/// handle shows and nothing about it can be dragged, selected, connected or deleted.
fn node<'a>(
    id: &str,
    at: (i32, i32),
    size: (u16, u16),
    piece: Piece<'a>,
    th: &'a Theme,
) -> Node<Boxed<'a>> {
    let handles = vec![
        Handle::target(HandlePosition::Top).with_hidden(true),
        Handle::source(HandlePosition::Bottom).with_hidden(true),
    ];
    Node::new(
        id,
        (f64::from(at.0), f64::from(at.1)),
        size,
        Boxed { piece, th },
    )
    .with_handles(handles)
    .with_draggable(false)
    .with_selectable(false)
    .with_connectable(false)
    .with_deletable(false)
}

/// A connector in the classic look: a dim dotted rule with, optionally, one bright travelling
/// dot and an arrow head. Its cells are world cells, so it is the same picture at any pan.
#[derive(Debug, Default)]
pub struct Dotted {
    cells: Vec<(i32, i32)>,
    /// The step of `cells` the bright dot is on.
    at: Option<usize>,
    dot: &'static str,
    travelling: &'static str,
    dim: Style,
    bright: Style,
    /// An arrow head drawn over whatever is there, as `ui::centre` does under the cards.
    head: Option<((i32, i32), &'static str, Style)>,
}

impl EdgeContent for Dotted {
    fn compute_path(&self, ctx: &EdgePathContext) -> Path {
        Path::straight_with_direction(ctx.from, ctx.to, ctx.source_position, ctx.target_position)
    }

    /// Blank cells only, except that the travelling dot may take the place of a plain one, so a
    /// connector never cuts through a box and two connectors share their trunk.
    fn render(&self, ctx: &EdgeRenderContext, buf: &mut Buffer) {
        let at = |(x, y): (i32, i32)| {
            let (tx, ty) = ctx.world_to_terminal(Position::new(f64::from(x), f64::from(y)));
            ctx.is_in_bounds(tx, ty).then_some((tx as u16, ty as u16))
        };
        for (i, &cell) in self.cells.iter().enumerate() {
            let Some(pos) = at(cell) else { continue };
            let here = buf[pos].symbol();
            let travelling = self.at == Some(i);
            if here == " " || (travelling && here == self.dot) {
                let (glyph, style) = if travelling {
                    (self.travelling, self.bright)
                } else {
                    (self.dot, self.dim)
                };
                buf[pos].set_symbol(glyph).set_style(style);
            }
        }
        if let Some((pos, glyph, style)) = self
            .head
            .and_then(|(cell, g, s)| at(cell).map(|p| (p, g, s)))
        {
            buf[pos].set_symbol(glyph).set_style(style);
        }
    }
}

/// A node's rect on the screen, cut to the centre area. Read back from the flow after it
/// rendered, so the hit map is where the boxes really are.
fn on_screen(flow: &Flow<Boxed<'_>, Dotted>, id: &str, area: Rect) -> Option<Rect> {
    let (l, t, r, b) = flow.node_terminal_rect(id)?;
    let clip = |v: i32, lo: u16, hi: u16| {
        u16::try_from(v.clamp(i32::from(lo), i32::from(hi))).unwrap_or(lo)
    };
    Some(Rect {
        x: clip(l, area.left(), area.right()),
        y: clip(t, area.top(), area.bottom()),
        width: clip(r, area.left(), area.right()).saturating_sub(clip(
            l,
            area.left(),
            area.right(),
        )),
        height: clip(b, area.top(), area.bottom()).saturating_sub(clip(
            t,
            area.top(),
            area.bottom(),
        )),
    })
}

/// The centre column: lead box, fork layer, delegate row, cards, return box, joined by edges.
/// Same arguments and same placement as the classic `ui::centre`.
pub(crate) fn centre(
    buf: &mut Buffer,
    area: Rect,
    v: &Views,
    th: &Theme,
    (compact, first_card, tick): (bool, usize, i64),
) -> Centre {
    const W: u16 = 40;
    let narrow = |r: Rect| Rect {
        x: r.x + r.width.saturating_sub(W) / 2,
        width: r.width.min(W),
        ..r
    };
    let roomy = area.height >= 28;
    let fork_h = if compact { 5 } else { 6 };
    let y = Cell::new(area.y);
    let slot = |h: u16| {
        let r = Rect {
            y: y.get(),
            height: h.min(area.bottom().saturating_sub(y.get())),
            ..area
        };
        y.set(y.get() + h);
        r
    };
    // A cell of a connector, in world coordinates (the centre area's corner is the origin).
    let world = |x: u16, y: u16| {
        (
            i32::from(x) - i32::from(area.x),
            i32::from(y) - i32::from(area.y),
        )
    };
    let (dim, dot, travelling) = (Style::new().fg(th.dim), th.g("·"), th.g("●"));
    let connector = |cells: Vec<(i32, i32)>, at: Option<usize>, bright: Style| Dotted {
        cells,
        at,
        dot,
        travelling,
        dim,
        bright,
        head: None,
    };
    let mut c = Centre {
        cards: Vec::new(),
        lead: Rect::default(),
        ret: Rect::default(),
    };
    let mut nodes: Vec<Node<Boxed<'_>>> = Vec::new();
    let mut edges: Vec<Edge<Dotted>> = Vec::new();
    let lead_label = get(v.lead, "model_label")
        .and_then(as_str)
        .unwrap_or("")
        .to_owned();

    let lead = slot(4);
    let has_lead = lead.height > 0;
    if has_lead {
        c.lead = narrow(lead);
        nodes.push(node(
            "lead",
            world(c.lead.x, c.lead.y),
            (c.lead.width, c.lead.height),
            Piece::Lead(v.lead),
            th,
        ));
    }
    let gap = if roomy { Some(slot(2)) } else { None };
    let fork = narrow(slot(fork_h));
    let has_fork = fork.height > 0;
    if has_fork {
        nodes.push(node(
            "fork",
            world(fork.x, fork.y),
            (fork.width, fork.height),
            Piece::Fork {
                v: v.fork,
                footer: !compact,
                lead: lead_label,
            },
            th,
        ));
    }
    if let Some(gap) = gap.filter(|g| g.height > 0 && has_lead && has_fork) {
        // The lead's orange dot travels down to the fork layer.
        let x = narrow(gap).x + narrow(gap).width / 2;
        let at = usize::try_from(tick.rem_euclid(2)).unwrap_or(0);
        let cells = vec![world(x, gap.y), world(x, gap.y + 1)];
        edges.push(
            Edge::new("lead-fork", "lead", "fork").with_content(connector(
                cells,
                Some(at),
                Style::new().fg(th.lead),
            )),
        );
    }
    if roomy {
        let r = slot(1);
        if r.height > 0 {
            widgets::delegate_label(buf, r, v.delegate, th);
        }
    }
    let connector_y = y.get();
    if roomy {
        slot(2);
    }
    let cards_h = area.bottom().saturating_sub(y.get()).min(8);
    let cards_top = y.get();
    let cards_area = slot(cards_h);
    let all = as_list(v.cards);
    let planned = if cards_area.height > 0 {
        widgets::card_rects(cards_area, all.len(), first_card)
    } else {
        Vec::new()
    };
    // Every card whose box reaches into the column, at the stride the window uses: the window's own
    // are where the classic row puts them, the others sit beside it and come in by panning.
    let mut shown: Vec<(usize, i32)> = Vec::new();
    if let Some(&(first, base)) = planned.first() {
        let (bx, by) = world(base.x, base.y);
        for (j, card) in all.iter().enumerate() {
            let x = bx + (j as i32 - first as i32) * i32::from(CARD_W + 1);
            if x + i32::from(CARD_W) > 0 && x < i32::from(area.width) {
                shown.push((j, x));
                nodes.push(node(
                    &format!("card:{j}"),
                    (x, by),
                    (CARD_W, base.height),
                    Piece::Card(card),
                    th,
                ));
            }
        }
    }
    if let (Some((first, _)), Some((last, _))) = (planned.first(), planned.last()) {
        let card_h = cards_area.height.min(7);
        if cards_area.height > card_h {
            let row = Rect {
                y: cards_area.y + card_h,
                height: 1,
                ..cards_area
            };
            widgets::card_counts(buf, row, th, (*first, last + 1, all.len()));
        }
    }
    let mut ret_planned = None;
    if roomy {
        y.set(y.get().max(cards_top + cards_h + 2));
        let r = narrow(slot(3));
        if r.height > 0 {
            nodes.push(node(
                "return",
                world(r.x, r.y),
                (r.width, r.height),
                Piece::Return(v.ret),
                th,
            ));
            ret_planned = Some(r);
        }
    }
    if roomy && has_fork && !shown.is_empty() {
        // The fork splits into one dotted path per card, a dot travelling down each working one.
        let mid = narrow(Rect {
            y: connector_y,
            height: 1,
            ..area
        });
        let (xc, cy) = world(mid.x + mid.width / 2, connector_y);
        let offsets = as_list(get(v.delegate, "connectors").unwrap_or(&Value::Null));
        let centre_of = |x: i32| x + i32::from(CARD_W / 2);
        for &(j, x) in &shown {
            let xi = centre_of(x);
            let mut path = vec![(xc, cy), (xc, cy + 1)];
            let across: Vec<i32> = if xi >= xc {
                (xc + 1..=xi).collect()
            } else {
                (xi..xc).rev().collect()
            };
            path.extend(across.into_iter().map(|x| (x, cy + 1)));
            let at = offsets
                .get(j)
                .and_then(as_i64)
                .filter(|o| *o >= 0)
                .map(|o| usize::try_from(o).unwrap_or(0) % path.len());
            edges.push(
                Edge::new(format!("fork-card:{j}"), "fork", format!("card:{j}"))
                    .with_content(connector(path, at, Style::new().fg(th.swarm))),
            );
        }
        // Below the cards the paths merge into one arrow at the return box; its head pulses
        // while a result is on its way.
        let merge = cards_top + cards_h;
        if merge + 3 <= area.bottom() && ret_planned.is_some() {
            let xs: Vec<i32> = shown.iter().map(|&(_, x)| centre_of(x)).collect();
            let (lo, hi) = (
                xs.iter().copied().min().unwrap_or(xc),
                xs.iter().copied().max().unwrap_or(xc),
            );
            let pulse = matches!(get(v.ret, "pulse"), Some(Value::Bool(true)));
            let head = if pulse && (tick / 4) % 2 != 0 {
                Style::new().fg(th.dim_text)
            } else {
                Style::new().fg(th.lead).add_modifier(Modifier::BOLD)
            };
            let my = i32::from(merge) - i32::from(area.y);
            let mut merged = connector((lo..=hi).map(|x| (x, my)).collect(), None, dim);
            merged.head = Some(((xc, my + 1), th.g("▾"), head));
            edges.push(
                Edge::new("cards-return", format!("card:{}", shown[0].0), "return")
                    .with_content(merged),
            );
        }
    }
    let Ok(flow) = Flow::with_graph(nodes, edges) else {
        return c;
    };
    let mut flow = flow
        .with_locked(true)
        .with_theme(FlowTheme::Custom(palette(th)));
    (&mut flow).render(area, buf);
    // The hit map and the rings follow the boxes where the flow put them.
    if has_lead {
        c.lead = on_screen(&flow, "lead", area).unwrap_or(c.lead);
    }
    c.ret = ret_planned.map_or(c.ret, |r| on_screen(&flow, "return", area).unwrap_or(r));
    c.cards = shown
        .iter()
        .filter_map(|&(j, _)| {
            on_screen(&flow, &format!("card:{j}"), area)
                .filter(|r| r.width > 0)
                .map(|r| (j, r))
        })
        .collect();
    c
}
