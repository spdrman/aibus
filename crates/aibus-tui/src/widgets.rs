//! Widgets: pure functions of one `view:` value and a theme. They have no access to the graph.

use aibus_core::state::value::{as_i64, as_list, as_str, get};
use aibus_graph::Value;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Widget};

use crate::layout::LEDGER_COLS;
use crate::theme::Theme;

const SPINNER: [&str; 4] = ["◐", "◓", "◑", "◒"];
const PARTIAL: [&str; 8] = [" ", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    get(v, key).and_then(as_str).unwrap_or("")
}

fn flag(v: &Value, key: &str) -> bool {
    matches!(get(v, key), Some(Value::Bool(true)))
}

fn real(v: &Value, key: &str) -> Option<f64> {
    match get(v, key)? {
        Value::F64(x) => Some(*x),
        Value::I64(x) => Some(*x as f64),
        _ => None,
    }
}

/// `1314` as `1,314`.
fn thousands(x: i64) -> String {
    let digits = x.abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if x < 0 {
        out.insert(0, '-');
    }
    out
}

/// Writes `s` at column `x` of row `y` inside `area`, clipped to it. Returns the next column.
pub(crate) fn put(buf: &mut Buffer, area: Rect, x: u16, y: u16, s: &str, style: Style) -> u16 {
    if y >= area.height || x >= area.width {
        return x;
    }
    let (px, _) = buf.set_stringn(
        area.x + x,
        area.y + y,
        s,
        usize::from(area.width - x),
        style,
    );
    px - area.x
}

/// `s` against the right edge of `area`.
fn put_right(buf: &mut Buffer, area: Rect, y: u16, s: &str, style: Style) {
    let w = u16::try_from(s.chars().count()).unwrap_or(u16::MAX);
    put(buf, area, area.width.saturating_sub(w), y, s, style);
}


pub(crate) fn fill(buf: &mut Buffer, area: Rect, bg: Color) {
    buf.set_style(area, Style::new().bg(bg));
}

/// A panel with a coloured border and a title in the top border; returns the inside.
pub(crate) fn panel(buf: &mut Buffer, area: Rect, title: &str, border: Color, th: &Theme) -> Rect {
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(border))
        .title(format!(" {title} "))
        .title_style(Style::new().fg(border).add_modifier(Modifier::BOLD))
        .style(Style::new().bg(th.panel).fg(th.fg));
    let inner = block.inner(area);
    block.render(area, buf);
    inner
}

/// Ten cells, `p` of them filled, with eighth-block resolution.
fn bar(p: f64, cells: usize) -> (String, usize) {
    let eighths = (p.clamp(0.0, 1.0) * cells as f64 * 8.0).round() as usize;
    let (full, part) = (eighths / 8, eighths % 8);
    let mut s = "█".repeat(full);
    if part > 0 {
        s.push_str(PARTIAL[part]);
    }
    let used = full + usize::from(part > 0);
    (s, cells - used)
}

fn role_colour(role: &str, th: &Theme) -> Color {
    match role {
        "lead" => th.lead,
        "swarm" => th.swarm,
        "router" => th.router,
        "advisor" => th.advisor,
        _ => th.fg,
    }
}

/// Two rows: the three role claims, then a rule. Then the legend row below it.
pub fn header(buf: &mut Buffer, area: Rect, v: &Value, th: &Theme) {
    fill(buf, area, th.bg);
    let bold = Modifier::BOLD;
    let lead = text(v, "lead_label").to_uppercase();
    let swarm = text(v, "swarm_label").to_uppercase();
    let mut parts = vec![("AIBUS".to_owned(), Style::new().fg(th.fg).bg(th.highlight).add_modifier(bold))];
    if !lead.is_empty() {
        parts.push((format!("{lead} LEADS"), Style::new().fg(th.lead).add_modifier(bold)));
    }
    if !swarm.is_empty() {
        parts.push((format!("{swarm} SWARMS"), Style::new().fg(th.swarm).add_modifier(bold)));
    }
    let width: usize = parts.iter().map(|(t, _)| t.chars().count() + 7).sum::<usize>() - 7;
    let mut x = u16::try_from(usize::from(area.width).saturating_sub(width) / 2).unwrap_or(0);
    for (i, (t, style)) in parts.iter().enumerate() {
        if i > 0 {
            x = put(buf, area, x, 0, "   ·   ", Style::new().fg(th.dim_text));
        }
        x = put(buf, area, x, 0, t, *style);
    }
    put(buf, area, 0, 1, &"─".repeat(usize::from(area.width)), Style::new().fg(th.dim));
}

