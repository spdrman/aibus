//! Recorded `Msg` streams (10 section 5): `msgs.jsonl` holds what a run sent, one JSON object per
//! line, and replaying it through `reduce` must land on the snapshot hash that was recorded.
//!
//! Only the messages a run task sends are recorded. Tool output and the run summary are
//! redacted to their lengths (a replay needs no more than that); the fake provider's text and
//! the tool inputs are kept.

use std::time::Duration;

use aibus_graph::mini::MiniGraph;
use aibus_graph::Graph;
use aibus_provider::{StopReason, StreamChunk, Usage};
use serde_json::{json, Value};

use crate::app::App;
use crate::ids::{ForkId, PermissionId, RunId, ToolCallId};
use crate::msg::{
    Checkpoint, Decision, HookEvent, HookOutcome, Msg, PermissionRequest, RouterAnswer,
    RouterQuestion, ToolCall, ToolResult,
};
use crate::run::{RunError, RunResult};
use crate::Role;

fn chunk_json(c: &StreamChunk) -> Value {
    match c {
        StreamChunk::TextDelta(t) => json!({"k": "text", "v": t}),
        StreamChunk::ThinkingDelta(t) => json!({"k": "thinking", "v": t}),
        StreamChunk::ToolUseStart { id, name } => {
            json!({"k": "tool_start", "id": id.0.to_string(), "name": name})
        }
        StreamChunk::ToolUseInputDelta { id, json_fragment } => {
            json!({"k": "tool_input", "id": id.0.to_string(), "v": json_fragment})
        }
        StreamChunk::ToolUseEnd { id } => json!({"k": "tool_end", "id": id.0.to_string()}),
        StreamChunk::Usage(u) => json!({"k": "usage", "v": u}),
        StreamChunk::Logprob { token, logprob } => {
            json!({"k": "logprob", "token": token, "logprob": logprob})
        }
        StreamChunk::Done(r) => json!({"k": "done", "v": r}),
        StreamChunk::Waiting(t) => json!({"k": "waiting", "v": t}),
    }
}

/// One line of `msgs.jsonl`, or `None` for a message a run task never sends.
pub fn msg_to_json(msg: &Msg) -> Option<Value> {
    Some(match msg {
        Msg::RunStarted {
            run,
            agent,
            role,
            parent,
            model,
            account,
        } => json!({
            "msg": "RunStarted", "run": run.to_string(), "agent": agent.as_str(),
            "role": role.as_str(), "parent": parent.map(|p| p.to_string()),
            "model": model.0, "account": account.0,
        }),
        Msg::Stream { run, chunks } => json!({
            "msg": "Stream", "run": run.to_string(),
            "chunks": chunks.iter().map(chunk_json).collect::<Vec<_>>(),
        }),
        Msg::ToolCallRequested { run, call } => json!({
            "msg": "ToolCallRequested", "run": run.to_string(), "call": call.id.0.to_string(),
            "name": call.name, "input": call.input, "needs_permission": call.needs_permission,
        }),
        Msg::ToolCallFinished {
            run,
            call_id,
            result,
            duration,
        } => json!({
            "msg": "ToolCallFinished", "run": run.to_string(), "call": call_id.0.to_string(),
            "content_len": result.content.len(), "is_error": result.is_error,
            "ms": duration.as_millis() as u64,
        }),
        Msg::RunFinished { run, result } => {
            let mut r = serde_json::to_value(result).ok()?;
            r["summary"] = "".into();
            r["summary_len"] = result.summary.len().into();
            r["summary_file"] = Value::Null;
            json!({"msg": "RunFinished", "run": run.to_string(), "result": r})
        }
        Msg::RunFailed { run, error } => {
            json!({"msg": "RunFailed", "run": run.to_string(), "error": error})
        }
        Msg::UsageUpdate { run, usage } => {
            json!({"msg": "UsageUpdate", "run": run.to_string(), "usage": usage})
        }
        Msg::PermissionNeeded { request: r } => json!({
            "msg": "PermissionNeeded", "id": r.id.to_string(), "agent": r.agent.as_str(),
            "run": r.run.to_string(), "call": r.call.0.to_string(), "tool": r.tool,
            "summary": r.summary,
        }),
        Msg::PermissionDecided {
            id,
            decision,
            reason,
        } => json!({
            "msg": "PermissionDecided", "id": id.to_string(), "decision": decision,
            "reason": reason,
        }),
        Msg::HookFired {
            run,
            event,
            outcome,
        } => json!({
            "msg": "HookFired", "run": run.map(|r| r.to_string()), "event": event,
            "exit_code": outcome.exit_code, "message": outcome.message,
        }),
        Msg::RouterAnswered {
            fork,
            question,
            answer,
        } => json!({
            "msg": "RouterAnswered", "fork": fork.0, "kind": question.kind.as_str(),
            "context": question.context, "choices": question.choices,
            "choice": answer.choice, "p": answer.p, "verdict": answer.verdict,
            "latency_ms": answer.latency.as_millis() as u64, "source": answer.source,
        }),
        Msg::AdvisorSpoke {
            checkpoint,
            advice,
            tokens_read,
        } => json!({
            "msg": "AdvisorSpoke", "checkpoint": checkpoint.name(), "advice": advice,
            "tokens_read": tokens_read,
        }),
        Msg::CheckpointHit { checkpoint, detail } => json!({
            "msg": "CheckpointHit", "checkpoint": checkpoint.name(), "detail": detail,
        }),
        Msg::Compacted { run, pre_tokens } => {
            json!({"msg": "Compacted", "run": run.to_string(), "pre_tokens": pre_tokens})
        }
        Msg::HandoffWritten { path } => {
            json!({"msg": "HandoffWritten", "path": path.display().to_string()})
        }
        _ => return None,
    })
}

