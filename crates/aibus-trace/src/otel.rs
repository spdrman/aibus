//! The canonical trace as OpenTelemetry spans (#292, spec 7.2).
//!
//! A pure mapping: events in, an OTLP/JSON `ExportTraceServiceRequest` out. Nothing here opens a
//! socket (the exporter lives in `aibus-otel`), and the local trace stays the source of truth: this
//! reads a trace, it never writes one.
//!
//! Shape: one span for the workflow run, one per attempt, one per agent, one per delegation and one
//! per tool call. Span names are fixed strings (`aibus.attempt`, `aibus.tool`, ...) and the
//! variable parts (tool name, agent role, runtime) are attributes, so the name set stays small.
//! Route decisions, approvals, artifacts, usage, gaps and warnings are events on their attempt
//! span. Message chunks are left out: they are payload, not structure.
//!
//! Ids: the trace id is the workflow run id, a span id is the first 8 bytes of a hash of the run
//! and a stable key, so exporting the same trace twice gives the same ids. Prompt text, tool input
//! and output never leave: only the small `data` fields the mapping names, and every string goes
//! through the redactor first. Usage keeps its quality (`reported`, `derived`, `estimated`,
//! `unknown`) as an attribute, and an unknown value is sent without a number.

use std::collections::{HashMap, HashSet};

use aibus_core::redact::Redactor;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{AttemptStatus, EventKind, Metric, TraceEvent, REDACTION_VERSION};

/// Revision ids linked onto the trace (spec 7.2): identifiers only, never the prompt itself.
#[derive(Clone, Debug, Default)]
pub struct Revisions {
    pub prompt: Option<String>,
    pub workflow: Option<String>,
}

/// What the mapping could not fully say, so a thin trace is visible before it is exported.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Completeness {
    pub spans: usize,
    /// Tool, agent or attempt spans that never saw an end event (exported with `aibus.complete=false`).
    pub unfinished: usize,
    /// An end event with no start (a tool completing that never started): dropped, counted here.
    pub orphan_ends: usize,
    /// Agents that name a delegating tool or parent that is not in the trace.
    pub unresolved_parents: usize,
    /// Attempts with no usage event at all.
    pub attempts_without_usage: usize,
}

struct Span {
    id: [u8; 8],
    parent: Option<[u8; 8]>,
    name: &'static str,
    start: u64,
    end: Option<u64>,
    status: u8,
    attrs: Vec<(String, Value)>,
    events: Vec<(u64, String, Vec<(String, Value)>)>,
}

/// Unix nanoseconds of `YYYY-MM-DDTHH:MM:SS[.fff]Z`, the format the store writes.
fn nanos(ts: &str) -> Option<u64> {
    let b = ts.strip_suffix('Z')?;
    let (date, time) = b.split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let (hms, frac) = time.split_once('.').unwrap_or((time, "0"));
    let mut t = hms.split(':').map(|x| x.parse::<i64>().ok());
    let (h, mi, s) = (t.next()??, t.next()??, t.next()??);
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = u64::try_from(days * 86_400 + h * 3_600 + mi * 60 + s).ok()?;
    let digits: String = frac.chars().take(9).collect();
    let ns = format!("{digits:0<9}").parse::<u64>().ok()?;
    Some(secs * 1_000_000_000 + ns)
}

fn span_id(run: Uuid, kind: &str, key: &[&str]) -> [u8; 8] {
    let mut h = Sha256::new();
    h.update(run.as_bytes());
    h.update(kind.as_bytes());
    for k in key {
        h.update([0]);
        h.update(k.as_bytes());
    }
    h.finalize()[..8].try_into().expect("8 of 32 bytes")
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn when(e: &TraceEvent) -> u64 {
    e.occurred_at
        .as_deref()
        .and_then(nanos)
        .or_else(|| nanos(&e.observed_at))
        .unwrap_or(0)
}

fn text(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_owned)
}

fn metric_attrs(m: &Metric) -> Vec<(String, Value)> {
    let k = format!("aibus.usage.{}", m.name);
    let mut a = vec![
        (format!("{k}.unit"), json!(m.unit)),
        (
            format!("{k}.quality"),
            serde_json::to_value(m.quality).unwrap_or(Value::Null),
        ),
        (format!("{k}.source"), json!(m.source)),
    ];
    if let Some(v) = m.value {
        a.push((k, json!(v)));
    }
    a
}

