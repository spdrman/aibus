//! Offline replay (spec 7.3): the execution graph and timeline rebuilt from a stored trace, with
//! no provider and no process. It reproduces what was recorded, not anything the model kept to
//! itself, and a gap stays a gap.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write;

use serde::Serialize;
use serde_json::Value;
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

/// The wire name of an enum value (`tool_started`, `cli_stream`, ...).
fn wire(v: impl Serialize) -> String {
    match serde_json::to_value(v) {
        Ok(Value::String(s)) => s,
        _ => "?".into(),
    }
}

fn evidence_list(ev: &[Evidence]) -> String {
    ev.iter()
        .map(|e| format!("#{}:{}/{}", e.sequence, wire(e.visibility), wire(e.origin)))
        .collect::<Vec<_>>()
        .join(",")
}

impl Projection {
    /// A stable text rendering (graph, then gaps, then timeline). Two projections of the same
    /// trace render to the same bytes.
    pub fn render(&self) -> String {
        let mut s = String::new();
        let opt = |o: &Option<String>| o.clone().unwrap_or_else(|| "-".into());
        for a in &self.attempts {
            let status = a.status.map_or_else(|| "-".into(), wire);
            let _ = writeln!(
                s,
                "attempt {} status={status} reconcile={} evidence={}",
                a.attempt_id,
                a.requires_reconciliation,
                evidence_list(&a.evidence)
            );
            for m in &a.usage {
                let value = m.value.map_or_else(|| "unknown".into(), |v| v.to_string());
                let _ = writeln!(
                    s,
                    "  usage {}={value} {} ({}, {})",
                    m.name,
                    m.unit,
                    wire(m.quality),
                    m.source
                );
            }
        }
        for a in &self.agents {
            let _ = writeln!(
                s,
                "agent {} role={} status={} evidence={}",
                a.agent_id,
                opt(&a.role),
                opt(&a.status),
                evidence_list(&a.evidence)
            );
        }
        for e in &self.edges {
            let _ = writeln!(
                s,
                "edge {} -> {} via {} level={} cross_checked={} sources={}",
                opt(&e.parent),
                e.child,
                opt(&e.via_tool_use_id),
                wire(e.level),
                e.cross_checked,
                evidence_list(&e.sources)
            );
        }
        for t in &self.tools {
            let outcome = match t.outcome {
                ToolOutcome::Completed => "completed",
                ToolOutcome::Failed => "failed",
                ToolOutcome::Unknown => "unknown",
            };
            let _ = writeln!(
                s,
                "tool {} agent={} name={} outcome={outcome} evidence={}",
                t.tool_use_id,
                opt(&t.agent_id),
                opt(&t.name),
                evidence_list(&t.evidence)
            );
        }
        for g in &self.gaps {
            let _ = writeln!(s, "gap #{} {}", g.sequence, g.reason);
        }
        s.push_str("timeline\n");
        for r in &self.timeline {
            let _ = writeln!(
                s,
                "#{} {} {} {} {} {} agent={}{}",
                r.sequence,
                r.observed_at,
                opt(&r.occurred_at),
                wire(r.kind),
                wire(r.visibility),
                wire(r.origin),
                opt(&r.agent_id),
                if r.corroborates { " corroborates" } else { "" }
            );
        }
        s
    }
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// Replays `events` (in any order: they are sorted by `sequence` first).
pub fn project(events: &[TraceEvent]) -> Projection {
    let mut sorted: Vec<&TraceEvent> = events.iter().collect();
    sorted.sort_by_key(|e| (e.sequence, e.event_id));

    let mut p = Projection::default();
    let mut attempts: HashMap<Uuid, usize> = HashMap::new();
    let mut agents: HashMap<(Uuid, String), usize> = HashMap::new();
    let mut tools: HashMap<(Uuid, String), usize> = HashMap::new();
    // (attempt, child) -> (parent tool use, parent agent), first one that names them.
    let mut parent_of: HashMap<(Uuid, String), (Option<String>, Option<String>)> = HashMap::new();
    // (attempt, tool use) -> (delegating agent, evidence)
    let mut delegations: HashMap<(Uuid, String), (Option<String>, Vec<Evidence>)> = HashMap::new();
    // (attempt, child) -> its agent_started evidence
    let mut started: HashMap<(Uuid, String), Vec<Evidence>> = HashMap::new();

    for e in sorted {
        let ev = Evidence {
            event_id: e.event_id,
            sequence: e.sequence,
            visibility: e.visibility,
            origin: e.origin,
        };
        p.timeline.push(TimelineRow {
            sequence: e.sequence,
            observed_at: e.observed_at.clone(),
            occurred_at: e.occurred_at.clone(),
            kind: e.kind,
            visibility: e.visibility,
            origin: e.origin,
            agent_id: e.agent_id.clone(),
            corroborates: e.corroborates.is_some(),
        });
        let att = e.attempt_id;
        let mut attempt = |p: &mut Projection| -> usize {
            *attempts.entry(att).or_insert_with(|| {
                p.attempts.push(AttemptView {
                    attempt_id: att,
                    status: None,
                    requires_reconciliation: false,
                    usage: Vec::new(),
                    evidence: Vec::new(),
                });
                p.attempts.len() - 1
            })
        };
        match e.kind {
            EventKind::AttemptStarted | EventKind::AttemptCompleted | EventKind::AttemptFailed => {
                let i = attempt(&mut p);
                let a = &mut p.attempts[i];
                a.evidence.push(ev);
                if e.kind != EventKind::AttemptStarted {
                    a.status = serde_json::from_value(e.data["status"].clone()).ok();
                    a.requires_reconciliation =
                        e.data["requires_reconciliation"].as_bool().unwrap_or(false);
                }
            }
            EventKind::UsageReported
                if e.data["scope"] == "attempt" && e.corroborates.is_none() =>
            {
                let i = attempt(&mut p);
                p.attempts[i].usage =
                    serde_json::from_value(e.data["metrics"].clone()).unwrap_or_default();
            }
            EventKind::AgentStarted | EventKind::AgentStopped => {
                let Some(id) = e.agent_id.clone() else {
                    continue;
                };
                let key = (att, id.clone());
                let i = *agents.entry(key.clone()).or_insert_with(|| {
                    p.agents.push(AgentNode {
                        attempt_id: att,
                        agent_id: id,
                        role: None,
                        status: None,
                        evidence: Vec::new(),
                    });
                    p.agents.len() - 1
                });
                let node = &mut p.agents[i];
                node.evidence.push(ev.clone());
                if e.kind == EventKind::AgentStarted {
                    if node.role.is_none() {
                        node.role =
                            text(&e.data, "role").or_else(|| text(&e.data, "subagent_type"));
                    }
                    started.entry(key.clone()).or_default().push(ev);
                    if e.parent_tool_use_id.is_some() || e.parent_agent_id.is_some() {
                        parent_of
                            .entry(key)
                            .or_insert((e.parent_tool_use_id.clone(), e.parent_agent_id.clone()));
                    }
                } else if let Some(status) = text(&e.data, "status") {
                    // A stop the vendor reported beats one aibus inferred from the process ending.
                    if node.status.is_none()
                        || e.visibility != Visibility::Inferred
                            && node.status.as_deref() == Some("ended_with_process")
                    {
                        node.status = Some(status);
                    }
                }
            }
            EventKind::TaskDelegated => {
                let Some(tu) = e.native.tool_use_id.clone() else {
                    continue;
                };
                let d = delegations.entry((att, tu)).or_insert((None, Vec::new()));
                if d.0.is_none() {
                    d.0.clone_from(&e.agent_id);
                }
                d.1.push(ev);
            }
            EventKind::ToolStarted | EventKind::ToolCompleted | EventKind::ToolFailed => {
                let Some(tu) = e.native.tool_use_id.clone() else {
                    continue;
                };
                let i = *tools.entry((att, tu.clone())).or_insert_with(|| {
                    p.tools.push(ToolNode {
                        attempt_id: att,
                        tool_use_id: tu,
                        agent_id: None,
                        name: None,
                        outcome: ToolOutcome::Unknown,
                        evidence: Vec::new(),
                    });
                    p.tools.len() - 1
                });
                let t = &mut p.tools[i];
                t.evidence.push(ev);
                if t.agent_id.is_none() {
                    t.agent_id.clone_from(&e.agent_id);
                }
                if t.name.is_none() {
                    t.name = text(&e.data, "tool");
                }
                if t.outcome == ToolOutcome::Unknown {
                    t.outcome = match e.kind {
                        EventKind::ToolCompleted => ToolOutcome::Completed,
                        EventKind::ToolFailed => ToolOutcome::Failed,
                        _ => ToolOutcome::Unknown,
                    };
                }
            }
            EventKind::GapDetected => {
                p.gaps.push(GapRow {
                    sequence: e.sequence,
                    attempt_id: att,
                    reason: text(&e.data, "reason").unwrap_or_else(|| "unspecified".into()),
                });
            }
            _ => {}
        }
    }

    for a in &p.agents {
        if a.role.as_deref() == Some("lead") {
            continue;
        }
        let key = (a.attempt_id, a.agent_id.clone());
        let (via, named_parent) = parent_of.get(&key).cloned().unwrap_or_default();
        let deleg = via
            .as_ref()
            .and_then(|tu| delegations.get(&(a.attempt_id, tu.clone())));
        let parent = named_parent.or_else(|| deleg.and_then(|d| d.0.clone()));
        let mut sources: Vec<Evidence> = deleg.map(|d| d.1.clone()).unwrap_or_default();
        sources.extend(started.get(&key).cloned().unwrap_or_default());
        if sources.is_empty() {
            sources.extend(a.evidence.iter().cloned());
        }
        sources.sort_by_key(|s| s.sequence);
        let origins: BTreeSet<Origin> = sources.iter().map(|s| s.origin).collect();
        let level = if parent.is_some() {
            sources
                .iter()
                .map(|s| s.visibility)
                .min()
                .unwrap_or(Visibility::Unavailable)
        } else {
            let first = sources.first().map_or(0, |s| s.sequence);
            p.gaps.push(GapRow {
                sequence: first,
                attempt_id: a.attempt_id,
                reason: "parent_unavailable".into(),
            });
            Visibility::Unavailable
        };
        p.edges.push(Edge {
            attempt_id: a.attempt_id,
            parent,
            child: a.agent_id.clone(),
            via_tool_use_id: via,
            level,
            cross_checked: origins.len() > 1,
            sources,
        });
    }
    p.gaps.sort_by_key(|g| g.sequence);
    p
}
