//! The causal inspector view model and the reproducibility report (#294, spec 8.1 "Inspector",
//! 7.3). The inspector is one selected node as typed fields: its prompt and output when the trace
//! kept them, its native ids, the source events behind it, its gaps, elapsed time, reported usage
//! and linked artifacts. Whatever is not in the trace is a `Known::Gap` with the reason. The
//! reproducibility report says what a recorded replay needs (the trace, offline) apart from what
//! a fresh re-execution would need, and which of those inputs the trace does not carry.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write;

use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::compare::{environment, ArtifactRef, Known};
use crate::view::{
    clean, clean_metric, data_str, edge_line, opt, payload_bytes, span_ms, Recorded,
};
use crate::{project, DataQuality, EventKind, Metric, Origin, TraceEvent, Visibility};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Agent,
    Tool,
    Attempt,
}

/// Text the run kept (under payload retention), and which event it came from.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Retained {
    pub from_event: u64,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EvidenceRow {
    pub position: u64,
    pub kind: EventKind,
    pub visibility: Visibility,
    pub origin: Origin,
    pub native_type: Option<String>,
    pub parser: String,
    pub raw_sha256: Option<String>,
    pub payload_ref: Option<String>,
    /// The position of the event this one confirms.
    pub confirms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NativeId {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TraceGap {
    pub position: u64,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InspectorView {
    pub node: String,
    pub kind: NodeKind,
    pub attempt_id: Uuid,
    pub prompt: Known<Retained>,
    pub output: Known<Retained>,
    pub native_ids: Vec<NativeId>,
    /// How the node is linked to its parent, with the evidence level.
    pub link: Known<String>,
    pub source_evidence: Vec<EvidenceRow>,
    pub gaps: Vec<TraceGap>,
    pub elapsed: Metric,
    pub usage: Known<Vec<Metric>>,
    pub artifacts: Known<Vec<ArtifactRef>>,
    pub worktree_diff: Known<String>,
}

/// A retained payload as text: its JSON pretty-printed when it is JSON, cleaned for a terminal and
/// cut at 4 KiB.
fn payload_text(rec: &Recorded, payload_ref: &str) -> Option<String> {
    /// The most text of one retained payload an inspector field holds.
    const MAX_TEXT: usize = 4096;
    let b = payload_bytes(rec, payload_ref)?;
    let text = serde_json::from_slice::<Value>(&b)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| String::from_utf8_lossy(&b).into_owned());
    let mut out: String = text
        .lines()
        .map(|l| clean(l, 400))
        .collect::<Vec<_>>()
        .join("\n");
    if out.len() > MAX_TEXT {
        let mut cut = MAX_TEXT;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push_str("\n... cut at 4096 bytes");
    }
    Some(out)
}

/// The retained payloads of `events`, joined, with the position of the first.
fn retained(rec: &Recorded, events: &[&TraceEvent]) -> Option<Retained> {
    let mut parts = Vec::new();
    let mut first = None;
    for e in events {
        if let Some(t) = e.payload_ref.as_deref().and_then(|r| payload_text(rec, r)) {
            first.get_or_insert(e.position);
            parts.push(t);
        }
        if parts.len() == 3 {
            break;
        }
    }
    Some(Retained {
        from_event: first?,
        text: parts.join("\n"),
    })
}

/// The inspector of one agent id, tool use id or attempt id; `None` when nothing has that id.
pub fn inspector(rec: &Recorded, node: &str) -> Option<InspectorView> {
    let p = project(&rec.events);
    let seq_of: HashMap<Uuid, u64> = rec
        .events
        .iter()
        .map(|e| (e.event_id, e.position))
        .collect();
    let no_diff =
        || Known::Gap("no worktree diff is recorded in a trace; nothing stores one yet".into());
    let live = |events: &[&TraceEvent], kind: EventKind| -> Vec<TraceEvent> {
        events
            .iter()
            .filter(|e| e.kind == kind && e.corroborates.is_none())
            .map(|e| (*e).clone())
            .collect()
    };

    let (kind, attempt_id, mine): (NodeKind, Uuid, Vec<&TraceEvent>) =
        if let Some(a) = p.agents.iter().find(|a| a.agent_id == node) {
            let m = rec
                .events
                .iter()
                .filter(|e| e.attempt_id == a.attempt_id && e.agent_id.as_deref() == Some(node))
                .collect();
            (NodeKind::Agent, a.attempt_id, m)
        } else if let Some(t) = p.tools.iter().find(|t| t.tool_use_id == node) {
            let m = rec
                .events
                .iter()
                .filter(|e| {
                    e.attempt_id == t.attempt_id && e.native.tool_use_id.as_deref() == Some(node)
                })
                .collect();
            (NodeKind::Tool, t.attempt_id, m)
        } else if let Some(a) = p.attempts.iter().find(|a| a.attempt_id.to_string() == node) {
            let m = rec
                .events
                .iter()
                .filter(|e| e.attempt_id == a.attempt_id)
                .collect();
            (NodeKind::Attempt, a.attempt_id, m)
        } else {
            return None;
        };

    let mut ids: BTreeSet<(&str, String)> = BTreeSet::new();
    for e in &mine {
        if let Some(v) = &e.native.session_id {
            ids.insert(("session_id", clean(v, 160)));
        }
        if let Some(v) = &e.native.tool_use_id {
            ids.insert(("tool_use_id", clean(v, 160)));
        }
        if let Some(v) = &e.native.native_type {
            ids.insert(("native_type", clean(v, 160)));
        }
    }
    let native_ids = ids
        .into_iter()
        .map(|(n, v)| NativeId {
            name: n.into(),
            value: v,
        })
        .collect();

    let edge = p
        .edges
        .iter()
        .find(|e| e.attempt_id == attempt_id && e.child == node);
    let link = match kind {
        NodeKind::Agent => match edge {
            Some(e) if e.parent.is_none() => {
                Known::Gap("no event names the parent of this agent (unavailable)".into())
            }
            Some(e) => Known::Value(format!(
                "parent {}; {}",
                opt(e.parent.as_deref()),
                edge_line(e)
            )),
            None if p
                .agents
                .iter()
                .any(|a| a.agent_id == node && a.role.as_deref() == Some("lead")) =>
            {
                Known::Value("none: the lead is the process aibus started".into())
            }
            None => Known::Gap("no delegation edge in the trace".into()),
        },
        NodeKind::Tool => match mine.iter().find_map(|e| e.agent_id.as_deref()) {
            Some(a) => Known::Value(format!("run by agent {}", clean(a, 160))),
            None => Known::Gap("no event names the agent that ran this tool use".into()),
        },
        NodeKind::Attempt => match rec
            .events
            .iter()
            .find(|e| e.attempt_id == attempt_id)
            .and_then(|e| e.parent_attempt_id)
        {
            Some(pa) => Known::Value(format!(
                "aibus-managed child of attempt {pa} (linked by the scheduler)"
            )),
            None => Known::Value("provider-native: no parent attempt".into()),
        },
    };

    let prompt_gap = |why: &str| Known::Gap(format!("prompt text not retained: {why}"));
    let prompt = match kind {
        NodeKind::Agent => match edge.and_then(|e| e.via_tool_use_id.as_deref()) {
            Some(via) => {
                let delegating: Vec<&TraceEvent> = rec
                    .events
                    .iter()
                    .filter(|e| {
                        e.attempt_id == attempt_id
                            && e.kind == EventKind::TaskDelegated
                            && e.corroborates.is_none()
                            && e.native.tool_use_id.as_deref() == Some(via)
                    })
                    .collect();
                retained(rec, &delegating).map_or_else(
                    || prompt_gap("the delegating tool use kept no payload (a plan keeps prompt hashes; text only under payload retention)"),
                    Known::Value,
                )
            }
            None => prompt_gap("no delegating tool use is known for this agent (a plan keeps prompt hashes; text only under payload retention)"),
        },
        NodeKind::Tool => Known::Gap("not applicable: a tool use has no prompt of its own".into()),
        NodeKind::Attempt => prompt_gap("an attempt has no prompt of its own"),
    };

    let output = match kind {
        NodeKind::Agent => {
            let chunks = live(&mine, EventKind::MessageChunk);
            let refs: Vec<&TraceEvent> = chunks.iter().collect();
            retained(rec, &refs).map_or_else(
                || {
                    if chunks.is_empty() {
                        Known::Gap("no message recorded (unavailable)".into())
                    } else {
                        Known::Gap(format!(
                            "{} message{} observed, text not retained (a metadata-only trace)",
                            chunks.len(),
                            if chunks.len() == 1 { "" } else { "s" }
                        ))
                    }
                },
                Known::Value,
            )
        }
        NodeKind::Tool => {
            let done: Vec<&TraceEvent> = mine
                .iter()
                .copied()
                .filter(|e| matches!(e.kind, EventKind::ToolCompleted | EventKind::ToolFailed))
                .collect();
            retained(rec, &done).map_or_else(
                || Known::Gap("tool output not retained (a metadata-only trace)".into()),
                Known::Value,
            )
        }
        NodeKind::Attempt => {
            Known::Gap("an attempt has no output of its own; open one of its agents".into())
        }
    };

    let source_evidence = mine
        .iter()
        .map(|e| EvidenceRow {
            position: e.position,
            kind: e.kind,
            visibility: e.visibility,
            origin: e.origin,
            native_type: e.native.native_type.as_deref().map(|t| clean(t, 160)),
            parser: clean(&e.source.parser, 160),
            raw_sha256: e.source.raw_sha256.as_deref().map(|t| clean(t, 160)),
            payload_ref: e.payload_ref.as_deref().map(|t| clean(t, 160)),
            confirms: e.corroborates.and_then(|c| seq_of.get(&c).copied()),
        })
        .collect();

    let gaps = mine
        .iter()
        .filter(|e| e.kind == EventKind::GapDetected)
        .map(|e| TraceGap {
            position: e.position,
            reason: data_str(e, "reason")
                .map_or_else(|| "no reason given".into(), |r| clean(r, 160)),
        })
        .collect();

    let elapsed = span_ms(&mine).map_or_else(
        || Metric {
            name: "elapsed_ms".into(),
            value: None,
            unit: "ms".into(),
            quality: DataQuality::Unknown,
            source: "no usable timestamps for this node".into(),
        },
        |ms| Metric {
            name: "elapsed_ms".into(),
            value: Some(ms as f64),
            unit: "ms".into(),
            quality: DataQuality::Derived,
            source: "aibus clock, first to last event of this node".into(),
        },
    );

    let usage = match kind {
        NodeKind::Agent => {
            let ms: Vec<Metric> = live(&mine, EventKind::UsageReported)
                .iter()
                .filter(|e| e.data["scope"] == "agent")
                .flat_map(|e| {
                    serde_json::from_value::<Vec<Metric>>(e.data["metrics"].clone())
                        .unwrap_or_default()
                })
                .map(|m| clean_metric(&m))
                .collect();
            if ms.is_empty() {
                Known::Gap("usage not reported for this agent (unknown)".into())
            } else {
                Known::Value(ms)
            }
        }
        NodeKind::Tool => Known::Gap("usage is not reported per tool use".into()),
        NodeKind::Attempt => p
            .attempts
            .iter()
            .find(|a| a.attempt_id == attempt_id)
            .filter(|a| !a.usage.is_empty())
            .map_or_else(
                || Known::Gap("usage not reported for this attempt (unknown)".into()),
                |a| Known::Value(a.usage.iter().map(clean_metric).collect()),
            ),
    };

    let made: Vec<ArtifactRef> = live(&mine, EventKind::ArtifactCreated)
        .iter()
        .map(|e| ArtifactRef {
            position: e.position,
            name: data_str(e, "name").map(|n| clean(n, 160)),
            payload_ref: e.payload_ref.as_deref().map(|r| clean(r, 160)),
        })
        .collect();
    let artifacts = if made.is_empty() {
        Known::Gap("no artifact_created event for this node".into())
    } else {
        Known::Value(made)
    };

    Some(InspectorView {
        node: clean(node, 160),
        kind,
        attempt_id,
        prompt,
        output,
        native_ids,
        link,
        source_evidence,
        gaps,
        elapsed,
        usage,
        artifacts,
        worktree_diff: no_diff(),
    })
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Needed {
    pub name: String,
    pub value: Known<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Replay {
    /// Replay reads the recorded events and calls nothing: it is always offline.
    pub offline: bool,
    pub available: bool,
    pub events: usize,
    /// What makes the replay incomplete: skipped lines, a torn tail, payloads not kept.
    pub caveats: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Rerun {
    /// Always `rerun`: a new model execution is never called a replay.
    pub label: String,
    pub available: bool,
    pub blocked_by: Option<String>,
    pub outcome_may_differ: bool,
    pub needs: Vec<Needed>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Reproducibility {
    pub run: String,
    pub replay: Replay,
    pub rerun: Rerun,
}

/// What a replay of `rec` needs and what a fresh run would need, with every input the trace does
/// not carry named as a gap.
pub fn reproducibility(id: &str, rec: &Recorded) -> Reproducibility {
    let mut caveats = Vec::new();
    let r = &rec.report;
    if r.corrupt_lines > 0 {
        caveats.push(format!("{} corrupt lines were skipped", r.corrupt_lines));
    }
    if r.torn_bytes > 0 {
        caveats.push(format!(
            "{} torn bytes at the end were not read",
            r.torn_bytes
        ));
    }
    if r.unsupported_schema > 0 {
        caveats.push(format!(
            "{} lines of another schema were skipped",
            r.unsupported_schema
        ));
    }
    if let Some(e) = &rec.plan_error {
        caveats.push(format!("plan.json could not be read: {}", clean(e, 160)));
    }
    let kept = rec
        .events
        .iter()
        .filter_map(|e| e.payload_ref.as_deref())
        .filter(|p| payload_bytes(rec, p).is_some())
        .count();
    if kept == 0 {
        caveats.push("payloads not retained: the replay shows what happened, not the text".into());
    }

    let env = environment(rec, None);
    let mut needs: Vec<Needed> = [
        "provider_extension",
        "cli_version",
        "model",
        "permission_mode",
        "workflow_plan",
    ]
    .iter()
    .filter_map(|n| env.iter().find(|e| e.name == *n))
    .map(|e| Needed {
        name: e.name.clone(),
        value: e.value.clone(),
    })
    .collect();
    needs.push(Needed {
        name: "prompts".into(),
        value: if kept > 0 {
            Known::Value(format!(
                "{kept} retained payloads, redacted when stored; they may not be every prompt"
            ))
        } else {
            Known::Gap("prompt text not retained; a plan keeps prompt hashes".into())
        },
    });
    needs.push(Needed {
        name: "repository_revision".into(),
        value: Known::Gap("the trace records no git revision or working tree".into()),
    });
    needs.push(Needed {
        name: "environment".into(),
        value: Known::Gap(
            "environment variables, tool and MCP versions and sandbox settings are not recorded in a trace".into(),
        ),
    });

    Reproducibility {
        run: clean(id, 160),
        replay: Replay {
            offline: true,
            available: !rec.events.is_empty(),
            events: rec.events.len(),
            caveats,
        },
        rerun: Rerun {
            label: "rerun".into(),
            available: false,
            blocked_by: Some(
                "a rerun starts a new attempt of the CLI, which needs the scheduler (#288, #289)"
                    .into(),
            ),
            outcome_may_differ: true,
            needs,
        },
    }
}

pub fn render_reproducibility(r: &Reproducibility) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "reproducibility of {}", r.run);
    s.push_str("\nreplay\n");
    s.push_str("  offline: reads the recorded events, calls no model and starts no process\n");
    let _ = writeln!(
        s,
        "  available: {}, {} recorded events",
        if r.replay.available { "yes" } else { "no" },
        r.replay.events
    );
    for c in &r.replay.caveats {
        let _ = writeln!(s, "  caveat: {c}");
    }
    s.push_str("\nrerun\n");
    s.push_str("  not a replay: a rerun is a new execution, and its outcome may differ from the recording\n");
    let _ = writeln!(
        s,
        "  available: {}{}",
        if r.rerun.available { "yes" } else { "no" },
        r.rerun
            .blocked_by
            .as_ref()
            .map_or_else(String::new, |b| format!(" ({b})"))
    );
    s.push_str("  what it would need, and what the trace gives:\n");
    for n in &r.rerun.needs {
        match &n.value {
            Known::Value(v) => {
                let _ = writeln!(s, "    {}: {v}", n.name);
            }
            Known::Gap(why) => {
                let _ = writeln!(s, "    {}: gap: {why}", n.name);
            }
        }
    }
    s
}