/// The OTLP/JSON request for the whole trace, plus its completeness diagnostics. `events` may be
/// in any order; corroborating events are evidence, not spans, and are skipped.
pub fn to_otlp(
    events: &[TraceEvent],
    revisions: &Revisions,
    redactor: &Redactor,
) -> (Value, Completeness) {
    let mut evs: Vec<&TraceEvent> = events
        .iter()
        .filter(|e| e.corroborates.is_none() && e.kind != EventKind::MessageChunk)
        .collect();
    evs.sort_by_key(|e| e.event_id);
    let run = evs.first().map_or(Uuid::nil(), |e| e.workflow_run_id);
    let run_span = span_id(run, "run", &[]);
    let attempts: HashSet<Uuid> = evs.iter().map(|e| e.attempt_id).collect();

    let mut spans: Vec<Span> = Vec::new();
    // key -> index, per span kind
    let mut at_ix: HashMap<Uuid, usize> = HashMap::new();
    let mut ag_ix: HashMap<(Uuid, String), usize> = HashMap::new();
    let mut tool_ix: HashMap<(Uuid, String), usize> = HashMap::new();
    let mut deleg_ix: HashMap<(Uuid, String), usize> = HashMap::new();
    let mut used: HashSet<Uuid> = HashSet::new();
    let mut c = Completeness::default();

    let (t0, t1) = evs
        .iter()
        .map(|e| when(e))
        .fold((u64::MAX, 0), |(a, b), t| (a.min(t), b.max(t)));
    spans.push(Span {
        id: run_span,
        parent: None,
        name: "aibus.workflow.run",
        start: if t0 == u64::MAX { 0 } else { t0 },
        end: Some(t1),
        status: 0,
        attrs: vec![("aibus.workflow_run_id".into(), json!(run.to_string()))],
        events: Vec::new(),
    });

    for e in &evs {
        let t = when(e);
        let att = e.attempt_id;
        if !at_ix.contains_key(&att) {
            let parent = match e.parent_attempt_id {
                Some(p) if attempts.contains(&p) => span_id(run, "attempt", &[&p.to_string()]),
                _ => run_span,
            };
            at_ix.insert(att, spans.len());
            spans.push(Span {
                id: span_id(run, "attempt", &[&att.to_string()]),
                parent: Some(parent),
                name: "aibus.attempt",
                start: t,
                end: None,
                status: 0,
                attrs: vec![
                    ("aibus.attempt_id".into(), json!(att.to_string())),
                    (
                        "aibus.provider_extension".into(),
                        json!(e.provider_extension),
                    ),
                ],
                events: Vec::new(),
            });
        }
        let ai = at_ix[&att];
        let agent_key = e.agent_id.clone().map(|a| (att, a));
        match e.kind {
            EventKind::AttemptStarted => {}
            EventKind::AttemptCompleted | EventKind::AttemptFailed => {
                let status: Option<AttemptStatus> =
                    serde_json::from_value(e.data["status"].clone()).ok();
                let s = &mut spans[ai];
                s.end = Some(t);
                s.status = match (e.kind, status) {
                    (_, Some(AttemptStatus::Completed)) => 1,
                    (_, Some(AttemptStatus::Cancelled)) => 0,
                    (EventKind::AttemptFailed, _) | (_, Some(_)) => 2,
                    _ => 1,
                };
            }
            EventKind::AgentStarted => {
                if let Some(k) = agent_key {
                    let role = text(&e.data, "role").or_else(|| text(&e.data, "subagent_type"));
                    let mut attrs = vec![
                        ("aibus.agent.id".into(), json!(k.1)),
                        (
                            "aibus.visibility".into(),
                            json!(format!("{:?}", e.visibility).to_lowercase()),
                        ),
                    ];
                    if let Some(r) = role {
                        attrs.push(("aibus.agent.role".into(), json!(r)));
                    }
                    ag_ix.entry(k.clone()).or_insert_with(|| {
                        spans.push(Span {
                            id: span_id(run, "agent", &[&att.to_string(), &k.1]),
                            parent: None,
                            name: "aibus.agent",
                            start: t,
                            end: None,
                            status: 0,
                            attrs,
                            events: Vec::new(),
                        });
                        spans.len() - 1
                    });
                    // parent resolved below, from these hints
                    let i = ag_ix[&k];
                    if let Some(p) = &e.parent_tool_use_id {
                        spans[i]
                            .attrs
                            .push(("aibus.parent_tool_use_id".into(), json!(p)));
                    }
                    if let Some(p) = &e.parent_agent_id {
                        spans[i]
                            .attrs
                            .push(("aibus.parent_agent_id".into(), json!(p)));
                    }
                }
            }
            EventKind::AgentStopped => {
                if let Some(i) = agent_key.and_then(|k| ag_ix.get(&k).copied()) {
                    spans[i].end = Some(t);
                    if text(&e.data, "status").as_deref() == Some("failed") {
                        spans[i].status = 2;
                    } else {
                        spans[i].status = 1;
                    }
                } else {
                    c.orphan_ends += 1;
                }
            }
            EventKind::TaskDelegated => {
                if let Some(tu) = e.native.tool_use_id.clone() {
                    let mut attrs = vec![("aibus.tool_use_id".into(), json!(tu))];
                    if let Some(a) = &e.agent_id {
                        attrs.push(("aibus.agent.id".into(), json!(a)));
                    }
                    if let Some(r) = text(&e.data, "subagent_type") {
                        attrs.push(("aibus.agent.role".into(), json!(r)));
                    }
                    deleg_ix.insert((att, tu.clone()), spans.len());
                    spans.push(Span {
                        id: span_id(run, "delegation", &[&att.to_string(), &tu]),
                        parent: None,
                        name: "aibus.delegation",
                        start: t,
                        end: Some(t),
                        status: 1,
                        attrs,
                        events: Vec::new(),
                    });
                }
            }
            EventKind::ToolStarted => {
                if let Some(tu) = e.native.tool_use_id.clone() {
                    let mut attrs = vec![("aibus.tool_use_id".into(), json!(tu))];
                    if let Some(n) = text(&e.data, "tool") {
                        attrs.push(("aibus.tool.name".into(), json!(n)));
                    }
                    if let Some(a) = &e.agent_id {
                        attrs.push(("aibus.agent.id".into(), json!(a)));
                    }
                    tool_ix.entry((att, tu.clone())).or_insert_with(|| {
                        spans.push(Span {
                            id: span_id(run, "tool", &[&att.to_string(), &tu]),
                            parent: None,
                            name: "aibus.tool",
                            start: t,
                            end: None,
                            status: 0,
                            attrs,
                            events: Vec::new(),
                        });
                        spans.len() - 1
                    });
                }
            }
            EventKind::ToolCompleted | EventKind::ToolFailed => {
                let i = e
                    .native
                    .tool_use_id
                    .clone()
                    .and_then(|tu| tool_ix.get(&(att, tu)).copied());
                match i {
                    Some(i) => {
                        spans[i].end = Some(t);
                        spans[i].status = if e.kind == EventKind::ToolFailed {
                            2
                        } else {
                            1
                        };
                    }
                    None => c.orphan_ends += 1,
                }
            }
            k => {
                // Everything else is an event on the attempt span.
                let mut attrs = vec![(
                    "aibus.visibility".into(),
                    json!(format!("{:?}", e.visibility).to_lowercase()),
                )];
                if k == EventKind::UsageReported {
                    used.insert(att);
                    let metrics: Vec<Metric> =
                        serde_json::from_value(e.data["metrics"].clone()).unwrap_or_default();
                    for m in &metrics {
                        attrs.extend(metric_attrs(m));
                    }
                    if e.data["scope"] == "attempt" {
                        // The last attempt-wide report is cumulative: it is the attempt's usage.
                        spans[ai]
                            .attrs
                            .retain(|(k, _)| !k.starts_with("aibus.usage."));
                        for m in &metrics {
                            spans[ai].attrs.extend(metric_attrs(m));
                        }
                    }
                }
                for key in ["status", "reason", "decision", "name", "kind"] {
                    if let Some(v) = text(&e.data, key) {
                        attrs.push((format!("aibus.{key}"), json!(v)));
                    }
                }
                let name = serde_json::to_value(k)
                    .ok()
                    .and_then(|v| v.as_str().map(|s| format!("aibus.{s}")))
                    .unwrap_or_else(|| "aibus.event".into());
                spans[ai].events.push((t, name, attrs));
            }
        }
    }

    // Parents: an agent hangs under the delegation or tool that spawned it, else its parent agent,
    // else its attempt. A named parent that is not in the trace is counted, not invented.
    let ids: Vec<(usize, Option<[u8; 8]>, bool)> = ag_ix
        .iter()
        .map(|((att, _), &i)| {
            let attr = |k: &str| {
                spans[i]
                    .attrs
                    .iter()
                    .find(|(n, _)| n == k)
                    .and_then(|(_, v)| v.as_str().map(str::to_owned))
            };
            let tool = attr("aibus.parent_tool_use_id");
            let agent = attr("aibus.parent_agent_id");
            let mut resolved = None;
            if let Some(tu) = &tool {
                resolved = deleg_ix
                    .get(&(*att, tu.clone()))
                    .or_else(|| tool_ix.get(&(*att, tu.clone())))
                    .map(|&j| spans[j].id);
            }
            if resolved.is_none() {
                if let Some(a) = &agent {
                    resolved = ag_ix.get(&(*att, a.clone())).map(|&j| spans[j].id);
                }
            }
            let unresolved = resolved.is_none() && (tool.is_some() || agent.is_some());
            (i, resolved.or(Some(spans[at_ix[att]].id)), unresolved)
        })
        .collect();
    for (i, p, unresolved) in ids {
        spans[i].parent = p;
        c.unresolved_parents += usize::from(unresolved);
    }
    // Delegations and tools hang under the agent that issued them, else their attempt.
    let by_agent: Vec<(usize, Option<[u8; 8]>)> = deleg_ix
        .iter()
        .chain(tool_ix.iter())
        .map(|((att, _), &i)| {
            let agent = spans[i]
                .attrs
                .iter()
                .find(|(n, _)| n == "aibus.agent.id")
                .and_then(|(_, v)| v.as_str().map(str::to_owned));
            let p = agent
                .and_then(|a| ag_ix.get(&(*att, a)).map(|&j| spans[j].id))
                .or(Some(spans[at_ix[att]].id));
            (i, p)
        })
        .collect();
    for (i, p) in by_agent {
        spans[i].parent = p;
    }

    c.attempts_without_usage = attempts.iter().filter(|a| !used.contains(a)).count();
    c.unfinished = spans.iter().filter(|s| s.end.is_none()).count();
    c.spans = spans.len();

    let attr = |(k, v): &(String, Value)| {
        let mut v = v.clone();
        redactor.redact_json(&mut v);
        let any = match &v {
            Value::String(s) => json!({ "stringValue": s }),
            Value::Bool(b) => json!({ "boolValue": b }),
            Value::Number(n) if n.is_i64() || n.is_u64() => json!({ "intValue": n.to_string() }),
            Value::Number(n) => json!({ "doubleValue": n }),
            other => json!({ "stringValue": other.to_string() }),
        };
        json!({ "key": k, "value": any })
    };
    let out: Vec<Value> = spans
        .iter()
        .map(|s| {
            let mut attrs: Vec<Value> = s.attrs.iter().map(attr).collect();
            if s.end.is_none() {
                attrs.push(attr(&("aibus.complete".into(), json!(false))));
            }
            let mut o = json!({
                "traceId": hex(run.as_bytes()),
                "spanId": hex(&s.id),
                "name": s.name,
                "kind": 1,
                "startTimeUnixNano": s.start.to_string(),
                "endTimeUnixNano": s.end.unwrap_or(s.start).to_string(),
                "attributes": attrs,
                "events": s.events.iter().map(|(t, n, a)| json!({
                    "timeUnixNano": t.to_string(),
                    "name": n,
                    "attributes": a.iter().map(attr).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "status": { "code": s.status },
            });
            if let Some(p) = s.parent {
                o["parentSpanId"] = json!(hex(&p));
            }
            o
        })
        .collect();

    let mut res = vec![
        ("service.name".to_owned(), json!("aibus")),
        (
            "aibus.redaction_version".to_owned(),
            json!(REDACTION_VERSION),
        ),
    ];
    if let Some(p) = &revisions.prompt {
        res.push(("aibus.prompt_revision".into(), json!(p)));
    }
    if let Some(w) = &revisions.workflow {
        res.push(("aibus.workflow_revision".into(), json!(w)));
    }
    let body = json!({ "resourceSpans": [{
        "resource": { "attributes": res.iter().map(attr).collect::<Vec<_>>() },
        "scopeSpans": [{ "scope": { "name": "aibus-trace" }, "spans": out }],
    }]});
    (body, c)
}
