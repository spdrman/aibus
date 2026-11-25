//! The aibus desktop host (B1.1 spike, #304). Rust owns the data: the A1.1 capture of a real Claude
//! Code run goes through `aibus-supervisor`'s normaliser into `aibus-trace` events, `project` turns
//! them into the graph and timeline, and only that projection crosses the IPC boundary. The webview
//! cannot name a file, a command or a process, and no credential, raw line or path is in what it gets.
#![forbid(unsafe_code)]

use std::time::{Duration, SystemTime};

use aibus_supervisor::claude::EXTENSION_ID;
use aibus_supervisor::{Ending, Normalizer};
use aibus_trace::{project, AttemptRef, Projection, TraceEvent};
use serde::Serialize;
use tauri::ipc::Channel;
use uuid::Uuid;

/// Where the data on screen came from. The webview shows it on every view, so recorded data never
/// passes for a live run. The spike only has the fixture: following a live supervised run needs the
/// store-tailing work of Phase 2 (docs/desktop/B1.1-go-no-go.md).
#[derive(Serialize, Clone, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Provenance {
    Fixture {
        label: &'static str,
        capture: &'static str,
        cli_version: &'static str,
    },
}

/// One replay step: the whole projection after one more captured line.
#[derive(Serialize, Clone, Debug)]
pub struct Snapshot {
    pub provenance: Provenance,
    pub step: usize,
    pub steps: usize,
    /// Milliseconds since the captured process was spawned.
    pub t_ms: u64,
    pub projection: Projection,
}

#[derive(Serialize, Clone, Debug)]
pub struct StackInfo {
    pub tauri: &'static str,
    pub os: &'static str,
    pub arch: &'static str,
    pub webview: Option<String>,
}

/// The capture through the normaliser, one snapshot per input line with its recorded offset. Stream
/// lines come in their captured order and the hook channel after them (the hooks file is read once
/// the process is gone, as in the supervisor's own fixture tests), so the last snapshot carries the
/// cross-checked delegation edges.
pub fn replay_steps() -> Vec<Snapshot> {
    // Compiled in, so there is no path argument for the webview to abuse.
    let stdout = include_str!("../../../fixtures/claude-code/spike-281/07-deleg.stdout.jsonl");
    let hooks = include_str!("../../../fixtures/claude-code/spike-281/07-deleg.hooks.jsonl");
    let observed = include_str!("../../../fixtures/claude-code/spike-281/07-deleg.observed.tsv");
    let meta_txt = include_str!("../../../fixtures/claude-code/spike-281/07-deleg.meta.txt");
    let meta = |key: &str| -> u64 {
        meta_txt
            .lines()
            .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("no numeric {key} in the capture meta"))
    };
    let started = SystemTime::UNIX_EPOCH + Duration::from_millis(meta("started_unix_ms"));
    let wall_ms = meta("wall_ms");
    let at = |ms: u64| started + Duration::from_millis(ms);
    let provenance = Provenance::Fixture {
        label: "Recorded fixture, not a live run",
        capture: "fixtures/claude-code/spike-281/07-deleg (A1.1, 2026-11-14)",
        cli_version: "2.1.311",
    };
    let mut norm = Normalizer::new(AttemptRef {
        workflow_run_id: Uuid::from_u128(0xB11_0001),
        attempt_id: Uuid::from_u128(0xB11_0002),
        parent_attempt_id: None,
        provider_extension: EXTENSION_ID.into(),
    });

    // (offset ms, the events that line produced)
    let mut steps: Vec<(u64, Vec<TraceEvent>)> = Vec::new();
    let info = serde_json::json!({ "cli_version": "2.1.311" });
    steps.push((0, norm.start(started, info).events));
    let offsets = observed.lines().skip(1).map(|l| {
        l.split('\t')
            .nth(1)
            .and_then(|v| v.parse::<u64>().ok())
            .expect("observed.tsv offset")
    });
    for (line, ms) in stdout.lines().filter(|l| !l.is_empty()).zip(offsets) {
        steps.push((ms, norm.stream_line(line.as_bytes(), at(ms)).events));
    }
    for line in hooks.lines().filter(|l| !l.is_empty()) {
        steps.push((wall_ms, norm.hook_line(line.as_bytes(), at(wall_ms)).events));
    }
    let wall = Duration::from_millis(wall_ms);
    let end = norm.finish(Ending::Exited(0), wall, at(wall_ms)).events;
    steps.push((wall_ms, end));

    let total = steps.len();
    let mut events = Vec::new();
    steps
        .into_iter()
        .enumerate()
        .map(|(i, (t_ms, batch))| {
            events.extend(batch);
            Snapshot {
                provenance: provenance.clone(),
                step: i + 1,
                steps: total,
                t_ms,
                projection: project(&events),
            }
        })
        .collect()
}