fn field<'a>(v: &'a Value, k: &str) -> Result<&'a Value, String> {
    v.get(k).ok_or_else(|| format!("missing {k}"))
}

fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str, String> {
    field(v, k)?
        .as_str()
        .ok_or_else(|| format!("{k}: not a string"))
}

fn run_id(v: &Value, k: &str) -> Result<RunId, String> {
    text(v, k)?.parse().map_err(|e| format!("{k}: {e}"))
}

fn call_id(v: &Value, k: &str) -> Result<ToolCallId, String> {
    text(v, k)?
        .parse()
        .map(ToolCallId)
        .map_err(|e| format!("{k}: {e}"))
}

fn from_value<T: serde::de::DeserializeOwned>(v: &Value, k: &str) -> Result<T, String> {
    serde_json::from_value(field(v, k)?.clone()).map_err(|e| format!("{k}: {e}"))
}

fn chunk_from_json(v: &Value) -> Result<StreamChunk, String> {
    Ok(match text(v, "k")? {
        "text" => StreamChunk::TextDelta(text(v, "v")?.into()),
        "thinking" => StreamChunk::ThinkingDelta(text(v, "v")?.into()),
        "tool_start" => StreamChunk::ToolUseStart {
            id: call_id(v, "id")?,
            name: text(v, "name")?.into(),
        },
        "tool_input" => StreamChunk::ToolUseInputDelta {
            id: call_id(v, "id")?,
            json_fragment: text(v, "v")?.into(),
        },
        "tool_end" => StreamChunk::ToolUseEnd {
            id: call_id(v, "id")?,
        },
        "usage" => StreamChunk::Usage(from_value::<Usage>(v, "v")?),
        "logprob" => StreamChunk::Logprob {
            token: text(v, "token")?.into(),
            logprob: field(v, "logprob")?.as_f64().ok_or("logprob")? as f32,
        },
        "done" => StreamChunk::Done(from_value::<StopReason>(v, "v")?),
        "waiting" => StreamChunk::Waiting(text(v, "v")?.into()),
        other => return Err(format!("unknown chunk kind {other}")),
    })
}

