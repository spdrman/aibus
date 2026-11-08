//! The glyph set and its ASCII table (05 section 6). With `[ui] ascii = true` or `--ascii` every
//! glyph is swapped for plain characters: the ones the spec names through `Theme::g` where the
//! replacement may be wider than one cell, and everything else (box drawing, partial blocks) by
//! a sweep over the finished frame, so no widget has to know about it.

use ratatui::buffer::Buffer;

use crate::theme::Theme;

/// The glyph set of 05 section 6 and what `[ui] ascii` puts instead. The spec lists thirteen
/// substitutes for nineteen glyphs; the six it leaves out (`▌ ● · ▸ ▾ ◂ ◌`) get the obvious
/// single character.
pub const GLYPHS: [(&str, &str); 19] = [
    ("◆", "*"),
    ("◇", "o"),
    ("◐", "|"),
    ("◓", "/"),
    ("◑", "-"),
    ("◒", "\\"),
    ("✓", "ok"),
    ("✗", "x"),
    ("»", ">>"),
    ("→", "->"),
    ("▮", "#"),
    ("█", "="),
    ("▌", "_"),
    ("●", "@"),
    ("·", "."),
    ("▸", ">"),
    ("▾", "v"),
    ("◂", "<"),
    ("◌", ":"),
];

/// Drawing characters outside the glyph set that a frame also contains: borders, the partial
/// blocks of a bar, the legend square.
const DRAWING: [(&str, &str); 22] = [
    ("─", "-"),
    ("│", "|"),
    ("┌", "+"),
    ("┐", "+"),
    ("└", "+"),
    ("┘", "+"),
    ("├", "+"),
    ("┤", "+"),
    ("┬", "+"),
    ("┴", "+"),
    ("┼", "+"),
    ("▏", "-"),
    ("▎", "-"),
    ("▍", "-"),
    ("▋", "="),
    ("▊", "="),
    ("▉", "="),
    ("■", "#"),
    ("▶", ">"),
    ("░", "."),
    ("┆", ":"),
    ("╌", "-"),
];

impl Theme {
    /// `glyph` as is, or its ASCII substitute under `[ui] ascii`. Use this where the
    /// substitute may be wider than a cell (`ok`, `>>`, `->`); the frame sweep handles the rest.
    pub fn g(&self, glyph: &'static str) -> &'static str {
        if !self.ascii {
            return glyph;
        }
        GLYPHS
            .iter()
            .find(|(u, _)| *u == glyph)
            .map_or(glyph, |(_, a)| a)
    }
}

/// Replaces every non-ASCII cell of the frame by one ASCII character.
pub fn sweep(buf: &mut Buffer) {
    for cell in &mut buf.content {
        if cell.symbol().is_ascii() {
            continue;
        }
        let one = |a: &str| a.chars().next().unwrap_or('?');
        let c = GLYPHS
            .iter()
            .chain(DRAWING.iter())
            .find(|(u, _)| *u == cell.symbol())
            .map_or('?', |(_, a)| one(a));
        cell.set_char(c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    #[test]
    fn the_table_is_the_spec_set_and_the_spec_substitutes() {
        let set: String = GLYPHS.iter().map(|(u, _)| *u).collect();
        assert_eq!(set, "◆◇◐◓◑◒✓✗»→▮█▌●·▸▾◂◌");
        // The thirteen the spec spells out, in its order.
        let spelled = ["*", "o", "|", "/", "-", "\\", "ok", "x", ">>", "->", "#", "=", "."];
        let ours: Vec<&str> = GLYPHS.iter().map(|(_, a)| *a).collect();
        for want in spelled {
            assert!(ours.contains(&want), "{want}");
        }
        assert!(GLYPHS.iter().chain(DRAWING.iter()).all(|(_, a)| a.is_ascii()));
    }

    #[test]
    fn g_is_the_identity_unless_ascii() {
        let mut t = Theme::recording();
        assert_eq!(t.g("✓"), "✓");
        t.ascii = true;
        assert_eq!((t.g("✓"), t.g("»"), t.g("→"), t.g("◆")), ("ok", ">>", "->", "*"));
    }

    #[test]
    fn the_sweep_leaves_nothing_but_ascii_behind() {
        let all: String = GLYPHS.iter().chain(DRAWING.iter()).map(|(u, _)| *u).collect();
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 2));
        buf.set_string(0, 0, &all, ratatui::style::Style::new());
        buf.set_string(0, 1, "é ok", ratatui::style::Style::new());
        sweep(&mut buf);
        assert!(buf.content.iter().all(|c| c.symbol().is_ascii()));
        assert_eq!(buf[(0, 0)].symbol(), "*");
        assert_eq!(buf[(1, 1)].symbol(), " ");
        assert_eq!(buf[(0, 1)].symbol(), "?");
    }
}
