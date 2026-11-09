//! The transcript screen (05 section 3): a full-screen swap with one run's conversation. Messages,
//! tool calls collapsed to one line (`▸ bash  cargo test  (1.2 s, 4 KiB)`), a compaction marker,
//! the account in the header, and the text selection the mouse makes. The widget sees only the
//! `view:transcript` value; the rows are already wrapped and windowed for this size.

use aibus_core::state::value::{as_list, get};
use aibus_graph::Value;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Clear, Widget};

use crate::hit::{HitId, HitKind};
use crate::screens::{buttons, button, entry_line, Entry};
use crate::theme::{Interact, Theme};
use crate::ui::Pointer;
use crate::widgets::{clip, flag, int, panel, put, text};

static NULL: Value = Value::Null;

/// A selection of body cells: `(row, col)` of both ends, in reading order.
pub type Cells = ((u16, u16), (u16, u16));

/// What a transcript frame leaves behind.
#[derive(Clone, Debug, Default)]
pub struct Geom {
    pub area: Rect,
    /// The text area, one row per line of `lines`.
    pub body: Rect,
    /// The plain text of each body row, for selecting and copying.
    pub lines: Vec<String>,
    pub hits: Vec<(HitId, Rect, HitKind)>,
    pub track: Option<(Rect, Rect)>,
    /// All rows of the transcript, and the call (by ordinal) each body row shows.
    pub total: usize,
    pub tools: Vec<Option<usize>>,
}

/// `1.2 s`, `340 ms`.
pub fn duration(ms: i64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        format!("{:.1} s", ms as f64 / 1000.0)
    }
}

/// `4 KiB`, `812 B`, `1.2 MiB`.
pub fn size(bytes: i64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{} KiB", (b + 512) / 1024),
        b => format!("{:.1} MiB", b as f64 / (1024.0 * 1024.0)),
    }
}

/// The plain text of a tool row, which is also what is drawn.
fn tool_line(th: &Theme, row: &Value) -> String {
    let glyph = if flag(row, "expanded") { th.g("▾") } else { th.g("▸") };
    let mut line = format!("{glyph} {}  {}", text(row, "name"), text(row, "summary"));
    match get(row, "ok") {
        Some(Value::Bool(ok)) => {
            line.push_str(&format!("  ({}, {})", duration(int(row, "ms")), size(int(row, "bytes"))));
            if !ok {
                line.push(' ');
                line.push_str(th.g("✗"));
            }
        }
        _ => line.push_str("  (running)"),
    }
    line
}

fn short(label: &str) -> String {
    label.split_whitespace().next().unwrap_or("").to_lowercase()
}

/// The header: `search · haiku · anthropic-work`.
pub fn title(v: &Value) -> String {
    let mut t = format!("{} · {} · {}", text(v, "agent"), short(text(v, "model")), text(v, "account"));
    let status = text(v, "status");
    if !status.is_empty() {
        t.push_str(&format!(" · {status}"));
    }
    t
}