/// The inverse of `msg_to_json`; redacted fields come back as filler of the recorded length.
pub fn msg_from_json(v: &Value) -> Result<Msg, String> {
    let run = || run_id(v, "run");
    Ok(match text(v, "msg")? {
        "RunStarted" => Msg::RunStarted {
            run: run()?,
            agent: text(v, "agent")?
                .parse()
                .map_err(|e| format!("agent: {e}"))?,
            role: Role::parse(text(v, "role")?).ok_or("role")?,
            parent: match field(v, "parent")? {
                Value::Null => None,
                _ => Some(run_id(v, "parent")?),
            },
            model: crate::ids::ModelId(text(v, "model")?.into()),
            account: crate::ids::AccountName(text(v, "account")?.into()),
        },
        "Stream" => Msg::Stream {
            run: run()?,
            chunks: field(v, "chunks")?
                .as_array()
                .ok_or("chunks")?
                .iter()
                .map(chunk_from_json)
                .collect::<Result<_, _>>()?,
        },
        "ToolCallRequested" => Msg::ToolCallRequested {
            run: run()?,
            call: ToolCall {
                id: call_id(v, "call")?,
                name: text(v, "name")?.into(),
                input: field(v, "input")?.clone(),
                needs_permission: field(v, "needs_permission")?.as_bool().ok_or("flag")?,
            },
        },
        "ToolCallFinished" => Msg::ToolCallFinished {
            run: run()?,
            call_id: call_id(v, "call")?,
            result: ToolResult {
                content: "?".repeat(field(v, "content_len")?.as_u64().ok_or("len")? as usize),
                is_error: field(v, "is_error")?.as_bool().ok_or("flag")?,
                ..Default::default()
            },
            duration: Duration::from_millis(field(v, "ms")?.as_u64().ok_or("ms")?),
        },
        "RunFinished" => {
            let r = field(v, "result")?;
            let mut result: RunResult = from_value(v, "result")?;
            result.summary = "?".repeat(field(r, "summary_len")?.as_u64().ok_or("len")? as usize);
            Msg::RunFinished {
                run: run()?,
                result,
            }
        }
        "RunFailed" => Msg::RunFailed {
            run: run()?,
            error: from_value::<RunError>(v, "error")?,
        },
        "UsageUpdate" => Msg::UsageUpdate {
            run: run()?,
            usage: from_value(v, "usage")?,
        },
        "PermissionNeeded" => Msg::PermissionNeeded {
            request: PermissionRequest {
                id: permission_id(v)?,
                agent: text(v, "agent")?
                    .parse()
                    .map_err(|e| format!("agent: {e}"))?,
                run: run()?,
                call: call_id(v, "call")?,
                tool: text(v, "tool")?.into(),
                summary: text(v, "summary")?.into(),
            },
        },
        "PermissionDecided" => Msg::PermissionDecided {
            id: permission_id(v)?,
            decision: from_value::<Decision>(v, "decision")?,
            reason: from_value(v, "reason")?,
        },
        "HookFired" => Msg::HookFired {
            run: match field(v, "run")? {
                Value::Null => None,
                _ => Some(run()?),
            },
            event: from_value::<HookEvent>(v, "event")?,
            outcome: HookOutcome {
                exit_code: field(v, "exit_code")?.as_i64().ok_or("exit_code")? as i32,
                message: from_value(v, "message")?,
            },
        },
        "RouterAnswered" => Msg::RouterAnswered {
            fork: ForkId(field(v, "fork")?.as_u64().ok_or("fork")?),
            question: RouterQuestion {
                kind: text(v, "kind")?.into(),
                context: text(v, "context")?.into(),
                choices: from_value(v, "choices")?,
            },
            answer: RouterAnswer {
                choice: text(v, "choice")?.into(),
                p: field(v, "p")?.as_f64().ok_or("p")? as f32,
                verdict: from_value(v, "verdict")?,
                latency: Duration::from_millis(field(v, "latency_ms")?.as_u64().ok_or("ms")?),
                source: from_value(v, "source")?,
            },
        },
        "AdvisorSpoke" => Msg::AdvisorSpoke {
            checkpoint: checkpoint(v)?,
            advice: text(v, "advice")?.into(),
            tokens_read: field(v, "tokens_read")?.as_u64().ok_or("tokens_read")?,
        },
        "CheckpointHit" => Msg::CheckpointHit {
            checkpoint: checkpoint(v)?,
            detail: text(v, "detail")?.into(),
        },
        "Compacted" => Msg::Compacted {
            run: run()?,
            pre_tokens: field(v, "pre_tokens")?.as_u64().ok_or("pre_tokens")?,
        },
        "HandoffWritten" => Msg::HandoffWritten {
            path: text(v, "path")?.into(),
        },
        other => return Err(format!("cannot replay {other}")),
    })
}

fn permission_id(v: &Value) -> Result<PermissionId, String> {
    text(v, "id")?.parse().map_err(|e| format!("id: {e}"))
}

fn checkpoint(v: &Value) -> Result<Checkpoint, String> {
    Ok(match text(v, "checkpoint")? {
        "before_plan" => Checkpoint::BeforePlan,
        "same_error_x2" => Checkpoint::SameErrorX2,
        "before_done" => Checkpoint::BeforeDone,
        other => Checkpoint::Custom(other.into()),
    })
}

/// Steps every line of `msgs.jsonl` into `app`. The caller builds the app, so it picks the graph
/// backend and whether the dashboard's `view:` nodes are registered.
pub fn replay_into<G: Graph>(app: &mut App<G>, jsonl: &str) -> Result<(), String> {
    for (n, line) in jsonl.lines().enumerate().filter(|(_, l)| !l.is_empty()) {
        let v: Value = serde_json::from_str(line).map_err(|e| format!("line {}: {e}", n + 1))?;
        let msg = msg_from_json(&v).map_err(|e| format!("line {}: {e}", n + 1))?;
        app.step(msg).map_err(|e| format!("line {}: {e}", n + 1))?;
    }
    Ok(())
}

/// FNV-1a over the snapshot's JSON: it only has to be stable, not secret.
pub fn snapshot_hash<G: Graph>(graph: &G) -> String {
    let json = serde_json::to_vec(&graph.snapshot()).expect("a snapshot serialises");
    let hash = json.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("fnv1a64:{hash:016x}")
}

/// Replays `msgs.jsonl` through `reduce` on the mini engine and returns the snapshot hash.
pub fn replay_hash(jsonl: &str) -> Result<String, String> {
    let mut app = App::new(MiniGraph::new(), || 0);
    replay_into(&mut app, jsonl)?;
    Ok(snapshot_hash(&app.graph))
}
