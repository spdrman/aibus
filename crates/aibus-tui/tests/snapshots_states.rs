//! C1.2, mouse acceptance: frame snapshots of the interaction states (hover, pressed, disabled,
//! menu open, tooltip) in the three themes, text and styles. The crop is the card row and what
//! hangs below it, which is where the menu and the tooltip open.

mod common;

use aibus_tui::hit::HitId;
use aibus_tui::Theme;
use common::*;
use ratatui::layout::Rect;

const THEMES: [(&str, Theme); 3] = [
    ("recording", Theme::recording()),
    ("light", Theme::light()),
    ("mono", Theme::mono()),
];

fn rig(theme: Theme) -> Rig {
    let mut app = session();
    worker(
        &mut app,
        2,
        "search",
        "maps the repo",
        Some(0.71),
        "running",
    );
    worker(
        &mut app,
        3,
        "grep",
        "finds call sites",
        Some(0.35),
        "partial",
    );
    worker(
        &mut app,
        4,
        "audit",
        "runs in a worktree",
        Some(1.0),
        "completed",
    );
    ticks(&mut app, 3);
    Rig::new(app, (98, 46), theme)
}

fn crop_of(r: &Rig) -> Rect {
    let card = r.rect(HitId::Card(0));
    let centre = r.ui.drawn.areas.centre;
    Rect {
        x: centre.x,
        y: card.y.saturating_sub(1),
        width: centre.width,
        height: 17,
    }
}

fn shot(state: &str, theme: &str, r: &Rig) {
    assert_crop(
        &r.frame(),
        crop_of(r),
        &format!("state_{state}_{theme}"),
        true,
    );
}

#[test]
fn hover() {
    for (name, theme) in THEMES {
        let mut r = rig(theme);
        let (c, row) = r.centre(HitId::Card(1));
        r.hover(c, row);
        shot("hover", name, &r);
    }
}

#[test]
fn pressed() {
    for (name, theme) in THEMES {
        let mut r = rig(theme);
        let (c, row) = r.centre(HitId::Card(1));
        r.down(c, row);
        shot("pressed", name, &r);
    }
}

/// The menu of a running run: Resume and Copy result do not apply, and the pointer rests on
/// Resume without lighting it.
#[test]
fn disabled() {
    for (name, theme) in THEMES {
        let mut r = rig(theme);
        let (c, row) = r.centre(HitId::Card(0));
        r.right_down(c, row);
        let (x, y) = r.find("Resume").unwrap();
        r.hover(x + 1, y);
        shot("disabled", name, &r);
    }
}

/// The menu of a partial run, with the pointer on Fork.
#[test]
fn menu_open() {
    for (name, theme) in THEMES {
        let mut r = rig(theme);
        let (c, row) = r.centre(HitId::Card(1));
        r.right_down(c, row);
        let (x, y) = r.find("Fork").unwrap();
        r.hover(x + 1, y);
        shot("menu", name, &r);
    }
}

#[test]
fn tooltip() {
    for (name, theme) in THEMES {
        let mut r = rig(theme);
        let (c, row) = r.centre(HitId::Card(1));
        r.hover(c, row);
        r.advance(500);
        shot("tooltip", name, &r);
    }
}
