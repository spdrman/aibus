//! #308 (B1.5): the Phase 1 gate's datasets, count-based smoke tests and timing probe.
//!
//! Three datasets, all built from the recorded spike trace (`fixtures/claude-code/traces-284/07-deleg`) with a
//! fixed seed, so every machine gets the same events:
//!
//! * small: the spike fixture as recorded (29 events, a lead and two native subagents).
//! * medium: 10,000 events, 50 agents.
//! * large: 100,000 events, 500 agents.
//!
//! The smoke tests only count (agents, edges, tools, timeline rows, batches, a bounded batch size),
//! so they are safe in CI. The timing probe is `#[ignore]` and prints one JSON line per dataset; it is
//! never a gate. Run it through the bench queue, see docs/desktop/B1.5-phase1-gate.md.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use aibus_bridge::{handle, Command, EventBatch, Reply, Request, BRIDGE_SCHEMA, MAX_BATCH};
use aibus_trace::{project, AttemptRef, EventIds, EventKind, Origin, TraceEvent, Visibility};
use serde_json::{json, Value};
use uuid::Uuid;

const SEED: u64 = 0x308_308_308;

fn recorded() -> Vec<TraceEvent> {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/claude-code/traces-284/07-deleg/events.jsonl"),
    )
    .unwrap()
    .lines()
    .map(|l| serde_json::from_str(l).unwrap())
    .collect()
}

/// The shape of the spike trace (a lead delegating to children that each run tools, a hook
/// confirming every child) scaled to `agents` agents and exactly `total` events. Origin, evidence
/// level, source and data of each kind come from the recorded events, ids and counts are generated.
fn dataset(agents: usize, total: usize) -> Vec<TraceEvent> {
    assert!(agents >= 2);
    let tpl = recorded();
    let attempt = AttemptRef {
        workflow_run_id: Uuid::from_u128(308),
        attempt_id: Uuid::from_u128(309),
        parent_attempt_id: None,
        provider_extension: tpl[0].provider_extension.clone(),
    };
    let mut ids = EventIds::default();
    let mut out: Vec<TraceEvent> = Vec::with_capacity(total);
    let mut push = |kind: EventKind, origin: Origin, f: &dyn Fn(&mut TraceEvent)| -> Uuid {
        let n = out.len() as u64 + 1;
        let at = SystemTime::UNIX_EPOCH + Duration::from_millis(1_800_000_000_000 + n * 5);
        let t = tpl.iter().find(|e| e.kind == kind && e.origin == origin);
        let vis = t.map_or(Visibility::Observed, |t| t.visibility);
        let mut e = TraceEvent::new(
            &attempt,
            ids.next(at, &[&n.to_le_bytes()]),
            kind,
            origin,
            vis,
            at,
            "claude-code-stream-json/1",
        );
        e.data = t.map_or(Value::Null, |t| t.data.clone());
        e.evidence_refs = vec![format!("raw:stdout:L{n}")];
        f(&mut e);
        let id = e.event_id;
        out.push(e);
        id
    };
    let agent = |i: usize| {
        if i == 0 {
            "lead".to_owned()
        } else {
            format!("agent-{i:04}")
        }
    };
    push(EventKind::AttemptStarted, Origin::AibusScheduler, &|_| {});
    push(EventKind::AgentStarted, Origin::CliStream, &|e| {
        e.agent_id = Some("lead".into());
        e.data = json!({ "role": "lead" });
    });
    for i in 1..agents {
        let tool = format!("deleg-{i:04}");
        let (a, t) = (agent(i), tool.clone());
        push(EventKind::TaskDelegated, Origin::CliStream, &|e| {
            e.agent_id = Some("lead".into());
            e.native.tool_use_id = Some(t.clone());
            e.data = json!({ "subagent_type": format!("worker-{}", i % 8) });
        });
        let child = push(EventKind::AgentStarted, Origin::CliStream, &|e| {
            e.agent_id = Some(a.clone());
            e.parent_agent_id = Some("lead".into());
            e.parent_tool_use_id = Some(tool.clone());
            e.data = json!({ "role": format!("worker-{}", i % 8) });
        });
        push(EventKind::AgentStarted, Origin::Hook, &|e| {
            e.agent_id = Some(agent(i));
            e.corroborates = Some(child);
        });
    }
    let tail = agents; // one stop per child plus the attempt end
    let fill = total - (2 + 3 * (agents - 1)) - tail;
    // xorshift64*: no dependency, the same numbers on every machine.
    let mut state = SEED;
    let mut below = |n: usize| -> usize {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        (state.wrapping_mul(0x2545_F491_4F6C_DD1D) % n.max(1) as u64) as usize
    };
    for k in 0..fill / 2 {
        let (who, id) = (agent(below(agents)), format!("tool-{k:06}"));
        let name = ["Read", "Grep", "Edit", "Bash"][below(4)];
        push(EventKind::ToolStarted, Origin::CliStream, &|e| {
            e.agent_id = Some(who.clone());
            e.native.tool_use_id = Some(id.clone());
            e.data = json!({ "tool": name });
        });
        push(EventKind::ToolCompleted, Origin::CliStream, &|e| {
            e.agent_id = Some(who.clone());
            e.native.tool_use_id = Some(id.clone());
        });
    }
    if fill % 2 == 1 {
        push(EventKind::UsageReported, Origin::CliStream, &|e| {
            e.data = json!({ "scope": "attempt", "metrics": [] });
        });
    }
    for i in 1..agents {
        push(EventKind::AgentStopped, Origin::CliStream, &|e| {
            e.agent_id = Some(agent(i))
        });
    }
    push(EventKind::AttemptCompleted, Origin::AibusScheduler, &|e| {
        e.data = json!({ "status": "completed" });
    });
    assert_eq!(out.len(), total);
    out
}

