//! The p99 gate shared by the benches (10 section 9), included with `#[path]` so the causl
//! crate, which is outside the workspace, can use it too. It only needs `std`.
//!
//! Every number is a per-iteration time in microseconds. `Gate::best_of` runs a measurement a
//! few times and keeps the round with the lowest p99: load on the machine only ever adds time,
//! so the minimum over rounds is the least noisy estimate. `Gate::judge` then compares it with
//! the absolute budget from 08, and with the committed baseline for the runner class. Because a
//! busy runner slows everything alike, the baseline is scaled by how long a fixed piece of work
//! (`calibration_us`) took now against when the baseline was saved, and a small absolute floor
//! (`NOISE_FLOOR_US`) is added to what counts as a 20 % regression.
//!
//! * `AIBUS_BENCH_GATE=1`: exit non-zero over budget or over 20 % regression. Without it a miss
//!   is only printed, so it is still recorded.
//! * `AIBUS_BENCH_SAVE=1`: write the baseline for this runner class.
//! * `AIBUS_RUNNER_CLASS`: the directory under `benches/baselines/` (default `local`).
#![allow(dead_code)]

use std::hint::black_box;
use std::path::PathBuf;
use std::time::Instant;

/// A p99 more than this far over the (scaled) baseline fails.
pub const REGRESSION: f64 = 1.20;

/// Added on top of that, because a p99 of a few tens of microseconds moves by more than 20 %
/// between two quiet runs; the budgets this guards are in milliseconds.
pub const NOISE_FLOOR_US: f64 = 25.0;

pub struct Stats {
    pub p50: f64,
    pub p99: f64,
    pub max: f64,
}

pub struct Gate {
    dir: PathBuf,
    enforce: bool,
    save: bool,
    calibration_us: f64,
    failed: Vec<String>,
}

impl Gate {
    pub fn new() -> Self {
        /// Time of a fixed workload (FNV over 256 KiB, eight times), the best of 15. It does not touch
        /// aibus code, so it moves with the machine and not with the commit.
        fn calibrate() -> f64 {
            let buf: Vec<u8> = (0..256 * 1024u32).map(|i| (i % 251) as u8).collect();
            (0..15)
                .map(|_| {
                    let start = Instant::now();
                    for _ in 0..8 {
                        let mut h = 0xcbf2_9ce4_8422_2325u64;
                        for b in black_box(&buf) {
                            h = (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
                        }
                        black_box(h);
                    }
                    start.elapsed().as_nanos() as f64 / 1000.0
                })
                .fold(f64::MAX, f64::min)
        }
        let class = std::env::var("AIBUS_RUNNER_CLASS").unwrap_or_else(|_| "local".into());
        let gate = Self {
            dir: [env!("CARGO_MANIFEST_DIR"), "../../benches/baselines", &class]
                .iter()
                .collect(),
            enforce: std::env::var("AIBUS_BENCH_GATE").as_deref() == Ok("1"),
            save: std::env::var("AIBUS_BENCH_SAVE").as_deref() == Ok("1"),
            calibration_us: calibrate(),
            failed: Vec::new(),
        };
        println!(
            "runner class {class}, calibration {:.0} us, gate {}, save {}",
            gate.calibration_us, gate.enforce, gate.save
        );
        gate
    }

    /// Runs `round` `rounds` times and keeps the statistics of the one with the lowest p99.
    pub fn best_of(rounds: usize, mut round: impl FnMut() -> Vec<f64>) -> Stats {
        (0..rounds)
            .map(|_| {
                let mut samples = round();
                samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let at = |p: usize| samples[(samples.len() * p / 100).min(samples.len() - 1)];
                Stats {
                    p50: at(50),
                    p99: at(99),
                    max: samples[samples.len() - 1],
                }
            })
            .min_by(|a, b| a.p99.partial_cmp(&b.p99).unwrap())
            .expect("at least one round")
    }

    fn baseline(&self, name: &str) -> Option<(f64, f64)> {
        let text = std::fs::read_to_string(self.dir.join(format!("{name}.txt"))).ok()?;
        let field = |key: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(key)?.trim().parse::<f64>().ok())
        };
        Some((field("p99_us ")?, field("calibration_us ")?))
    }

    /// Prints `name`, checks it against `budget_us` and the baseline, saves it if asked.
    pub fn judge(&mut self, name: &str, s: &Stats, budget_us: f64) {
        println!(
            "{name}: p50 {:.0} us, p99 {:.0} us, max {:.0} us (budget {budget_us:.0} us p99)",
            s.p50, s.p99, s.max
        );
        if s.p99 > budget_us {
            self.miss(format!(
                "{name}: p99 {:.0} us is over the {budget_us:.0} us budget",
                s.p99
            ));
        }
        match self.baseline(name) {
            Some((base, calibration)) => {
                let allowed =
                    base * (self.calibration_us / calibration) * REGRESSION + NOISE_FLOOR_US;
                println!(
                    "  baseline p99 {base:.0} us at calibration {calibration:.0} us, allowed {allowed:.0} us"
                );
                if s.p99 > allowed {
                    self.miss(format!(
                        "{name}: p99 {:.0} us is over {allowed:.0} us (baseline + 20 % + noise floor)",
                        s.p99
                    ));
                }
            }
            None => println!("  no baseline for this runner class (AIBUS_BENCH_SAVE=1 writes one)"),
        }
        if self.save {
            std::fs::create_dir_all(&self.dir).unwrap();
            std::fs::write(
                self.dir.join(format!("{name}.txt")),
                format!("p99_us {:.1}\ncalibration_us {:.1}\n", s.p99, self.calibration_us),
            )
            .unwrap();
        }
    }

    fn miss(&mut self, what: String) {
        println!("WARNING: {what}");
        self.failed.push(what);
    }

    /// Exits non-zero when the gate is on and anything missed.
    pub fn finish(self) {
        if self.enforce && !self.failed.is_empty() {
            eprintln!("bench gate failed:\n{}", self.failed.join("\n"));
            std::process::exit(1);
        }
    }
}
