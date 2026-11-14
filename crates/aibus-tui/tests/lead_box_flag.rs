//! Issue #198: the right-aligned model flag of the main session box must not paint over the
//! effort word. In a 36-cell inner row `effort ▮▮▮▮ high` takes 16 cells and `defaults.model
//! sonnet` takes 21, so the two used to overlap and the row read `higdefaults.model sonnet`.

use aibus_core::state::value::{map, s};
use aibus_graph::Value;
use aibus_tui::widgets::lead_box;
use aibus_tui::Theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

fn row(buf: &Buffer, y: u16, width: u16) -> String {
    (0..width)
        .map(|x| buf[(x, y)].symbol().to_owned())
        .collect()
}

fn lead(flag: &str) -> Value {
    map([
        ("model_label", s("claude-sonnet-4-5")),
        ("label", s("EIGHT")),
        ("effort", s("high")),
        ("model_flag", s(flag)),
        ("highlight", Value::Bool(false)),
    ])
}

#[test]
fn a_long_model_flag_leaves_the_effort_word_intact_and_a_gap() {
    let area = Rect::new(0, 0, 40, 4);
    let mut buf = Buffer::empty(area);
    lead_box(
        &mut buf,
        area,
        &lead("defaults.model sonnet"),
        &Theme::recording(),
    );
    let line = row(&buf, 2, 40);
    assert!(
        line.contains("effort ▮▮▮▮ high "),
        "the effort word was overwritten: {line:?}"
    );
    assert!(!line.contains("higdefaults"), "{line:?}");
    assert!(
        line.trim_end_matches(['│', ' ']).ends_with(['n', '…']),
        "{line:?}"
    );
}

#[test]
fn a_short_flag_stays_flush_right_untouched() {
    let area = Rect::new(0, 0, 40, 4);
    let mut buf = Buffer::empty(area);
    lead_box(&mut buf, area, &lead("--model sonnet"), &Theme::recording());
    let line = row(&buf, 2, 40);
    assert!(line.contains("effort ▮▮▮▮ high"), "{line:?}");
    assert!(line.contains("--model sonnet │"), "{line:?}");
}
