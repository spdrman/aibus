//! The centre column drawn through rataflow, behind the `flow-diagram` feature (D1.1).
//!
//! This is the only file in the workspace that names `rataflow`. A fresh locked `Flow` is built
//! every frame from the same row budget the classic `ui::centre` uses, with world coordinates
//! equal to screen cells (zoom 1, no pan), so nothing here is state that can drift from the graph.

use std::cell::Cell;

use aibus_core::state::value::{as_list, as_str, get};
use aibus_graph::Value;
use rataflow::{
    Edge, Flow, Handle, HandlePosition, Node, NodeContent, NodeRenderContext, Palette, StepEdge,
    Theme as FlowTheme,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Widget;

use crate::theme::Theme;
use crate::ui::{Centre, Views};
use crate::widgets;

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
    Fork { v: &'a Value, footer: bool, lead: String },
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

/// A node at screen rect `r`, in world coordinates relative to `origin` (the centre area). No
/// handle shows and nothing about it can be dragged, selected, connected or deleted.
fn node<'a>(id: &str, r: Rect, origin: Rect, piece: Piece<'a>, th: &'a Theme) -> Node<Boxed<'a>> {
    let handles = vec![
        Handle::target(HandlePosition::Top).with_hidden(true),
        Handle::source(HandlePosition::Bottom).with_hidden(true),
    ];
    Node::new(id, (f64::from(r.x - origin.x), f64::from(r.y - origin.y)), (r.width, r.height), Boxed { piece, th })
        .with_handles(handles)
        .with_draggable(false)
        .with_selectable(false)
        .with_connectable(false)
        .with_deletable(false)
}

/// The centre column: lead box, fork layer, delegate row, cards, return box, joined by edges.
/// Same arguments and same placement as the classic `ui::centre`.
pub(crate) fn centre(
    buf: &mut Buffer,
    area: Rect,
    v: &Views,
    th: &Theme,
    (compact, first_card, _tick): (bool, usize, i64),
) -> Centre {
    const W: u16 = 40;
    let narrow = |r: Rect| Rect { x: r.x + r.width.saturating_sub(W) / 2, width: r.width.min(W), ..r };
    let roomy = area.height >= 28;
    let fork_h = if compact { 5 } else { 6 };
    let y = Cell::new(area.y);
    let slot = |h: u16| {
        let r = Rect { y: y.get(), height: h.min(area.bottom().saturating_sub(y.get())), ..area };
        y.set(y.get() + h);
        r
    };
    let mut c = Centre { cards: Vec::new(), lead: Rect::default(), ret: Rect::default() };
    let mut nodes: Vec<Node<Boxed<'_>>> = Vec::new();
    let lead_label = get(v.lead, "model_label").and_then(as_str).unwrap_or("").to_owned();

    let lead = slot(4);
    let has_lead = lead.height > 0;
    if has_lead {
        c.lead = narrow(lead);
        nodes.push(node("lead", c.lead, area, Piece::Lead(v.lead), th));
    }
    if roomy {
        slot(2);
    }
    let fork = narrow(slot(fork_h));
    let has_fork = fork.height > 0;
    if has_fork {
        nodes.push(node("fork", fork, area, Piece::Fork { v: v.fork, footer: !compact, lead: lead_label }, th));
    }
    if roomy {
        let r = slot(1);
        if r.height > 0 {
            widgets::delegate_label(buf, r, v.delegate, th);
        }
    }
    if roomy {
        slot(2);
    }
    let cards_h = area.bottom().saturating_sub(y.get()).min(8);
    let cards_area = slot(cards_h);
    if cards_area.height > 0 {
        let all = as_list(v.cards);
        c.cards = widgets::card_rects(cards_area, all.len(), first_card);
        for (i, r) in &c.cards {
            nodes.push(node(&format!("card:{i}"), *r, area, Piece::Card(&all[*i]), th));
        }
        if let (Some((first, _)), Some((last, _))) = (c.cards.first(), c.cards.last()) {
            let card_h = cards_area.height.min(7);
            if cards_area.height > card_h {
                let row = Rect { y: cards_area.y + card_h, height: 1, ..cards_area };
                widgets::card_counts(buf, row, th, (*first, last + 1, all.len()));
            }
        }
    }
    let mut ret_id = None;
    if roomy {
        y.set(y.get().max(cards_area.y + cards_h + 2));
        let r = narrow(slot(3));
        if r.height > 0 {
            c.ret = r;
            nodes.push(node("return", r, area, Piece::Return(v.ret), th));
            ret_id = Some("return");
        }
    }

    let mut edges: Vec<Edge<StepEdge>> = Vec::new();
    if roomy && has_lead && has_fork {
        edges.push(Edge::new("lead-fork", "lead", "fork"));
    }
    if roomy && has_fork {
        for (i, _) in &c.cards {
            edges.push(Edge::new(format!("fork-card:{i}"), "fork", format!("card:{i}")));
        }
    }
    if let (Some(ret), Some((first, _))) = (ret_id, c.cards.first()) {
        edges.push(Edge::new("cards-return", format!("card:{first}"), ret));
    }
    let Ok(flow) = Flow::with_graph(nodes, edges) else { return c };
    let mut flow = flow.with_locked(true).with_theme(FlowTheme::Custom(palette(th)));
    (&mut flow).render(area, buf);
    c
}
