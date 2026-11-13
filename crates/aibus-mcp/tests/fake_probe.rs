//! #232: `common::fake_aibus` makes sure the script it just wrote can be started, and that
//! readiness check must leave nothing behind: no `argv.txt`, `env.txt` or `session/` (tests assert
//! that no run was started), and no process that keeps writing after the helper has returned.
#![cfg(unix)]

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[test]
fn the_readiness_check_of_a_fake_script_leaves_no_trace_under_load() {
    // Sets `stop` on every way out, so a panic does not leave the busy threads spinning.
    struct Stop(Arc<AtomicBool>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    let _stop_on_exit = Stop(stop.clone());
    let cpus = std::thread::available_parallelism().map_or(4, |n| n.get());
    let mut background = Vec::new();
    // Four threads that fork in a loop, and twice as many busy threads as there are cpus, so a
    // freshly started shell is slow to get going.
    for _ in 0..4 {
        let stop = stop.clone();
        background.push(std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let _ = std::process::Command::new("true").status();
            }
        }));
    }
    for _ in 0..cpus * 2 {
        let stop = stop.clone();
        background.push(std::thread::spawn(move || {
            let mut x = 0u64;
            while !stop.load(Ordering::Relaxed) {
                x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
            }
        }));
    }
    let root = aibus_testkit::xdg_temp();
    let dirs: Vec<_> = (0..8)
        .map(|w| {
            let root = root.path().to_owned();
            std::thread::spawn(move || {
                (0..100)
                    .map(|i| {
                        let dir = root.join(format!("{w}-{i}"));
                        std::fs::create_dir(&dir).unwrap();
                        common::fake_aibus(&dir);
                        dir
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let dirs: Vec<_> = dirs.into_iter().flat_map(|w| w.join().unwrap()).collect();
    stop.store(true, Ordering::Relaxed);
    for t in background {
        t.join().unwrap();
    }
    // A process that outlived the helper would still be writing here.
    std::thread::sleep(Duration::from_millis(300));
    let mut left: Vec<String> = Vec::new();
    for d in &dirs {
        for f in ["argv.txt", "env.txt", "session"] {
            if d.join(f).exists() {
                left.push(format!("{}/{f}", d.file_name().unwrap().to_string_lossy()));
            }
        }
    }
    assert!(
        left.is_empty(),
        "{} files left behind by the readiness check of 800 scripts, e.g. {:?}",
        left.len(),
        &left[..left.len().min(3)]
    );
}

#[test]
fn a_real_run_still_runs_when_the_caller_has_the_probe_marker_in_its_environment() {
    // A real run is started by product code, which passes the caller's environment on, so the
    // marker of the readiness check must not be something a caller's environment can carry.
    let dir = aibus_testkit::xdg_temp();
    let fake = common::fake_aibus(dir.path());
    let status = std::process::Command::new(&fake)
        .args(["--cwd", "/", "--json", "--", "ok"])
        .env("AIBUS_FAKE_PROBE", "1")
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    assert!(
        dir.path().join("argv.txt").exists(),
        "the fake exited before its body ran"
    );
}
