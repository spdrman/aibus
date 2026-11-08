use aibus_testkit::tui::{render, replay_app};
use aibus_testkit::{assert_frame, assert_styles};

#[test]
fn frame_98x46() {
    let mut app = replay_app();
    let f = render(&mut app, (98, 46), 0);
    assert_frame!(f, "dashboard_98x46");
    assert_styles!(f, "dashboard_98x46");
}

#[test]
fn frame_80x24() {
    let mut app = replay_app();
    assert_frame!(render(&mut app, (80, 24), 0), "dashboard_80x24");
}

/// The spinner phase comes from the injected tick, so a later tick draws a different glyph.
#[test]
fn frame_98x46_after_two_ticks() {
    let mut app = replay_app();
    let f = render(&mut app, (98, 46), 2);
    assert_frame!(f, "dashboard_98x46_tick2");
}

#[test]
fn every_size_renders_without_panicking() {
    use aibus_core::App;
    use aibus_graph::mini::MiniGraph;
    for size in [(80, 24), (98, 46), (120, 50), (40, 10), (10, 3), (1, 1)] {
        let mut app = App::new(MiniGraph::new(), || 0);
        let keys = aibus_tui::register(&mut app.graph, app.store.clone());
        app.keys.extend(keys);
        render(&mut app, size, 0);
        let mut busy = replay_app();
        render(&mut busy, size, 1);
    }
}