pub fn legend(buf: &mut Buffer, area: Rect, v: &Value, th: &Theme) {
    fill(buf, area, th.bg);
    let mut x = 2;
    for e in as_list(get(v, "legend").unwrap_or(&Value::Null)) {
        let colour = role_colour(text(e, "role"), th);
        x = put(buf, area, x, 0, "■ ", Style::new().fg(colour));
        x = put(buf, area, x, 0, text(e, "text"), Style::new().fg(th.fg));
        x += 3;
    }
}

/// Full rail at 22 columns, or the three-diamond strip when `area` is narrower. Returns where
/// the three checkpoints were drawn (a click on one opens its advice).
pub fn rail(buf: &mut Buffer, area: Rect, v: &Value, th: &Theme) -> Vec<Rect> {
    let marks = as_list(get(v, "checkpoints").unwrap_or(&Value::Null));
    let diamond = |m: &Value| if flag(m, "active") { th.g("◆") } else { th.g("◇") };
    let mut hits = Vec::new();
    if area.width < 22 {
        fill(buf, area, th.bg);
        for (i, m) in marks.iter().enumerate() {
            let colour = if flag(m, "active") { th.advisor } else { th.dim_text };
            put(buf, area, 1, i as u16 * 2, diamond(m), Style::new().fg(colour));
            hits.push(Rect { y: area.y + i as u16 * 2, height: 1, ..area }.intersection(area));
        }
        return hits;
    }
    let inner = panel(buf, area, text(v, "title"), th.advisor, th);
    let inner = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    let dim = Style::new().fg(th.dim_text);
    put(buf, inner, 0, 0, text(v, "subtitle"), dim);
    let mut y = 2;
    for m in marks {
        let style = if flag(m, "active") {
            Style::new().fg(th.advisor).bg(th.highlight)
        } else {
            Style::new().fg(th.advisor)
        };
        let row = Rect { y: inner.y + y, height: 1, ..inner };
        if y < inner.height {
            fill(buf, row, if flag(m, "active") { th.highlight } else { th.panel });
            hits.push(row);
        }
        put(buf, inner, 0, y, &format!("{} {}", diamond(m), text(m, "name")), style);
        y += 1;
        if text(m, "name") == "before a plan" {
            put(buf, inner, 1, y, "reads the whole", dim);
            put(buf, inner, 1, y + 1, "session, every", dim);
            put(buf, inner, 1, y + 2, "tool call, result", dim);
            put(buf, inner, 0, y + 3, "last advice:", Style::new().fg(th.fg));
            y += 4;
            for a in as_list(get(v, "advice").unwrap_or(&Value::Null)) {
                put(buf, inner, 0, y, &format!("{} {}", th.g("»"), as_str(a).unwrap_or("")), Style::new().fg(th.advisor));
                y += 1;
            }
            let calls = real(v, "calls").unwrap_or(0.0) as i64;
            let read = get(v, "tokens_read").and_then(as_i64).unwrap_or(0);
            put(buf, inner, 0, y, "calls", dim);
            put_right(buf, inner, y, &calls.to_string(), Style::new().fg(th.fg));
            put(buf, inner, 0, y + 1, "tokens read", dim);
            put_right(buf, inner, y + 1, &format!("{}k", read / 1000), Style::new().fg(th.fg));
            put(buf, inner, 2, y + 2, "silent on every", dim);
            put(buf, inner, 2, y + 3, "routine turn", dim);
            y += 5;
        }
    }
    hits
}

