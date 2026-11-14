//! The NDJSON event for a `Msg` (06 section 2): `type` is the `Msg` name, the other field names
//! are the spec's. Every string in an event passes through the redactor before it is printed.

use std::collections::HashMap;
use std::time::SystemTime;

use aibus_core::state::value::value_to_json;
use aibus_core::{Decision, Msg, Redactor};
use aibus_provider::StreamChunk;
use serde_json::{json, Value};

pub use aibus_core::persist::time::{date, rfc3339};

/// Longest prefix of `s` that fits in `max` bytes and ends on a character boundary.
fn prefix(s: &str, max: usize) -> &str {
    let mut end = max.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Turns `Msg`s into events. It remembers each tool call's input so that `PermissionNeeded`
/// can show it (06 section 2), and keeps every body under `result_max_bytes`.
pub struct Events<'a> {
    redactor: &'a Redactor,
    max_bytes: usize,
    inputs: HashMap<String, Value>,
}

impl<'a> Events<'a> {
    pub fn new(redactor: &'a Redactor, max_bytes: usize) -> Self {
        Self {
            redactor,
            max_bytes,
            inputs: HashMap::new(),
        }
    }

    /// A tool input too big to inline is replaced by its size (the full body is in the
    /// transcript, 06 section 4).
    fn bounded(&self, input: &Value) -> Value {
        let bytes = input.to_string().len();
        if bytes > self.max_bytes {
            json!({ "truncated": true, "bytes": bytes })
        } else {
            input.clone()
        }
    }

    /// The events for one message, redacted: usually one, several when a streamed text is longer
    /// than `max_bytes`, none for a stream with no text in it. Messages the spec's schema does
    /// not list carry their redacted summary (ids, counts, lengths) under the same `type` and `t`.
    pub fn event(&mut self, msg: &Msg, at: SystemTime) -> Vec<Value> {
        let mut all = match msg {
            Msg::Stream { run, chunks } => {
                let text: String = chunks
                    .iter()
                    .filter_map(|c| match c {
                        StreamChunk::TextDelta(t) => Some(t.as_str()),
                        _ => None,
                    })
                    .collect();
                let mut parts = Vec::new();
                let mut rest = text.as_str();
                while !rest.is_empty() {
                    let (head, tail) = rest.split_at(prefix(rest, self.max_bytes.max(4)).len());
                    parts.push(json!({ "run": run.to_string(), "text": head }));
                    rest = tail;
                }
                parts
            }
            other => vec![self.single(other)],
        };
        for fields in &mut all {
            if let Value::Object(o) = fields {
                o.insert("type".into(), msg.name().into());
                o.insert("t".into(), rfc3339(at).into());
            }
            self.redactor.redact_json(fields);
        }
        all
    }

