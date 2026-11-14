//! `workflow` (01 section 8.4): the lead runs a named chain. Every step is an ordinary foreground
//! run with the lead as its parent, started the way `delegate` starts one, so it counts against
//! the same limits and shows up as a card. The tool answers with the whole report, each step's
//! result wrapped as data, which is how the lead sees them and decides what to do next.

use std::path::PathBuf;
use std::sync::Arc;

use aibus_provider::ToolSpec;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::readonly::input;
use crate::agent::{Isolation, PermissionMode};
use crate::hooks::HookRunner;
use crate::ids::{AgentName, RunId};
use crate::msg::{Command, Msg};
use crate::orchestrator::{Orchestrator, Waited};
use crate::run::result;
use crate::tool::{Need, Tool, ToolCx, ToolError, ToolOutput};
use crate::workflow::{self, GateCall, GateVerdict, HostError, StepHost, StepOutcome};

pub struct RunWorkflow {
    orch: Arc<Orchestrator>,
    hooks: Arc<HookRunner>,
    /// Where workflow files are looked for, project first (`workflow::dirs`).
    dirs: Vec<PathBuf>,
}

impl RunWorkflow {
    pub fn new(orch: Arc<Orchestrator>, hooks: Arc<HookRunner>, dirs: Vec<PathBuf>) -> Self {
        Self { orch, hooks, dirs }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowInput {
    name: String,
    #[serde(default)]
    input: String,
}

/// The real host: steps are runs under `parent`, gates run through the hook runner.
struct Host<'a> {
    tool: &'a RunWorkflow,
    parent: RunId,
    tx: &'a tokio::sync::mpsc::Sender<Msg>,
    cancel: &'a CancellationToken,
}

#[async_trait::async_trait]
impl StepHost for Host<'_> {
    async fn run(&self, agent: &AgentName, prompt: String) -> Result<StepOutcome, HostError> {
        let orch = &self.tool.orch;
        let (run, wait) = orch
            .reserve(self.parent, false)
            .map_err(HostError::Failed)?;
        let Some(wait) = wait else {
            return Err(HostError::Failed("no foreground wait".into()));
        };
        let queued = Msg::Command(Command::Delegate {
            agent: agent.clone(),
            prompt,
            background: false,
            isolation: Isolation::None,
            parent: Some(self.parent),
            run: Some(run),
            model: None,
            account: None,
        });
        if self.tx.send(queued).await.is_err() {
            return Err(HostError::Cancelled);
        }
        let waited = tokio::select! {
            biased;
            () = self.cancel.cancelled() => {
                orch.cancel(run);
                return Err(HostError::Cancelled);
            }
            w = wait => w,
        };
        match waited {
            Ok(Waited::Done(r)) => Ok(StepOutcome {
                status: r.status,
                summary: r.summary.clone(),
                wrapped: result::wrap(&r),
                error: r.error.as_ref().map(|e| e.message.clone()),
            }),
            Ok(Waited::Detached) => Err(HostError::Stop(
                "the step was moved to the background; its report will arrive on its own".into(),
            )),
            Err(_) => Err(HostError::Failed("the orchestrator went away".into())),
        }
    }

    async fn gate(&self, command: &str, call: &GateCall<'_>) -> Result<GateVerdict, HostError> {
        let stdin = json!({
            "hook_event_name": "workflow_gate",
            "workflow": call.workflow,
            "step": call.step,
            "agent_type": call.agent,
            "input": call.input,
            "status": call.outcome.status.as_value(),
            "summary": call.outcome.summary,
        })
        .to_string();
        self.tool
            .hooks
            .gate(command, stdin, Some(self.parent), self.cancel)
            .await
    }
}

#[async_trait::async_trait]
impl Tool for RunWorkflow {
    fn name(&self) -> &'static str {
        "workflow"
    }

    fn spec(&self) -> ToolSpec {
        let mut description = String::from(
            "Run a workflow: a fixed chain of sub-agents, one after the other, each seeing what the \
             ones before it said. You get every step's report back and can decide what to do next. \
             Available:\n",
        );
        for found in workflow::list(&self.dirs) {
            if let Ok(wf) = found.runnable() {
                let agents: Vec<&str> = wf.steps.iter().map(|s| s.agent.as_str()).collect();
                description.push_str(&format!(
                    "- {}: {} ({})\n",
                    found.name,
                    wf.description,
                    agents.join(" > ")
                ));
            }
        }
        ToolSpec {
            name: "workflow".into(),
            description,
            schema: json!({"type":"object","required":["name","input"],"additionalProperties":false,"properties":{
                "name":{"type":"string","minLength":1},
                "input":{"type":"string","description":"what the first step is asked to work on"}}}),
        }
    }

    /// The steps are ordinary runs, each under its own agent's permissions.
    fn needs_permission(&self, _: &Value, _: PermissionMode) -> Need {
        Need::Never
    }

    async fn call(&self, cx: &ToolCx, v: Value) -> Result<ToolOutput, ToolError> {
        let i: WorkflowInput = input(v)?;
        let wf = workflow::load(&self.dirs, &i.name).map_err(ToolError::Invalid)?;
        let roster = self.orch.roster();
        if let Some(step) = wf
            .steps
            .iter()
            .find(|s| !roster.agents.iter().any(|(a, _)| *a == s.agent))
        {
            return Err(ToolError::Invalid(format!(
                "workflow {} names the agent {}, which is not defined (aibus agent list)",
                wf.name, step.agent
            )));
        }
        let host = Host {
            tool: self,
            parent: cx.run,
            tx: &cx.tx,
            cancel: &cx.cancel,
        };
        let report = workflow::execute(&wf, &i.input, &host).await;
        if report.ending == workflow::Ending::Cancelled && cx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        Ok(ToolOutput {
            content: report.text(&wf),
            ..Default::default()
        })
    }
}
