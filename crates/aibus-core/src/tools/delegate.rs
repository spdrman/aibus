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
use crate::ids::{AccountName, AgentName};
use crate::msg::{Command, Msg};
use crate::orchestrator::{Orchestrator, Waited};
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

    fn needs_permission(&self, _: &Value, _: PermissionMode) -> Need {
        Need::Never
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
        if cx.tx.send(queued).await.is_err() {
            return Err(ToolError::Cancelled);
        }
        let Some(wait) = wait else {
            return Ok(json_out(json!({ "run_id": run.to_string() })));
        };
        let waited = tokio::select! {
            biased;
            () = cx.cancel.cancelled() => {
                self.orch.cancel(run);
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
}
