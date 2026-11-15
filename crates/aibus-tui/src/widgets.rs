//! Widgets: pure functions of one `view:` value and a theme. They have no access to the graph.

use aibus_core::state::value::{as_i64, as_list, as_str, get};
use aibus_graph::Value;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, Widget};

use crate::hit::Segment;
use crate::layout::LEDGER_COLS;
use crate::theme::{Interact, Theme};

const SPINNER: [&str; 4] = ["◐", "◓", "◑", "◒"];
const PARTIAL: [&str; 8] = [" ", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];

pub(crate) fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    get(v, key).and_then(as_str).unwrap_or("")
}

pub(crate) fn flag(v: &Value, key: &str) -> bool {
    matches!(get(v, key), Some(Value::Bool(true)))
}

pub(crate) fn real(v: &Value, key: &str) -> Option<f64> {
    match get(v, key)? {
        Value::F64(x) => Some(*x),
        Value::I64(x) => Some(*x as f64),
        _ => None,
    }
}

pub(crate) fn int(v: &Value, key: &str) -> i64 {
    get(v, key).and_then(as_i64).unwrap_or(0)
}

/// `1314` as `1,314`.
fn thousands(x: i64) -> String {
    let digits = x.abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if x < 0 {
        out.insert(0, '-');
    }
    out
}

/// `122000` as `122k`, `1020000` as `1020k`; under a thousand as it is.
fn kilo(x: i64) -> String {
    if x >= 1000 {
        format!("{}k", x / 1000)
    } else {
        x.to_string()
    }
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
pub(crate) fn put_right(buf: &mut Buffer, area: Rect, y: u16, s: &str, style: Style) {
    let w = u16::try_from(s.chars().count()).unwrap_or(u16::MAX);
    put(buf, area, area.width.saturating_sub(w), y, s, style);
}

/// `s` cut to `max` cells, with `…` where it was cut.
pub(crate) fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let keep: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{keep}…")
}

/// `s` broken at spaces into lines of at most `width` characters, with the character index each
/// line starts at (so a typewriter can reveal them in order).
fn wrap(s: &str, width: usize) -> Vec<(usize, String)> {
    let mut lines: Vec<(usize, String)> = Vec::new();
    let mut at = 0;
    for word in s.split(' ') {
        let w = word.chars().count();
        match lines.last_mut() {
            Some((_, line)) if line.chars().count() + 1 + w <= width => {
                line.push(' ');
                line.push_str(word);
            }
            _ => lines.push((at, word.to_owned())),
        }
        at += w + 1;
    }
    lines
}

pub(crate) fn fill(buf: &mut Buffer, area: Rect, bg: Color) {
    buf.set_style(area, Style::new().bg(bg));
}

/// A panel with a coloured border and a title in the top border; returns the inside.
pub(crate) fn panel(buf: &mut Buffer, area: Rect, title: &str, border: Color, th: &Theme) -> Rect {
    let mut block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(border))
        .style(Style::new().bg(th.panel).fg(th.fg));
    if !title.is_empty() {
        block = block
            .title(format!(" {title} "))
            .title_style(Style::new().fg(border).add_modifier(Modifier::BOLD));
    }
    let inner = block.inner(area);
    block.render(area, buf);
    inner
}

/// `panel` with one blank column on each side of the inside, as the recorded boxes have.
pub(crate) fn padded(buf: &mut Buffer, area: Rect, title: &str, border: Color, th: &Theme) -> Rect {
    let inner = panel(buf, area, title, border, th);
    Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner }
}

