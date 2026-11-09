//! Theme tokens (05 section 7). Widgets take every colour from here; a grep test keeps
//! colour literals out of the other files.

use ratatui::style::{Color, Modifier, Style};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub bg: Color,
    pub panel: Color,
    pub fg: Color,
    /// Sampled dim. Only for rules and travelling dots: at 2.8:1 it is too faint for text.
    pub dim: Color,
    /// Dim text that still clears 4.5:1 on `bg`, `panel`, `highlight` and the state backgrounds.
    pub dim_text: Color,
    pub highlight: Color,
    pub lead: Color,
    pub swarm: Color,
    pub router: Color,
    pub advisor: Color,
    pub ok: Color,
    pub warn: Color,
    pub error: Color,
    pub split: Color,
    pub split_rest: Color,
    /// Background of the element under the pointer.
    pub hover: Color,
    /// Background of an element while the button is down on it.
    pub pressed: Color,
    /// Background of the selected card or row.
    pub selected: Color,
    /// Border of the focused pane, and of a drag handle under the pointer.
    pub focus: Color,
    /// No colour at all: states are told apart by modifiers instead.
    pub mono: bool,
    /// Swap the unicode glyphs for ASCII (05 section 6). Travels with the theme so a widget
    /// keeps its one `&Theme` parameter.
    pub ascii: bool,
}

/// How an element is being touched. `Theme::interact` turns it into a style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interact {
    Hover,
    Pressed,
    Selected,
}

impl Theme {
    /// The palette sampled from the recording.
    pub const fn recording() -> Self {
        Self {
            bg: Color::Rgb(0x1a, 0x1b, 0x26),
            panel: Color::Rgb(0x1f, 0x20, 0x29),
            fg: Color::Rgb(0xc0, 0xca, 0xf5),
            dim: Color::Rgb(0x56, 0x5f, 0x89),
            dim_text: Color::Rgb(0x8c, 0x96, 0xc2),
            highlight: Color::Rgb(0x2a, 0x2b, 0x3d),
            lead: Color::Rgb(0xf0, 0xa0, 0x70),
            swarm: Color::Rgb(0x7a, 0xa2, 0xf7),
            router: Color::Rgb(0x9e, 0xce, 0x6a),
            advisor: Color::Rgb(0xbb, 0x9a, 0xf7),
            ok: Color::Rgb(0x9e, 0xce, 0x6a),
            warn: Color::Rgb(0xe0, 0xaf, 0x68),
            error: Color::Rgb(0xf7, 0x76, 0x8e),
            split: Color::Rgb(0xf0, 0xa0, 0x70),
            split_rest: Color::Rgb(0x6b, 0x4a, 0x3a),
            hover: Color::Rgb(0x25, 0x26, 0x3a),
            pressed: Color::Rgb(0x14, 0x15, 0x1d),
            selected: Color::Rgb(0x2a, 0x2b, 0x3d),
            focus: Color::Rgb(0x7d, 0xcf, 0xff),
            mono: false,
            ascii: false,
        }
    }

    /// Dark text on pale panels, checked against WCAG AA in `tests/theme_contrast.rs`.
    pub const fn light() -> Self {
        Self {
            bg: Color::Rgb(0xf4, 0xf4, 0xf8),
            panel: Color::Rgb(0xff, 0xff, 0xff),
            fg: Color::Rgb(0x1a, 0x1b, 0x26),
            dim: Color::Rgb(0x7a, 0x80, 0x9c),
            dim_text: Color::Rgb(0x4b, 0x52, 0x73),
            highlight: Color::Rgb(0xe2, 0xe5, 0xf2),
            lead: Color::Rgb(0x9a, 0x42, 0x08),
            swarm: Color::Rgb(0x22, 0x50, 0xc0),
            router: Color::Rgb(0x2a, 0x64, 0x10),
            advisor: Color::Rgb(0x6a, 0x3c, 0xc4),
            ok: Color::Rgb(0x2a, 0x64, 0x10),
            warn: Color::Rgb(0x84, 0x52, 0x00),
            error: Color::Rgb(0xb0, 0x1c, 0x3c),
            split: Color::Rgb(0x9a, 0x42, 0x08),
            split_rest: Color::Rgb(0xd9, 0xc3, 0xb8),
            hover: Color::Rgb(0xec, 0xee, 0xf7),
            pressed: Color::Rgb(0xd9, 0xdc, 0xee),
            selected: Color::Rgb(0xdd, 0xe1, 0xf0),
            focus: Color::Rgb(0x1d, 0x4e, 0xd8),
            mono: false,
            ascii: false,
        }
    }

    /// No colour: the terminal's own foreground and background, and modifiers for emphasis.
    pub const fn mono() -> Self {
        Self {
            bg: Color::Reset,
            panel: Color::Reset,
            fg: Color::Reset,
            dim: Color::Reset,
            dim_text: Color::Reset,
            highlight: Color::Reset,
            lead: Color::Reset,
            swarm: Color::Reset,
            router: Color::Reset,
            advisor: Color::Reset,
            ok: Color::Reset,
            warn: Color::Reset,
            error: Color::Reset,
            split: Color::Reset,
            split_rest: Color::Reset,
            hover: Color::Reset,
            pressed: Color::Reset,
            selected: Color::Reset,
            focus: Color::Reset,
            mono: true,
            ascii: false,
        }
    }

