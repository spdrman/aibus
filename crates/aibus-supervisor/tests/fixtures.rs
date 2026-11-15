//! Every captured Claude Code stream from the spike (#281) through the normaliser and the store:
//! the event sequence and evidence levels each one must produce, and the cases #282 has to survive
//! (several `result`s, a killed run, a denial, duplicates, reordering, truncation, unknown events).

mod common;

use std::collections::BTreeSet;
use std::fs;

use aibus_supervisor::{Ending, Normalizer};
use aibus_trace::{project, AttemptStatus, EventKind, Origin, ToolOutcome, TraceEvent, Visibility};
use common::*;

fn kinds(events: &[TraceEvent], kind: EventKind) -> Vec<&TraceEvent> {
    events.iter().filter(|e| e.kind == kind).collect()
}

/// The facts a projection counts, without evidence levels or origins: what must not change when
/// lines are duplicated or arrive in another order.
fn facts(events: &[TraceEvent]) -> (BTreeSet<String>, BTreeSet<String>, BTreeSet<String>) {
    let p = project(events);
    let agents = p
        .agents
        .iter()
        .map(|a| format!("{} {:?}", a.agent_id, a.role))
        .collect();
    let tools = p
        .tools
        .iter()
        .map(|t| {
            format!(
                "{} {:?} {:?} {:?}",
                t.tool_use_id, t.agent_id, t.name, t.outcome
            )
        })
        .collect();
    let edges = p
        .edges
        .iter()
        .map(|e| format!("{:?} -> {} via {:?}", e.parent, e.child, e.via_tool_use_id))
        .collect();
    (agents, tools, edges)
}

#[test]
fn every_fixture_replays_to_its_golden_projection() {
    let bless = std::env::var_os("AIBUS_BLESS").is_some();
    let mut stale = Vec::new();
    for name in RUNS {
        for (feed, suffix) in [(Feed::Stream, ""), (Feed::StreamThenHooks, ".hooks")] {
            if feed == Feed::StreamThenHooks && load(name).hooks.is_empty() {
                continue;
            }
            let rendered = project(&replay(name, feed)).render();
            let path = golden_dir().join(format!("{name}{suffix}.txt"));
            if bless {
                fs::create_dir_all(golden_dir()).unwrap();
                fs::write(&path, &rendered).unwrap();
            } else if fs::read_to_string(&path).ok().as_deref() != Some(rendered.as_str()) {
                stale.push(path.display().to_string());
            }
        }
    }
    assert!(
        stale.is_empty(),
        "projection differs from the golden (AIBUS_BLESS=1 rewrites them): {stale:#?}"
    );
}

#[test]
fn foreground_lead_delegates_to_two_observed_children_each_with_one_read() {
    let events = replay("11-foreground", Feed::Stream);
    let p = project(&events);
    let lead = p
        .agents
        .iter()
        .find(|a| a.role.as_deref() == Some("lead"))
        .expect("a lead");
    assert_eq!(p.agents.len(), 3, "{:#?}", p.agents);
    let children: BTreeSet<_> = p
        .agents
        .iter()
        .filter(|a| a.agent_id != lead.agent_id)
        .map(|a| a.agent_id.as_str())
        .collect();
    assert_eq!(
        children,
        BTreeSet::from(["a51dc098ebaef1926", "a8462ecb2e68a2980"])
    );
    assert_eq!(p.edges.len(), 2);
    for e in &p.edges {
        assert_eq!(e.parent.as_deref(), Some(lead.agent_id.as_str()));
        assert_eq!(e.level, Visibility::Observed, "{e:#?}");
        assert!(e
            .via_tool_use_id
            .as_deref()
            .is_some_and(|t| t.starts_with("toolu_")));
    }
    let reads: Vec<_> = p
        .tools
        .iter()
        .filter(|t| t.name.as_deref() == Some("Read"))
        .collect();
    assert_eq!(reads.len(), 2);
    assert!(reads.iter().all(|t| t.outcome == ToolOutcome::Completed
        && children.contains(t.agent_id.as_deref().unwrap())));
    assert_eq!(
        p.tools.len(),
        2,
        "the Agent calls are delegations, not tools"
    );
    assert_eq!(p.attempts[0].status, Some(AttemptStatus::Completed));
    assert!(!p.attempts[0].requires_reconciliation);
}

