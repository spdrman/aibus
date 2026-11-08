use aibus_tui::areas;
use ratatui::layout::Rect;

#[test]
fn row_budget_at_98x46() {
    let a = areas(Rect::new(0, 0, 98, 46));
    assert_eq!((a.header.height, a.legend.height), (2, 1));
    assert_eq!(a.rail.height, 30);
    assert_eq!(a.centre.height, 30);
    assert_eq!(a.log.height, 6);
    assert_eq!((a.prompt.height, a.status.height), (1, 1));
    assert_eq!(a.rail.width, 22);
    // In order, and the bottom row is the status bar.
    assert_eq!(a.status.bottom(), 46);
    assert!(a.header.bottom() <= a.legend.y);
    assert!(a.legend.bottom() <= a.rail.y);
    assert!(a.rail.bottom() <= a.log.y);
    assert!(a.log.bottom() <= a.prompt.y);
    assert!(a.prompt.bottom() <= a.status.y);
    // 41 content rows leave 5 spacing rows.
    let used = a.header.height + a.legend.height + a.rail.height + a.log.height + 2;
    assert_eq!(46 - used, 5);
}

#[test]
fn small_terminals_collapse_the_rail_and_shrink_the_log() {
    let a = areas(Rect::new(0, 0, 80, 24));
    assert_eq!(a.rail.width, 3);
    assert_eq!(a.log.height, 5, "border plus 3 rows");
    assert_eq!(a.status.bottom(), 24);
    let tall = areas(Rect::new(0, 0, 120, 50));
    assert_eq!(tall.log.height, 10, "extra rows go to the log");
    assert_eq!(tall.centre.height, 30);
}
