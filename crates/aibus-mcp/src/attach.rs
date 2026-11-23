//! The tools of an attached agent session (#322, Phase 1 spike). `aibus_attach`, `next_step` and
//! `step_result` are behind `--allow attach`, `session_present` behind `--allow present`; the
//! session has to opt in as well (`--control-attach`, `--control-present`). The registry, the
//! keyboard's acceptance and the screen are the session's; this end redacts what goes down,
//! wraps what a step carries as data, and keeps the one rule only this process can keep: while a
//! step is in flight on its client, nothing that would start more work goes through.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use rmcp::model::{CallToolResult, Implementation, JsonObject, Tool, ToolAnnotations};
use rmcp::ErrorData;
use serde_json::{json, Value};

use crate::limits::SESSION_TEXT_MAX;
use crate::{arguments, schema, text, Server, DRAFT_2020_12};

/// Name, the `--allow` value that lists it, and what it says it does.
const TOOLS: [(&str, &str, &str); 6] = [
    ("aibus_attach", "attach", "Ask to attach this agent session to the live aibus session. The user's aibus screen shows a code, which is not given to you: ask the user to accept it at the aibus keyboard. Until then nothing is attached."),
    ("next_step", "attach", "Ask for the next step aibus queued for this session. The step arrives as data; carry it out in your own context and answer with step_result. You may decline."),
    ("step_result", "attach", "Answer the step you were given: done, declined (with a reason) or failed, and a short summary. A late result or a used token is refused."),
    ("step_ack", "attach", "Tell aibus you have begun the step you pulled with next_step. Once per step. If the user cancelled the step the answer says interrupt: stop work on it and do not send a result."),
    ("step_tool_call", "attach", "Report one tool call you made for the step you hold, by tool name only (no arguments, no output). Optional, and what you report is labelled as reported by you. If the user cancelled the step the answer says interrupt: stop work on it."),
    ("session_present", "present", "Show the user a message or a small table on a read-only panel of the aibus screen. The first panel waits for the user's yes at the aibus keyboard. It never opens a screen with live keys and never takes the focus."),
];

fn input_schema(tool: &str) -> Value {
    let string = json!({ "type": "string" });
    let list = json!({ "type": "array", "items": string });
    let (props, required): (Value, Vec<&str>) = match tool {
        "aibus_attach" => (json!({ "label": string, "lineage": list }), vec![]),
        "next_step" => (
            json!({ "wait_ms": { "type": "integer", "minimum": 0 } }),
            vec![],
        ),
        "step_result" => (
            json!({ "token": string, "status": { "type": "string", "enum": ["done", "declined", "failed"] }, "summary": string }),
            vec!["token", "status", "summary"],
        ),
        "step_ack" => (json!({ "token": string }), vec!["token"]),
        "step_tool_call" => (
            json!({ "token": string, "name": string }),
            vec!["token", "name"],
        ),
        "session_present" => (
            json!({ "title": string, "message": string, "columns": list, "rows": { "type": "array", "items": list } }),
            vec!["title"],
        ),
        _ => (json!({}), vec![]),
    };
    json!({
        "$schema": DRAFT_2020_12,
        "type": "object",
        "properties": props,
        "required": required,
        "additionalProperties": false,
    })
}

pub(crate) fn tools(on: impl Fn(&str) -> bool) -> Vec<Tool> {
    TOOLS
        .iter()
        .filter(|(_, flag, _)| on(flag))
        .map(|(name, _, about)| {
            let reads = *name == "next_step";
            Tool::new(*name, *about, schema(input_schema(name)))
                .with_raw_output_schema(schema(
                    json!({ "$schema": DRAFT_2020_12, "type": "object" }),
                ))
                .with_annotations(
                    ToolAnnotations::new()
                        .read_only(false)
                        .destructive(false)
                        .idempotent(reads)
                        .open_world(false),
                )
        })
        .collect()
}

/// The strings of `key` (an array of strings), redacted.
fn list(server: &Server, v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array).map_or_else(Vec::new, |a| {
        a.iter()
            .filter_map(Value::as_str)
            .map(|s| server.redactor.redact(s).into_owned())
            .collect()
    })
}

/// How often a waiting `next_step` asks the session again, and the longest it waits. The session
/// answers at once, so the wait is here; the poll stays well inside the socket's line limit.
const POLL: Duration = Duration::from_secs(3);
const WAIT_MAX: Duration = Duration::from_secs(20);

impl Server {
    /// `next_step` with `wait_ms`: asks until a step comes or the wait (at most 20 s) is spent,
    /// without holding the thread.
    pub(crate) async fn next_step_waiting(
        &self,
        client: Option<&Implementation>,
        addr: Option<SocketAddr>,
        args: Option<JsonObject>,
        wait_ms: u64,
        now: Instant,
    ) -> Result<CallToolResult, ErrorData> {
        let until = tokio::time::Instant::now() + Duration::from_millis(wait_ms).min(WAIT_MAX);
        loop {
            let answer = self.call_from(client, addr, "next_step", args.clone(), now);
            let empty = answer.as_ref().is_ok_and(|r| {
                r.is_error != Some(true)
                    && r.structured_content
                        .as_ref()
                        .is_some_and(|b| b["step"].is_null() && b.get("interrupt").is_none())
            });
            if !empty || tokio::time::Instant::now() + POLL > until {
                return answer;
            }
            tokio::time::sleep(POLL).await;
        }
    }