#[test]
fn with_hooks_every_child_edge_is_cross_checked_and_nothing_is_counted_twice() {
    let stream = replay("11-foreground", Feed::Stream);
    let both = replay("11-foreground", Feed::StreamThenHooks);
    assert_eq!(facts(&stream), facts(&both));
    let p = project(&both);
    assert!(
        p.edges
            .iter()
            .all(|e| e.cross_checked && e.level == Visibility::Observed),
        "{:#?}",
        p.edges
    );
    // Every hook event that names a fact the stream already gave is a correlated corroboration.
    for e in both
        .iter()
        .filter(|e| e.origin == Origin::Hook && e.kind != EventKind::TraceWarning)
    {
        assert!(
            e.corroborates.is_some(),
            "a hook event counted as a new fact: {e:#?}"
        );
        assert_eq!(e.visibility, Visibility::Correlated);
    }
    assert!(p
        .tools
        .iter()
        .all(|t| t.evidence.iter().any(|ev| ev.origin == Origin::Hook)));
}

#[test]
fn three_results_in_one_process_end_one_attempt_at_exit() {
    let (batches, n) = normalise(&load("07-deleg"), Feed::Stream);
    assert_eq!(n.counters().results, 3);
    let events = stored(batches);
    let ends: Vec<_> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            matches!(
                e.kind,
                EventKind::AttemptCompleted | EventKind::AttemptFailed
            )
        })
        .collect();
    assert_eq!(ends.len(), 1, "one end, at process exit");
    assert_eq!(ends[0].0, events.len() - 1, "and it is the last event");
    assert_eq!(ends[0].1.kind, EventKind::AttemptCompleted);
    assert_eq!(ends[0].1.data["results_seen"], 3);
    // Each result's usage is kept and marked cumulative; the cost is an estimate, never a bill.
    let usage: Vec<_> = kinds(&events, EventKind::UsageReported)
        .into_iter()
        .filter(|e| e.data["scope"] == "attempt")
        .collect();
    assert_eq!(usage.len(), 3);
    let p = project(&events);
    let cost = p.attempts[0]
        .usage
        .iter()
        .find(|m| m.name == "cost_usd")
        .expect("a cost metric");
    assert_eq!(cost.quality, aibus_trace::DataQuality::Estimated);
    let billed = p.attempts[0]
        .usage
        .iter()
        .find(|m| m.name == "billed_cost_usd")
        .expect("billed cost is listed as unknown");
    assert_eq!(
        (billed.quality, billed.value),
        (aibus_trace::DataQuality::Unknown, None)
    );
}

#[test]
fn a_killed_run_is_interrupted_never_completed() {
    let events = replay("10-killed", Feed::Stream);
    let last = events.last().unwrap();
    assert_eq!(last.kind, EventKind::AttemptFailed);
    assert_eq!(last.data["status"], "interrupted");
    assert!(kinds(&events, EventKind::AttemptCompleted).is_empty());
    // The children died with the process: their stop is inferred, and labelled as such.
    let p = project(&events);
    assert_eq!(p.attempts[0].status, Some(AttemptStatus::Interrupted));
    for stop in kinds(&events, EventKind::AgentStopped)
        .into_iter()
        .filter(|e| e.visibility == Visibility::Inferred)
    {
        assert_eq!(stop.origin, Origin::Derived);
        assert_eq!(stop.data["status"], "ended_with_process");
    }
    assert!(
        p.gaps.iter().any(|g| g.reason == "stream_truncated"),
        "{:?}",
        p.gaps
    );
}

/// Run 15 of the spike: killed while both children were still reading.
#[test]
fn a_run_killed_while_its_children_work_is_interrupted_and_their_stops_are_inferred() {
    let events = replay("15-killed-midrun", Feed::StreamThenHooks);
    let p = project(&events);
    assert_eq!(p.attempts[0].status, Some(AttemptStatus::Interrupted));
    assert!(kinds(&events, EventKind::AttemptCompleted).is_empty());
    let children: Vec<_> = p
        .agents
        .iter()
        .filter(|a| a.role.as_deref() != Some("lead"))
        .collect();
    assert!(!children.is_empty());
    for c in children {
        assert_eq!(c.status.as_deref(), Some("ended_with_process"), "{c:#?}");
        let stop = c.evidence.last().unwrap();
        assert_eq!(
            (stop.visibility, stop.origin),
            (Visibility::Inferred, Origin::Derived)
        );
    }
    // A tool that never reported back is unknown, and the attempt says it needs reconciling.
    if p.tools.iter().any(|t| t.outcome == ToolOutcome::Unknown) {
        assert!(p.attempts[0].requires_reconciliation);
        assert!(p.gaps.iter().any(|g| g.reason == "tool_outcome_unknown"));
    }
    assert!(p.gaps.iter().any(|g| g.reason == "stream_truncated"));
}