    pub fn by_name(name: &str) -> Option<Self> {
        match name {
            "recording" => Some(Self::recording()),
            "light" => Some(Self::light()),
            "mono" => Some(Self::mono()),
            _ => None,
        }
    }

    /// The theme a session runs with (05 section 7): the configured one (an unknown name falls
    /// back to `recording`), `mono` when `NO_COLOR` is set and not empty, hex colours when
    /// `COLORTERM` says `truecolor` or `24bit` and the nearest 256-colour index otherwise.
    pub fn resolve(name: &str, no_color: Option<&str>, colorterm: Option<&str>, ascii: bool) -> Self {
        let mut t = if no_color.is_some_and(|v| !v.is_empty()) {
            Self::mono()
        } else {
            Self::by_name(name).unwrap_or_else(Self::recording)
        };
        if !matches!(colorterm, Some("truecolor" | "24bit")) {
            t = t.to_256();
        }
        t.ascii = ascii;
        t
    }

    /// Every hex colour replaced by the closest index of the xterm 256-colour palette.
    pub fn to_256(self) -> Self {
        let near = |c: Color| match c {
            Color::Rgb(r, g, b) => Color::Indexed(nearest_256(r, g, b)),
            other => other,
        };
        Self {
            bg: near(self.bg),
            panel: near(self.panel),
            fg: near(self.fg),
            dim: near(self.dim),
            dim_text: near(self.dim_text),
            highlight: near(self.highlight),
            lead: near(self.lead),
            swarm: near(self.swarm),
            router: near(self.router),
            advisor: near(self.advisor),
            ok: near(self.ok),
            warn: near(self.warn),
            error: near(self.error),
            split: near(self.split),
            split_rest: near(self.split_rest),
            hover: near(self.hover),
            pressed: near(self.pressed),
            selected: near(self.selected),
            focus: near(self.focus),
            ..self
        }
    }

    /// The style of an element that is hovered, pressed or selected.
    pub fn interact(&self, state: Interact) -> Style {
        if self.mono {
            return Style::new().add_modifier(match state {
                Interact::Hover => Modifier::UNDERLINED,
                Interact::Pressed => Modifier::REVERSED,
                Interact::Selected => Modifier::BOLD | Modifier::REVERSED,
            });
        }
        Style::new().bg(match state {
            Interact::Hover => self.hover,
            Interact::Pressed => self.pressed,
            Interact::Selected => self.selected,
        })
    }

    /// The border (and title) style of the focused pane.
    pub fn focused(&self) -> Style {
        if self.mono {
            Style::new().add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(self.focus).add_modifier(Modifier::BOLD)
        }
    }
}

/// The index in the xterm 256-colour palette (16 to 255; the first 16 are whatever the user's
/// terminal says) closest to `rgb` by squared distance: the best of the 6x6x6 cube and the grey
/// ramp.
pub fn nearest_256(r: u8, g: u8, b: u8) -> u8 {
    const LEVELS: [i32; 6] = [0, 95, 135, 175, 215, 255];
    let dist = |p: [i32; 3]| {
        let d = |a: i32, b: i32| (a - b) * (a - b);
        d(p[0], i32::from(r)) + d(p[1], i32::from(g)) + d(p[2], i32::from(b))
    };
    let level = |v: u8| {
        (0..6)
            .min_by_key(|&i| (LEVELS[i] - i32::from(v)).abs())
            .unwrap_or(0)
    };
    let (ri, gi, bi) = (level(r), level(g), level(b));
    let cube = [LEVELS[ri], LEVELS[gi], LEVELS[bi]];
    // The grey nearest to the mean, rounded to a step of 10 from 8, in integers.
    let sum = i32::from(r) + i32::from(g) + i32::from(b);
    let step = ((sum - 9) / 30).clamp(0, 23);
    let grey = 8 + 10 * step;
    if dist([grey; 3]) < dist(cube) {
        232 + step as u8
    } else {
        (16 + 36 * ri + 6 * gi + bi) as u8
    }
}

