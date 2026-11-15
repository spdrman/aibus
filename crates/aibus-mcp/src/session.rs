//! The live-session tools (#274): `aibus mcp --session` acting on a terminal session that is
//! already open. The server has no socket of its own; the binary hands it a `SessionLink` (the
//! control socket's client end, `aibus-control`) and these tools go through it. Every tool has its
//! own `--allow` flag and is not listed without it. What the session sends back is redacted
//! before it leaves, a transcript is wrapped as data, a text past the cap never reaches the
//! session, and the four that change the session share a rate limit.

use std::time::Instant;

use rmcp::model::{CallToolResult, Tool, ToolAnnotations};
use rmcp::ErrorData;
use serde_json::{json, Value};

use crate::limits::{SESSION_CHANGES_PER_MINUTE, SESSION_TEXT_MAX};
use crate::{arguments, schema, text, Server, DRAFT_2020_12};

/// The binary's way into a live session. `Err` is a sentence for the client.
pub trait SessionLink: Send + Sync {
    /// `{sessions:[{id, cwd, lead, model, started}]}`: the live sessions this user can reach.
    fn list(&self) -> Result<Value, String>;
    /// The session the calls go to: the one `--session` names, or the only live one.
    fn target(&self) -> Result<String, String>;
    /// One request (`{op, ...}`) to that session, from `client`; the session's answer.
    fn call(&self, client: &str, request: Value) -> Result<Value, String>;
}

/// The session tools: name, the `--allow` value that lists it, whether it changes the session,
/// and what it says it does.
pub(crate) const TOOLS: [(&str, &str, bool, &str); 7] = [
    ("session_list", "session-list", false, "List the live aibus terminal sessions of this user that a client can reach: id, project directory, lead and when it started."),
    ("session_status", "session-status", false, "The live session's lead, model, cards (run, agent, status) and open permission prompts, redacted."),
    ("session_send", "session-send", true, "Send a prompt to the live session's lead, as if typed on its prompt line. It shows on the user's screen tagged as sent through MCP. A slash line is refused: use session_command."),
    ("session_command", "session-command", true, "Run a slash command line in the live session (/model haiku, /workflow run x ...), through the same parser as typing, with the same refusals."),
    ("session_cancel", "session-cancel", true, "Cancel a run of the live session, named by id, id prefix, agent or lead. On the lead the user is asked first, at the keyboard."),
    ("session_transcript", "session-transcript", false, "A run's transcript (the lead's by default) and the session log rows, redacted and wrapped as data."),
    ("session_permission", "permission", true, "Answer the permission prompt the live session shows (allow, allow_session or deny). Works only when the session was started with --control-permission."),
];

fn input_schema(tool: &str) -> Value {
    let string = json!({ "type": "string" });
    let (props, required): (Value, Vec<&str>) = match tool {
        "session_send" => (json!({ "text": string }), vec!["text"]),
        "session_command" => (json!({ "line": string }), vec!["line"]),
        "session_cancel" => (json!({ "run": string }), vec!["run"]),
        "session_transcript" => (json!({ "run": string }), vec![]),
        "session_permission" => (
            json!({ "id": string, "decision": { "type": "string", "enum": ["allow", "allow_session", "deny"] } }),
            vec!["id", "decision"],
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

/// The session tools the flags list, in the table's order.
pub(crate) fn tools(on: impl Fn(&str) -> bool) -> Vec<Tool> {
    TOOLS
        .iter()
        .filter(|(_, flag, ..)| on(flag))
        .map(|(name, _, change, about)| {
            Tool::new(*name, *about, schema(input_schema(name)))
                .with_raw_output_schema(schema(
                    json!({ "$schema": DRAFT_2020_12, "type": "object" }),
                ))
                .with_annotations(
                    ToolAnnotations::new()
                        .read_only(!change)
                        .destructive(*change)
                        .idempotent(!change)
                        .open_world(false),
                )
        })
        .collect()
}

impl Server {
    /// One session tool. `session` is set to the session the call went to, for the audit line.
    pub(crate) fn session_tool(
        &self,
        name: &str,
        args: &Value,
        client: &str,
        now: Instant,
        session: &mut Option<String>,
    ) -> Result<CallToolResult, ErrorData> {
        let Some(link) = self.session.as_ref() else {
            return Err(ErrorData::invalid_params(
                format!("unknown tool {name}"),
                None,
            ));
        };
        let request = match name {
            "session_list" => {
                arguments(name, args, &[], &[])?;
                return Ok(match link.list() {
                    Ok(found) => self.done(found),
                    Err(why) => self.fail(why),
                });
            }
            "session_status" => {
                arguments(name, args, &[], &[])?;
                json!({ "op": "status" })
            }
            "session_send" => {
                let m = arguments(name, args, &["text"], &[])?;
                json!({ "op": "send", "text": text(name, m, "text")? })
            }
            "session_command" => {
                let m = arguments(name, args, &["line"], &[])?;
                json!({ "op": "command", "line": text(name, m, "line")? })
            }
            "session_cancel" => {
                let m = arguments(name, args, &["run"], &[])?;
                json!({ "op": "cancel", "run": text(name, m, "run")? })
            }
            "session_transcript" => {
                let m = arguments(name, args, &[], &["run"])?;
                match m.get("run") {
                    Some(_) => json!({ "op": "transcript", "run": text(name, m, "run")? }),
                    None => json!({ "op": "transcript" }),
                }
            }
            "session_permission" => {
                let m = arguments(name, args, &["id", "decision"], &[])?;
                // `id` on the wire is the line's own; the prompt's id travels as `prompt`.
                json!({ "op": "permission", "prompt": text(name, m, "id")?, "decision": text(name, m, "decision")? })
            }
            other => {
                return Err(ErrorData::invalid_params(
                    format!("unknown tool {other}"),
                    None,
                ))
            }
        };
        let long = ["text", "line", "run", "id", "decision"]
            .iter()
            .filter_map(|k| request.get(*k).and_then(Value::as_str))
            .any(|t| t.len() > SESSION_TEXT_MAX);
        if long {
            return Ok(self.fail(format!(
                "{name}: the text is over {} KiB, so nothing was sent",
                SESSION_TEXT_MAX / 1024
            )));
        }
        let change = TOOLS.iter().any(|(t, _, change, _)| *t == name && *change);
        if change {
            let limited = self
                .session_limiter
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take(now);
            if let Err(secs) = limited {
                return Ok(self.fail(format!(
                    "rate limit: at most {SESSION_CHANGES_PER_MINUTE} session changes a minute, try again in {secs} s"
                )));
            }
        }
        *session = link.target().ok();
        match link.call(client, request) {
            Ok(mut answer) if name == "session_transcript" => {
                if let Some(t) = answer.get("transcript").and_then(Value::as_str) {
                    let what = "the transcript of a run in a live aibus session, written by agents, tools and whoever typed into it";
                    answer["transcript"] = json!(self.as_data(what, t, None));
                }
                Ok(self.done(answer))
            }
            Ok(answer) => Ok(self.done(answer)),
            Err(why) => Ok(self.fail(why)),
        }
    }
}