/// Streams the fixture to the webview over a Tauri channel at half the recorded pace, so the graph
/// visibly grows. The only argument is the channel: the webview picks nothing else.
#[tauri::command]
async fn replay_fixture(on_snapshot: Channel<Snapshot>) -> Result<usize, String> {
    let steps = replay_steps();
    let mut last = 0;
    for s in &steps {
        tokio::time::sleep(Duration::from_millis(s.t_ms.saturating_sub(last) * 2)).await;
        last = s.t_ms;
        on_snapshot.send(s.clone()).map_err(|e| e.to_string())?;
    }
    eprintln!("aibus-desktop: replayed {} fixture steps", steps.len());
    Ok(steps.len())
}

#[tauri::command]
fn stack_info() -> StackInfo {
    StackInfo {
        tauri: tauri::VERSION,
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        webview: tauri::webview_version().ok(),
    }
}

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![replay_fixture, stack_info])
        .run(tauri::generate_context!())
        .expect("error while running the aibus desktop host");
}

#[cfg(test)]
mod tests {
    use super::*;
    use aibus_trace::{AttemptStatus, ToolOutcome};

    #[test]
    fn fixture_has_a_lead_two_subagents_and_their_tools() {
        let last = replay_steps().pop().expect("steps").projection;
        let roles: Vec<_> = last.agents.iter().map(|a| a.role.as_deref()).collect();
        assert_eq!(
            roles.iter().filter(|r| **r == Some("lead")).count(),
            1,
            "{roles:?}"
        );
        assert!(
            roles.contains(&Some("reader-a")) && roles.contains(&Some("reader-b")),
            "{roles:?}"
        );
        assert!(last.edges.len() >= 2, "one delegation edge per subagent");
        assert!(last.edges.iter().all(|e| e.cross_checked));
        let reads = last
            .tools
            .iter()
            .filter(|t| t.name.as_deref() == Some("Read"))
            .count();
        assert!(reads >= 2, "each subagent reads a file");
        assert!(last
            .tools
            .iter()
            .all(|t| t.outcome == ToolOutcome::Completed));
        assert_eq!(last.attempts[0].status, Some(AttemptStatus::Completed));
    }

    #[test]
    fn snapshots_grow_and_offsets_never_go_backwards() {
        let s = replay_steps();
        assert_eq!(s.last().map(|x| x.step), Some(s.len()));
        assert!(s.windows(2).all(|w| w[0].t_ms <= w[1].t_ms));
        assert!(s
            .windows(2)
            .all(|w| w[0].projection.timeline.len() <= w[1].projection.timeline.len()));
    }

    #[test]
    fn every_snapshot_says_it_is_a_fixture() {
        for s in replay_steps() {
            let v = serde_json::to_value(&s).unwrap();
            assert_eq!(v["provenance"]["kind"], "fixture");
        }
    }

    /// Usage metric names like `input_tokens` are fine; credential names, the CLI's path, the prompt
    /// and raw vendor lines are not.
    #[test]
    fn nothing_credential_or_raw_crosses_the_boundary() {
        let wire = serde_json::to_string(&replay_steps().pop().unwrap())
            .unwrap()
            .to_lowercase();
        for needle in [
            "access_token",
            "refresh_token",
            "api_key",
            "apikey",
            "sk-ant",
            "password",
            "oauth",
            "bearer",
            "cookie",
            "authorization",
            "/opt/homebrew",
            "/spike/work",
            "use the agent tool",
            "\"type\":\"assistant\"",
        ] {
            assert!(
                !wire.contains(needle),
                "{needle} appears in the IPC payload"
            );
        }
    }

    /// The webview's test data is this crate's real output, so the two sides cannot drift. Run with
    /// AIBUS_BLESS=1 to rewrite it.
    #[test]
    fn final_snapshot_matches_the_golden_the_webview_tests_load() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../src/test/final-snapshot.json");
        let now = serde_json::to_string_pretty(&replay_steps().pop().unwrap()).unwrap() + "\n";
        if std::env::var_os("AIBUS_BLESS").is_some() {
            std::fs::write(&path, &now).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap_or_default(),
            now,
            "stale golden: run with AIBUS_BLESS=1"
        );
    }
}
