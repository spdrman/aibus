//! The TUI shell (05 sections 4, 6 and 7): layout rules, key map, focus cycle, help screen, the
//! glyph set and the colour modes, on a `TestBackend`.

mod common;

use aibus_core::state::{FOCUS, MODAL};
use aibus_core::*;
use aibus_graph::{Graph, Value};
use aibus_testkit::tui::{heavy_app, replay_app};
use aibus_testkit::{assert_frame, assert_styles};
use aibus_tui::keys::Keymap;
use aibus_tui::Theme;
use common::*;
use crossterm::event::{KeyCode as K, KeyModifiers};
use ratatui::style::Color;

fn rig(size: (u16, u16)) -> Rig {
    Rig::new(replay_app(), size, Theme::recording())
}

fn focus(r: &Rig) -> String {
    match r.app.graph.read_key(FOCUS) {
        Some(Value::Str(s)) => s.to_string(),
        other => panic!("{other:?}"),
    }
}

fn focus_cards(r: &mut Rig) {
    r.app.step(Msg::Ui(UiIntent::Focus(Pane::Cards))).unwrap();
    r.settle();
}

fn last_line(text: &str) -> &str {
    text.lines().last().unwrap_or("")
}

// ---- layout (05 section 6) -------------------------------------------------------------

/// 98x46 and 80x24 are in `snapshots_dashboard.rs`.
#[test]
fn snapshot_at_120x50() {
    let wide = rig((120, 50)).frame();
    assert_frame!(wide, "shell_120x50");
    assert_styles!(wide, "shell_120x50");
}

#[test]
fn below_80x24_the_frame_is_a_message() {
    for (w, h) in [(79, 24), (80, 23), (40, 10)] {
        let r = rig((w, h));
        let t = r.text();
        assert!(t.contains("terminal too small"), "{w}x{h}: {t}");
        assert!(t.contains(&format!("need 80x24, have {w}x{h}")), "{w}x{h}: {t}");
        assert!(!t.contains("session log"));
        assert!(r.ui.drawn.hits.regions().is_empty(), "nothing to click on a message");
    }
    assert!(rig((80, 24)).text().contains("session log"));
}

#[test]
fn the_rail_is_a_strip_under_98_columns_and_a_opens_it() {
    assert_eq!(rig((97, 46)).ui.drawn.areas.rail.width, 3);
    assert_eq!(rig((98, 46)).ui.drawn.areas.rail.width, 22);
    let mut r = rig((80, 24));
    assert!(!r.text().contains("ADVISOR"));
    focus_cards(&mut r);
    assert_eq!(names(&r.press('a')), ["Ui(Focus(Advisor))", "Ui(ToggleRail)"]);
    assert!(r.text().contains("ADVISOR"), "{}", r.text());
    assert_frame!(r.frame(), "shell_rail_open_80x24");
    // On the open rail `a` only keeps the focus there; the strip toggles back.
    assert_eq!(names(&r.press('a')), ["Ui(Focus(Advisor))"]);
    r.app.step(Msg::Ui(UiIntent::ToggleRail)).unwrap();
    r.settle();
    assert!(!r.text().contains("ADVISOR"));
}

#[test]
fn the_ledger_segment_needs_100_columns() {
    for (cols, shown) in [(99, false), (100, true), (120, true)] {
        let mut app = replay_app();
        app.step(Msg::Command(Command::LedgerUpdate {
            section: "goal".into(),
            text: "ship it".into(),
        }))
        .unwrap();
        let r = Rig::new(app, (cols, 40), Theme::recording());
        let status = last_line(&r.text()).to_owned();
        assert_eq!(status.contains("ledger: ship it"), shown, "{cols}: {status}");
    }
}

#[test]
fn under_30_rows_the_log_has_3_rows_and_the_fork_layer_loses_its_footer() {
    for (rows, log_rows, footer) in [(24, 3, false), (29, 3, false), (30, 4, true), (46, 4, true)] {
        let r = rig((98, rows));
        assert_eq!(r.ui.drawn.log.visible, log_rows, "{rows} rows");
        assert_eq!(r.text().contains("sharp → runs in code"), footer, "{rows} rows");
    }
}

