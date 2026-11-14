//! `fork`, `resume_run` and `send_message` (01 sections 5.2 and 5.4): the lead's tools for
//! runs it started, or runs that already ended. All three go through the orchestrator, like
//! `delegate` does.

use std::sync::Arc;

use aibus_provider::ToolSpec;
use serde::Deserialize;
use serde_json::{json, Value};

use super::delegate::queue_and_answer;
use super::readonly::input;
use crate::agent::PermissionMode;
use crate::msg::{Command, Msg};
use crate::orchestrator::Orchestrator;
use crate::tool::{Need, Tool, ToolCx, ToolError, ToolOutput};

fn json_out(v: Value) -> ToolOutput {
    ToolOutput {
        content: v.to_string(),
        ..Default::default()
    }
}

/// Starts a run that inherits this run's whole conversation. It works in the background and
/// its result comes back as a message, like a background `delegate`.
pub struct Fork {
    orch: Arc<Orchestrator>,
}

impl Fork {
    pub fn new(orch: Arc<Orchestrator>) -> Self {
        Self { orch }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ForkInput {
    prompt: String,
}

#[async_trait::async_trait]
impl Tool for Fork {
    fn name(&self) -> &'static str {
        "fork"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "fork".into(),
            description: "Start a copy of yourself that has read everything you have read so far \
                and does `prompt`. It works in the background; its calls stay out of your \
                conversation and only its result comes back, as a message. A fork cannot fork."
                .into(),
            schema: json!({"type":"object","required":["prompt"],"additionalProperties":false,"properties":{
                "prompt":{"type":"string","minLength":1}}}),
        }
    }

    fn needs_permission(&self, _: &Value, _: PermissionMode) -> Need {
        Need::Never
    }

    async fn call(&self, cx: &ToolCx, v: Value) -> Result<ToolOutput, ToolError> {
        let i: ForkInput = input(v)?;
        if i.prompt.trim().is_empty() {
            return Err(ToolError::Invalid("`prompt` must not be empty".into()));
        }
        let (run, wait) = match self.orch.reserve(cx.run, true) {
            Ok(reserved) => reserved,
            Err(error) => return Ok(json_out(json!({ "error": error }))),
        };
        let queued = Msg::Command(Command::Fork {
            prompt: i.prompt,
            parent: Some(cx.run),
            run: Some(run),
        });
        queue_and_answer(&self.orch, cx, queued, run, wait).await
    }
}

/// Continues a run that ended, by id or by agent name, with a new message.
pub struct ResumeRun {
    orch: Arc<Orchestrator>,
}

impl ResumeRun {
    pub fn new(orch: Arc<Orchestrator>) -> Self {
        Self { orch }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResumeInput {
    run: String,
    message: String,
    background: Option<bool>,
}

#[async_trait::async_trait]
impl Tool for ResumeRun {
    fn name(&self) -> &'static str {
        "resume_run"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "resume_run".into(),
            description: "Continue a run that has ended (often one that stopped at its turn or \
                token limit) with a new message. It keeps its conversation, definition, tools \
                and account. `run` is a run id, or an agent name for the latest run of that agent."
                .into(),
            schema: json!({"type":"object","required":["run","message"],"additionalProperties":false,"properties":{
                "run":{"type":"string"},
                "message":{"type":"string","minLength":1},
                "background":{"type":"boolean","default":true}}}),
        }
    }

    fn needs_permission(&self, _: &Value, _: PermissionMode) -> Need {
        Need::Never
    }

    async fn call(&self, cx: &ToolCx, v: Value) -> Result<ToolOutput, ToolError> {
        let i: ResumeInput = input(v)?;
        if i.message.trim().is_empty() {
            return Err(ToolError::Invalid("`message` must not be empty".into()));
        }
        let target = self.orch.resolve(&i.run).map_err(ToolError::Invalid)?;
        if self.orch.is_live(target) {
            return Err(ToolError::Invalid(format!(
                "run {target} is still going; use send_message to talk to it"
            )));
        }
        let background = i.background.unwrap_or(true);
        let (run, wait) = match self.orch.reserve_as(cx.run, background, Some(target)) {
            Ok(reserved) => reserved,
            Err(error) => return Ok(json_out(json!({ "error": error }))),
        };
        let queued = Msg::Command(Command::Resume {
            run,
            message: i.message,
            parent: Some(cx.run),
        });
        queue_and_answer(&self.orch, cx, queued, run, wait).await
    }
}

/// Queues a message for a run that is still going.
pub struct SendMessage {
    orch: Arc<Orchestrator>,
}

impl SendMessage {
    pub fn new(orch: Arc<Orchestrator>) -> Self {
        Self { orch }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendInput {
    run: String,
    text: String,
}

#[async_trait::async_trait]
impl Tool for SendMessage {
    fn name(&self) -> &'static str {
        "send_message"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "send_message".into(),
            description: "Send a message to a run that is still going. It reads it at the start \
                of its next turn. For a run that has ended, use resume_run."
                .into(),
            schema: json!({"type":"object","required":["run","text"],"additionalProperties":false,"properties":{
                "run":{"type":"string"},
                "text":{"type":"string","minLength":1}}}),
        }
    }

    fn needs_permission(&self, _: &Value, _: PermissionMode) -> Need {
        Need::Never
    }

    async fn call(&self, _: &ToolCx, v: Value) -> Result<ToolOutput, ToolError> {
        let i: SendInput = input(v)?;
        if i.text.trim().is_empty() {
            return Err(ToolError::Invalid("`text` must not be empty".into()));
        }
        let run = self.orch.resolve(&i.run).map_err(ToolError::Invalid)?;
        self.orch.message(run, i.text).map_err(ToolError::Invalid)?;
        Ok(json_out(
            json!({ "queued": true, "run_id": run.to_string() }),
        ))
    }
}
