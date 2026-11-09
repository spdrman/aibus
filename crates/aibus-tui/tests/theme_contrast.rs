//! WCAG 2.x contrast of the built-in themes (05 sections 6.1 and 7): 4.5:1 for text, 3:1 for
//! bars and borders. Every pair below is measured here and the measurement is pinned in a named
//! table, so a palette change shows up as a failing number rather than a silent drift.
//!
//! Text pairs are every text colour on `bg`, `panel`, `highlight` and the three interaction
//! backgrounds (`hover`, `pressed`, `selected`). Bar pairs are the role colours, `split` and the
//! `focus` border on `bg` and `panel`.

use aibus_tui::Theme;
use ratatui::style::Color;

fn linear(c: Color) -> [f64; 3] {
    let Color::Rgb(r, g, b) = c else {
        panic!("not rgb: {c:?}")
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

fn ratio(a: Color, b: Color) -> f64 {
    let lum = |c| {
        let [r, g, b] = linear(c);
        0.2126 * r + 0.7152 * g + 0.0722 * b
    };
    let (x, y) = (lum(a), lum(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

fn pick(t: &Theme, name: &str) -> Color {
    match name {
        "bg" => t.bg,
        "panel" => t.panel,
        "fg" => t.fg,
        "dim" => t.dim,
        "dim_text" => t.dim_text,
        "highlight" => t.highlight,
        "lead" => t.lead,
        "swarm" => t.swarm,
        "router" => t.router,
        "advisor" => t.advisor,
        "ok" => t.ok,
        "warn" => t.warn,
        "error" => t.error,
        "split" => t.split,
        "split_rest" => t.split_rest,
        "hover" => t.hover,
        "pressed" => t.pressed,
        "selected" => t.selected,
        "focus" => t.focus,
        other => panic!("no token {other}"),
    }
}

fn check(theme: &str, t: &Theme, table: &[(&str, &str, f64)], min: f64) {
    for (fg, bg, measured) in table {
        let r = ratio(pick(t, fg), pick(t, bg));
        println!("{theme}: {fg} on {bg} = {r:.2}");
        assert!(r >= min, "{theme}: {fg} on {bg} is {r:.2}, needs {min}");
        assert!(
            (r - measured).abs() < 0.01,
            "{theme}: {fg} on {bg} measures {r:.2}, the table says {measured}"
        );
    }
}

#[test]
fn recording_text_pairs_meet_4_5() {
    check("recording", &Theme::recording(), &RECORDING_TEXT, 4.5);
}

#[test]
fn recording_bars_and_borders_meet_3() {
    check("recording", &Theme::recording(), &RECORDING_BARS, 3.0);
}

/// The orange of a split row is text too (the verdict word, the log's `split → <lead>`, the
/// footer on `highlight`), so it is measured as text on every background it sits on.
#[test]
fn split_text_meets_4_5_on_every_background() {
    check("recording", &Theme::recording(), &RECORDING_SPLIT_TEXT, 4.5);
    check("light", &Theme::light(), &LIGHT_SPLIT_TEXT, 4.5);
}

#[test]
fn light_text_pairs_meet_4_5() {
    check("light", &Theme::light(), &LIGHT_TEXT, 4.5);
}

#[test]
fn light_bars_and_borders_meet_3() {
    check("light", &Theme::light(), &LIGHT_BARS, 3.0);
}

/// `dim` (rules, travelling dots) and `split_rest` (the empty part of a split bar, whose value is
/// also printed) are decorative: nothing reads them. Pinned so a change shows up.
const RECORDING_DIM_ON_BG: f64 = 2.76;
const RECORDING_SPLIT_REST_ON_PANEL: f64 = 2.06;
const LIGHT_DIM_ON_BG: f64 = 3.55;
const LIGHT_SPLIT_REST_ON_PANEL: f64 = 1.69;

#[test]
fn decorative_pairs_are_recorded() {
    for (t, dim, rest) in [
        (Theme::recording(), RECORDING_DIM_ON_BG, RECORDING_SPLIT_REST_ON_PANEL),
        (Theme::light(), LIGHT_DIM_ON_BG, LIGHT_SPLIT_REST_ON_PANEL),
    ] {
        assert!((ratio(t.dim, t.bg) - dim).abs() < 0.01);
        assert!((ratio(t.split_rest, t.panel) - rest).abs() < 0.01);
    }
    // The light theme's rules clear 3:1 as well.
    assert!(ratio(Theme::light().dim, Theme::light().bg) >= 3.0);
}

#[test]
fn the_interaction_tokens_can_be_told_apart() {
    for t in [Theme::recording(), Theme::light()] {
        assert_ne!(t.hover, t.pressed);
        assert_ne!(t.hover, t.panel);
        assert_ne!(t.pressed, t.panel);
        assert_ne!(t.focus, t.lead);
        assert_ne!(t.focus, t.swarm);
    }
}

#[test]
fn mono_carries_no_colour() {
    let t = Theme::mono();
    for name in ["bg", "panel", "fg", "dim", "lead", "swarm", "hover", "pressed", "selected", "focus"] {
        assert_eq!(pick(&t, name), Color::Reset, "{name}");
    }
}

const RECORDING_TEXT: [(&str, &str, f64); 54] = [
    ("fg", "bg", 10.59),
    ("fg", "panel", 10.03),
    ("fg", "highlight", 8.61),
    ("fg", "hover", 9.18),
    ("fg", "pressed", 11.26),
    ("fg", "selected", 8.61),
    ("dim_text", "bg", 5.91),
    ("dim_text", "panel", 5.60),
    ("dim_text", "highlight", 4.80),
    ("dim_text", "hover", 5.12),
    ("dim_text", "pressed", 6.284),
    ("dim_text", "selected", 4.80),
    ("lead", "bg", 8.11),
    ("lead", "panel", 7.69),
    ("lead", "highlight", 6.59),
    ("lead", "hover", 7.03),
    ("lead", "pressed", 8.63),
    ("lead", "selected", 6.59),
    ("swarm", "bg", 6.79),
    ("swarm", "panel", 6.43),
    ("swarm", "highlight", 5.52),
    ("swarm", "hover", 5.88),
    ("swarm", "pressed", 7.22),
    ("swarm", "selected", 5.52),
    ("router", "bg", 9.35),
    ("router", "panel", 8.86),
    ("router", "highlight", 7.60),
    ("router", "hover", 8.11),
    ("router", "pressed", 9.95),
    ("router", "selected", 7.60),
    ("advisor", "bg", 7.39),
    ("advisor", "panel", 7.00),
    ("advisor", "highlight", 6.01),
    ("advisor", "hover", 6.41),
    ("advisor", "pressed", 7.86),
    ("advisor", "selected", 6.01),
    ("ok", "bg", 9.35),
    ("ok", "panel", 8.86),
    ("ok", "highlight", 7.60),
    ("ok", "hover", 8.11),
    ("ok", "pressed", 9.95),
    ("ok", "selected", 7.60),
    ("warn", "bg", 8.55),
    ("warn", "panel", 8.10),
    ("warn", "highlight", 6.95),
    ("warn", "hover", 7.41),
    ("warn", "pressed", 9.09),
    ("warn", "selected", 6.95),
    ("error", "bg", 6.46),
    ("error", "panel", 6.12),
    ("error", "highlight", 5.25),
    ("error", "hover", 5.60),
    ("error", "pressed", 6.87),
    ("error", "selected", 5.25),
];
const RECORDING_BARS: [(&str, &str, f64); 12] = [
    ("lead", "bg", 8.11),
    ("lead", "panel", 7.69),
    ("swarm", "bg", 6.79),
    ("swarm", "panel", 6.43),
    ("router", "bg", 9.35),
    ("router", "panel", 8.86),
    ("advisor", "bg", 7.39),
    ("advisor", "panel", 7.00),
    ("split", "bg", 8.11),
    ("split", "panel", 7.69),
    ("focus", "bg", 9.96),
    ("focus", "panel", 9.44),
];
// worst text 4.80; dim/bg 2.76; split_rest/panel 2.06
const LIGHT_TEXT: [(&str, &str, f64); 54] = [
    ("fg", "bg", 15.58),
    ("fg", "panel", 17.09),
    ("fg", "highlight", 13.61),
    ("fg", "hover", 14.76),
    ("fg", "pressed", 12.55),
    ("fg", "selected", 13.11),
    ("dim_text", "bg", 6.95),
    ("dim_text", "panel", 7.63),
    ("dim_text", "highlight", 6.07),
    ("dim_text", "hover", 6.59),
    ("dim_text", "pressed", 5.60),
    ("dim_text", "selected", 5.85),
    ("lead", "bg", 6.06),
    ("lead", "panel", 6.65),
    ("lead", "highlight", 5.30),
    ("lead", "hover", 5.75),
    ("lead", "pressed", 4.89),
    ("lead", "selected", 5.10),
    ("swarm", "bg", 6.43),
    ("swarm", "panel", 7.05),
    ("swarm", "highlight", 5.62),
    ("swarm", "hover", 6.09),
    ("swarm", "pressed", 5.18),
    ("swarm", "selected", 5.41),
    ("router", "bg", 6.54),
    ("router", "panel", 7.17),
    ("router", "highlight", 5.71),
    ("router", "hover", 6.19),
    ("router", "pressed", 5.27),
    ("router", "selected", 5.50),
    ("advisor", "bg", 6.26),
    ("advisor", "panel", 6.87),
    ("advisor", "highlight", 5.47),
    ("advisor", "hover", 5.94),
    ("advisor", "pressed", 5.05),
    ("advisor", "selected", 5.27),
    ("ok", "bg", 6.54),
    ("ok", "panel", 7.17),
    ("ok", "highlight", 5.71),
    ("ok", "hover", 6.19),
    ("ok", "pressed", 5.27),
    ("ok", "selected", 5.50),
    ("warn", "bg", 6.00),
    ("warn", "panel", 6.59),
    ("warn", "highlight", 5.25),
    ("warn", "hover", 5.69),
    ("warn", "pressed", 4.84),
    ("warn", "selected", 5.05),
    ("error", "bg", 6.22),
    ("error", "panel", 6.82),
    ("error", "highlight", 5.43),
    ("error", "hover", 5.89),
    ("error", "pressed", 5.01),
    ("error", "selected", 5.23),
];
const LIGHT_BARS: [(&str, &str, f64); 12] = [
    ("lead", "bg", 6.06),
    ("lead", "panel", 6.65),
    ("swarm", "bg", 6.43),
    ("swarm", "panel", 7.05),
    ("router", "bg", 6.54),
    ("router", "panel", 7.17),
    ("advisor", "bg", 6.26),
    ("advisor", "panel", 6.87),
    ("split", "bg", 6.06),
    ("split", "panel", 6.65),
    ("focus", "bg", 6.11),
    ("focus", "panel", 6.70),
];
// worst text 4.84; dim/bg 3.55; split_rest/panel 1.69
const RECORDING_SPLIT_TEXT: [(&str, &str, f64); 6] = [
    ("split", "bg", 8.11),
    ("split", "panel", 7.69),
    ("split", "highlight", 6.59),
    ("split", "hover", 7.03),
    ("split", "pressed", 8.63),
    ("split", "selected", 6.59),
];
const LIGHT_SPLIT_TEXT: [(&str, &str, f64); 6] = [
    ("split", "bg", 6.06),
    ("split", "panel", 6.65),
    ("split", "highlight", 5.30),
    ("split", "hover", 5.75),
    ("split", "pressed", 4.89),
    ("split", "selected", 5.10),
];