// ---- focus, keys, prompt ------------------------------------------------------------------

#[test]
fn tab_cycles_prompt_cards_log_rail_prompt_and_shift_tab_goes_back() {
    let mut r = rig((98, 46));
    let mut seen = vec![focus(&r)];
    for _ in 0..4 {
        r.key(K::Tab);
        seen.push(focus(&r));
    }
    assert_eq!(seen, ["input", "cards", "log", "advisor", "input"]);
    r.key_with(K::BackTab, KeyModifiers::SHIFT);
    assert_eq!(focus(&r), "advisor");
    // The focused pane shows it: the log border takes the focus colour.
    r.key(K::Tab);
    r.key(K::Tab);
    r.key(K::Tab);
    assert_eq!(focus(&r), "log");
    let log = r.ui.drawn.areas.log;
    assert_eq!(r.cell(log.x, log.y + 2).fg, Theme::recording().focus);
}

#[test]
fn the_prompt_takes_text_and_plain_keys_are_not_commands_there() {
    let mut r = rig((98, 46));
    for c in "tqa?x".chars() {
        assert!(r.press(c).is_empty(), "{c} was a command in the prompt");
    }
    assert_eq!(r.ui.prompt.text, "tqa?x");
    assert!(r.text().contains("tqa?x"), "{}", r.text());
    r.key(K::Backspace);
    assert_eq!(r.ui.prompt.text, "tqa?");
    r.key(K::Esc);
    assert_eq!(r.ui.prompt.text, "");
    for c in "hi".chars() {
        r.press(c);
    }
    assert_eq!(names(&r.key(K::Enter)), ["SubmitPrompt { text: \"hi\" }"]);
    assert_eq!(r.ui.prompt.text, "");
    // An empty prompt sends nothing.
    assert!(r.key(K::Enter).is_empty());
}

#[test]
fn aibus_opens_with_an_empty_prompt_and_its_placeholder() {
    let mut r = rig((80, 24));
    assert_eq!(r.ui.prompt.text, "");
    r.ui.prompt.placeholder = "aibus --model sonnet".into();
    r.ui.prompt.cwd = "~/proj".into();
    r.settle();
    let text = r.text();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[lines.len() - 2], " ~/proj $ aibus --model sonnet \u{258c}");
}

#[test]
fn dashboard_keys_do_what_the_table_says() {
    let mut r = rig((98, 46));
    focus_cards(&mut r);
    assert!(names(&r.press('t')).iter().all(|m| m.starts_with("Command(OpenTranscript(")));
    r.app.step(Msg::Command(Command::CloseModal)).unwrap();
    r.settle();
    assert_eq!(names(&r.press(':')), ["Command(OpenPalette)"]);
    r.key(K::Esc);
    assert_eq!(names(&r.press('x')).len(), 1, "stop the selected run");
    assert!(names(&r.press('x'))[0].starts_with("Command(Cancel"));
    assert_eq!(names(&r.press('q')), ["Command(Quit { force: false })"]);
    assert!(r.app.should_quit());
}

// ---- help (05 section 3) ----------------------------------------------------------------

#[test]
fn help_is_built_from_the_live_key_map_and_any_key_closes_it() {
    for size in [(98, 46), (80, 24)] {
        let mut r = rig(size);
        focus_cards(&mut r);
        assert_eq!(names(&r.press('?')), ["Command(OpenHelp)"]);
        let text = r.text();
        for row in r.ui.keys.help_rows() {
            assert!(text.contains(row.text), "{size:?}: help lacks `{}`", row.text);
            assert!(text.contains(&row.keys), "{size:?}: help lacks keys `{}`", row.keys);
        }
        assert!(text.contains("any key closes"));
        if size == (98, 46) {
            assert_frame!(r.frame(), "shell_help_98x46");
        } else {
            assert_frame!(r.frame(), "shell_help_80x24");
        }
        assert_eq!(names(&r.press('z')), ["Command(CloseModal)"]);
        assert_eq!(r.app.graph.read_key(MODAL), Some(&Value::Null));
        assert!(!r.text().contains("any key closes"));
    }
}