#[test]
fn a_denied_write_is_one_failed_tool_with_its_reason() {
    let events = replay("09-denied", Feed::Stream);
    let p = project(&events);
    let writes: Vec<_> = p
        .tools
        .iter()
        .filter(|t| t.name.as_deref() == Some("Write"))
        .collect();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].outcome, ToolOutcome::Failed);
    let failed = kinds(&events, EventKind::ToolFailed);
    let first = failed.iter().find(|e| e.corroborates.is_none()).unwrap();
    assert_eq!(first.data["reason"], "permission_denied");
    assert!(
        failed.iter().filter(|e| e.corroborates.is_none()).count() == 1,
        "the error tool_result confirms, it does not add"
    );
    assert_eq!(
        p.attempts[0].status,
        Some(AttemptStatus::Completed),
        "the CLI handled the denial and finished"
    );
}

#[test]
fn init_says_task_and_calls_say_agent_and_both_become_agent() {
    let events = replay("11-foreground", Feed::Stream);
    let lead = kinds(&events, EventKind::AgentStarted)
        .into_iter()
        .find(|e| e.data["role"] == "lead")
        .unwrap();
    let tools: Vec<&str> = lead.data["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        tools.contains(&"Agent") && !tools.contains(&"Task"),
        "{tools:?}"
    );
    for d in kinds(&events, EventKind::TaskDelegated) {
        assert_eq!(d.data["tool"], "Agent");
    }
}

#[test]
fn child_messages_are_keyed_on_agent_id() {
    let events = replay("11-foreground", Feed::Stream);
    let children = ["a51dc098ebaef1926", "a8462ecb2e68a2980"];
    let child_chunks: Vec<_> = kinds(&events, EventKind::MessageChunk)
        .into_iter()
        .filter(|e| e.parent_tool_use_id.is_some())
        .collect();
    assert!(!child_chunks.is_empty());
    assert!(
        child_chunks
            .iter()
            .all(|e| children.contains(&e.agent_id.as_deref().unwrap())),
        "{child_chunks:#?}"
    );
}

#[test]
fn observed_at_is_aibus_time_and_occurred_at_is_only_the_vendors() {
    let events = replay("11-foreground", Feed::Stream);
    for e in &events {
        assert!(
            e.observed_at.starts_with("2026-11-14T21:19:"),
            "{}",
            e.observed_at
        );
        match e.native.native_type.as_deref() {
            Some("assistant" | "user") => assert!(e.occurred_at.is_some(), "{e:#?}"),
            Some("system/task_updated") => assert!(e.occurred_at.is_some()),
            Some(t) if t.starts_with("system/") || t == "result" => {
                assert_eq!(e.occurred_at, None, "{t}")
            }
            _ => {}
        }
    }
}

#[test]
fn hidden_reasoning_is_an_unavailable_gap_not_a_message() {
    let events = replay("11-foreground", Feed::Stream);
    let gaps: Vec<_> = kinds(&events, EventKind::GapDetected)
        .into_iter()
        .filter(|e| e.data["reason"] == "reasoning_not_exposed")
        .collect();
    assert!(!gaps.is_empty());
    assert!(gaps.iter().all(|g| g.visibility == Visibility::Unavailable));
}

#[test]
fn a_team_request_under_print_shows_two_ordinary_subagents() {
    let p = project(&replay("14-teams-attempt", Feed::Stream));
    assert_eq!(p.agents.len(), 3);
    assert_eq!(p.edges.len(), 2);
    assert!(p.edges.iter().all(|e| e.level == Visibility::Observed));
}

