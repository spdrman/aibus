//! `delegate` (01 section 4.1): hand a task to a sub-agent. The model picks the agent from the
//! roster in the description; aibus never routes by keyword (D-07).
//!
//! The call asks the orchestrator for a run, queues it with `Command::Delegate`, and then either
//! answers at once with the run id (background, the default) or waits for the child's result
//! (foreground).

use std::sync::Arc;

use aibus_provider::ToolSpec;
use serde::Deserialize;
use serde_json::{json, Value};

use super::readonly::input;
use crate::agent::{Isolation, PermissionMode};
use crate::ids::{AccountName, AgentName, RunId};
use crate::msg::{Command, Msg};
use crate::orchestrator::{Orchestrator, Waited};
use tokio::sync::oneshot;
use crate::run::result;
use crate::tool::{Need, Tool, ToolCx, ToolError, ToolOutput};

pub struct Delegate {
    orch: Arc<Orchestrator>,
}

impl Delegate {
    pub fn new(orch: Arc<Orchestrator>) -> Self {
        Self { orch }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DelegateInput {
    agent: AgentName,
    prompt: String,
    background: Option<bool>,
    #[serde(default)]
    isolation: Isolation,
    model: Option<String>,
    account: Option<String>,
}

#[async_trait::async_trait]
impl Tool for Delegate {
    fn name(&self) -> &'static str {
        "delegate"
    }

    fn spec(&self) -> ToolSpec {
        let roster = self.orch.roster();
        let mut description = String::from("Hand a task to a sub-agent. Roster:\n");
        description.push_str(&roster.text);
        if !roster.builtins.is_empty() {
            let names: Vec<&str> = roster.builtins.iter().map(AgentName::as_str).collect();
            description.push_str(&format!("Built in: {}\n", names.join(", ")));
        }
        let agents: Vec<&str> = roster.agents.iter().map(|(a, _)| a.as_str()).collect();
        ToolSpec {
            name: "delegate".into(),
            description,
            schema: json!({"type":"object","required":["agent","prompt"],"additionalProperties":false,"properties":{
                "agent":{"type":"string","enum":agents},
                "prompt":{"type":"string","minLength":1},
                "background":{"type":"boolean","default":true},
                "isolation":{"type":"string","enum":["none","worktree"]},
                "model":{"type":"string"},
                "account":{"type":"string"}}}),
        }
    }

    /// A worktree is a checkout the model can run commands in, so it asks like `bash` does
    /// (09 section 3): when the call asks for one, or the agent's definition does.
    fn needs_permission(&self, input: &Value, _: PermissionMode) -> Need {
        let asked = input.get("isolation").and_then(Value::as_str) == Some("worktree");
        let by_definition = input
            .get("agent")
            .and_then(Value::as_str)
            .is_some_and(|a| self.orch.roster().isolated.iter().any(|i| i.as_str() == a));
        if asked || by_definition {
            Need::Ask
        } else {
            Need::Never
        }
    }

    async fn call(&self, cx: &ToolCx, v: Value) -> Result<ToolOutput, ToolError> {
        let i: DelegateInput = input(v)?;
        if i.prompt.trim().is_empty() {
            return Err(ToolError::Invalid("`prompt` must not be empty".into()));
        }
        let roster = self.orch.roster();
        if !roster.agents.iter().any(|(a, _)| *a == i.agent) {
            return Err(ToolError::Invalid(format!("no agent named {}", i.agent)));
        }
        // A definition with `background: true` forces background (01 section 4.3).
        let background = i.background.unwrap_or(true) || roster.forces_background(&i.agent);
        let json_out = |v: Value| ToolOutput {
            content: v.to_string(),
            ..Default::default()
        };

        let (run, wait) = match self.orch.reserve(cx.run, background) {
            Ok(reserved) => reserved,
            Err(error) => return Ok(json_out(json!({ "error": error }))),
        };
        let queued = Msg::Command(Command::Delegate {
            agent: i.agent,
            prompt: i.prompt,
            background,
            isolation: i.isolation,
            parent: Some(cx.run),
            run: Some(run),
            model: i.model,
            account: i.account.map(|a| AccountName(a.into())),
        });
        queue_and_answer(&self.orch, cx, queued, run, wait).await
    }
}

/// What `delegate`, `fork` and `resume_run` do once they hold a run id: queue the command, then
/// answer at once with the id (background) or when the run's result arrives (foreground).
pub(super) async fn queue_and_answer(
    orch: &Orchestrator,
    cx: &ToolCx,
    queued: Msg,
    run: RunId,
    wait: Option<oneshot::Receiver<Waited>>,
) -> Result<ToolOutput, ToolError> {
    let json_out = |v: Value| ToolOutput {
        content: v.to_string(),
        ..Default::default()
    };
    if cx.tx.send(queued).await.is_err() {
        return Err(ToolError::Cancelled);
    }
    let Some(wait) = wait else {
        return Ok(json_out(json!({ "run_id": run.to_string() })));
    };
    let waited = tokio::select! {
        biased;
        () = cx.cancel.cancelled() => {
            orch.cancel(run);
            return Err(ToolError::Cancelled);
        }
        w = wait => w,
    };
    Ok(match waited {
        Ok(Waited::Done(r)) => ToolOutput {
            content: result::wrap(&r),
            ..Default::default()
        },
        Ok(Waited::Detached) => json_out(json!({ "run_id": run.to_string(), "background": true })),
        Err(_) => json_out(json!({ "error": "the orchestrator went away" })),
    })
}
