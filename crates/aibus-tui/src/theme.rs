//! Theme tokens (05 section 7). Widgets take every colour from here; a grep test keeps
//! colour literals out of the other files.

use ratatui::style::Color;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub bg: Color,
    pub panel: Color,
    pub fg: Color,
    /// Sampled dim. Only for rules and travelling dots: at 2.8:1 it is too faint for text.
    pub dim: Color,
    /// Dim text that still clears 4.5:1 on `bg`, `panel` and `highlight`.
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linear(c: Color) -> [f64; 3] {
        let Color::Rgb(r, g, b) = c else {
            panic!("not rgb")
        };
        [r, g, b].map(|v| {
            let v = f64::from(v) / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        })
    }

    /// WCAG 2.x contrast ratio.
    fn ratio(a: Color, b: Color) -> f64 {
        let lum = |c| {
            let [r, g, b] = linear(c);
            0.2126 * r + 0.7152 * g + 0.0722 * b
        };
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn text_pairs_meet_aa() {
        let t = Theme::recording();
        let mut report = String::new();
        for (name, fg) in [
            ("fg", t.fg),
            ("dim_text", t.dim_text),
            ("lead", t.lead),
            ("swarm", t.swarm),
            ("router", t.router),
            ("advisor", t.advisor),
            ("warn", t.warn),
            ("error", t.error),
        ] {
            for (bname, bg) in [("bg", t.bg), ("panel", t.panel), ("highlight", t.highlight)] {
                let r = ratio(fg, bg);
                report.push_str(&format!("{name} on {bname}: {r:.2}\n"));
                assert!(r >= 4.5, "{name} on {bname} is {r:.2}");
            }
        }
        println!("{report}");
    }

    #[test]
    fn borders_and_bars_meet_3_to_1() {
        let t = Theme::recording();
        for (name, c) in [
            ("lead", t.lead),
            ("swarm", t.swarm),
            ("router", t.router),
            ("advisor", t.advisor),
            ("split", t.split),
        ] {
            let r = ratio(c, t.panel);
            println!("{name} border/bar on panel: {r:.2}");
            assert!(r >= 3.0, "{name} is {r:.2}");
        }
    }

    /// The two sampled colours that fall short are decorative (rules, dots, the empty part
    /// of a split bar whose value is also printed as a number). The measurement is pinned so
    /// a change shows up.
    #[test]
    fn sampled_decorative_pairs_are_recorded() {
        let t = Theme::recording();
        let dim = ratio(t.dim, t.bg);
        let rest = ratio(t.split_rest, t.panel);
        println!("dim on bg: {dim:.2}, split_rest on panel: {rest:.2}");
        assert!((2.7..2.9).contains(&dim), "{dim}");
        assert!((2.0..2.2).contains(&rest), "{rest}");
    }
}
