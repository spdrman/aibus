//! Offline replay (spec 7.3): the execution graph and timeline rebuilt from a stored trace, with
//! no provider and no process. It reproduces what was recorded, not anything the model kept to
//! itself, and a gap stays a gap.

use uuid::Uuid;

use crate::{AttemptStatus, EventKind, Metric, Origin, TraceEvent, Visibility};

/// One source event behind a node or an edge.
#[derive(Clone, Debug, PartialEq)]
pub struct Evidence {
    pub event_id: Uuid,
    pub sequence: u64,
    pub visibility: Visibility,
    pub origin: Origin,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AttemptView {
    pub attempt_id: Uuid,
    pub status: Option<AttemptStatus>,
    pub requires_reconciliation: bool,
    /// The last attempt-wide usage the vendor reported (it is cumulative), with qualities.
    pub usage: Vec<Metric>,
    pub evidence: Vec<Evidence>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentNode {
    pub attempt_id: Uuid,
    pub agent_id: String,
    /// `lead`, or the subagent type.
    pub role: Option<String>,
    pub status: Option<String>,
    pub evidence: Vec<Evidence>,
}

/// A delegation edge. `sources` is never empty (spec 2.2: a causal edge points to its events).
#[derive(Clone, Debug, PartialEq)]
pub struct Edge {
    pub attempt_id: Uuid,
    /// `None` when nothing says who delegated: the edge is then `Unavailable`.
    pub parent: Option<String>,
    pub child: String,
    pub via_tool_use_id: Option<String>,
    /// The strongest level among the sources.
    pub level: Visibility,
    /// More than one independent channel backs the edge.
    pub cross_checked: bool,
    pub sources: Vec<Evidence>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolOutcome {
    Completed,
    Failed,
    /// Started and never finished in the trace.
    Unknown,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolNode {
    pub attempt_id: Uuid,
    pub tool_use_id: String,
    pub agent_id: Option<String>,
    pub name: Option<String>,
    pub outcome: ToolOutcome,
    pub evidence: Vec<Evidence>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TimelineRow {
    pub sequence: u64,
    pub observed_at: String,
    pub occurred_at: Option<String>,
    pub kind: EventKind,
    pub visibility: Visibility,
    pub origin: Origin,
    pub agent_id: Option<String>,
    pub corroborates: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GapRow {
    pub sequence: u64,
    pub attempt_id: Uuid,
    pub reason: String,
}

/// The replayed run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Projection {
    pub attempts: Vec<AttemptView>,
    pub agents: Vec<AgentNode>,
    pub edges: Vec<Edge>,
    pub tools: Vec<ToolNode>,
    pub timeline: Vec<TimelineRow>,
    pub gaps: Vec<GapRow>,
}

impl Projection {
    /// A stable text rendering (graph, then gaps, then timeline). Two projections of the same
    /// trace render to the same bytes.
    pub fn render(&self) -> String {
        todo!()
    }
}

/// Replays `events` (in any order: they are sorted by `sequence` first).
pub fn project(_events: &[TraceEvent]) -> Projection {
    todo!()
}
