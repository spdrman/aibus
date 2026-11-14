//! 08 section 2, soft: eight runs streaming at once with the dashboard open should use under
//! 30 % of one core. The load is what the coalescer sends: one `Msg::Stream` per run every
//! 16 ms, the tick at 30 Hz and a draw after every commit that dirtied a widget, on the A1.3
//! reference session. Busy time over simulated time is the share of one core.
//!
//! The share is logged (run with `--nocapture`); only "cannot keep up" (over 100 %) fails,
//! since the wall clock of a shared runner stretches it.

use std::time::{Duration, Instant};

use aibus_core::Msg;
use aibus_provider::StreamChunk;
use aibus_testkit::tui::{heavy_app, run_id};
use aibus_tui::{Theme, Ui};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

const SIMULATED: Duration = Duration::from_secs(10);
const FRAME: Duration = Duration::from_millis(16);

#[test]
fn eight_runs_streaming_with_the_dashboard_open() {
    let mut app = heavy_app();
    let mut term = Terminal::new(TestBackend::new(98, 46)).unwrap();
    let mut ui = Ui::new(Theme::recording());
    ui.note_commit(&app.graph);
    ui.draw(&mut term, &app.graph).unwrap();

    let frames = SIMULATED.as_millis() / FRAME.as_millis();
    let (mut busy, mut commits, mut draws) = (Duration::ZERO, 0u32, 0u32);
    for frame in 0..frames {
        let start = Instant::now();
        for seq in 1..=8u128 {
            app.step(Msg::Stream {
                run: run_id(seq),
                chunks: vec![StreamChunk::TextDelta(format!(
                    "frame {frame:05} run {seq} the quick brown fox jumps over the lazy dog\n"
                ))],
            })
            .unwrap();
            commits += 1;
        }
        // 30 ticks a second is every second frame of 16 ms, give or take.
        if frame % 2 == 0 {
            app.step(Msg::Tick).unwrap();
            commits += 1;
        }
        ui.note_commit(&app.graph);
        if ui.draw(&mut term, &app.graph).unwrap() {
            draws += 1;
        }
        busy += start.elapsed();
    }
    let share = busy.as_secs_f64() / SIMULATED.as_secs_f64();
    println!(
        "eight runs streaming, dashboard open: {:.1} % of one core over {SIMULATED:?} ({commits} commits, {draws} draws, busy {busy:?}), target under 30 %",
        share * 100.0
    );
    assert!(draws > 0, "the dashboard drew");
    assert!(share < 1.0, "cannot keep up: {:.0} % of one core", share * 100.0);
}
