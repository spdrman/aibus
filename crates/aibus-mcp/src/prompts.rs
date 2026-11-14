//! The three prompts (#100): text with `{{slot}}` holes from `prompts/*.md`, filled with the
//! agent roster, the shipped examples or a workflow file. Everything that came from disk is
//! wrapped as data (`Server::as_data`); the caller's own `goal` is escaped but is not data.

use aibus_core::run::result::escape_markers;
use aibus_core::workflow;
use rmcp::model::{
    ContentBlock, GetPromptResult, JsonObject, Prompt, PromptArgument, PromptMessage, Role,
};
use rmcp::ErrorData;
use serde_json::json;

use crate::Server;

pub fn list() -> Vec<Prompt> {
    let arg = |name: &str, what: &str, required: bool| {
        PromptArgument::new(name)
            .with_description(what)
            .with_required(required)
    };
    vec![
        Prompt::new(
            "author_workflow",
            Some("Brief for writing a new workflow: the format, the agents you can use, the shipped examples and the house rules."),
            Some(vec![
                arg("goal", "What the workflow should get done.", true),
                arg("scope", "`project` (the default) or `user`.", false),
            ]),
        ),
        Prompt::new(
            "review_workflow",
            Some("Review an existing workflow for steps nobody reads, gates that cannot block and misplaced on_fail."),
            Some(vec![
                arg("name", "The workflow's name.", true),
                arg("scope", "`project` or `user`; the project's wins when left out.", false),
            ]),
        ),
        Prompt::new(
            "explain_run",
            Some("Explain a run this server started."),
            Some(vec![arg("run", "The run's handle.", true)]),
        ),
    ]
}

/// `template` with each `{{slot}}` in `slots` replaced, in one pass: a `{{...}}` that is not a
/// slot (the workflow templates in the text) stays, and so does one inside a slot's value.
fn fill(template: &str, slots: &[(&str, String)]) -> String {
    let (mut out, mut rest) = (String::new(), template);
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let tail = &rest[open..];
        let hit = slots
            .iter()
            .find(|(k, _)| tail.starts_with(&format!("{{{{{k}}}}}")));
        match hit {
            Some((k, v)) => {
                out.push_str(v);
                rest = &tail[k.len() + 4..];
            }
            None => {
                out.push_str("{{");
                rest = &tail[2..];
            }
        }
    }
    out.push_str(rest);
    out
}

impl Server {
    pub fn get_prompt_text(
        &self,
        name: &str,
        args: &JsonObject,
    ) -> Result<GetPromptResult, ErrorData> {
        let bad = |why: &str| ErrorData::invalid_params(format!("{name}: {why}"), None);
        let allowed: &[&str] = match name {
            "author_workflow" => &["goal", "scope"],
            "review_workflow" => &["name", "scope"],
            "explain_run" => &["run"],
            _ => return Err(ErrorData::invalid_params("unknown prompt", None)),
        };
        if args.keys().any(|k| !allowed.contains(&k.as_str())) {
            return Err(bad(&format!("takes {}", allowed.join(", "))));
        }
        let get = |k: &str| -> Result<Option<&str>, ErrorData> {
            match args.get(k) {
                None => Ok(None),
                Some(v) => v
                    .as_str()
                    .map(Some)
                    .ok_or_else(|| bad(&format!("{k} must be a string"))),
            }
        };
        let need = |k: &str| {
            get(k)?
                .filter(|v| !v.trim().is_empty())
                .ok_or_else(|| bad(&format!("{k} is required")))
        };
        let scope = get("scope")?;
        let text = match name {
            "author_workflow" => {
                let target = match scope {
                    None | Some("project") => "project",
                    Some("user") => "user",
                    Some(_) => return Err(bad("scope is `project` or `user`")),
                };
                let roster: String = self
                    .agent_rows()
                    .iter()
                    .map(|a| {
                        let description = a["description"]
                            .as_str()
                            .unwrap_or_default()
                            .replace('\n', " ");
                        format!(
                            "- {} ({}): {}\n",
                            a["name"].as_str().unwrap_or_default(),
                            a["scope"].as_str().unwrap_or_default(),
                            description
                        )
                    })
                    .collect();
                let examples: Vec<String> = workflow::EXAMPLES
                    .iter()
                    .map(|(n, toml)| {
                        self.as_data(
                            &format!("the shipped example workflow `{n}`"),
                            toml,
                            Some("toml"),
                        )
                    })
                    .collect();
                fill(
                    include_str!("../prompts/author_workflow.md"),
                    &[
                        ("goal", escape_markers(&self.redactor.redact(need("goal")?))),
                        ("scope", target.to_owned()),
                        (
                            "agents",
                            self.as_data("the agent roster", &roster, Some("text")),
                        ),
                        ("examples", examples.join("\n\n")),
                    ],
                )
            }
            "review_workflow" => {
                let wanted = need("name")?;
                let Some((_, path, file)) = self.workflow_file(wanted, scope)? else {
                    return Err(bad("no such workflow"));
                };
                let roster: Vec<_> = self
                    .agent_rows()
                    .iter()
                    .filter_map(|r| r["name"].as_str()?.parse().ok())
                    .collect();
                let checked = workflow::validate(&file, wanted, &roster);
                let shown = self.shorten(&path.to_string_lossy().replace('\\', "/"));
                fill(
                    include_str!("../prompts/review_workflow.md"),
                    &[
                        ("name", wanted.to_owned()),
                        (
                            "file",
                            self.as_data(
                                &format!("the workflow file {shown}"),
                                &file,
                                Some("toml"),
                            ),
                        ),
                        (
                            "findings",
                            self.as_data(
                                "the result of checking that file",
                                &json!(checked).to_string(),
                                Some("json"),
                            ),
                        ),
                    ],
                )
            }
            _ => {
                let run = need("run")?;
                if !self.caps.run {
                    "Runs are not enabled on this server. It was started without `--allow run`, so there is no run to explain.".to_owned()
                } else if run.len() > 40
                    || !run.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                {
                    return Err(bad("run is a handle from this server"));
                } else {
                    fill(
                        include_str!("../prompts/explain_run.md"),
                        &[("run", run.to_owned())],
                    )
                }
            }
        };
        Ok(GetPromptResult::new(vec![PromptMessage::new(
            Role::User,
            ContentBlock::text(text),
        )]))
    }
}