#[allow(clippy::too_many_arguments)]
pub fn draw(buf: &mut Buffer, full: Rect, v: &Value, th: &Theme, pointer: &Pointer, sel: Option<Cells>, entry: Option<&Entry>, copied: bool) -> Geom {
    Clear.render(full, buf);
    let border = match text(v, "role") {
        "lead" => th.lead,
        "worker" => th.swarm,
        _ => th.focus,
    };
    let inner = {
        let i = panel(buf, full, &title(v), border, th);
        Rect { x: i.x + 1, width: i.width.saturating_sub(2), ..i }
    };
    let dim = Style::new().fg(th.dim_text);
    // Row 0: what the view is showing besides the live end.
    let notice = if let Some(oldest) = get(v, "evicted").and_then(aibus_core::state::value::as_i64) {
        Some((format!("evicted, oldest retained t={oldest}"), Style::new().fg(th.warn)))
    } else if let Some(t) = get(v, "at").and_then(aibus_core::state::value::as_i64) {
        Some((format!("stepped back to t={t}  ([ older, ] newer, f live)"), Style::new().fg(th.warn)))
    } else if flag(v, "loading") {
        Some(("reading the transcript from disk...".to_owned(), dim))
    } else {
        get(v, "error").and_then(aibus_core::state::value::as_str).map(|e| (format!("failed: {e}"), Style::new().fg(th.error)))
    };
    if let Some((line, style)) = notice {
        put(buf, inner, 0, 0, &clip(&line, usize::from(inner.width)), style);
    }
    let height = int(v, "height").max(1) as u16;
    let body = Rect { y: inner.y + 1, height: height.min(inner.height.saturating_sub(3)), width: inner.width.saturating_sub(1), ..inner };
    let rows = as_list(get(v, "rows").unwrap_or(&NULL));
    let mut geom = Geom { area: full, body, total: int(v, "total").max(0) as usize, ..Geom::default() };
    for (i, row) in rows.iter().take(usize::from(body.height)).enumerate() {
        let y = i as u16;
        let r = Rect { y: body.y + y, height: 1, ..body };
        let (line, style) = match text(row, "kind") {
            "user" => (format!("│ {}", text(row, "text")), Style::new().fg(th.lead)),
            "detail" => (text(row, "text").to_owned(), dim),
            "compact" => (format!("⌁ compacted at {}k", int(row, "tokens") / 1000), dim),
            "note" => (text(row, "text").to_owned(), Style::new().fg(if flag(row, "error") { th.error } else { th.dim_text })),
            "tool" => {
                let failed = matches!(get(row, "ok"), Some(Value::Bool(false)));
                (tool_line(th, row), Style::new().fg(if failed { th.error } else { th.fg }))
            }
            _ => (text(row, "text").to_owned(), Style::new().fg(th.fg)),
        };
        let line = if th.ascii { line.replace('⌁', "~") } else { line };
        put(buf, r, 0, 0, &clip(&line, usize::from(r.width)), style);
        if text(row, "kind") == "tool" {
            if flag(row, "selected") {
                buf.set_style(r, th.interact(Interact::Selected));
            }
            if matches!(get(row, "ok"), Some(Value::Bool(true))) {
                // The name stays plain; only the glyph of a finished call is lit.
                put(buf, r, 0, 0, th.g(if flag(row, "expanded") { "▾" } else { "▸" }), Style::new().fg(th.ok).patch(if flag(row, "selected") { th.interact(Interact::Selected) } else { Style::new() }));
            }
        }
        geom.lines.push(clip(&line, usize::from(r.width)));
        geom.tools.push((text(row, "kind") == "tool").then(|| int(row, "ordinal") as usize));
        geom.hits.push((HitId::TrRow(i), r, HitKind::Text));
    }
    // Selected cells: reading order from the first to the last.
    if let Some(((r0, c0), (r1, c1))) = sel {
        for row in r0..=r1 {
            let (from, to) = (if row == r0 { c0 } else { 0 }, if row == r1 { c1 } else { body.width.saturating_sub(1) });
            for col in from..=to.min(body.width.saturating_sub(1)) {
                if row < body.height {
                    buf.set_style(Rect { x: body.x + col, y: body.y + row, width: 1, height: 1 }, th.interact(Interact::Selected));
                }
            }
        }
    }
    // The scrollbar: a track on the right edge, a thumb sized by how much shows.
    let (total, first) = (int(v, "total").max(0) as usize, int(v, "first").max(0) as usize);
    let visible = usize::from(body.height);
    if total > visible && visible > 0 {
        let x = body.right();
        let track = Rect { x, y: body.y, width: 1, height: body.height };
        let len = (visible * visible / total).clamp(1, visible);
        let top = first.min(total - visible) * (visible - len) / (total - visible);
        let thumb = Rect { y: track.y + top as u16, height: len as u16, ..track };
        let lit = [pointer.hover, pointer.pressed].contains(&Some(HitId::TrThumb));
        for y in thumb.top()..thumb.bottom() {
            buf.set_string(x, y, th.g("█"), Style::new().fg(if lit { th.focus } else { th.dim_text }).bg(th.panel));
        }
        geom.track = Some((track, thumb));
        geom.hits.push((HitId::TrTrack, track, HitKind::Track));
        geom.hits.push((HitId::TrThumb, thumb, HitKind::Thumb));
    }
    // Scrolled away from the bottom: following is paused and a button says how to resume.
    let follow = flag(v, "follow");
    if !follow && body.height > 0 {
        let label = "jump to latest";
        let w = label.chars().count() as u16 + 4;
        let r = Rect { x: body.right().saturating_sub(w + 1), y: body.bottom() - 1, width: w, height: 1 };
        let state = if pointer.pressed == Some(HitId::JumpLatest) {
            Some(Interact::Pressed)
        } else if pointer.hover == Some(HitId::JumpLatest) {
            Some(Interact::Hover)
        } else {
            None
        };
        button(buf, r, "", label, th, state, true);
        geom.hits.push((HitId::JumpLatest, r, HitKind::Button));
    }
    // The bottom bar: the keys as buttons, or the text field a key opened.
    let bar = Rect { y: inner.bottom().saturating_sub(2), height: 2, ..inner };
    if let Some(e) = entry {
        entry_line(buf, bar, 0, e, th);
        let (b, _) = buttons(buf, bar, 1, &[("send_entry", "Enter", "Send", true), ("cancel_entry", "Esc", "Back", true)], th, pointer);
        geom.hits.extend(b.into_iter().map(|(id, r)| (id, r, HitKind::Button)));
    } else {
        let live = flag(v, "live");
        let mut items: Vec<(&'static str, &str, &str, bool)> = vec![("close", "Esc", "Close", true), ("toggle", "Enter", "Expand", true)];
        items.push(("older", "[", "Older", true));
        items.push(("newer", "]", "Newer", get(v, "at").is_some_and(|a| !matches!(a, Value::Null))));
        if flag(v, "retryable") {
            items.push(("retry", "r", "Retry", true));
        }
        if live {
            items.push(("stop", "x", "Stop", true));
            items.push(("compact", "c", "Compact", true));
        }
        if live || flag(v, "partial") {
            items.push(("message", "m", "Message", true));
        }
        if flag(v, "partial") {
            items.push(("resume", "R", "Resume", true));
        }
        if sel.is_some() {
            items.push(("copy", "Ctrl+C", if copied { "Copied" } else { "Copy" }, true));
        }
        let (b, _) = buttons(buf, bar, 0, &items, th, pointer);
        geom.hits.extend(b.into_iter().map(|(id, r)| {
            // The copy button is the `copy` hint of a selection.
            let id = if id == HitId::Button("copy") { HitId::CopyHint } else { id };
            (id, r, HitKind::Button)
        }));
    }
    geom
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_durations_read_like_the_spec_example() {
        assert_eq!(duration(1200), "1.2 s");
        assert_eq!(duration(340), "340 ms");
        assert_eq!(size(4096), "4 KiB");
        assert_eq!(size(812), "812 B");
        assert_eq!(size(1_300_000), "1.2 MiB");
    }
}
