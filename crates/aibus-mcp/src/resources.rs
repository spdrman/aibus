//! `aibus://` resources (#100): an allow-list of four things and nothing else. The workflow
//! files and the agent list the read tools already show, and the workflow schema compiled in.
//! No path, session, config, account or log is reachable from here.

use rmcp::model::{Resource, ResourceTemplate};
use rmcp::ErrorData;
use serde_json::Value;

use crate::Server;

const SCHEMA: &str = include_str!("../../../docs/schemas/workflow/v1.json");

/// The static resources, in a fixed order.
pub fn list() -> Vec<Resource> {
    vec![
        Resource::new("aibus://workflows", "workflows")
            .with_description(
                "The workflow_list result: every workflow of this project and of the user.",
            )
            .with_mime_type("application/json"),
        Resource::new("aibus://schema/workflow", "workflow-schema")
            .with_description("The JSON Schema (v1) of a workflow file.")
            .with_mime_type("application/schema+json"),
        Resource::new("aibus://agents", "agents")
            .with_description("The agent_list result: the agents a workflow step can name.")
            .with_mime_type("application/json"),
    ]
}

pub fn templates() -> Vec<ResourceTemplate> {
    vec![
        ResourceTemplate::new("aibus://workflows/{scope}/{name}", "workflow-file")
            .with_description("One workflow file as TOML. The scope is `project` or `user`.")
            .with_mime_type("application/toml"),
    ]
}

/// The run resources, listed only with `--allow run` (#105).
pub fn run_templates() -> Vec<ResourceTemplate> {
    vec![
        ResourceTemplate::new("aibus://runs/{run}", "run-status")
            .with_description("The run_status body of a run this server started, without the tail.")
            .with_mime_type("application/json"),
        ResourceTemplate::new("aibus://runs/{run}/log", "run-log")
            .with_description("The redacted output of a run this server started, wrapped as data and cut at the result cap.")
            .with_mime_type("text/plain"),
    ]
}

impl Server {
    /// The mime type and text of `uri`. Anything outside the allow-list, a name that is not a
    /// plain workflow name, and a workflow that does not exist are all `-32602`.
    pub fn read_resource_text(&self, uri: &str) -> Result<(&'static str, String), ErrorData> {
        let unknown = || ErrorData::invalid_params("unknown resource", None);
        let path = uri.strip_prefix("aibus://").ok_or_else(unknown)?;
        let json = |mut v: Value| {
            self.redactor.redact_json(&mut v);
            Ok(("application/json", v.to_string()))
        };
        match path.split('/').collect::<Vec<_>>().as_slice() {
            ["workflows"] => json(self.list_workflows()),
            ["agents"] => json(serde_json::json!({ "agents": self.agent_rows() })),
            ["schema", "workflow"] => Ok(("application/schema+json", SCHEMA.to_owned())),
            ["workflows", scope, name] => match self.workflow_file(name, Some(scope))? {
                Some((_, _, text)) => {
                    Ok(("application/toml", self.redactor.redact(&text).into_owned()))
                }
                None => Err(unknown()),
            },
            // Handles this process gave out and nothing else: an id from disk or from another
            // process is not in the table, so it is as unknown as any other string.
            ["runs", run, rest @ ..] if self.caps.run && matches!(rest, [] | ["log"]) => {
                let runs = self.runs.as_ref().ok_or_else(unknown)?;
                if !rest.is_empty() {
                    let max = aibus_core::config::Limits::default().result_max_bytes;
                    let text = runs
                        .log_text(run, &self.redactor, max)
                        .ok_or_else(unknown)?;
                    Ok(("text/plain", text))
                } else {
                    let mut body = runs.body(run, &self.redactor).ok_or_else(unknown)?;
                    body.as_object_mut().map(|o| o.remove("tail"));
                    json(body)
                }
            }
            _ => Err(unknown()),
        }
    }
}