/// The SGR parameters that set `c` as foreground or background (the demo's ANSI writer).
pub fn sgr(c: Color, fg: bool) -> String {
    let (base, bright) = if fg { (30, 90) } else { (40, 100) };
    match c {
        Color::Reset => (if fg { 39 } else { 49 }).to_string(),
        Color::Rgb(r, g, b) => format!("{};2;{r};{g};{b}", if fg { 38 } else { 48 }),
        Color::Indexed(n) => format!("{};5;{n}", if fg { 38 } else { 48 }),
        Color::Black => base.to_string(),
        Color::Red => (base + 1).to_string(),
        Color::Green => (base + 2).to_string(),
        Color::Yellow => (base + 3).to_string(),
        Color::Blue => (base + 4).to_string(),
        Color::Magenta => (base + 5).to_string(),
        Color::Cyan => (base + 6).to_string(),
        Color::Gray => (base + 7).to_string(),
        Color::DarkGray => bright.to_string(),
        Color::LightRed => (bright + 1).to_string(),
        Color::LightGreen => (bright + 2).to_string(),
        Color::LightYellow => (bright + 3).to_string(),
        Color::LightBlue => (bright + 4).to_string(),
        Color::LightMagenta => (bright + 5).to_string(),
        Color::LightCyan => (bright + 6).to_string(),
        Color::White => (bright + 7).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(i: u8) -> [i32; 3] {
        match i {
            16..=231 => {
                let c = i - 16;
                let l = |v: u8| [0, 95, 135, 175, 215, 255][usize::from(v)];
                [l(c / 36), l(c / 6 % 6), l(c % 6)]
            }
            _ => [8 + 10 * (i32::from(i) - 232); 3],
        }
    }

    fn dist(a: [i32; 3], b: [i32; 3]) -> i32 {
        (0..3).map(|i| (a[i] - b[i]) * (a[i] - b[i])).sum()
    }

    /// The mapping picks a palette entry no other entry beats, on a grid over the whole cube and
    /// on every colour of the built-in themes.
    #[test]
    fn nearest_256_is_the_closest_palette_entry() {
        let mut samples: Vec<[u8; 3]> = Vec::new();
        for r in (0..=255).step_by(15) {
            for g in (0..=255).step_by(15) {
                for b in (0..=255).step_by(15) {
                    samples.push([r, g, b]);
                }
            }
        }
        for t in [Theme::recording(), Theme::light()] {
            for c in [t.bg, t.panel, t.fg, t.dim, t.lead, t.swarm, t.router, t.advisor, t.split] {
                if let Color::Rgb(r, g, b) = c {
                    samples.push([r, g, b]);
                }
            }
        }
        for [r, g, b] in samples {
            let got = nearest_256(r, g, b);
            assert!(got >= 16, "{r},{g},{b} mapped into the user palette");
            let want = (16..=255u8)
                .map(|i| dist(rgb(i), [i32::from(r), i32::from(g), i32::from(b)]))
                .min()
                .unwrap();
            let have = dist(rgb(got), [i32::from(r), i32::from(g), i32::from(b)]);
            assert_eq!(have, want, "{r},{g},{b} -> {got}");
        }
    }

    #[test]
    fn known_colours_map_to_known_indices() {
        assert_eq!(nearest_256(0, 0, 0), 16);
        assert_eq!(nearest_256(255, 255, 255), 231);
        assert_eq!(nearest_256(255, 0, 0), 196);
        assert_eq!(nearest_256(0x80, 0x80, 0x80), 244);
        // The recording's background is a near-black grey, not a cube corner.
        assert!(nearest_256(0x1a, 0x1b, 0x26) >= 232);
    }

    fn resolved(name: &str, no_color: Option<&str>, colorterm: Option<&str>) -> Theme {
        Theme::resolve(name, no_color, colorterm, false)
    }

    #[test]
    fn no_color_forces_mono_whatever_else_is_set() {
        for name in ["recording", "light", "mono"] {
            let t = resolved(name, Some("1"), Some("truecolor"));
            assert!(t.mono, "{name}");
            assert_eq!(t.fg, Color::Reset);
        }
        // An empty NO_COLOR is "not set" (no-color.org).
        assert!(!resolved("recording", Some(""), Some("truecolor")).mono);
        assert!(!resolved("recording", None, Some("truecolor")).mono);
    }

    #[test]
    fn truecolor_keeps_hex_and_anything_else_uses_the_256_palette() {
        for ct in [Some("truecolor"), Some("24bit")] {
            assert_eq!(resolved("recording", None, ct), Theme::recording());
        }
        for ct in [None, Some(""), Some("yes")] {
            let t = resolved("recording", None, ct);
            assert!(matches!(t.bg, Color::Indexed(_)), "{ct:?}");
            assert_eq!(t.bg, Color::Indexed(nearest_256(0x1a, 0x1b, 0x26)));
            assert_eq!(t.lead, Color::Indexed(nearest_256(0xf0, 0xa0, 0x70)));
        }
        // Mono has nothing to map.
        assert_eq!(resolved("mono", None, None).fg, Color::Reset);
    }

    #[test]
    fn names_resolve_and_an_unknown_one_is_the_recording() {
        assert_eq!(Theme::by_name("light"), Some(Theme::light()));
        assert_eq!(Theme::by_name("nope"), None);
        assert_eq!(resolved("nope", None, Some("truecolor")), Theme::recording());
        assert!(Theme::resolve("light", None, Some("truecolor"), true).ascii);
    }

    #[test]
    fn mono_tells_states_apart_by_modifier() {
        let t = Theme::mono();
        let all = [Interact::Hover, Interact::Pressed, Interact::Selected].map(|s| t.interact(s));
        assert!(all[0] != all[1] && all[1] != all[2] && all[0] != all[2]);
        assert_eq!(Theme::recording().interact(Interact::Hover).bg, Some(Theme::recording().hover));
    }
}
