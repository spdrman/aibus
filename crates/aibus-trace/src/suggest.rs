//! Optimization suggestions from a recorded run (#293, spec section 9.2).
//!
//! Every suggestion names the trace events it rests on (never empty, every id is in the run) and the
//! direction the benefit would go. There is no projected saving anywhere: a suggestion is a
//! `Hypothesis` about what to try, or a `Recommendation` about something the trace shows plainly, and its
//! `basis` says whether the pattern is counted from events (`Evidence`) or only suggested by
//! them (`Heuristic`; the trace holds no tool input, so two searches are never known to be the
//! same search). A suggestion from a run that failed a check or hit a permission denial is marked
//! `safe_to_recommend: false`: a leaner run that broke the gate is not a model to copy.
//!
//! Covered: repeated searches, repeated failures, delegations whose child ran no tool, and
//! delegations that ran one after the other with nothing between them. The rest of the spec list
//! (routing, model fit, approval waiting) needs events nothing emits yet; see `docs/DEFERRED.md`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::eval::{EvaluationResult, Verdict};
use crate::{project, EventKind, TraceEvent};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    RepeatedSearch,
    RepeatedFailure,
    UnusedDelegation,
    SerializableDelegation,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Label {
    /// Something the trace shows, worth acting on.
    Recommendation,
    /// A thing to try; whether it helps is untested.
    Hypothesis,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    /// Counted from events.
    Evidence,
    /// Suggested by events but not shown by them.
    Heuristic,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    FewerToolCalls,
    FewerFailures,
    FewerDelegations,
    LessWallTime,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub category: Category,
    pub label: Label,
    pub basis: Basis,
    pub direction: Direction,
    /// What the trace shows.
    pub observation: String,
    /// What to try. Never a number of seconds, tokens or dollars saved.
    pub suggestion: String,
    /// The events behind it. Never empty.
    pub trace_event_ids: Vec<Uuid>,
    /// False when the run it came from failed a check or was denied a permission.
    pub safe_to_recommend: bool,
}

/// Suggestions for one evaluated run and its trace.
pub fn suggest(result: &EvaluationResult, events: &[TraceEvent]) -> Vec<Suggestion> {
    // Tools whose repeated use is worth a look, matched on the tool name the trace carries.
    const SEARCH_TOOLS: [&str; 4] = ["Grep", "Glob", "WebSearch", "WebFetch"];
    // Calls (searches) or failures by one agent before they are mentioned.
    const SEARCH_REPEAT_MIN: usize = 3;
    const FAILURE_REPEAT_MIN: usize = 2;
    let mut sorted: Vec<&TraceEvent> = events.iter().filter(|e| e.corroborates.is_none()).collect();
    sorted.sort_by_key(|e| e.event_id);
    let safe = result.verdict == Verdict::Pass && result.value("permission_denials") == Some(0.0);
    let mut out = Vec::new();
    let mut add = |category,
                   label,
                   basis,
                   direction,
                   observation: String,
                   suggestion: &str,
                   ids: Vec<Uuid>| {
        if !ids.is_empty() {
            out.push(Suggestion {
                category,
                label,
                basis,
                direction,
                observation,
                suggestion: suggestion.into(),
                trace_event_ids: ids,
                safe_to_recommend: safe,
            });
        }
    };
    let tool = |e: &TraceEvent| e.data["tool"].as_str().unwrap_or("?").to_owned();

    let mut searches: BTreeMap<(Option<String>, String), Vec<Uuid>> = BTreeMap::new();
    let mut failures: BTreeMap<(Option<String>, String), Vec<Uuid>> = BTreeMap::new();
    for e in &sorted {
        let key = (e.agent_id.clone(), tool(e));
        match e.kind {
            EventKind::ToolStarted if SEARCH_TOOLS.contains(&key.1.as_str()) => {
                searches.entry(key).or_default().push(e.event_id);
            }
            EventKind::ToolFailed if e.data["reason"] != "permission_denied" => {
                failures.entry(key).or_default().push(e.event_id);
            }
            _ => {}
        }
    }
    for ((agent, name), ids) in searches {
        if ids.len() >= SEARCH_REPEAT_MIN {
            add(
                Category::RepeatedSearch,
                Label::Hypothesis,
                Basis::Heuristic,
                Direction::FewerToolCalls,
                format!("{} made {} {name} calls; the trace holds no tool input, so they may not be the same search", agent.as_deref().unwrap_or("an agent"), ids.len()),
                "try giving the agent the answer once (a shared note or a narrower brief) and compare the call count",
                ids,
            );
        }
    }
    for ((agent, name), ids) in failures {
        if ids.len() >= FAILURE_REPEAT_MIN {
            add(
                Category::RepeatedFailure,
                Label::Recommendation,
                Basis::Evidence,
                Direction::FewerFailures,
                format!("{} had {} failed {name} calls", agent.as_deref().unwrap_or("an agent"), ids.len()),
                "look at why the same tool keeps failing before the next run, and stop retrying it unchanged",
                ids,
            );
        }
    }

    let p = project(events);
    // An absence of tool events says nothing when the trace has gaps.
    if p.gaps.iter().all(|g| g.reason == "parent_unavailable") {
        for edge in &p.edges {
            if !p.tools.iter().any(|t| {
                t.attempt_id == edge.attempt_id
                    && t.agent_id.as_deref() == Some(edge.child.as_str())
            }) {
                add(
                    Category::UnusedDelegation,
                    Label::Hypothesis,
                    Basis::Evidence,
                    Direction::FewerDelegations,
                    format!("child agent {} ran no tool call", edge.child),
                    "check whether the delegation was needed; the parent may have done it itself",
                    edge.sources.iter().map(|s| s.event_id).collect(),
                );
            }
        }
    }

    // Children of one parent that ran one after another with the parent idle between them.
    let mut span: BTreeMap<&str, (Option<Uuid>, Option<Uuid>)> = BTreeMap::new();
    for e in &sorted {
        let Some(a) = e.agent_id.as_deref() else {
            continue;
        };
        let s = span.entry(a).or_default();
        match e.kind {
            EventKind::AgentStarted if s.0.is_none() => s.0 = Some(e.event_id),
            EventKind::AgentStopped if s.1.is_none() => s.1 = Some(e.event_id),
            _ => {}
        }
    }
    let mut by_parent: BTreeMap<&str, Vec<(Uuid, Uuid, &str)>> = BTreeMap::new();
    for edge in &p.edges {
        let Some(parent) = edge.parent.as_deref() else {
            continue;
        };
        if let Some((Some(start), Some(stop))) = span.get(edge.child.as_str()) {
            by_parent
                .entry(parent)
                .or_default()
                .push((*start, *stop, edge.child.as_str()));
        }
    }
    for (parent, mut kids) in by_parent {
        kids.sort();
        let mut ids = Vec::new();
        for w in kids.windows(2) {
            let (stop, next) = (w[0].1, w[1].0);
            let parent_busy = sorted.iter().any(|e| {
                e.agent_id.as_deref() == Some(parent)
                    && e.kind == EventKind::ToolStarted
                    && e.event_id > stop
                    && e.event_id < next
            });
            if stop < next && !parent_busy {
                ids.extend([stop, next]);
            }
        }
        ids.sort();
        ids.dedup();
        add(
            Category::SerializableDelegation,
            Label::Hypothesis,
            Basis::Heuristic,
            Direction::LessWallTime,
            format!("{parent} delegated tasks one after another and did nothing between them; the trace does not say whether they depend on each other"),
            "if the tasks are independent, try starting them together and compare wall time and the checks",
            ids,
        );
    }
    out
}
