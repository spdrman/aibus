//! 08 section 5: a session of 12 simulated hours with 300 runs stays under 500 MiB resident,
//! and every cap in the table holds all the way through. The clock is a counter the test moves
//! (no sleeping), `eng:mem` is sampled every 10 simulated seconds as the TUI loop does, and the
//! providers are not involved: each run is a `RunStarted`, a stream of `Msg::Stream`s and a
//! `RunFinished`, which is all the store and the graph ever see of it.
//!
//! The resident number comes from `/proc/self/statm`, so it is only asserted on Linux (the
//! containers); elsewhere the caps are still checked. This file holds one test on purpose: the
//! process-wide resident size must be this session's and nothing else's.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use aibus_core::persist::session::{write_snapshot, SNAPSHOT};
use aibus_core::state::derive::transcript_rows;
use aibus_core::state::store::{LOG_RING, SUMMARY_HEAD, TAIL_MAX};
use aibus_core::state::run_key;
use aibus_core::*;
use aibus_graph::mini::MiniGraph;
use aibus_testkit::xdg_temp;
use aibus_graph::{Graph, Value, COMMIT_HISTORY_CAP};
use aibus_provider::StreamChunk;

const HOURS: u64 = 12;
const RUNS: usize = 300;
/// `max_concurrent_runs`.
const CONCURRENT: usize = 8;
const SAMPLE_EVERY_MS: u64 = 10_000;
const WIDTH: usize = 100;
const MEM_BUDGET_MIB: u32 = 500;
const SNAPSHOT_BUDGET: u64 = 2 * 1024 * 1024;
const MODEL_CACHE_BUDGET: u64 = 20 * 1024 * 1024;

/// Resident set in MiB where `/proc` has it.
fn rss_mib() -> Option<u32> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    u32::try_from(pages * 4096 / (1024 * 1024)).ok()
}

struct Live {
    run: RunId,
    entry: u64,
    streamed: usize,
}