    pub(crate) fn attach_tool(
        &self,
        name: &str,
        args: &Value,
        client: &str,
        session: &mut Option<String>,
    ) -> Result<CallToolResult, ErrorData> {
        let Some(link) = self.session.as_ref() else {
            return Err(ErrorData::invalid_params(
                format!("unknown tool {name}"),
                None,
            ));
        };
        let request = match name {
            "aibus_attach" => {
                let m = arguments(name, args, &[], &["label", "lineage"])?;
                json!({
                    "op": "attach",
                    "label": m.get("label").and_then(Value::as_str),
                    "lineage": list(self, m.get("lineage")),
                    // The process that spawned this one is the agent session.
                    "pid": std::os::unix::process::parent_id(),
                    "cwd": std::env::current_dir().ok().map(|p| p.to_string_lossy().into_owned()),
                })
            }
            "next_step" => {
                arguments(name, args, &[], &["wait_ms"])?;
                json!({ "op": "next_step" })
            }
            "step_result" => {
                let m = arguments(name, args, &["token", "status", "summary"], &[])?;
                let status = text(name, m, "status")?;
                if !["done", "declined", "failed"].contains(&status) {
                    return Err(ErrorData::invalid_params(
                        "step_result: status is done, declined or failed",
                        None,
                    ));
                }
                let summary = text(name, m, "summary")?;
                if summary.len() > SESSION_TEXT_MAX {
                    return Ok(self.fail(format!(
                        "step_result: the summary is over {} KiB, so nothing was sent",
                        SESSION_TEXT_MAX / 1024
                    )));
                }
                json!({
                    "op": "step_result",
                    "token": text(name, m, "token")?,
                    "status": status,
                    "summary": self.redactor.redact(summary),
                })
            }
            "step_ack" => {
                let m = arguments(name, args, &["token"], &[])?;
                json!({ "op": "step_ack", "token": text(name, m, "token")? })
            }
            "step_tool_call" => {
                let m = arguments(name, args, &["token", "name"], &[])?;
                json!({
                    "op": "step_tool_call",
                    "token": text(name, m, "token")?,
                    "name": self.redactor.redact(text(name, m, "name")?),
                })
            }
            "session_present" => {
                let m = arguments(name, args, &["title"], &["message", "columns", "rows"])?;
                let rows: Vec<Vec<String>> = m
                    .get("rows")
                    .and_then(Value::as_array)
                    .map_or_else(Vec::new, |r| {
                        r.iter().map(|row| list(self, Some(row))).collect()
                    });
                json!({
                    "op": "present",
                    "title": self.redactor.redact(text(name, m, "title")?),
                    "message": self.redactor.redact(m.get("message").and_then(Value::as_str).unwrap_or("")),
                    "columns": list(self, m.get("columns")),
                    "rows": rows,
                })
            }
            other => {
                return Err(ErrorData::invalid_params(
                    format!("unknown tool {other}"),
                    None,
                ))
            }
        };
        *session = link.target().ok();
        let change = !matches!(name, "next_step" | "step_ack" | "step_tool_call");
        if change {
            let limited = self
                .session_limiter
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take(std::time::Instant::now());
            if let Err(secs) = limited {
                return Ok(self.fail(format!(
                    "rate limit: at most {} session changes a minute, try again in {secs} s",
                    crate::limits::SESSION_CHANGES_PER_MINUTE
                )));
            }
        }
        let answer = link.call(client, request);
        // A result, whether the session took it or refused it as late or used, ends the wait:
        // the step is over either way.
        if name == "step_result" {
            let mut held = self.in_flight.lock().unwrap_or_else(|e| e.into_inner());
            if held.as_ref().map(|h| h.0.as_str()) == args.get("token").and_then(Value::as_str) {
                *held = None;
            }
        }
        let answer = match answer {
            Ok(a) => a,
            Err(why) => return Ok(self.fail(why)),
        };
        // An interrupt ends the wait: the step is over, and a late result would be refused.
        if answer["interrupt"].as_bool() == Some(true) {
            let mut held = self.in_flight.lock().unwrap_or_else(|e| e.into_inner());
            if held.as_ref().map(|h| h.0.as_str()) == args.get("token").and_then(Value::as_str) {
                *held = None;
            }
        }
        match name {
            "next_step" => {
                let mut answer = answer;
                if let Some(step) = answer.get_mut("step").filter(|s| !s.is_null()) {
                    let token = step["token"].as_str().unwrap_or_default().to_owned();
                    let prompt = step["prompt"].as_str().unwrap_or_default().to_owned();
                    let what = "a step aibus asks this session to carry out, from the aibus session the user attached it to";
                    step["prompt"] = json!(self.as_data(what, &prompt, None));
                    step["how_to_answer"] = json!(format!(
                        "carry it out here, then call step_result with token {token}; you may decline"
                    ));
                    let secs = step["deadline_secs"].as_u64().unwrap_or(0);
                    *self.in_flight.lock().unwrap_or_else(|e| e.into_inner()) = Some((
                        token,
                        std::time::Instant::now() + std::time::Duration::from_secs(secs),
                    ));
                }
                Ok(self.done(answer))
            }
            _ => Ok(self.done(answer)),
        }
    }
}