/// `cells` cells, `p` of them filled, with eighth-block resolution; returns the glyphs and how
/// many cells are left over.
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
        "error" => th.error,
        "warn" => th.warn,
        "dim" => th.dim_text,
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
    let off = text(v, "status") == "off";
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
    // Padded on the left only: the advice needs the 17 columns of `» plan skips auth`.
    let inner = panel(buf, area, text(v, "title"), if off { th.dim_text } else { th.advisor }, th);
    let inner = Rect { x: inner.x + 1, width: inner.width.saturating_sub(1), ..inner };
    let dim = Style::new().fg(th.dim_text);
    put(buf, inner, 0, 0, text(v, "subtitle"), dim);
    let mut y = 2u16;
    let timeline = |buf: &mut Buffer, y: u16, s: &str, style: Style| {
        put(buf, inner, 0, y, "│", Style::new().fg(th.dim));
        put(buf, inner, 2, y, s, style);
    };
    let body = inner.width.saturating_sub(2) as usize;
    for m in marks {
        let active = flag(m, "active");
        let style = if off {
            dim
        } else if active {
            Style::new().fg(th.advisor).bg(th.highlight).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(th.advisor)
        };
        let row = Rect { y: inner.y + y, height: 1, ..inner };
        if y < inner.height {
            fill(buf, row, if active { th.highlight } else { th.panel });
            hits.push(row);
        }
        put(buf, inner, 0, y, &format!("{} {}", diamond(m), text(m, "name")), style);
        y += 1;
        if off {
            continue;
        }
        match text(m, "name") {
            "before a plan" => {
                for line in ["reads the whole", "session, every", "tool call and", "every result"] {
                    timeline(buf, y, line, dim);
                    y += 1;
                }
                timeline(buf, y, "last advice:", Style::new().fg(th.fg));
                y += 1;
                let advice = as_list(get(v, "advice").unwrap_or(&Value::Null));
                let typed = usize::try_from(int(v, "typing_cursor")).unwrap_or(0);
                for (i, a) in advice.iter().enumerate() {
                    let newest = i + 1 == advice.len();
                    let full = as_str(a).unwrap_or("");
                    // The newest advice is typed out one character per tick.
                    let shown: String = if newest { full.chars().take(typed).collect() } else { full.to_owned() };
                    let lines = wrap(full, body.saturating_sub(2));
                    let mut any = false;
                    for (j, (start, line)) in lines.iter().enumerate() {
                        let n = line.chars().count();
                        let seen: String = shown.chars().skip(*start).take(n).collect();
                        if j > 0 && seen.is_empty() {
                            break;
                        }
                        let lead = if j == 0 { th.g("»") } else { " " };
                        timeline(buf, y, &format!("{lead} {seen}"), Style::new().fg(th.advisor));
                        y += 1;
                        any = true;
                    }
                    if !any {
                        timeline(buf, y, th.g("»"), Style::new().fg(th.advisor));
                        y += 1;
                    }
                }
                timeline(buf, y, "calls", dim);
                put_right(buf, inner, y, &int(v, "calls").to_string(), Style::new().fg(th.fg));
                y += 1;
                timeline(buf, y, "tokens read", dim);
                put_right(buf, inner, y, &kilo(int(v, "tokens_read")), Style::new().fg(th.fg));
                y += 1;
                for line in ["silent on every", "routine turn"] {
                    timeline(buf, y, line, dim);
                    y += 1;
                }
            }
            "same error x2" => {
                for line in ["never writes", "code itself.", "Sonnet applies", "the advice."] {
                    timeline(buf, y, line, dim);
                    y += 1;
                }
            }
            _ => {}
        }
    }
    hits
}

pub fn lead_box(buf: &mut Buffer, area: Rect, v: &Value, th: &Theme) {
    if matches!(v, Value::Null) {
        return;
    }
    let border = if flag(v, "highlight") { th.advisor } else { th.lead };
    let inner = padded(buf, area, &format!("{} · main session", text(v, "model_label")), border, th);
    put(buf, inner, 0, 0, text(v, "label"), Style::new().fg(th.fg));
    let effort = text(v, "effort");
    // Four slots: low 1, medium 2, high 4, max 4 and a `+`.
    let filled = match effort {
        "low" => 1,
        "medium" => 2,
        _ => 4,
    };
    let x = put(buf, inner, 0, 1, "effort ", Style::new().fg(th.dim_text));
    let x = put(buf, inner, x, 1, &th.g("▮").repeat(filled), Style::new().fg(th.lead));
    let x = put(buf, inner, x, 1, &th.g("·").repeat(4 - filled), Style::new().fg(th.dim_text));
    let x = if effort == "max" { put(buf, inner, x, 1, "+", Style::new().fg(th.lead)) } else { x };
    let x = put(buf, inner, x + 1, 1, effort, Style::new().fg(th.fg));
    let flag_text = text(v, "model_flag");
    // The flag gets what is left of the row after the effort text and one cell of gap.
    let room = usize::from(inner.width.saturating_sub(x + 1));
    if !flag_text.is_empty() && room > 0 {
        put_right(buf, inner, 1, &clip(flag_text, room), Style::new().fg(th.dim_text));
    }
}

