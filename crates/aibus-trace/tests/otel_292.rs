//! #292: the canonical trace as OTLP spans. Hierarchy follows what the trace recorded, usage keeps
//! its quality, secrets are redacted, revisions ride along, and a thin trace says so.

use std::time::{Duration, SystemTime};

use aibus_core::ids::AccountName;
use aibus_core::redact::Redactor;
use aibus_trace::otel::{to_otlp, Revisions};
use aibus_trace::{AttemptRef, EventIds, EventKind, Origin, TraceEvent, Visibility};
use serde_json::{json, Value};
use uuid::Uuid;

fn attempt() -> AttemptRef {
    AttemptRef {
        workflow_run_id: Uuid::from_u128(1),
        attempt_id: Uuid::from_u128(2),
        parent_attempt_id: None,
        provider_extension: "org.aibus.test".into(),
    }
}

fn trace(with_usage: bool, finish_tool: bool) -> Vec<TraceEvent> {
    use EventKind::*;
    let mut v: Vec<TraceEvent> = Vec::new();
    let mut add = |kind, f: &dyn Fn(&mut TraceEvent)| {
        let n = v.len() as u64 + 1;
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000 + n);
        let mut e = TraceEvent::new(
            &attempt(),
            EventIds::default().next(at, &[&n.to_le_bytes()]),
            kind,
            Origin::CliStream,
            Visibility::Observed,
            at,
            "test/1",
        );
        f(&mut e);
        v.push(e);
    };
    add(AttemptStarted, &|_| {});
    add(AgentStarted, &|e| {
        e.agent_id = Some("lead".into());
        e.data = json!({ "role": "lead" });
    });
    add(TaskDelegated, &|e| {
        e.agent_id = Some("lead".into());
        e.native.tool_use_id = Some("toolu_A".into());
        e.data = json!({ "subagent_type": "reader" });
    });
    add(AgentStarted, &|e| {
        e.agent_id = Some("child".into());
        e.parent_agent_id = Some("lead".into());
        e.parent_tool_use_id = Some("toolu_A".into());
    });
    add(ToolStarted, &|e| {
        e.agent_id = Some("child".into());
        e.native.tool_use_id = Some("toolu_R".into());
        e.data = json!({ "tool": "Read /home/me/sk-secret-123" });
    });
    if finish_tool {
        add(ToolCompleted, &|e| {
            e.agent_id = Some("child".into());
            e.native.tool_use_id = Some("toolu_R".into());
        });
    }
    add(AgentStopped, &|e| {
        e.agent_id = Some("child".into());
        e.data = json!({ "status": "completed" });
    });
    if with_usage {
        add(UsageReported, &|e| {
            e.data = json!({ "scope": "attempt", "metrics": [
                { "name": "input_tokens", "value": 120.0, "unit": "tokens", "quality": "reported", "source": "vendor" },
                { "name": "cost", "value": null, "unit": "usd", "quality": "unknown", "source": "none" },
            ]});
        });
    }
    add(AttemptCompleted, &|e| {
        e.data = json!({ "status": "completed", "requires_reconciliation": false });
    });
    v
}

fn spans(body: &Value) -> Vec<Value> {
    body["resourceSpans"][0]["scopeSpans"][0]["spans"]
        .as_array()
        .unwrap()
        .clone()
}

fn find<'a>(s: &'a [Value], name: &str, attr: Option<(&str, &str)>) -> &'a Value {
    s.iter()
        .find(|x| {
            x["name"] == name
                && attr.is_none_or(|(k, v)| {
                    x["attributes"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|a| a["key"] == k && a["value"]["stringValue"] == v)
                })
        })
        .unwrap_or_else(|| panic!("no span {name}"))
}

fn attr<'a>(s: &'a Value, key: &str) -> Option<&'a Value> {
    s["attributes"]
        .as_array()?
        .iter()
        .find(|a| a["key"] == key)
        .map(|a| &a["value"])
}