#[test]
fn a_rebound_key_shows_in_help_and_works() {
    let mut r = rig((98, 46));
    let (keys, warnings) = Keymap::with_overrides(&[("open_help".into(), vec!["F1".into()])]);
    assert!(warnings.is_empty());
    r.ui.keys = keys;
    focus_cards(&mut r);
    assert!(r.press('?').is_empty(), "the old key is gone");
    assert_eq!(names(&r.key(K::F(1))), ["Command(OpenHelp)"]);
    assert!(r.text().contains("F1"), "{}", r.text());
    // Every key closes it, including Esc and a click.
    assert_eq!(names(&r.key(K::Esc)), ["Command(CloseModal)"]);
    r.key(K::F(1));
    let (c, row) = r.centre(aibus_tui::hit::HitId::Modal);
    assert_eq!(names(&r.down(c, row)), ["Command(CloseModal)"]);
}

// ---- cards that do not fit ----------------------------------------------------------------

#[test]
fn more_than_three_cards_scroll_with_the_selection() {
    let mut r = Rig::new(heavy_app(), (98, 46), Theme::recording());
    assert!(r.text().contains("4 more ▸"), "{}", r.text());
    assert!(!r.text().contains("◂"));
    r.app.step(Msg::Ui(UiIntent::Select(Pane::Cards, Selection::Index(5)))).unwrap();
    r.settle();
    let t = r.text();
    assert!(t.contains("◂ 3 more"), "{t}");
    assert!(t.contains("1 more ▸"), "{t}");
    assert_eq!(r.ui.drawn.cards, 7);
    assert!(r.ui.drawn.hits.find(aibus_tui::hit::HitId::Card(5)).is_some());
    assert!(r.ui.drawn.hits.find(aibus_tui::hit::HitId::Card(0)).is_none());
}

// ---- themes, colour modes and the glyph set -----------------------------------------------

fn cells(r: &Rig) -> Vec<ratatui::buffer::Cell> {
    r.term.backend().buffer().content.clone()
}

#[test]
fn the_light_theme_draws_the_same_frame_in_its_own_colours() {
    let dark = rig((98, 46)).frame();
    let light = Rig::new(replay_app(), (98, 46), Theme::light()).frame();
    assert_eq!(light.text, dark.text);
    assert_styles!(light, "shell_98x46_light");
    assert!(light.styles.contains("#f4f4f8"));
}

#[test]
fn no_color_draws_with_the_terminals_own_colours() {
    let theme = Theme::resolve("recording", Some("1"), Some("truecolor"), false);
    let r = Rig::new(replay_app(), (98, 46), theme);
    for c in cells(&r) {
        assert_eq!((c.fg, c.bg), (Color::Reset, Color::Reset), "{:?}", c.symbol());
    }
    assert_eq!(r.text(), rig((98, 46)).text());
}

#[test]
fn without_truecolor_every_colour_is_a_256_palette_index() {
    let theme = Theme::resolve("recording", None, Some("256color"), false);
    let r = Rig::new(replay_app(), (98, 46), theme);
    let all = cells(&r);
    assert!(all.iter().all(|c| !matches!(c.fg, Color::Rgb(..)) && !matches!(c.bg, Color::Rgb(..))));
    assert!(all.iter().any(|c| matches!(c.bg, Color::Indexed(_))));
    assert_eq!(r.text(), rig((98, 46)).text());
}

#[test]
fn ascii_swaps_every_glyph_and_widens_nothing_else() {
    let theme = Theme::resolve("recording", None, Some("truecolor"), true);
    let r = Rig::new(replay_app(), (98, 46), theme);
    let t = r.text();
    assert!(t.is_ascii(), "{}", t.chars().filter(|c| !c.is_ascii()).collect::<String>());
    assert!(t.contains("ok done"), "{t}");
    assert!(t.contains(">> plan skips auth"), "{t}");
    assert!(t.contains("sharp -> runs in code"), "{t}");
    assert!(t.contains("+-"), "borders are plus and minus");
    // The same layout: every line is as long as the unicode frame's.
    let wide = rig((98, 46)).text();
    for (a, b) in t.lines().zip(wide.lines()) {
        assert!(a.chars().count() <= b.chars().count() + 3, "{a:?} vs {b:?}");
    }
}