/// A router bar: `round(p * 10)` cells with a `▌` half cell, the rest in `rest`.
fn fork_bar(p: f64, fg: Color, rest: Option<Color>, th: &Theme) -> Vec<(String, Style)> {
    let halves = (p.clamp(0.0, 1.0) * 20.0).round() as usize;
    let (full, half) = (halves / 2, halves % 2);
    let left = 10 - full - half;
    let mut out = vec![(th.g("█").repeat(full), Style::new().fg(fg))];
    if half == 1 {
        let style = Style::new().fg(fg);
        out.push((th.g("▌").to_owned(), rest.map_or(style, |r| style.bg(r))));
    }
    out.push(match rest {
        Some(r) => (th.g("█").repeat(left), Style::new().fg(r)),
        None => (" ".repeat(left), Style::new()),
    });
    out
}

/// `footer` is false on a compact terminal (under 30 rows). `lead` is what a split goes to.
pub fn fork_layer(buf: &mut Buffer, area: Rect, v: &Value, th: &Theme, footer: bool, lead: &str) {
    if !flag(v, "enabled") {
        let inner = padded(buf, area, "router · off", th.dim_text, th);
        put(buf, inner, 0, 0, "not running", Style::new().fg(th.dim_text));
        return;
    }
    let inner = padded(buf, area, text(v, "title"), th.router, th);
    let forks = format!("forks {}", thousands(int(v, "forks")));
    put_right(buf, Rect { y: area.y, height: 1, x: area.x, width: area.width.saturating_sub(2) }, 0, &forks, Style::new().fg(th.router).add_modifier(Modifier::BOLD));
    for (i, row) in as_list(get(v, "rows").unwrap_or(&Value::Null)).iter().enumerate() {
        let y = i as u16;
        put(buf, inner, 0, y, text(row, "label"), Style::new().fg(th.fg));
        let split = text(row, "verdict") == "split";
        let (fg, rest) = if split { (th.split, Some(th.split_rest)) } else { (th.router, None) };
        let Some(p) = real(row, "p") else {
            put(buf, inner, 15, y, "···", Style::new().fg(th.dim_text));
            continue;
        };
        let mut x = 15;
        for (s, style) in fork_bar(p, fg, rest, th) {
            x = put(buf, inner, x, y, &s, style);
        }
        let x = put(buf, inner, x + 1, y, &format!("{p:.2}"), Style::new().fg(th.fg));
        put(buf, inner, x + 1, y, text(row, "verdict"), Style::new().fg(fg));
    }
    let foot = 3;
    if footer && inner.height > foot {
        let any = flag(v, "any_split");
        let arrow = th.g("→");
        let x = put(buf, inner, 0, foot, &format!("sharp {arrow} runs in code  "), Style::new().fg(th.dim_text));
        let style = if any {
            Style::new().fg(th.split).bg(th.highlight).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(th.dim_text)
        };
        let lead = lead.split_whitespace().next().unwrap_or("");
        put(buf, inner, x, foot, &format!("split {arrow} {lead}"), style);
    }
}

/// Rows of a card's inside: model, bar, two label lines, status.
pub const CARD_W: u16 = 19;