    fn single(&mut self, msg: &Msg) -> Value {
        match msg {
            Msg::RunStarted {
                run,
                agent,
                model,
                account,
                ..
            } => json!({
                "run": run.to_string(),
                "agent": agent.as_str(),
                "model": model.0.as_str(),
                "account": account.0.as_str(),
            }),
            Msg::ToolCallRequested { run, call } => {
                let input = self.bounded(&call.input);
                self.inputs.insert(call.id.0.to_string(), input.clone());
                json!({
                    "run": run.to_string(),
                    "call": { "id": call.id.0.to_string(), "name": call.name.as_str(), "input": input },
                })
            }
            Msg::ToolCallFinished {
                run,
                call_id,
                result,
                duration,
            } => json!({
                "run": run.to_string(),
                "call_id": call_id.0.to_string(),
                "ok": !result.is_error,
                "bytes": result.content.len(),
                "ms": duration.as_millis() as u64,
            }),
            Msg::PermissionNeeded { request } => json!({
                "id": request.id.to_string(),
                "agent": request.agent.as_str(),
                "run": request.run.to_string(),
                "tool": request.tool.as_str(),
                "input": self.inputs.get(&request.call.0.to_string()).cloned().unwrap_or(Value::Null),
                "summary": request.summary,
            }),
            Msg::PermissionDecided {
                id,
                decision,
                reason,
            } => json!({
                "id": id.to_string(),
                "decision": match decision {
                    Decision::Allow => "allowed",
                    Decision::AllowSession => "allowed_session",
                    Decision::Deny => "denied",
                },
                "reason": reason,
            }),
            Msg::RouterAnswered {
                fork,
                question,
                answer,
            } => json!({
                "fork": fork.0,
                "kind": question.kind.as_str(),
                "choice": answer.choice,
                "p": (f64::from(answer.p) * 1e4).round() / 1e4,
                "verdict": answer.verdict.as_str(),
            }),
            Msg::AdvisorSpoke {
                checkpoint,
                advice,
                tokens_read,
            } => json!({
                "checkpoint": checkpoint.name(),
                "advice": prefix(advice, self.max_bytes),
                "tokens_read": tokens_read,
            }),
            Msg::RunFinished { run, result } => json!({
                "run": run.to_string(),
                "status": result.status.as_value(),
                "turns": result.turns,
                "usage": {
                    "input_tokens": result.usage.input_tokens,
                    "output_tokens": result.usage.output_tokens,
                    "cost_usd": result.usage.cost_usd,
                },
                "summary": prefix(&result.summary, self.max_bytes),
                "summary_file": result.summary_file,
            }),
            Msg::RunFailed { run, error } => json!({
                "run": run.to_string(),
                "kind": error.kind.as_str(),
                "message": error.message,
            }),
            other => {
                let mut v = value_to_json(&other.summary());
                if let Value::Object(o) = &mut v {
                    o.remove("msg");
                }
                v
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use super::*;
    use aibus_core::{RunId, ToolCall, ToolCallId};

    #[test]
    fn dates_and_timestamps() {
        let t = UNIX_EPOCH + std::time::Duration::from_millis(1_794_181_685_012);
        assert_eq!(date(t), "2026-11-08");
        assert_eq!(rfc3339(t), "2026-11-08T23:48:05.012Z");
        assert_eq!(date(UNIX_EPOCH), "1970-01-01");
        // A leap day.
        assert_eq!(
            date(UNIX_EPOCH + std::time::Duration::from_secs(1_709_164_800)),
            "2024-02-29"
        );
    }

    fn stream(text: &str) -> Msg {
        Msg::Stream {
            run: RunId::new(),
            chunks: vec![StreamChunk::TextDelta(text.into())],
        }
    }

    #[test]
    fn a_long_stream_is_split_on_character_boundaries_and_loses_nothing() {
        let r = Redactor::new();
        let mut ev = Events::new(&r, 10);
        let text = "aé".repeat(12);
        let lines = ev.event(&stream(&text), UNIX_EPOCH);
        assert!(lines.len() > 1);
        let joined: String = lines.iter().map(|l| l["text"].as_str().unwrap()).collect();
        assert_eq!(joined, text);
        assert!(lines
            .iter()
            .all(|l| l["text"].as_str().unwrap().len() <= 10));
        // A stream with no text (thinking only) prints nothing.
        let quiet = Msg::Stream {
            run: RunId::new(),
            chunks: vec![StreamChunk::ThinkingDelta("hmm".into())],
        };
        assert!(ev.event(&quiet, UNIX_EPOCH).is_empty());
    }

    #[test]
    fn a_tool_input_over_the_cap_is_replaced_by_its_size() {
        let r = Redactor::new();
        let mut ev = Events::new(&r, 32);
        let call = |input: Value| Msg::ToolCallRequested {
            run: RunId::new(),
            call: ToolCall {
                id: ToolCallId(ulid::Ulid::new()),
                name: "write".into(),
                input,
                needs_permission: false,
            },
        };
        let small = ev.event(&call(json!({"path": "a"})), UNIX_EPOCH);
        assert_eq!(small[0]["call"]["input"]["path"], "a");
        let big = ev.event(&call(json!({"content": "x".repeat(100)})), UNIX_EPOCH);
        assert_eq!(big[0]["call"]["input"]["truncated"], true);
        assert!(big[0]["call"]["input"]["bytes"].as_u64().unwrap() > 100);
    }
}