fn write_run(root: &Path, events: &[TraceEvent]) {
    let dir = root.join("run-a");
    fs::create_dir_all(&dir).unwrap();
    let mut body = String::new();
    for e in events {
        body.push_str(&serde_json::to_string(e).unwrap());
        body.push('\n');
    }
    fs::write(dir.join("events.jsonl"), body).unwrap();
}

fn ask(root: &Path, command: Command) -> Reply {
    let req = Request {
        schema_version: BRIDGE_SCHEMA,
        command,
    };
    handle(root, &req).unwrap()
}

fn batch(root: &Path, after: Option<String>) -> EventBatch {
    match ask(
        root,
        Command::Events {
            run: "run-a".into(),
            after,
            limit: Some(MAX_BATCH),
        },
    ) {
        Reply::Events { batch } => batch,
        other => panic!("{other:?}"),
    }
}

/// What the counts of `dataset(agents, total)` have to be.
fn check_projection(events: &[TraceEvent], agents: usize, total: usize) {
    let tools = (total - (1 + 1 + 3 * (agents - 1)) - agents) / 2;
    let p = project(events);
    assert_eq!(p.attempts.len(), 1);
    assert_eq!(p.agents.len(), agents);
    assert_eq!(p.edges.len(), agents - 1);
    assert!(
        p.edges.iter().all(|e| e.cross_checked),
        "every child is confirmed by its hook"
    );
    assert_eq!(p.tools.len(), tools);
    assert_eq!(
        p.timeline.len(),
        total,
        "the timeline has one row per event"
    );
    assert!(p.gaps.is_empty());
}

#[test]
fn small_is_the_recorded_spike_trace() {
    let events = recorded();
    assert_eq!(events.len(), 29);
    let p = project(&events);
    assert_eq!((p.agents.len(), p.edges.len(), p.tools.len()), (3, 2, 2));
    let root = tempfile::tempdir().unwrap();
    write_run(root.path(), &events);
    let Reply::Projection { projection } = ask(
        root.path(),
        Command::Projection {
            run: "run-a".into(),
        },
    ) else {
        panic!("not a projection")
    };
    assert_eq!(projection.agents.len(), 3);
}

#[test]
fn the_datasets_are_the_same_events_on_every_run() {
    let (a, b) = (dataset(50, 10_000), dataset(50, 10_000));
    assert!(a == b, "same seed, same events");
    assert!(
        a.windows(2).all(|w| w[0].event_id < w[1].event_id),
        "ids ascend"
    );
    // One event in the middle pins the generator's output to the seed, not just to itself.
    assert_eq!(a[5_000].kind, EventKind::ToolCompleted);
    assert_eq!(a[5_000].agent_id.as_deref(), Some("agent-0043"));
}

