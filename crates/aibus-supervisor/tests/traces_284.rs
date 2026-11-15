//! The recorded traces #284's views and replay are tested on: every spike run (#281) through the
//! normaliser and into a real `TraceStore`, kept under `fixtures/claude-code/traces-284/` as the
//! store wrote them. `11-foreground.retained` keeps payloads, so the inspector has message text and
//! tool output to show. `AIBUS_BLESS=1` rewrites them; otherwise each is rebuilt in a temporary
//! directory and has to match byte for byte.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use aibus_core::Redactor;
use aibus_trace::{PayloadPolicy, StoreLimits, TraceStore};
use common::*;

fn traces_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/claude-code/traces-284")
}

/// Every file under `dir`, relative, with its bytes, sorted.
fn files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p.strip_prefix(dir).unwrap().to_string_lossy().into_owned();
                out.push((rel, fs::read(&p).unwrap()));
            }
        }
    }
    out.sort();
    out
}

fn write_trace(dir: &Path, name: &str, feed: Feed, policy: PayloadPolicy) {
    let (batches, _) = normalise(&load(name), feed);
    let (mut store, _) =
        TraceStore::open(dir, Redactor::new(), policy, StoreLimits::default()).unwrap();
    for b in batches {
        store.append(b).unwrap();
    }
}

#[test]
fn every_spike_run_is_kept_as_a_recorded_trace_fixture() {
    let bless = std::env::var_os("AIBUS_BLESS").is_some();
    let mut cases: Vec<(String, &str, Feed, PayloadPolicy)> = Vec::new();
    for name in RUNS {
        cases.push((
            name.to_owned(),
            name,
            Feed::Stream,
            PayloadPolicy::MetadataOnly,
        ));
        if !load(name).hooks.is_empty() {
            cases.push((
                format!("{name}.hooks"),
                name,
                Feed::StreamThenHooks,
                PayloadPolicy::MetadataOnly,
            ));
        }
    }
    cases.push((
        "11-foreground.retained".into(),
        "11-foreground",
        Feed::StreamThenHooks,
        PayloadPolicy::Retain { max_bytes: 1 << 20 },
    ));
    let mut stale = Vec::new();
    for (dir_name, run, feed, policy) in cases {
        let want = traces_dir().join(&dir_name);
        if bless {
            // The store appends, so a blessed trace starts from an empty directory.
            let _ = fs::remove_dir_all(&want);
            write_trace(&want, run, feed, policy);
            continue;
        }
        let tmp = tempfile::tempdir().unwrap();
        let got = tmp.path().join("run");
        write_trace(&got, run, feed, policy);
        if files(&got) != files(&want) {
            stale.push(dir_name);
        }
    }
    assert!(
        stale.is_empty(),
        "recorded traces differ from the fixtures (AIBUS_BLESS=1 rewrites them): {stale:?}"
    );
}