#[test]
fn twelve_hours_and_three_hundred_runs_stay_inside_every_cap() {
    let clock = Arc::new(AtomicU64::new(1_700_000_000_000));
    let now = clock.clone();
    let mut app = App::new(MiniGraph::new(), move || now.load(Ordering::SeqCst));
    app.step(Msg::Resize { cols: WIDTH as u16, rows: 46 }).unwrap();

    let total_ms = HOURS * 3_600_000;
    let gap_ms = total_ms / RUNS as u64;
    let mut live: VecDeque<Live> = VecDeque::new();
    let (mut started, mut finished, mut samples) = (0usize, 0usize, 0u64);
    let (mut peak_mib, mut peak_content) = (0u32, 0usize);
    let mut t = 0u64;

    let text = |n: usize| format!("step {n:08} the quick brown fox jumps over the lazy dog and keeps going a while\n").repeat(128);

    while t < total_ms {
        // A new run each `gap_ms`; the oldest one ends when eight are running.
        if t >= started as u64 * gap_ms && started < RUNS {
            if live.len() == CONCURRENT {
                let old = live.pop_front().unwrap();
                finish(&mut app, &old);
                finished += 1;
                assert_finished_run_is_only_a_head(&app, &old);
            }
            let run = RunId::new();
            app.step(Msg::RunStarted {
                run,
                agent: "worker".parse().unwrap(),
                role: Role::Worker,
                parent: None,
                model: ModelId("m".into()),
                account: AccountName("a".into()),
            })
            .unwrap();
            let entry = match app.graph.read_key(&run_key(run, "blocks")) {
                Some(Value::Handle(h)) => h.id,
                other => panic!("no transcript handle for the new run: {other:?}"),
            };
            // The transcript view is open on every thirtieth run, as a derived of its own.
            if started % 30 == 0 {
                let key = format!("view:transcript:{run}:lines");
                app.graph.derived(
                    &key.as_str().into(),
                    transcript_rows(app.store.clone(), run.to_string(), WIDTH),
                );
            }
            live.push_back(Live { run, entry, streamed: 0 });
            started += 1;
        }

        // Each running run streams a batch per step; the tail seals blocks past 64 KiB.
        for l in &mut live {
            let batch = text(t as usize / 1000);
            l.streamed += batch.len();
            app.step(Msg::Stream {
                run: l.run,
                chunks: vec![StreamChunk::TextDelta(batch)],
            })
            .unwrap();
        }

        // Something worth a log row every 20 simulated seconds, so the ring fills and wraps.
        if (t / SAMPLE_EVERY_MS) % 2 == 0 {
            app.step(Msg::IoError { context: format!("step {t}"), error: "e".into() }).unwrap();
        }

        // `eng:mem`: one step is 10 simulated seconds, one sample.
        if let Some(mib) = rss_mib() {
            peak_mib = peak_mib.max(mib);
            app.step(Msg::Ui(UiIntent::Mem(mib))).unwrap();
        }
        samples += 1;

        // The caps, at every step: the log ring, the commit ring, and the store holding no more
        // than what the running runs hold plus a 2 KiB head for each run ever started.
        let stats = app.store.stats();
        assert!(stats.log_rows <= LOG_RING, "log ring: {} rows", stats.log_rows);
        assert!(
            app.graph.commit_log().len() <= COMMIT_HISTORY_CAP,
            "commit ring: {}",
            app.graph.commit_log().len()
        );
        let held: usize = live.iter().map(|l| l.streamed).sum();
        assert!(
            stats.content_bytes <= held + started * SUMMARY_HEAD,
            "the store holds {} bytes; running runs streamed {held}, {started} heads",
            stats.content_bytes
        );
        peak_content = peak_content.max(stats.content_bytes);
        // The wrapped-line cache holds blocks of runs that are still running, at the one width.
        assert!(stats.wrap_width == 0 || stats.wrap_width == WIDTH);
        assert!(
            stats.wrap_blocks <= live.iter().map(|l| app.store.blocks(l.entry).len()).sum::<usize>(),
            "wrapped caches outlived their blocks"
        );

        t += SAMPLE_EVERY_MS;
        clock.store(1_700_000_000_000 + t, Ordering::SeqCst);
    }
    while let Some(old) = live.pop_front() {
        finish(&mut app, &old);
        finished += 1;
        assert_finished_run_is_only_a_head(&app, &old);
    }
    assert_eq!((started, finished), (RUNS, RUNS));
    assert_eq!(samples, total_ms / SAMPLE_EVERY_MS, "eng:mem is sampled every 10 s");

    // Every ring is full by now, and nothing but 2 KiB heads is left in the store.
    let stats = app.store.stats();
    assert_eq!(stats.log_rows, LOG_RING);
    assert_eq!(app.graph.commit_log().len(), COMMIT_HISTORY_CAP);
    assert_eq!(stats.wrap_blocks, 0, "finished runs keep no wrapped lines");
    assert!(
        stats.content_bytes <= RUNS * SUMMARY_HEAD,
        "{} bytes left for {RUNS} finished runs",
        stats.content_bytes
    );

    // The width a cache is kept for is the current one: asking at another drops the rest.
    let probe = app.store.new_transcript();
    app.store.append(probe, &"y".repeat(TAIL_MAX + 40 * 1024));
    let block = app.store.blocks(probe)[0].clone();
    app.store.wrapped(&block, 80);
    app.store.wrapped(&block, 100);
    let stats = app.store.stats();
    assert_eq!((stats.wrap_width, stats.wrap_blocks), (100, 1));

    // The snapshot is handles only: well under 2 MiB on disk after a whole day.
    let dir = xdg_temp();
    write_snapshot(dir.path(), &app.graph.snapshot()).unwrap();
    let snapshot = std::fs::metadata(dir.path().join(SNAPSHOT)).unwrap().len();
    println!("snapshot on disk: {snapshot} bytes (budget {SNAPSHOT_BUDGET})");
    assert!(snapshot < SNAPSHOT_BUDGET);

    // The model tables the providers ship are the model caches' floor; refreshed lists are the
    // same shape. Together they are far under 20 MiB.
    let tables: u64 = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))
        .unwrap()
        .flatten()
        .filter(|d| d.file_name().to_string_lossy().starts_with("aibus-provider"))
        .flat_map(|d| std::fs::read_dir(d.path().join("src")).into_iter().flatten().flatten())
        .filter(|f| f.path().extension().is_some_and(|e| e == "json"))
        .map(|f| f.metadata().unwrap().len())
        .sum();
    println!("model tables: {tables} bytes (budget {MODEL_CACHE_BUDGET})");
    assert!(tables < MODEL_CACHE_BUDGET);

    println!(
        "{RUNS} runs over {HOURS} h: peak resident {peak_mib} MiB (budget {MEM_BUDGET_MIB}), peak store content {} KiB, {samples} eng:mem samples",
        peak_content / 1024
    );
    match rss_mib() {
        Some(end) => {
            assert!(peak_mib < MEM_BUDGET_MIB && end < MEM_BUDGET_MIB, "peak {peak_mib} MiB, end {end} MiB");
            assert!(matches!(app.graph.read_key("ctrl:mem"), Some(Value::I64(m)) if *m > 0));
        }
        None => println!("no /proc here: the resident size was not measured"),
    }
}

fn finish<G: Graph>(app: &mut App<G>, l: &Live) {
    app.step(Msg::RunFinished {
        run: l.run,
        result: RunResult {
            run_id: l.run,
            agent: "worker".parse().unwrap(),
            status: RunStatus::Completed,
            turns: 1,
            usage: Usage::default(),
            summary: String::new(),
            summary_file: None,
            duration: Duration::from_secs(1),
            error: None,
            worktree: None,
        },
    })
    .unwrap();
}

/// A finished run drops its `Conversation` (the run task ends) and its store content: no
/// blocks, no tail, and a head of at most 2 KiB.
fn assert_finished_run_is_only_a_head<G: Graph>(app: &App<G>, l: &Live) {
    assert!(app.store.blocks(l.entry).is_empty(), "blocks kept for a finished run");
    assert!(app.store.tail(l.entry).is_empty(), "tail kept for a finished run");
    assert!(app.store.head(l.entry).len() <= SUMMARY_HEAD);
    assert!(!app.store.head(l.entry).is_empty(), "the card keeps its head");
}
