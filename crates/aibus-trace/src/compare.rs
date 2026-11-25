//! Comparing two recorded runs (#294, spec 8.1 "Comparison"): quality, latency, tokens and cost,
//! tool counts, routing, outputs, git state and environment mismatches, side by side. Everything
//! comes from the two traces (and an evaluation result when one is handed in). Whatever the trace
//! does not carry is a `Known::Gap` with the reason, never a default, a zero or a guess, and two
//! runs are only called comparable when every field of their environment is known and equal
//! (invariant 9, spec 2.2). Nothing here reads anything but the two run directories.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use serde::Serialize;
use serde_json::Value;

use crate::eval::{CheckOutcome, EvaluationResult};
use crate::view::{clean, data_str, payload_bytes, span_ms, wire, Recorded, RunRow, RunState};
use crate::{
    project, sha256_hex, DataQuality, EventKind, Metric, Origin, Projection, ToolOutcome,
    TraceEvent, Visibility,
};

/// A value the trace carries, or the reason it does not.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Known<T> {
    Value(T),
    Gap(String),
}

/// One run and the evaluation result that goes with it, when there is one.
pub struct RunInput<'a> {
    pub id: &'a str,
    pub rec: &'a Recorded,
    pub result: Option<&'a EvaluationResult>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolCounts {
    pub total: usize,
    pub completed: usize,
    pub failed: usize,
    /// Started and never reported back.
    pub unknown: usize,
    pub by_name: BTreeMap<String, usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RouteRow {
    pub position: u64,
    pub visibility: Visibility,
    pub origin: Origin,
    /// The text and number fields of the event's metadata, as recorded.
    pub fields: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ArtifactRef {
    pub position: u64,
    pub name: Option<String>,
    pub payload_ref: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Outputs {
    pub message_chunks: usize,
    pub message_bytes: Known<u64>,
    /// Events that point at a payload the run kept, of the events that could.
    pub payloads_retained: usize,
    pub artifacts: Vec<ArtifactRef>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Quality {
    pub suite: String,
    pub case_id: String,
    pub variant: String,
    pub verdict: String,
    pub checks_passed: usize,
    pub checks_failed: usize,
    /// Checks nobody reported an outcome for. They never count as passes.
    pub checks_not_run: usize,
}

/// One environment field of one run.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EnvEntry {
    pub name: String,
    pub value: Known<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RunSide {
    pub id: String,
    pub state: RunState,
    pub provider: Option<String>,
    pub events: usize,
    pub attempts: usize,
    pub agents: usize,
    /// Delegation edges by level: observed, correlated, inferred, unavailable.
    pub edges: [usize; 4],
    pub gaps: usize,
    pub tools: ToolCounts,
    /// First to last event on aibus's clock, derived; `unknown` when the trace has no times.
    pub latency: Metric,
    /// Attempt-wide usage with each number's quality; always has a `cost_usd` row.
    pub usage: Vec<Metric>,
    pub routing: Known<Vec<RouteRow>>,
    pub outputs: Outputs,
    pub quality: Known<Quality>,
    pub git_revision: Known<String>,
    pub worktree_diff: Known<String>,
    pub environment: Vec<EnvEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Delta {
    pub metric: String,
    pub a: Metric,
    pub b: Metric,
    /// `b - a`, only when both are known and in the same unit.
    pub change: Known<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvVerdict {
    Same,
    Differs,
    /// One side or both do not know.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EnvRow {
    pub name: String,
    pub a: Known<String>,
    pub b: Known<String>,
    pub verdict: EnvVerdict,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Comparable {
    /// Every environment field is known and equal.
    Yes,
    /// At least one field differs.
    No(Vec<String>),
    /// Nothing differs that is known, but these fields are unknown on a side.
    Unknown(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Comparison {
    pub a: RunSide,
    pub b: RunSide,
    pub deltas: Vec<Delta>,
    pub environment: Vec<EnvRow>,
    pub comparable: Comparable,
}

/// Weakest last: how far a number can be trusted.
fn weakness(q: DataQuality) -> u8 {
    match q {
        DataQuality::Reported => 0,
        DataQuality::Derived => 1,
        DataQuality::Estimated => 2,
        DataQuality::Unknown => 3,
    }
}

fn unknown(name: &str, unit: &str, why: &str) -> Metric {
    Metric {
        name: name.into(),
        value: None,
        unit: unit.into(),
        quality: DataQuality::Unknown,
        source: why.into(),
    }
}

/// Attempt-wide usage over every attempt of the run: a number is summed only when every attempt
/// that reports it gives a value in the same unit, and keeps the weakest quality among them.
fn usage_of(p: &Projection) -> Vec<Metric> {
    let mut by_name: BTreeMap<String, Vec<&Metric>> = BTreeMap::new();
    for m in p.attempts.iter().flat_map(|a| a.usage.iter()) {
        by_name.entry(m.name.clone()).or_default().push(m);
    }
    let mut out: Vec<Metric> = by_name
        .into_iter()
        .map(|(name, ms)| {
            let unit = ms[0].unit.clone();
            let same_unit = ms.iter().all(|m| m.unit == unit);
            let values: Option<Vec<f64>> = ms.iter().map(|m| m.value).collect();
            let quality = ms
                .iter()
                .map(|m| m.quality)
                .max_by_key(|q| weakness(*q))
                .unwrap_or(DataQuality::Unknown);
            let source = if ms.len() == 1 {
                clean(&ms[0].source, 160)
            } else {
                format!(
                    "{}, summed over {} attempts",
                    clean(&ms[0].source, 160),
                    ms.len()
                )
            };
            match values {
                _ if ms.len() == 1 => Metric {
                    name: clean(&name, 160),
                    value: ms[0].value,
                    unit: clean(&unit, 160),
                    quality: ms[0].quality,
                    source,
                },
                Some(v) if same_unit => Metric {
                    name: clean(&name, 160),
                    value: Some(v.iter().sum()),
                    unit: clean(&unit, 160),
                    quality,
                    source,
                },
                _ => unknown(
                    &clean(&name, 160),
                    &clean(&unit, 160),
                    "an attempt gave no value or another unit, so the attempts are not summed",
                ),
            }
        })
        .collect();
    if !out.iter().any(|m| m.name == "cost_usd") {
        out.push(unknown("cost_usd", "usd", "no cost in the trace"));
    }
    out
}

/// The distinct non-empty strings of `key` in the data of the events of `kinds`, sorted and
/// joined, or `None` when there are none.
fn data_values(rec: &Recorded, kinds: &[EventKind], key: &str) -> Option<String> {
    let set: BTreeSet<String> = rec
        .events
        .iter()
        .filter(|e| e.corroborates.is_none() && kinds.contains(&e.kind))
        .filter_map(|e| data_str(e, key))
        .filter(|v| !v.is_empty())
        .map(|v| clean(v, 160))
        .collect();
    (!set.is_empty()).then(|| set.into_iter().collect::<Vec<_>>().join(", "))
}

fn known(v: Option<String>, why: &str) -> Known<String> {
    v.map_or_else(|| Known::Gap(why.into()), Known::Value)
}

/// What the trace (and the evaluation result, when there is one) says about the environment the
/// run happened in, one entry per field, always in the same order.
pub(crate) fn environment(rec: &Recorded, result: Option<&EvaluationResult>) -> Vec<EnvEntry> {
    let started = [EventKind::AttemptStarted, EventKind::AgentStarted];
    let provider = rec
        .events
        .iter()
        .find(|e| !e.provider_extension.is_empty())
        .map(|e| clean(&e.provider_extension, 160));
    let parsers: BTreeSet<String> = rec
        .events
        .iter()
        .map(|e| clean(&e.source.parser, 160))
        .collect();
    let plan = match (&rec.plan, &rec.plan_error) {
        (Some(p), _) => Known::Value(p.get("plan_hash").and_then(Value::as_str).map_or_else(
            || format!("sha256:{}", sha256_hex(p.to_string().as_bytes())),
            |h| clean(h, 160),
        )),
        (None, Some(e)) => Known::Gap(format!("plan.json could not be read: {}", clean(e, 160))),
        (None, None) => Known::Gap("no plan.json next to the trace".into()),
    };
    let from_result = |f: &dyn Fn(&EvaluationResult) -> Option<String>, why: &str| {
        known(result.and_then(f).map(|v| clean(&v, 160)), why)
    };
    let cli = data_values(rec, &started, "cli_version").or_else(|| {
        result
            .and_then(|r| r.context.cli_version.as_deref())
            .map(|v| clean(v, 160))
    });
    let entry = |name: &str, value: Known<String>| EnvEntry {
        name: name.into(),
        value,
    };
    vec![
        entry("provider_extension", known(provider, "no event names the provider extension")),
        entry(
            "parser",
            known(
                (!parsers.is_empty()).then(|| parsers.into_iter().collect::<Vec<_>>().join(", ")),
                "the trace has no events",
            ),
        ),
        entry("cli_version", known(cli, "no CLI version in the trace")),
        entry(
            "model",
            known(data_values(rec, &started, "model"), "no model reported in the trace"),
        ),
        entry(
            "permission_mode",
            known(
                data_values(rec, &started, "permission_mode"),
                "no permission mode reported in the trace",
            ),
        ),
        entry("workflow_plan", plan),
        entry(
            "repository_revision",
            from_result(
                &|r: &EvaluationResult| Some(r.context.repository_revision.clone()),
                "the trace records no git revision; an evaluation result for the run gives one",
            ),
        ),
        entry(
            "extension_version",
            from_result(
                &|r: &EvaluationResult| Some(r.context.extension_version.clone()),
                "the trace records no extension version; an evaluation result for the run gives one",
            ),
        ),
    ]
}

fn fields_of(data: &Value) -> BTreeMap<String, String> {
    data.as_object()
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| {
                    let text = match v {
                        Value::String(s) => clean(s, 160),
                        Value::Number(n) => n.to_string(),
                        Value::Bool(b) => b.to_string(),
                        _ => return None,
                    };
                    Some((clean(k, 160), text))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One run read as a side of a comparison.
pub fn side(run: &RunInput) -> RunSide {
    let rec = run.rec;
    let row = RunRow::of(run.id, rec);
    let p = project(&rec.events);
    let live: Vec<&TraceEvent> = rec
        .events
        .iter()
        .filter(|e| e.corroborates.is_none())
        .collect();

    let mut tools = ToolCounts {
        total: p.tools.len(),
        completed: 0,
        failed: 0,
        unknown: 0,
        by_name: BTreeMap::new(),
    };
    for t in &p.tools {
        match t.outcome {
            ToolOutcome::Completed => tools.completed += 1,
            ToolOutcome::Failed => tools.failed += 1,
            ToolOutcome::Unknown => tools.unknown += 1,
        }
        let name = t
            .name
            .as_deref()
            .map_or_else(|| "(unnamed)".to_owned(), |n| clean(n, 160));
        *tools.by_name.entry(name).or_default() += 1;
    }

    let all: Vec<&TraceEvent> = rec.events.iter().collect();
    let latency = span_ms(&all).map_or_else(
        || unknown("latency_ms", "ms", "no usable timestamps in the trace"),
        |ms| Metric {
            name: "latency_ms".into(),
            value: Some(ms as f64),
            unit: "ms".into(),
            quality: DataQuality::Derived,
            source: "aibus clock, first to last event".into(),
        },
    );

    let routes: Vec<RouteRow> = live
        .iter()
        .filter(|e| e.kind == EventKind::RouteDecided)
        .map(|e| RouteRow {
            position: e.position,
            visibility: e.visibility,
            origin: e.origin,
            fields: fields_of(&e.data),
        })
        .collect();
    let routing = if routes.is_empty() {
        Known::Gap(
            "no route_decided event in the trace: why it ran this way is not recorded".into(),
        )
    } else {
        Known::Value(routes)
    };

    let chunks: Vec<&&TraceEvent> = live
        .iter()
        .filter(|e| e.kind == EventKind::MessageChunk)
        .collect();
    let sizes: Option<Vec<u64>> = chunks
        .iter()
        .map(|e| e.data.get("bytes").and_then(Value::as_u64))
        .collect();
    let message_bytes = match sizes {
        Some(s) if !s.is_empty() => Known::Value(s.iter().sum()),
        Some(_) => Known::Gap("no message was recorded".into()),
        None => Known::Gap("a message chunk carries no size".into()),
    };
    let outputs = Outputs {
        message_chunks: chunks.len(),
        message_bytes,
        payloads_retained: live
            .iter()
            .filter_map(|e| e.payload_ref.as_deref())
            .filter(|r| payload_bytes(rec, r).is_some())
            .count(),
        artifacts: live
            .iter()
            .filter(|e| e.kind == EventKind::ArtifactCreated)
            .map(|e| ArtifactRef {
                position: e.position,
                name: data_str(e, "name").map(|n| clean(n, 160)),
                payload_ref: e.payload_ref.as_deref().map(|r| clean(r, 160)),
            })
            .collect(),
    };

    let quality = run.result.map_or_else(
        || {
            Known::Gap(
                "no evaluation result was given for this run, so its tests are unknown".into(),
            )
        },
        |r| {
            let n = |o: CheckOutcome| r.checks.iter().filter(|c| c.outcome == o).count();
            Known::Value(Quality {
                suite: clean(&r.suite, 160),
                case_id: clean(&r.case_id, 160),
                variant: clean(&r.variant, 160),
                verdict: wire(r.verdict),
                checks_passed: n(CheckOutcome::Pass),
                checks_failed: n(CheckOutcome::Fail),
                checks_not_run: n(CheckOutcome::NotRun),
            })
        },
    );
    let environment = environment(rec, run.result);
    let git_revision = environment
        .iter()
        .find(|e| e.name == "repository_revision")
        .map_or_else(|| Known::Gap("not recorded".into()), |e| e.value.clone());

    RunSide {
        id: row.id,
        state: row.state,
        provider: row.provider,
        events: row.events,
        attempts: row.attempts,
        agents: row.agents,
        edges: row.edges,
        gaps: row.gaps,
        tools,
        latency,
        usage: usage_of(&p),
        routing,
        outputs,
        quality,
        git_revision,
        worktree_diff: Known::Gap(
            "no git or worktree diff is recorded in a trace; nothing stores one yet".into(),
        ),
        environment,
    }
}

fn change(a: &Metric, b: &Metric) -> Known<f64> {
    match (a.value, b.value) {
        (Some(x), Some(y)) if a.unit == b.unit => Known::Value(y - x),
        (Some(_), Some(_)) => Known::Gap(format!("the units differ ({} and {})", a.unit, b.unit)),
        (None, None) => Known::Gap("both are unknown".into()),
        (None, _) => Known::Gap(format!("a is unknown ({})", a.source)),
        (_, None) => Known::Gap(format!("b is unknown ({})", b.source)),
    }
}

/// Two runs side by side.
pub fn compare_runs(a: &RunInput, b: &RunInput) -> Comparison {
    let (sa, sb) = (side(a), side(b));
    let mut names: BTreeSet<&str> = sa
        .usage
        .iter()
        .chain(&sb.usage)
        .map(|m| m.name.as_str())
        .collect();
    names.remove("latency_ms");
    let find = |s: &RunSide, name: &str| -> Metric {
        if name == "latency_ms" {
            return s.latency.clone();
        }
        s.usage
            .iter()
            .find(|m| m.name == name)
            .cloned()
            .unwrap_or_else(|| unknown(name, "", "not reported in this run's trace"))
    };
    let deltas = std::iter::once("latency_ms")
        .chain(names)
        .map(|name| {
            let (x, y) = (find(&sa, name), find(&sb, name));
            Delta {
                metric: name.to_owned(),
                change: change(&x, &y),
                a: x,
                b: y,
            }
        })
        .collect();

    let environment: Vec<EnvRow> = sa
        .environment
        .iter()
        .zip(&sb.environment)
        .map(|(x, y)| EnvRow {
            name: x.name.clone(),
            verdict: match (&x.value, &y.value) {
                (Known::Value(p), Known::Value(q)) if p == q => EnvVerdict::Same,
                (Known::Value(_), Known::Value(_)) => EnvVerdict::Differs,
                _ => EnvVerdict::Unknown,
            },
            a: x.value.clone(),
            b: y.value.clone(),
        })
        .collect();
    let named = |v: EnvVerdict| -> Vec<String> {
        environment
            .iter()
            .filter(|r| r.verdict == v)
            .map(|r| r.name.clone())
            .collect()
    };
    let comparable = if !named(EnvVerdict::Differs).is_empty() {
        Comparable::No(named(EnvVerdict::Differs))
    } else if !named(EnvVerdict::Unknown).is_empty() {
        Comparable::Unknown(named(EnvVerdict::Unknown))
    } else {
        Comparable::Yes
    };
    Comparison {
        a: sa,
        b: sb,
        deltas,
        environment,
        comparable,
    }
}

fn known_text<T>(k: &Known<T>, f: impl FnOnce(&T) -> String) -> String {
    match k {
        Known::Value(v) => f(v),
        Known::Gap(why) => format!("gap: {why}"),
    }
}

/// A number for a person: at most 6 decimals, no trailing zeros, so 0.1 + 0.2 reads 0.3. The JSON
/// keeps the exact value.
fn num(v: f64) -> String {
    let t = format!("{v:.6}");
    let t = t.trim_end_matches('0').trim_end_matches('.');
    if t.is_empty() || t == "-" {
        "0".into()
    } else {
        t.to_owned()
    }
}

fn metric_text(m: &Metric) -> String {
    match m.value {
        Some(v) => format!("{} {} ({}, {})", num(v), m.unit, wire(m.quality), m.source),
        None => format!("unknown ({})", m.source),
    }
}

/// The comparison as text: the five reviewer questions (what ran, why it was routed this way,
/// what each produced, what changed, which variant passed more tests), then cost and latency,
/// tool counts and the environment. Every gap is a line that says it is one.
pub fn render(c: &Comparison) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "comparison of {} (a) and {} (b)", c.a.id, c.b.id);
    match &c.comparable {
        Comparable::Yes => s.push_str("comparable: every environment field is known and equal\n"),
        Comparable::No(f) => {
            let _ = writeln!(
                s,
                "not comparable as like for like: {} differ",
                f.join(", ")
            );
        }
        Comparable::Unknown(f) => {
            let _ = writeln!(
                s,
                "comparability unknown: {} not known for both runs",
                f.join(", ")
            );
        }
    }
    let both = |s: &mut String, f: &dyn Fn(&RunSide) -> String| {
        let _ = writeln!(s, "  a: {}", f(&c.a));
        let _ = writeln!(s, "  b: {}", f(&c.b));
    };
    s.push_str("\nwhat ran\n");
    both(&mut s, &|r: &RunSide| {
        format!(
            "{} {}, provider {}, {} events, {} attempts, {} agents, edges observed/correlated/inferred/unavailable {}/{}/{}/{}, {} gaps",
            r.id,
            wire(r.state),
            r.provider.as_deref().unwrap_or("unknown"),
            r.events,
            r.attempts,
            r.agents,
            r.edges[0],
            r.edges[1],
            r.edges[2],
            r.edges[3],
            r.gaps
        )
    });
    s.push_str("\nrouting\n");
    both(&mut s, &|r: &RunSide| {
        known_text(&r.routing, |rows| {
            rows.iter()
                .map(|x| {
                    let f: Vec<String> = x.fields.iter().map(|(k, v)| format!("{k}={v}")).collect();
                    format!(
                        "#{} {} {}: {}",
                        x.position,
                        wire(x.visibility),
                        wire(x.origin),
                        f.join(" ")
                    )
                })
                .collect::<Vec<_>>()
                .join("; ")
        })
    });
    s.push_str("\noutputs\n");
    both(&mut s, &|r: &RunSide| {
        let o = &r.outputs;
        let arts: Vec<String> = o
            .artifacts
            .iter()
            .map(|a| {
                format!(
                    "#{} {}",
                    a.position,
                    a.name.as_deref().unwrap_or("(unnamed)")
                )
            })
            .collect();
        format!(
            "{} message chunks ({}), {} payloads retained, artifacts: {}",
            o.message_chunks,
            known_text(&o.message_bytes, |b| format!("{b} bytes")),
            o.payloads_retained,
            if arts.is_empty() {
                "none recorded".into()
            } else {
                arts.join(", ")
            }
        )
    });
    s.push_str("\nquality\n");
    both(&mut s, &|r: &RunSide| {
        known_text(&r.quality, |q| {
            format!(
                "{} / {} variant {}: {}, {} passed, {} failed, {} not run",
                q.suite,
                q.case_id,
                q.variant,
                q.verdict,
                q.checks_passed,
                q.checks_failed,
                q.checks_not_run
            )
        })
    });
    s.push_str("\ngit and worktree\n");
    both(&mut s, &|r: &RunSide| {
        format!(
            "revision {}; worktree diff {}",
            known_text(&r.git_revision, |v| v.clone()),
            known_text(&r.worktree_diff, |v| v.clone())
        )
    });
    s.push_str("\nlatency, tokens and cost (b minus a)\n");
    for d in &c.deltas {
        let _ = writeln!(
            s,
            "  {}: a {}; b {}; change {}",
            d.metric,
            metric_text(&d.a),
            metric_text(&d.b),
            known_text(&d.change, |v| num(*v))
        );
    }
    s.push_str("\ntools\n");
    both(&mut s, &|r: &RunSide| {
        let t = &r.tools;
        let names: Vec<String> = t.by_name.iter().map(|(k, v)| format!("{k} {v}")).collect();
        format!(
            "{} total ({} completed, {} failed, {} never reported back): {}",
            t.total,
            t.completed,
            t.failed,
            t.unknown,
            if names.is_empty() {
                "none".into()
            } else {
                names.join(", ")
            }
        )
    });
    s.push_str("\nenvironment\n");
    for r in &c.environment {
        let _ = writeln!(
            s,
            "  {} [{}]: a {}; b {}",
            r.name,
            wire(r.verdict),
            known_text(&r.a, |v| v.clone()),
            known_text(&r.b, |v| v.clone())
        );
    }
    s
}
