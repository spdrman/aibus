//! `skill` (EPIC H, H1.1): load the instructions of a skill the model picked from the index in
//! this description. Read-only; the index is bounded, and the tool loads any discovered skill
//! by name, listed or not.

use std::sync::Arc;

use aibus_provider::ToolSpec;
use serde::Deserialize;
use serde_json::{json, Value};

use super::readonly::input;
use crate::agent::PermissionMode;
use crate::skill::{index, SkillSet, INDEX_BUDGET_TOKENS};
use crate::tool::{Need, Tool, ToolCx, ToolError, ToolOutput};

pub struct SkillTool {
    set: Arc<SkillSet>,
}

impl SkillTool {
    pub fn new(set: Arc<SkillSet>) -> Self {
        Self { set }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillInput {
    name: String,
}

#[async_trait::async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &'static str {
        "skill"
    }

    fn spec(&self) -> ToolSpec {
        let mut description = String::from(
            "Load the instructions of a skill when the task matches its description. Skills:\n",
        );
        description.push_str(&index(&self.set, INDEX_BUDGET_TOKENS).0);
        ToolSpec {
            name: "skill".into(),
            description,
            schema: json!({"type":"object","required":["name"],"additionalProperties":false,"properties":{
                "name":{"type":"string","minLength":1}}}),
        }
    }

    fn needs_permission(&self, _: &Value, _: PermissionMode) -> Need {
        Need::Never
    }

    async fn call(&self, _: &ToolCx, v: Value) -> Result<ToolOutput, ToolError> {
        let i: SkillInput = input(v)?;
        let set = self.set.clone();
        let name = i.name.clone();
        let body = tokio::task::spawn_blocking(move || set.body(&name))
            .await
            .map_err(|e| ToolError::Io(std::io::Error::other(e)))?;
        match body {
            Some(content) => Ok(ToolOutput {
                content,
                ..ToolOutput::default()
            }),
            None => Err(ToolError::Invalid(format!("no skill named `{}`", i.name))),
        }
    }
}
