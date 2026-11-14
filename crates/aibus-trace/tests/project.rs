//! Offline replay over hand-made traces: order independence, edges always sourced, missing
//! parents labelled, and corroborating events kept as evidence without becoming extra nodes.

use std::time::{Duration, SystemTime};

use aibus_trace::{
    derived_id, project, AttemptRef, EventKind, Origin, ToolOutcome, TraceEvent, Visibility,
};
use serde_json::json;
use uuid::Uuid;

fn attempt() -> AttemptRef {
    AttemptRef {
        workflow_run_id: Uuid::from_u128(1),
        attempt_id: Uuid::from_u128(2),
        parent_attempt_id: None,
        provider_extension: "org.aibus.test".into(),
    }
}

struct T(Vec<TraceEvent>);

impl T {
    fn push(
        &mut self,
        kind: EventKind,
        origin: Origin,
        vis: Visibility,
        f: impl FnOnce(&mut TraceEvent),
    ) -> Uuid {
        let n = self.0.len() as u64 + 1;
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000 + n);
        let mut e = TraceEvent::new(
            &attempt(),
            derived_id(&[&n.to_le_bytes()]),
            kind,
            origin,
            vis,
            at,
            "test/1",
        );
        e.sequence = n;
        f(&mut e);
        let id = e.event_id;
        self.0.push(e);
        id
    }
}

/// lead -> child (stream, then a hook confirming it) -> one Read tool (stream, then the hook) -> done.
fn delegation() -> Vec<TraceEvent> {
    use EventKind::*;
    use Origin::*;
    use Visibility::*;
    let mut t = T(Vec::new());
    t.push(AttemptStarted, AibusScheduler, Observed, |_| {});
    t.push(AgentStarted, CliStream, Observed, |e| {
        e.agent_id = Some("lead".into());
        e.data = json!({ "role": "lead" });
    });
    t.push(TaskDelegated, CliStream, Observed, |e| {
        e.agent_id = Some("lead".into());
        e.native.tool_use_id = Some("toolu_A".into());
        e.data = json!({ "subagent_type": "reader-a" });
    });
    let child = t.push(AgentStarted, CliStream, Observed, |e| {
        e.agent_id = Some("child".into());
        e.parent_agent_id = Some("lead".into());
        e.parent_tool_use_id = Some("toolu_A".into());
        e.data = json!({ "role": "reader-a" });
    });
    t.push(AgentStarted, Hook, Correlated, |e| {
        e.agent_id = Some("child".into());
        e.corroborates = Some(child);
    });
    let tool = t.push(ToolStarted, CliStream, Observed, |e| {
        e.agent_id = Some("child".into());
        e.native.tool_use_id = Some("toolu_R".into());
        e.data = json!({ "tool": "Read" });
    });
    t.push(ToolStarted, Hook, Correlated, |e| {
        e.agent_id = Some("child".into());
        e.native.tool_use_id = Some("toolu_R".into());
        e.corroborates = Some(tool);
    });
    t.push(ToolCompleted, CliStream, Observed, |e| {
        e.agent_id = Some("child".into());
        e.native.tool_use_id = Some("toolu_R".into());
    });
    t.push(AgentStopped, CliStream, Observed, |e| {
        e.agent_id = Some("child".into());
        e.data = json!({ "status": "completed" });
    });
    t.push(AttemptCompleted, AibusScheduler, Observed, |e| {
        e.data = json!({ "status": "completed", "requires_reconciliation": false });
    });
    t.0
}

#[test]
fn the_graph_follows_lead_to_child_to_tool_and_counts_each_fact_once() {
    let p = project(&delegation());
    assert_eq!(p.agents.len(), 2);
    assert_eq!(
        p.tools.len(),
        1,
        "the hook's tool_started is evidence, not a second tool"
    );
    let tool = &p.tools[0];
    assert_eq!(
        (tool.name.as_deref(), tool.outcome, tool.agent_id.as_deref()),
        (Some("Read"), ToolOutcome::Completed, Some("child"))
    );
    assert_eq!(tool.evidence.len(), 3);
    assert_eq!(p.edges.len(), 1);
    let e = &p.edges[0];
    assert_eq!(
        (
            e.parent.as_deref(),
            e.child.as_str(),
            e.via_tool_use_id.as_deref()
        ),
        (Some("lead"), "child", Some("toolu_A"))
    );
    assert_eq!(e.level, Visibility::Observed);
    assert!(e.cross_checked, "the hook channel confirms the child");
    assert_eq!(
        p.attempts[0].status,
        Some(aibus_trace::AttemptStatus::Completed)
    );
    assert!(p.gaps.is_empty());
}

#[test]
fn projection_and_rendering_do_not_depend_on_input_order() {
    let events = delegation();
    let a = project(&events);
    let mut reversed = events.clone();
    reversed.reverse();
    let b = project(&reversed);
    let mut shuffled = events.clone();
    shuffled.rotate_left(4);
    shuffled.swap(0, 3);
    let c = project(&shuffled);
    assert_eq!(a, b);
    assert_eq!(a, c);
    assert_eq!(a.render(), c.render());
    assert_eq!(a.render(), project(&events).render(), "rendering is stable");
}

#[test]
fn a_child_nothing_links_to_a_parent_is_an_unavailable_edge_and_a_gap() {
    let mut t = T(Vec::new());
    t.push(
        EventKind::AgentStarted,
        Origin::Hook,
        Visibility::Observed,
        |e| {
            e.agent_id = Some("orphan".into());
        },
    );
    let p = project(&t.0);
    assert_eq!(p.edges.len(), 1);
    let e = &p.edges[0];
    assert_eq!(
        (e.parent.as_ref(), e.level),
        (None, Visibility::Unavailable)
    );
    assert!(
        !e.sources.is_empty(),
        "even an unavailable edge points at the event that raised it"
    );
    assert!(
        p.gaps.iter().any(|g| g.reason == "parent_unavailable"),
        "{:?}",
        p.gaps
    );
}

#[test]
fn every_edge_has_a_source_and_gaps_stay_labelled() {
    let mut events = delegation();
    let n = events.len() as u64 + 1;
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let mut gap = TraceEvent::new(
        &attempt(),
        derived_id(&[b"gap"]),
        EventKind::GapDetected,
        Origin::CliStream,
        Visibility::Unavailable,
        at,
        "t/1",
    );
    gap.sequence = n;
    gap.data = json!({ "reason": "reasoning_not_exposed" });
    events.push(gap);
    let p = project(&events);
    assert!(p.edges.iter().all(|e| !e.sources.is_empty()));
    assert_eq!(p.gaps.len(), 1);
    assert!(p.render().contains("reasoning_not_exposed"));
}

#[test]
fn a_tool_that_never_finished_is_unknown_not_failed() {
    let mut t = T(Vec::new());
    t.push(
        EventKind::ToolStarted,
        Origin::CliStream,
        Visibility::Observed,
        |e| {
            e.native.tool_use_id = Some("toolu_X".into());
            e.data = json!({ "tool": "Write" });
        },
    );
    t.push(
        EventKind::AttemptFailed,
        Origin::AibusScheduler,
        Visibility::Observed,
        |e| {
            e.data = json!({ "status": "interrupted", "requires_reconciliation": true });
        },
    );
    let p = project(&t.0);
    assert_eq!(p.tools[0].outcome, ToolOutcome::Unknown);
    assert_eq!(
        p.attempts[0].status,
        Some(aibus_trace::AttemptStatus::Interrupted)
    );
    assert!(p.attempts[0].requires_reconciliation);
}