#[test]
fn hierarchy_follows_run_attempt_agent_delegation_tool() {
    let (body, c) = to_otlp(&trace(true, true), &Revisions::default(), &Redactor::new());
    let s = spans(&body);
    let run = find(&s, "aibus.workflow.run", None);
    let att = find(&s, "aibus.attempt", None);
    let lead = find(&s, "aibus.agent", Some(("aibus.agent.id", "lead")));
    let child = find(&s, "aibus.agent", Some(("aibus.agent.id", "child")));
    let deleg = find(&s, "aibus.delegation", None);
    let tool = find(&s, "aibus.tool", None);
    assert!(run.get("parentSpanId").is_none());
    assert_eq!(att["parentSpanId"], run["spanId"]);
    assert_eq!(lead["parentSpanId"], att["spanId"]);
    assert_eq!(deleg["parentSpanId"], lead["spanId"]);
    assert_eq!(
        child["parentSpanId"], deleg["spanId"],
        "child hangs under its delegation"
    );
    assert_eq!(tool["parentSpanId"], child["spanId"]);
    assert!(s.iter().all(|x| x["traceId"] == run["traceId"]));
    assert_eq!(c.unresolved_parents, 0);
    assert_eq!(c.orphan_ends, 0);
}

#[test]
fn export_is_deterministic_and_order_independent() {
    let mut a = trace(true, true);
    let first = to_otlp(&a, &Revisions::default(), &Redactor::new()).0;
    a.reverse();
    let second = to_otlp(&a, &Revisions::default(), &Redactor::new()).0;
    assert_eq!(first, second);
}

#[test]
fn usage_keeps_quality_and_unknown_has_no_number() {
    let (body, _) = to_otlp(&trace(true, true), &Revisions::default(), &Redactor::new());
    let s = spans(&body);
    let att = find(&s, "aibus.attempt", None);
    assert_eq!(
        attr(att, "aibus.usage.input_tokens.quality").unwrap()["stringValue"],
        "reported"
    );
    assert!(attr(att, "aibus.usage.input_tokens").is_some());
    assert_eq!(
        attr(att, "aibus.usage.cost.quality").unwrap()["stringValue"],
        "unknown"
    );
    assert!(
        attr(att, "aibus.usage.cost").is_none(),
        "an unknown value is never exported as a number"
    );
}

#[test]
fn secrets_are_redacted_before_export_and_names_stay_low_cardinality() {
    let r = Redactor::new();
    r.register(&AccountName("acct".into()), "sk-secret-123");
    let (body, _) = to_otlp(&trace(true, true), &Revisions::default(), &r);
    assert!(!body.to_string().contains("sk-secret-123"));
    let s = spans(&body);
    let mut names: Vec<&str> = s.iter().map(|x| x["name"].as_str().unwrap()).collect();
    names.sort_unstable();
    names.dedup();
    assert!(
        names
            .iter()
            .all(|n| n.starts_with("aibus.") && !n.contains(' ')),
        "{names:?}"
    );
}

#[test]
fn revisions_ride_on_the_resource_without_prompt_text() {
    let rev = Revisions {
        prompt: Some("prompt@3".into()),
        workflow: Some("wf@9".into()),
    };
    let (body, _) = to_otlp(&trace(true, true), &rev, &Redactor::new());
    let res = &body["resourceSpans"][0]["resource"];
    assert!(attr(res, "aibus.prompt_revision").is_some_and(|v| v["stringValue"] == "prompt@3"));
    assert!(attr(res, "aibus.workflow_revision").is_some_and(|v| v["stringValue"] == "wf@9"));
    assert!(attr(res, "service.name").is_some());
}

#[test]
fn a_thin_trace_reports_what_is_missing() {
    let (body, c) = to_otlp(
        &trace(false, false),
        &Revisions::default(),
        &Redactor::new(),
    );
    assert_eq!(c.attempts_without_usage, 1);
    assert_eq!(c.unfinished, 1, "the tool never completed");
    let s = spans(&body);
    let tool = find(&s, "aibus.tool", None);
    assert_eq!(attr(tool, "aibus.complete").unwrap()["boolValue"], false);
}
