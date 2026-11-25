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

/// Tools whose repeated use is worth a look. Matched on the tool name the trace carries.
const SEARCH_TOOLS: [&str; 4] = ["Grep", "Glob", "WebSearch", "WebFetch"];
/// Calls by one agent before repeated searches are mentioned.
const SEARCH_REPEAT_MIN: usize = 3;
/// Failures of one tool by one agent before repeated failures are mentioned.
const FAILURE_REPEAT_MIN: usize = 2;

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
    todo!("#293 red")
}
