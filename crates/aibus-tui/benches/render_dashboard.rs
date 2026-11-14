//! One dashboard render (view read, widget draw, ratatui diff) on a `TestBackend`, against the
//! A1.3 reference session (8 runs, a 100,000-line transcript, 2,000 log rows). A gate
//! (10 section 9):
//!
//! * a tick commit plus the redraw it causes: under 3 ms p99 at 98x46 (08 section 1),
//! * a resize, which repaints the whole buffer, the worst case: under 10 ms p99,
//! * neither may regress over 20 % against `benches/baselines/<runner class>/`.
//!
//! `AIBUS_BENCH_GATE=1` makes a miss fail, see `benches/gate.rs` for the rest.

use std::time::Instant;

use aibus_core::{App, Msg};
use aibus_graph::mini::MiniGraph;
use aibus_testkit::tui::{heavy_app, replay_app};
use aibus_tui::{Theme, Ui};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

#[path = "../../../benches/gate.rs"]
mod gate;

const RENDER_BUDGET_US: f64 = 3000.0;
const RESIZE_BUDGET_US: f64 = 10_000.0;
const SAMPLES: usize = 3000;
const ROUNDS: usize = 3;

fn measure(gate: &mut gate::Gate, name: &str, mut app: App<MiniGraph>, size: (u16, u16)) {
    let mut term = Terminal::new(TestBackend::new(size.0, size.1)).unwrap();
    let mut ui = Ui::new(Theme::recording());
    ui.note_commit(&app.graph);
    ui.draw(&mut term, &app.graph).unwrap();
    // A tick moves the running cards' spinners, so the draw is real and the diff is small.
    // This sample also counts the commit that recomputes the view nodes.
    let render = gate::Gate::best_of(ROUNDS, || {
        (0..SAMPLES)
            .map(|i| {
                let start = Instant::now();
                app.step(Msg::Tick).unwrap();
                ui.note_commit(&app.graph);
                let drew = ui.draw(&mut term, &app.graph).unwrap();
                let took = start.elapsed().as_nanos() as f64 / 1000.0;
                assert!(drew, "sample {i} did not draw");
                took
            })
            .collect()
    });
    // Resizing back and forth makes the backend repaint the whole buffer.
    let resize = gate::Gate::best_of(ROUNDS, || {
        (0..SAMPLES)
            .map(|i| {
                let (w, h) = if i % 2 == 0 {
                    (size.0 - 1, size.1)
                } else {
                    size
                };
                term.backend_mut().resize(w, h);
                let start = Instant::now();
                let drew = ui.draw(&mut term, &app.graph).unwrap();
                let took = start.elapsed().as_nanos() as f64 / 1000.0;
                assert!(drew, "resize sample {i} did not draw");
                took
            })
            .collect()
    });
    gate.judge(
        &format!("render_dashboard-{name}-render"),
        &render,
        RENDER_BUDGET_US,
    );
    gate.judge(
        &format!("render_dashboard-{name}-resize"),
        &resize,
        RESIZE_BUDGET_US,
    );
}

fn main() {
    let mut gate = gate::Gate::new();
    measure(&mut gate, "replay-98x46", replay_app(), (98, 46));
    measure(&mut gate, "heavy-98x46", heavy_app(), (98, 46));
    measure(&mut gate, "heavy-80x24", heavy_app(), (80, 24));
    gate.finish();
}
