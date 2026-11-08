//! The tools that talk to the user or to the run itself: `report_artifact`, `ask_user`, `plan`,
//! `enter_plan_mode` and `exit_plan_mode`. None of them touches the world beyond a path check;
//! what needs the run loop (waiting for an answer, switching mode) comes back as a `Signal`.

use aibus_provider::ToolSpec;
use serde::Deserialize;
use serde_json::{json, Value};

use super::policy::confine;
use super::readonly::input;
use crate::agent::PermissionMode;
use crate::run::Progress;
use crate::tool::{Artifact, Need, Signal, Tool, ToolCx, ToolError, ToolOutput};

pub struct ReportArtifact;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportInput {
    path: String,
    kind: String,
    progress: Option<f32>,
}

#[async_trait::async_trait]
impl Tool for ReportArtifact {
    fn name(&self) -> &'static str {
        "report_artifact"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "report_artifact".into(),
            description: "Mark a file as an output of this run. `progress` (0 to 1) updates the \
                run's percentage."
                .into(),
            schema: json!({"type":"object","required":["path","kind"],"additionalProperties":false,"properties":{
                "path":{"type":"string","minLength":1},
                "kind":{"type":"string","minLength":1},
                "progress":{"type":"number","minimum":0,"maximum":1}}}),
        }
    }

    fn needs_permission(&self, _: &Value, _: PermissionMode) -> Need {
        Need::Never
    }

    async fn call(&self, cx: &ToolCx, v: Value) -> Result<ToolOutput, ToolError> {
        let i: ReportInput = input(v)?;
        if i.kind.trim().is_empty() {
            return Err(ToolError::Invalid("`kind` must not be empty".into()));
        }
        if let Some(p) = i.progress {
            if !(0.0..=1.0).contains(&p) {
                return Err(ToolError::Invalid("`progress` must be between 0 and 1".into()));
            }
        }
        let path = confine(&cx.cwd, &i.path)?;
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        Ok(ToolOutput {
            content: format!("recorded {} as {}", path.display(), i.kind),
            artifacts: vec![Artifact {
                name,
                kind: i.kind,
                path,
            }],
            progress: i.progress.map(Progress::Fraction),
            ..Default::default()
        })
    }
}

pub struct AskUser;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AskInput {
    question: String,
    #[serde(default)]
    choices: Vec<String>,
}

#[async_trait::async_trait]
impl Tool for AskUser {
    fn name(&self) -> &'static str {
        "ask_user"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ask_user".into(),
            description: "Ask the user a question and wait for the answer.".into(),
            schema: json!({"type":"object","required":["question"],"additionalProperties":false,"properties":{
                "question":{"type":"string","minLength":1},
                "choices":{"type":"array","items":{"type":"string"}}}}),
        }
    }

    fn needs_permission(&self, _: &Value, _: PermissionMode) -> Need {
        Need::Never
    }

    async fn call(&self, _: &ToolCx, v: Value) -> Result<ToolOutput, ToolError> {
        let i: AskInput = input(v)?;
        if i.question.trim().is_empty() {
            return Err(ToolError::Invalid("`question` must not be empty".into()));
        }
        Ok(ToolOutput {
            signal: Some(Signal::AskUser {
                question: i.question,
                choices: i.choices,
            }),
            ..Default::default()
        })
    }
}

pub struct Plan;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanInput {
    plan: String,
}

#[async_trait::async_trait]
impl Tool for Plan {
    fn name(&self) -> &'static str {
        "plan"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "plan".into(),
            description: "Write down the plan for the work. Allowed in plan mode.".into(),
            schema: json!({"type":"object","required":["plan"],"additionalProperties":false,"properties":{
                "plan":{"type":"string","minLength":1}}}),
        }
    }

    fn needs_permission(&self, _: &Value, _: PermissionMode) -> Need {
        Need::Never
    }

    async fn call(&self, _: &ToolCx, v: Value) -> Result<ToolOutput, ToolError> {
        let i: PlanInput = input(v)?;
        Ok(ToolOutput {
            content: format!("plan recorded:\n{}", i.plan),
            ..Default::default()
        })
    }
}

pub struct EnterPlanMode;

#[async_trait::async_trait]
impl Tool for EnterPlanMode {
    fn name(&self) -> &'static str {
        "enter_plan_mode"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "enter_plan_mode".into(),
            description: "Switch to plan mode: read-only until `exit_plan_mode`.".into(),
            schema: json!({"type":"object","additionalProperties":false,"properties":{}}),
        }
    }

    fn needs_permission(&self, _: &Value, _: PermissionMode) -> Need {
        Need::Never
    }

    async fn call(&self, _: &ToolCx, v: Value) -> Result<ToolOutput, ToolError> {
        input::<Empty>(v)?;
        Ok(ToolOutput {
            content: "plan mode on: reads only, no writes or commands".into(),
            signal: Some(Signal::EnterPlan),
            ..Default::default()
        })
    }
}

pub struct ExitPlanMode;

#[async_trait::async_trait]
impl Tool for ExitPlanMode {
    fn name(&self) -> &'static str {
        "exit_plan_mode"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "exit_plan_mode".into(),
            description: "Leave plan mode and go back to the permission mode the run started with.".into(),
            schema: json!({"type":"object","additionalProperties":false,"properties":{}}),
        }
    }

    fn needs_permission(&self, _: &Value, _: PermissionMode) -> Need {
        Need::Never
    }

    async fn call(&self, _: &ToolCx, v: Value) -> Result<ToolOutput, ToolError> {
        input::<Empty>(v)?;
        Ok(ToolOutput {
            content: "plan mode off".into(),
            signal: Some(Signal::ExitPlan),
            ..Default::default()
        })
    }
}

/// The input of a tool that takes nothing: `{}`, and nothing else.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