#[test]
fn every_line_twice_changes_nothing_but_the_duplicate_count() {
    for name in RUNS {
        let run = load(name);
        let mut doubled = load(name);
        doubled.lines = run
            .lines
            .iter()
            .flat_map(|l| [l.clone(), l.clone()])
            .collect();
        doubled.offsets_ms = run.offsets_ms.iter().flat_map(|m| [*m, *m]).collect();
        let (single, _) = normalise(&run, Feed::Stream);
        let (twice, n) = normalise(&doubled, Feed::Stream);
        assert_eq!(n.counters().duplicates, run.lines.len() as u64, "{name}");
        let a = stored(single);
        let b = stored(twice);
        // Event ids come from the line's bytes, so a duplicate has the original's id; only the
        // raw line numbers in `evidence_refs` and the end summary's counters differ.
        let strip = |v: &[TraceEvent]| {
            v.iter()
                .filter(|e| {
                    !matches!(
                        e.kind,
                        EventKind::AttemptCompleted | EventKind::AttemptFailed
                    )
                })
                .cloned()
                .map(|mut e| {
                    e.evidence_refs.clear();
                    e
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            strip(&a),
            strip(&b),
            "{name}: only the end summary may differ"
        );
        assert_eq!(facts(&a), facts(&b), "{name}");
    }
}

#[test]
fn duplicate_hooks_never_double_count_tool_calls() {
    let mut run = load("11-foreground");
    run.hooks = run
        .hooks
        .iter()
        .flat_map(|h| [h.clone(), h.clone(), h.clone()])
        .collect();
    let events = stored(normalise(&run, Feed::StreamThenHooks).0);
    let p = project(&events);
    assert_eq!(p.tools.len(), 2);
    assert_eq!(p.agents.len(), 3);
}

#[test]
fn hooks_before_the_stream_give_the_same_graph() {
    for name in ["07-deleg", "11-foreground", "14-teams-attempt", "09-denied"] {
        let a = replay(name, Feed::StreamThenHooks);
        let b = replay(name, Feed::HooksThenStream);
        assert_eq!(facts(&a), facts(&b), "{name}");
    }
}

#[test]
fn a_stream_cut_anywhere_never_panics_and_never_completes_by_accident() {
    for name in ["07-deleg", "11-foreground"] {
        let full = load(name);
        let full_facts = facts(&replay(name, Feed::Stream));
        for cut in 0..full.lines.len() {
            for partial in [false, true] {
                let mut run = load(name);
                run.lines.truncate(cut);
                run.offsets_ms.truncate(cut);
                if partial {
                    let l = &full.lines[cut];
                    run.lines.push(l[..l.len() / 2].to_vec());
                    run.offsets_ms.push(full.offsets_ms[cut]);
                }
                // A cut stream is a process that was killed.
                run.ending = Ending::Signalled(9);
                let events = stored(normalise(&run, Feed::Stream).0);
                let end = events.last().unwrap();
                assert_eq!(end.kind, EventKind::AttemptFailed, "{name} cut at {cut}");
                assert_eq!(end.data["status"], "interrupted");
                let (agents, tools, _) = facts(&events);
                assert!(agents.len() <= full_facts.0.len() && tools.len() <= full_facts.1.len());
            }
        }
    }
}

#[test]
fn unknown_events_become_warnings_and_leave_the_graph_alone() {
    let base = facts(&replay("11-foreground", Feed::Stream));
    let mut run = load("11-foreground");
    let odd: [&[u8]; 3] = [
        br#"{"type":"brand_new_event","session_id":"s","x":1}"#,
        br#"{"type":"system","subtype":"brand_new_subtype","session_id":"s"}"#,
        br#"{"type":"assistant","message":{"content":[{"type":"brand_new_block"}]},"session_id":"s"}"#,
    ];
    for (i, l) in odd.iter().enumerate() {
        run.lines.insert(3 + i * 7, l.to_vec());
        run.offsets_ms.insert(3 + i * 7, run.offsets_ms[3 + i * 7]);
    }
    let events = stored(normalise(&run, Feed::Stream).0);
    assert_eq!(facts(&events), base);
    let warnings: Vec<_> = kinds(&events, EventKind::TraceWarning)
        .into_iter()
        .filter(|e| e.data["reason"] == "unknown_variant")
        .collect();
    assert_eq!(warnings.len(), 3, "{warnings:#?}");
    assert!(warnings
        .iter()
        .any(|w| w.native.native_type.as_deref() == Some("brand_new_event")));
}

#[test]
fn a_replay_of_the_stored_trace_renders_the_same_after_a_reopen() {
    let (batches, _) = normalise(&load("07-deleg"), Feed::StreamThenHooks);
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("run");
    let open = || {
        aibus_trace::TraceStore::open(
            &dir,
            aibus_core::Redactor::new(),
            aibus_trace::PayloadPolicy::MetadataOnly,
            Default::default(),
        )
        .unwrap()
        .0
    };
    let mut store = open();
    let half = batches.len() / 2;
    let mut batches = batches.into_iter();
    for b in batches.by_ref().take(half) {
        store.append(b).unwrap();
    }
    drop(store); // aibus restarts half way
    let mut store = open();
    for b in batches {
        store.append(b).unwrap();
    }
    drop(store);
    let (events, _) = aibus_trace::TraceStore::read(&dir).unwrap();
    let once = project(&events).render();
    let (again, _) = aibus_trace::TraceStore::read(&dir).unwrap();
    assert_eq!(once, project(&again).render());
    assert_eq!(
        once,
        project(&replay("07-deleg", Feed::StreamThenHooks)).render(),
        "a restart in the middle leaves the same trace"
    );
}

#[test]
fn a_normaliser_never_sees_one_attempt_end_twice() {
    let run = load("06-basic");
    let mut n = Normalizer::new(attempt());
    for l in &run.lines {
        n.stream_line(l, run.started);
    }
    let first = n.finish(Ending::Exited(0), run.wall, run.started);
    let second = n.finish(Ending::Exited(0), run.wall, run.started);
    assert_eq!(
        first
            .events
            .iter()
            .filter(|e| e.kind == EventKind::AttemptCompleted)
            .count(),
        1
    );
    assert!(second.events.is_empty(), "{:#?}", second.events);
}