pub fn lead_box(buf: &mut Buffer, area: Rect, v: &Value, th: &Theme) {
    if matches!(v, Value::Null) {
        return;
    }
    let border = if flag(v, "highlight") { th.advisor } else { th.lead };
    let inner = panel(buf, area, &format!("{} · main session", text(v, "model_label")), border, th);
    put(buf, inner, 0, 0, text(v, "label"), Style::new().fg(th.fg));
    let effort = text(v, "effort");
    let blocks = match effort {
        "low" => 1,
        "medium" => 2,
        _ => 4,
    };
    let meter = format!("{}{}", "▮".repeat(blocks), if effort == "max" { "+" } else { "" });
    let x = put(buf, inner, 0, 1, "effort ", Style::new().fg(th.dim_text));
    let x = put(buf, inner, x, 1, &meter, Style::new().fg(th.lead));
    put(buf, inner, x + 1, 1, effort, Style::new().fg(th.fg));
    let flag_text = text(v, "model_flag");
    if !flag_text.is_empty() {
        put_right(buf, inner, 1, flag_text, Style::new().fg(th.dim_text));
    }
}

/// `footer` is false on a compact terminal (under 30 rows).
pub fn fork_layer(buf: &mut Buffer, area: Rect, v: &Value, th: &Theme, footer: bool) {
    let inner = panel(buf, area, text(v, "title"), th.router, th);
    let forks = format!("forks {}", thousands(get(v, "forks").and_then(as_i64).unwrap_or(0)));
    put_right(buf, Rect { y: area.y, height: 1, x: area.x, width: area.width.saturating_sub(2) }, 0, &forks, Style::new().fg(th.router).add_modifier(Modifier::BOLD));
    for (i, row) in as_list(get(v, "rows").unwrap_or(&Value::Null)).iter().enumerate() {
        let y = i as u16;
        put(buf, inner, 0, y, text(row, "label"), Style::new().fg(th.fg));
        let split = text(row, "verdict") == "split";
        let (fg, rest) = if split { (th.split, th.split_rest) } else { (th.router, th.panel) };
        let Some(p) = real(row, "p") else {
            put(buf, inner, 15, y, "···", Style::new().fg(th.dim_text));
            continue;
        };
        let (filled, empty) = bar(p, 10);
        let x = put(buf, inner, 15, y, &filled, Style::new().fg(fg));
        let x = put(buf, inner, x, y, &"█".repeat(empty), Style::new().fg(rest));
        let x = put(buf, inner, x + 1, y, &format!("{p:.2}"), Style::new().fg(th.fg));
        put(buf, inner, x + 1, y, text(row, "verdict"), Style::new().fg(fg));
    }
    let foot = 3;
    if footer && inner.height > foot {
        let any = flag(v, "any_split");
        let arrow = th.g("→");
        let x = put(buf, inner, 0, foot, &format!("sharp {arrow} runs in code   "), Style::new().fg(th.dim_text));
        let style = if any { Style::new().fg(th.split) } else { Style::new().fg(th.dim_text) };
        put(buf, inner, x, foot, &format!("split {arrow} lead"), style);
    }
}