/// Where up to three cards go, centred in `area`, starting at card `start` (clamped so the
/// window stays full). Each is the card's index in the view and its rect.
pub fn card_rects(area: Rect, len: usize, start: usize) -> Vec<(usize, Rect)> {
    let start = start.min(len.saturating_sub(3));
    let end = (start + 3).min(len);
    let shown = end - start;
    if shown == 0 {
        return Vec::new();
    }
    let total = shown as u16 * CARD_W + (shown as u16 - 1);
    let x0 = area.x + area.width.saturating_sub(total) / 2;
    let card_h = area.height.min(7);
    (0..shown)
        .map(|i| {
            let rect = Rect { x: x0 + i as u16 * (CARD_W + 1), width: CARD_W, height: card_h, ..area }.intersection(area);
            (start + i, rect)
        })
        .collect()
}

/// One worker card inside `rect`.
pub fn card(buf: &mut Buffer, rect: Rect, c: &Value, th: &Theme) {
    let border = if text(c, "border") == "advisor" { th.advisor } else { th.swarm };
    let name = if flag(c, "selected") {
        format!("{} {}", th.g("▸"), text(c, "name"))
    } else {
        text(c, "name").to_owned()
    };
    let inner = padded(buf, rect, &name, border, th);
    put(buf, inner, 0, 0, text(c, "model_label"), Style::new().fg(th.swarm));
    let progress = get(c, "progress").unwrap_or(&Value::Null);
    let blue = Style::new().fg(th.swarm);
    match text(progress, "kind") {
        "indeterminate" => {
            // A 2-cell block bouncing along the 10-cell bar.
            let at = usize::try_from(int(progress, "value").clamp(0, 8)).unwrap_or(0);
            put(buf, inner, at as u16, 1, &th.g("█").repeat(2), blue);
        }
        _ => {
            let p = real(progress, "value").unwrap_or(0.0);
            let (filled, _) = bar(p, 10);
            let x = put(buf, inner, 0, 1, &filled, blue);
            let pad = 10u16.saturating_sub(x);
            put(buf, inner, x + pad + 1, 1, &format!("{:.0}%", p * 100.0), Style::new().fg(th.fg));
        }
    }
    let label = wrap(text(c, "label"), usize::from(inner.width));
    for (j, (_, line)) in label.iter().take(2).enumerate() {
        let last = j == 1 && label.len() > 2;
        let line = if last { clip(&format!("{line} …"), usize::from(inner.width)) } else { line.clone() };
        put(buf, inner, 0, 2 + j as u16, &line, Style::new().fg(th.dim_text));
    }
    let (glyph, colour, word) = match text(c, "status") {
        "done" => (th.g("✓").to_owned(), th.ok, "done"),
        "failed" => (th.g("✗").to_owned(), th.error, "failed"),
        "queued" => (th.g("◌").to_owned(), th.dim_text, "queued"),
        _ => {
            let phase = int(c, "spinner_phase").rem_euclid(4) as usize;
            (th.g(SPINNER[phase]).to_owned(), th.swarm, "running")
        }
    };
    put(buf, inner, 0, 4, &format!("{glyph} {word}"), Style::new().fg(colour));
}

/// The row under the cards: how many are before the window and how many after it. `window` is
/// the first card shown, the end of the shown ones and the total.
pub fn card_counts(buf: &mut Buffer, row: Rect, th: &Theme, (start, end, total): (usize, usize, usize)) {
    let dim = Style::new().fg(th.dim_text);
    if start > 0 {
        put(buf, row, 0, 0, &format!("{} {start} more", th.g("◂")), dim);
    }
    if end < total {
        put_right(buf, row, 0, &format!("{} more {}", total - end, th.g("▸")), dim);
    }
}

/// The label above the cards: `delegate to subagents · <model>`.
pub fn delegate_label(buf: &mut Buffer, area: Rect, v: &Value, th: &Theme) {
    let model = text(v, "swarm_model_label");
    if model.is_empty() {
        return;
    }
    let x = area.width.saturating_sub(24 + model.chars().count() as u16) / 2;
    let x = put(buf, area, x, 0, "delegate to subagents · ", Style::new().fg(th.dim_text));
    put(buf, area, x, 0, model, Style::new().fg(th.swarm));
}

