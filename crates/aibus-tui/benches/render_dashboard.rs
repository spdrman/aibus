//! A2.5 spike: one dashboard render (view read, widget draw, ratatui diff) on a `TestBackend`
//! at 98x46, against the A1.3 reference session (8 runs, a 100,000-line transcript, 2,000 log
//! rows). Prints p50 / p99 / max in microseconds. Exits non-zero over budget only when
//! `AIBUS_BENCH_GATE=1`, so a miss is still recorded.

use std::time::Instant;

use aibus_core::{App, Msg};
use aibus_graph::mini::MiniGraph;
use aibus_testkit::tui::{heavy_app, replay_app};
use aibus_tui::{Theme, Ui};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

const BUDGET_US: f64 = 3000.0;
const SAMPLES: usize = 5000;

fn pct(sorted: &[f64], p: usize) -> f64 {
    sorted[(sorted.len() * p / 100).min(sorted.len() - 1)]
}

fn report(name: &str, mut samples: Vec<f64>) -> f64 {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p99 = pct(&samples, 99);
    println!(
        "{name}: p50 {:.0} us, p99 {:.0} us, max {:.0} us",
        pct(&samples, 50),
        p99,
        samples[samples.len() - 1]
    );
    p99
}

fn measure(name: &str, mut app: App<MiniGraph>, size: (u16, u16)) -> f64 {
    let mut term = Terminal::new(TestBackend::new(size.0, size.1)).unwrap();
    let mut ui = Ui::new(Theme::recording());
    ui.note_commit(&app.graph);
    ui.draw(&mut term, &app.graph).unwrap();
    let mut ticked = Vec::with_capacity(SAMPLES);
    let mut resized = Vec::with_capacity(SAMPLES);
    for i in 0..SAMPLES {
        // A tick moves the running cards' spinners, so the draw is real and the diff is small.
        // This sample also counts the commit that recomputes the view nodes.
        let start = Instant::now();
        app.step(Msg::Tick).unwrap();
        ui.note_commit(&app.graph);
        let drew = ui.draw(&mut term, &app.graph).unwrap();
        ticked.push(start.elapsed().as_nanos() as f64 / 1000.0);
        assert!(drew, "sample {i} did not draw");
        // Resizing back and forth makes the backend repaint the whole buffer, the worst case.
        let (w, h) = if i % 2 == 0 { (size.0 - 1, size.1) } else { size };
        term.backend_mut().resize(w, h);
        let start = Instant::now();
        ui.draw(&mut term, &app.graph).unwrap();
        resized.push(start.elapsed().as_nanos() as f64 / 1000.0);
    }
    let a = report(&format!("{name}, tick commit + redraw"), ticked);
    let b = report(&format!("{name}, resize redraw only"), resized);
    a.max(b)
}

fn main() {
    let worst = [
        measure("replay 98x46", replay_app(), (98, 46)),
        measure("A1.3 fixture 98x46", heavy_app(), (98, 46)),
        measure("A1.3 fixture 80x24", heavy_app(), (80, 24)),
    ]
    .into_iter()
    .fold(0.0, f64::max);
    println!("worst p99 {worst:.0} us, budget {BUDGET_US:.0} us");
    if std::env::var("AIBUS_BENCH_GATE").as_deref() == Ok("1") && worst > BUDGET_US {
        std::process::exit(1);
    }
}