/// Up to three cards, centred, starting at card `start`. Cards before or after the window are
/// counted at the edges. Returns each drawn card's index in the view and where it is.
pub fn cards(buf: &mut Buffer, area: Rect, v: &Value, th: &Theme, start: usize) -> Vec<(usize, Rect)> {
    const W: u16 = 19;
    let all = as_list(v);
    let start = start.min(all.len().saturating_sub(3));
    let end = (start + 3).min(all.len());
    let shown = &all[start..end];
    let mut out = Vec::new();
    if shown.is_empty() {
        return out;
    }
    let total = shown.len() as u16 * W + (shown.len() as u16 - 1);
    let x0 = area.x + area.width.saturating_sub(total) / 2;
    for (i, c) in shown.iter().enumerate() {
        let rect = Rect { x: x0 + i as u16 * (W + 1), width: W, ..area }.intersection(area);
        out.push((start + i, rect));
        let border = if text(c, "border") == "advisor" { th.advisor } else { th.swarm };
        let name = if flag(c, "selected") {
            format!("{} {}", th.g("▸"), text(c, "name"))
        } else {
            text(c, "name").to_owned()
        };
        let block_inner = panel(buf, rect, &name, border, th);
        put(buf, block_inner, 0, 0, text(c, "model_label"), Style::new().fg(th.swarm));
        let state = text(c, "status");
        match real(c, "progress") {
            Some(p) => {
                let (filled, empty) = bar(p, 8);
                let x = put(buf, block_inner, 0, 1, &filled, Style::new().fg(th.swarm));
                let x = put(buf, block_inner, x, 1, &" ".repeat(empty), Style::new());
                put(buf, block_inner, x + 1, 1, &format!("{:.0}%", p * 100.0), Style::new().fg(th.fg));
            }
            None => {
                put(buf, block_inner, 0, 1, "···········", Style::new().fg(th.dim_text));
            }
        }
        let (glyph, colour, word) = match state {
            "done" => (th.g("✓").to_owned(), th.ok, "done"),
            "failed" => (th.g("✗").to_owned(), th.error, "failed"),
            "queued" => (th.g("◌").to_owned(), th.dim_text, "queued"),
            _ => {
                let phase = get(c, "spinner_phase").and_then(as_i64).unwrap_or(0).rem_euclid(4) as usize;
                (SPINNER[phase].to_owned(), th.swarm, "running")
            }
        };
        put(buf, block_inner, 0, 2, &format!("{glyph} {word}"), Style::new().fg(colour));
        let label = text(c, "label");
        let cut = if label.chars().count() <= usize::from(block_inner.width) {
            label.len()
        } else {
            label[..label.char_indices().nth(usize::from(block_inner.width)).map_or(label.len(), |(i, _)| i)]
                .rfind(' ')
                .unwrap_or(label.len())
        };
        put(buf, block_inner, 0, 3, label[..cut].trim(), Style::new().fg(th.dim_text));
        put(buf, block_inner, 0, 4, label[cut..].trim(), Style::new().fg(th.dim_text));
    }
    let dim = Style::new().fg(th.dim_text);
    let last = area.height.saturating_sub(1);
    if start > 0 {
        put(buf, area, 0, last, &format!("{} {start} more", th.g("◂")), dim);
    }
    if end < all.len() {
        put_right(buf, area, last, &format!("{} more {}", all.len() - end, th.g("▸")), dim);
    }
    out
}

/// The label above the cards: `delegate to subagents · <model>`.
pub fn delegate_label(buf: &mut Buffer, area: Rect, v: &Value, th: &Theme) {
    let model = as_list(v).first().map_or("", |c| text(c, "model_label"));
    if model.is_empty() {
        return;
    }
    let x = area.width.saturating_sub(24 + model.chars().count() as u16) / 2;
    let x = put(buf, area, x, 0, "delegate to subagents · ", Style::new().fg(th.dim_text));
    put(buf, area, x, 0, model, Style::new().fg(th.swarm));
}

pub fn return_box(buf: &mut Buffer, area: Rect, v: &Value, th: &Theme) {
    let border = if flag(v, "highlight") { th.advisor } else { th.lead };
    let inner = panel(buf, area, &format!("back to {} · lead", text(v, "lead_model_label")), border, th);
    put(buf, inner, 0, 0, "review + verify", Style::new().fg(th.fg));
}

/// What `session_log` drew, for the hit map and the scrollbar.
pub struct LogGeom {
    pub inner: Rect,
    /// Index in the view of the first row drawn.
    pub first: usize,
    pub shown: usize,
    pub total: usize,
}