pub fn return_box(buf: &mut Buffer, area: Rect, v: &Value, th: &Theme) {
    let border = if flag(v, "highlight") { th.advisor } else { th.lead };
    let inner = padded(buf, area, &format!("back to {} · lead", text(v, "lead_model_label")), border, th);
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

/// The log box. `scroll` is how many rows the view is held back from the newest. `filter` is the
/// text after `/` (with a cursor while `editing`); `follow` is false while the log holds its
/// place. `lead` is what a split goes to.
pub fn session_log(
    buf: &mut Buffer,
    area: Rect,
    v: &Value,
    th: &Theme,
    scroll: usize,
    (filter, editing, follow): (&str, bool, bool),
    lead: &str,
) -> LogGeom {
    let mut title = String::from("session log");
    if editing || !filter.is_empty() {
        title.push_str(&format!(" · /{filter}{}", if editing { th.g("▌") } else { "" }));
    }
    if !follow {
        title.push_str(" · paused");
    }
    let inner = padded(buf, area, &title, th.dim_text, th);
    let lines = as_list(v);
    let h = usize::from(inner.height);
    let scroll = scroll.min(lines.len().saturating_sub(h));
    let end = lines.len() - scroll;
    let first = end.saturating_sub(h);
    // Room for the actor column, and for the router columns on a router row.
    let width = usize::from(inner.width).saturating_sub(1);
    for (y, line) in lines[first..end].iter().enumerate() {
        let y = y as u16;
        put(buf, inner, 0, y, text(line, "time"), Style::new().fg(th.dim_text));
        // What a client of `aibus mcp` caused wears the `mcp` badge in place of the actor (#274).
        if text(line, "via").is_empty() {
            let colour = role_colour(text(line, "actor_colour"), th);
            put(buf, inner, 10, y, &clip(text(line, "actor"), 10), Style::new().fg(colour));
        } else {
            put(buf, inner, 10, y, "mcp", badge(th));
        }
        let p = real(line, "p");
        let verdict = text(line, "verdict");
        let (cols, split) = match (p, verdict) {
            (Some(p), v) if !v.is_empty() => {
                let word = if v == "split" { format!("split {} {}", th.g("→"), lead.to_lowercase()) } else { format!("sharp {} code", th.g("→")) };
                (Some((format!("p={p:.2}"), word)), v == "split")
            }
            _ => (None, false),
        };
        let reserved = cols.as_ref().map_or(0, |(a, b)| a.chars().count() + b.chars().count() + 4);
        let room = width.saturating_sub(21 + reserved);
        put(buf, inner, 21, y, &clip(text(line, "text"), room), Style::new().fg(th.fg));
        if let Some((p, word)) = cols {
            let style = Style::new().fg(if split { th.split } else { th.router });
            let wx = inner.width.saturating_sub(word.chars().count() as u16 + 1);
            put(buf, inner, wx, y, &word, style);
            put(buf, inner, wx.saturating_sub(p.chars().count() as u16 + 2), y, &p, Style::new().fg(th.fg));
        }
    }
    LogGeom { inner, first, shown: end - first, total: lines.len() }
}

pub use crate::prompt::Prompt;

/// The prompt: `<cwd> $ ` then the lines of the text, or the dim placeholder until the first
/// submit. The block cursor blinks on the tick counter (30 ticks on, 30 off). Returns the rect
/// of each row drawn and the char index each starts at, for the hit map.
pub fn prompt(buf: &mut Buffer, area: Rect, th: &Theme, p: &Prompt, focused: bool, tick: i64) -> Vec<(Rect, usize)> {
    fill(buf, area, th.bg);
    let lead = if focused { th.focused() } else { Style::new().fg(th.fg) };
    let prefix = format!("{} $ ", p.cwd);
    let indent = prefix.chars().count() as u16 + 1;
    let lines = p.lines();
    let rows = usize::from(area.height).max(1);
    let (cursor_line, cursor_col) = p.pos(p.at());
    let first = (cursor_line + 1).saturating_sub(rows).min(lines.len().saturating_sub(rows));
    let on = focused && Prompt::blink_on(tick);
    let sel = p.selection();
    // `/login <provider> <anything>` is refused on Enter (the key belongs in the masked field),
    // but until then what follows the provider could be a key, so it is drawn as dots.
    let secret_from = p.text.strip_prefix("/login").filter(|r| r.starts_with(char::is_whitespace)).and_then(|r| {
        let rest = r.trim_start().trim_start_matches(|c: char| !c.is_whitespace()).trim_start();
        (!rest.is_empty()).then(|| p.text.chars().count() - rest.chars().count())
    });
    let mut out = Vec::new();
    let mut start = 0;
    for (i, line) in lines.iter().enumerate() {
        let len = line.chars().count();
        if i >= first && i < first + rows {
            let y = (i - first) as u16;
            let row = Rect { y: area.y + y, height: 1, ..area };
            out.push((row, start));
            if i == 0 {
                put(buf, area, 1, y, &prefix, lead);
            }
            let room = usize::from(area.width.saturating_sub(indent + 2));
            // A long line shows its tail around the cursor.
            let skip = if i == cursor_line { (cursor_col + 1).saturating_sub(room) } else { 0 };
            if p.shows_placeholder() {
                put(buf, area, indent, y, &format!("{} ", p.placeholder), Style::new().fg(th.dim_text));
            } else {
                let shown: String = line
                    .chars()
                    .enumerate()
                    .skip(skip)
                    .take(room)
                    .map(|(k, c)| if secret_from.is_some_and(|from| start + k >= from) { '•' } else { c })
                    .collect();
                put(buf, area, indent, y, &shown, Style::new().fg(th.fg));
                if let Some((a, b)) = sel {
                    for k in 0..shown.chars().count() {
                        let at = start + skip + k;
                        if at >= a && at < b {
                            buf.set_style(Rect { x: area.x + indent + k as u16, y: area.y + y, width: 1, height: 1 }, th.interact(Interact::Selected));
                        }
                    }
                }
            }
            if i == cursor_line && on {
                let col = if p.shows_placeholder() { 0 } else { cursor_col - skip };
                let x = indent + col as u16;
                let past_end = cursor_col >= len || p.shows_placeholder();
                if past_end {
                    let at = if p.shows_placeholder() { indent + p.placeholder.chars().count() as u16 + 1 } else { x };
                    put(buf, area, at, y, th.g("▌"), Style::new().fg(th.router));
                } else {
                    buf.set_style(Rect { x: area.x + x, y: area.y + y, width: 1, height: 1 }, Style::new().add_modifier(Modifier::REVERSED));
                }
            }
        }
        start += len + 1;
    }
    out
}

/// The one-line hint under the prompt for the first sessions.
pub fn hint_row(buf: &mut Buffer, area: Rect, th: &Theme, notice: Option<&str>) {
    fill(buf, area, th.bg);
    let hint = "Enter send  ·  Alt+Enter new line  ·  Tab complete  ·  / commands  ·  : palette  ·  ? help";
    let (text, colour) = match notice {
        Some(n) => (n, th.warn),
        None => (hint, th.dim_text),
    };
    put(buf, area, 1, 0, &clip(text, usize::from(area.width).saturating_sub(2)), Style::new().fg(colour));
}

/// The completion popup above the prompt: one row per candidate. Returns each row.
pub fn popup(buf: &mut Buffer, full: Rect, above: Rect, items: &[String], sel: usize, th: &Theme) -> (Rect, Vec<(usize, Rect)>) {
    let shown = items.len().min(8);
    let widest = items.iter().map(|i| i.chars().count()).max().unwrap_or(0);
    let w = (widest + 4).clamp(8, usize::from(full.width)) as u16;
    let h = shown as u16 + 2;
    let rect = Rect { x: above.x + 1, y: above.y.saturating_sub(h).max(full.y), width: w, height: h.min(above.y.saturating_sub(full.y)).max(1) };
    Clear.render(rect, buf);
    let inner = panel(buf, rect, "", th.dim_text, th);
    let first = sel.saturating_sub(shown.saturating_sub(1));
    let mut rows = Vec::new();
    for (k, item) in items.iter().skip(first).take(shown).enumerate() {
        let r = Rect { y: inner.y + k as u16, height: 1, ..inner };
        if r.bottom() > inner.bottom() {
            break;
        }
        if first + k == sel {
            buf.set_style(r, th.interact(Interact::Selected));
        }
        put(buf, r, 0, 0, &format!(" {item}"), Style::new().fg(th.fg));
        rows.push((first + k, r));
    }
    (rect, rows)
}

/// The `mcp` badge: what a client of `aibus mcp` did, and who is connected (#274).
fn badge(th: &Theme) -> Style {
    Style::new().fg(th.warn).add_modifier(Modifier::BOLD)
}

/// The status bar. Returns where each clickable segment went. `remote` is the client of the
/// control socket, while one is connected.
pub fn status_bar(buf: &mut Buffer, area: Rect, v: &Value, th: &Theme, remote: Option<&str>) -> Vec<(Segment, Rect)> {
    fill(buf, area, th.bg);
    let sw = get(v, "swarm").unwrap_or(&Value::Null);
    let sub = get(v, "subagents").unwrap_or(&Value::Null);
    let adv = get(v, "advisor").unwrap_or(&Value::Null);
    let rt = get(v, "router").unwrap_or(&Value::Null);
    let router = if flag(rt, "enabled") {
        format!("{} [ {} forks]", text(rt, "label"), thousands(int(rt, "forks")))
    } else {
        format!("{} · off", text(rt, "label"))
    };
    let segs = [
        (Segment::Swarm, format!("{} [{}]", text(sw, "label"), text(sw, "state")), th.swarm),
        (Segment::Subagents, format!("subagents [{}/{}]", int(sub, "running"), int(sub, "total")), th.fg),
        (Segment::Advisor, format!("{} [{}]", text(adv, "label"), text(adv, "state")), th.advisor),
        (Segment::Router, router, th.router),
    ];
    let mut hits = Vec::new();
    // Wide gaps as in the recording while the bar has nothing else to say; tighter ones once
    // the ledger needs the room.
    let alerting = !as_list(get(v, "alerts").unwrap_or(&Value::Null)).is_empty();
    let (mut x, gap) = match area.width {
        _ if alerting => (1, 2),
        w if w >= LEDGER_COLS => (2, 3),
        w if w >= 98 => (2, 6),
        _ => (1, 2),
    };
    let at = |x: u16, w: u16| Rect { x: area.x + x, y: area.y, width: w.min(area.width.saturating_sub(x)), height: 1 };
    for (seg, s, colour) in &segs {
        let w = s.chars().count() as u16;
        hits.push((*seg, at(x, w)));
        x = put(buf, area, x, 0, s, Style::new().fg(*colour)) + gap;
    }
    // The right side, from the edge in: memory, BYPASS, a permission alert, then the ledger in
    // what is left. `right` is where the next item ends; nothing may reach the segments at `x`.
    let mut right = area.width.saturating_sub(1);
    let place = |buf: &mut Buffer, right: &mut u16, s: &str, style: Style| -> Option<Rect> {
        let w = s.chars().count() as u16;
        if *right < x + w + 1 {
            return None;
        }
        *right -= w;
        put(buf, area, *right, 0, s, style);
        let rect = at(*right, w);
        *right = right.saturating_sub(2);
        Some(rect)
    };
    if let Some(client) = remote {
        place(buf, &mut right, &format!("mcp {client}"), badge(th));
    }
    if let Some(mem) = get(v, "mem").and_then(as_str) {
        place(buf, &mut right, mem, Style::new().fg(th.dim_text));
    }
    for a in as_list(get(v, "alerts").unwrap_or(&Value::Null)) {
        let (style, seg) = match text(a, "kind") {
            "bypass" => (Style::new().fg(th.error).add_modifier(Modifier::BOLD), None),
            _ => (Style::new().fg(th.warn), Some(Segment::Alert)),
        };
        if let (Some(rect), Some(seg)) = (place(buf, &mut right, text(a, "text"), style), seg) {
            hits.push((seg, rect));
        }
    }
    if area.width >= LEDGER_COLS {
        if let Some(l) = get(v, "ledger").filter(|l| !matches!(l, Value::Null)) {
            let full = format!("ledger: {} · now: {}", text(l, "goal"), text(l, "now"));
            let room = usize::from(right.saturating_sub(x));
            if room >= 12 {
                let shown = clip(&full, room - 1);
                if let Some(rect) = place(buf, &mut right, &shown, Style::new().fg(th.dim_text)) {
                    hits.push((Segment::Ledger, rect));
                }
            }
        }
    }
    hits
}

/// The context menu: a small bordered list at the pointer, clamped to the screen. `hover` and
/// `pressed` are the item indexes under the pointer. Returns the box and each item's row.
pub fn menu(
    buf: &mut Buffer,
    full: Rect,
    v: &Value,
    th: &Theme,
    (hover, pressed): (Option<usize>, Option<usize>),
) -> (Rect, Vec<Rect>) {
    let items = as_list(get(v, "items").unwrap_or(&Value::Null));
    let widest = items.iter().map(|i| text(i, "label").chars().count()).max().unwrap_or(0);
    let w = (widest + 6).max(text(v, "title").chars().count() + 6).min(usize::from(full.width)) as u16;
    let h = (items.len() as u16 + 2).min(full.height);
    let col = u16::try_from(int(v, "col")).unwrap_or(0);
    let row = u16::try_from(int(v, "row")).unwrap_or(0);
    let rect = Rect {
        x: col.min(full.right().saturating_sub(w)).max(full.x),
        y: row.min(full.bottom().saturating_sub(h)).max(full.y),
        width: w,
        height: h,
    };
    Clear.render(rect, buf);
    let inner = padded(buf, rect, text(v, "title"), th.focus, th);
    let selected = usize::try_from(int(v, "selected")).unwrap_or(0);
    let mut rows = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let r = Rect { y: inner.y + i as u16, height: 1, ..inner };
        if r.bottom() > inner.bottom() {
            break;
        }
        let enabled = flag(item, "enabled");
        let mut style = if enabled {
            Style::new().fg(th.fg)
        } else if th.mono {
            Style::new().add_modifier(Modifier::DIM)
        } else {
            Style::new().fg(th.dim_text)
        };
        let state = if pressed == Some(i) && enabled {
            Some(Interact::Pressed)
        } else if hover == Some(i) {
            Some(Interact::Hover)
        } else if selected == i {
            Some(Interact::Selected)
        } else {
            None
        };
        if let Some(state) = state {
            buf.set_style(r, th.interact(state));
            if state == Interact::Selected {
                style = style.add_modifier(Modifier::BOLD);
            }
        }
        let marker = if selected == i { th.g("▸") } else { " " };
        put(buf, r, 0, 0, &format!("{marker} {}", text(item, "label")), style);
        rows.push(r);
    }
    (rect, rows)
}

/// A tooltip: a bordered box of lines next to `anchor`, over whatever is there. It is a layer,
/// so nothing under it moves.
pub fn tooltip(buf: &mut Buffer, full: Rect, anchor: Rect, lines: &[String], th: &Theme) -> Rect {
    let widest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0).min(60);
    let w = (widest + 6).min(usize::from(full.width)) as u16;
    let h = (lines.len() as u16 + 2).min(full.height);
    // Below the element when there is room, else above it.
    let y = if anchor.bottom() + h <= full.bottom() { anchor.bottom() } else { anchor.y.saturating_sub(h) };
    let rect = Rect {
        x: (anchor.x + 1).min(full.right().saturating_sub(w)).max(full.x),
        y: y.max(full.y),
        width: w,
        height: h,
    };
    Clear.render(rect, buf);
    let inner = padded(buf, rect, "", th.dim_text, th);
    for (i, l) in lines.iter().enumerate() {
        put(buf, inner, 0, i as u16, &clip(l, 60), Style::new().fg(th.fg));
    }
    rect
}