#[test]
fn medium_has_10000_events_and_50_agents_and_streams_in_bounded_batches() {
    let events = dataset(50, 10_000);
    check_projection(&events, 50, 10_000);
    let root = tempfile::tempdir().unwrap();
    write_run(root.path(), &events);
    let Reply::Projection { projection } = ask(
        root.path(),
        Command::Projection {
            run: "run-a".into(),
        },
    ) else {
        panic!("not a projection")
    };
    assert_eq!(projection.agents.len(), 50);
    // The cursor walk delivers every event once, in order, never more than MAX_BATCH at a time.
    let (mut after, mut seen, mut calls) = (None, 0usize, 0usize);
    loop {
        let b = batch(root.path(), after.clone());
        assert!(b.events.len() <= MAX_BATCH as usize);
        seen += b.events.len();
        calls += 1;
        after = b.next;
        if !b.more {
            break;
        }
    }
    assert_eq!((seen, calls), (10_000, 20));
}

#[test]
fn large_has_100000_events_and_500_agents() {
    check_projection(&dataset(500, 100_000), 500, 100_000);
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn min_median(mut v: Vec<f64>) -> (f64, f64) {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (v[0], v[v.len() / 2])
}

/// One JSON line per dataset: how long the bridge takes to load a run and build its projection,
/// how big that reply is, what one event batch costs at the start, middle and end of the run, and
/// (medium) a whole cursor walk. Each figure is the best and the median of five rounds. Nothing
/// here asserts a time. Run with `--ignored --nocapture`.
#[test]
#[ignore = "timing probe, run through the bench queue (docs/desktop/B1.5-phase1-gate.md)"]
fn timing_probe() {
    for (name, agents, total) in [("medium", 50, 10_000), ("large", 500, 100_000)] {
        let events = dataset(agents, total);
        let root = tempfile::tempdir().unwrap();
        write_run(root.path(), &events);
        let file_bytes = fs::metadata(root.path().join("run-a/events.jsonl"))
            .unwrap()
            .len();

        let mut t_projection = Vec::new();
        let mut projection_bytes = 0;
        for _ in 0..5 {
            let t = Instant::now();
            let r = ask(
                root.path(),
                Command::Projection {
                    run: "run-a".into(),
                },
            );
            t_projection.push(ms(t.elapsed()));
            projection_bytes = serde_json::to_string(&r).unwrap().len();
        }
        let mut t_project_only = Vec::new();
        for _ in 0..5 {
            let t = Instant::now();
            std::hint::black_box(project(&events));
            t_project_only.push(ms(t.elapsed()));
        }
        let mut t_batch = serde_json::Map::new();
        for (at, k) in [
            ("start", 0),
            ("middle", total / 2),
            ("end", total - MAX_BATCH as usize),
        ] {
            let after = (k > 0).then(|| events[k - 1].event_id.to_string());
            let (mut v, mut bytes, mut events_in) = (Vec::new(), 0, 0);
            for _ in 0..5 {
                let t = Instant::now();
                let b = batch(root.path(), after.clone());
                v.push(ms(t.elapsed()));
                bytes = serde_json::to_string(&b).unwrap().len();
                events_in = b.events.len();
            }
            let (min, med) = min_median(v);
            t_batch.insert(
                at.into(),
                json!({ "min_ms": min, "median_ms": med, "events": events_in, "reply_bytes": bytes }),
            );
        }
        let walk = (name == "medium").then(|| {
            let mut v = Vec::new();
            for _ in 0..5 {
                let (t, mut after) = (Instant::now(), None);
                loop {
                    let b = batch(root.path(), after);
                    after = b.next;
                    if !b.more {
                        break;
                    }
                }
                v.push(ms(t.elapsed()));
            }
            min_median(v)
        });
        // Peak resident set of this process in kB, from /proc (Linux); null elsewhere.
        let peak_rss_kb = fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("VmHWM:"))
                    .and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
            })
            .map_or(Value::Null, Value::from);
        let (pmin, pmed) = min_median(t_projection);
        let (omin, omed) = min_median(t_project_only);
        println!(
            "{}",
            json!({
                "dataset": name, "agents": agents, "events": total, "seed": SEED,
                "events_jsonl_bytes": file_bytes,
                "bridge_projection": { "min_ms": pmin, "median_ms": pmed, "reply_bytes": projection_bytes },
                "project_in_memory": { "min_ms": omin, "median_ms": omed },
                "batch_500": t_batch,
                "full_cursor_walk_ms": walk.map(|(min, med)| json!({ "min": min, "median": med })),
                "peak_rss_kb": peak_rss_kb,
            })
        );
    }
}