/// The log box. `scroll` is how many rows the view is held back from the newest.
pub fn session_log(buf: &mut Buffer, area: Rect, v: &Value, th: &Theme, scroll: usize) -> LogGeom {
    let inner = panel(buf, area, "session log", th.dim_text, th);
    let lines = as_list(v);
    let h = usize::from(inner.height);
    let scroll = scroll.min(lines.len().saturating_sub(h));
    let end = lines.len() - scroll;
    let first = end.saturating_sub(h);
    for (y, line) in lines[first..end].iter().enumerate() {
        let y = y as u16;
        put(buf, inner, 0, y, text(line, "time"), Style::new().fg(th.dim_text));
        let actor = text(line, "actor");
        let colour = match actor {
            "error" => th.error,
            "warn" => th.warn,
            _ => th.fg,
        };
        let x = put(buf, inner, 10, y, &format!("{actor:<8}"), Style::new().fg(colour));
        put(buf, inner, x + 1, y, text(line, "text"), Style::new().fg(th.fg));
    }
    LogGeom { inner, first, shown: end - first, total: lines.len() }
}

/// The prompt line: `<cwd> $ ` and what the user has typed, or the dim placeholder while empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    pub cwd: String,
    pub text: String,
    pub placeholder: String,
}

impl Default for Prompt {
    fn default() -> Self {
        Self {
            cwd: "~/app".into(),
            text: String::new(),
            placeholder: "aibus --model sonnet --advisor opus".into(),
        }
    }
}

pub fn prompt(buf: &mut Buffer, area: Rect, th: &Theme, p: &Prompt, focused: bool) {
    fill(buf, area, th.bg);
    let lead = if focused { th.focused() } else { Style::new().fg(th.fg) };
    let x = put(buf, area, 1, 0, &format!("{} $ ", p.cwd), lead);
    let x = if p.text.is_empty() {
        put(buf, area, x, 0, &format!("{} ", p.placeholder), Style::new().fg(th.dim_text))
    } else {
        // The tail, so the cursor stays on screen.
        let room = usize::from(area.width.saturating_sub(x + 2));
        let skip = p.text.chars().count().saturating_sub(room);
        let shown: String = p.text.chars().skip(skip).collect();
        put(buf, area, x, 0, &shown, Style::new().fg(th.fg))
    };
    put(buf, area, x, 0, th.g("▌"), Style::new().fg(th.router));
}

pub fn status_bar(buf: &mut Buffer, area: Rect, v: &Value, th: &Theme) {
    fill(buf, area, th.bg);
    let sw = get(v, "swarm").unwrap_or(&Value::Null);
    let sub = get(v, "subagents").unwrap_or(&Value::Null);
    let adv = get(v, "advisor").unwrap_or(&Value::Null);
    let rt = get(v, "router").unwrap_or(&Value::Null);
    let num = |x: &Value, k: &str| get(x, k).and_then(as_i64).unwrap_or(0);
    let segs = [
        (format!("{} [{}]", text(sw, "label").to_lowercase(), text(sw, "state")), th.swarm),
        (format!("subagents [{}/{}]", num(sub, "running"), num(sub, "total")), th.fg),
        (format!("{} [{}]", text(adv, "label"), text(adv, "state")), th.advisor),
        (format!("{} [ {} forks]", text(rt, "label"), thousands(num(rt, "forks"))), th.router),
    ];
    let (mut x, gap) = if area.width >= 98 { (2, 6) } else { (1, 2) };
    for (s, colour) in &segs {
        x = put(buf, area, x, 0, s, Style::new().fg(*colour)) + gap;
    }
    let alert = as_list(get(v, "alerts").unwrap_or(&Value::Null)).first().and_then(as_str);
    if let Some(a) = alert {
        put_right(buf, area, 0, a, Style::new().fg(th.warn));
    } else if area.width >= LEDGER_COLS {
        if let Some(l) = get(v, "ledger").filter(|l| !matches!(l, Value::Null)) {
            put_right(buf, area, 0, &format!("ledger: {} · now: {}", text(l, "goal"), text(l, "now")), Style::new().fg(th.dim_text));
        }
    }
}
