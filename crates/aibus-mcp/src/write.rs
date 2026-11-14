//! `workflow_create` and `workflow_update` (#103, behind `--allow write`).
//!
//! A write from a remote author is checked in this order, and nothing touches the disk until the
//! last check before the rename has passed: the argument shape, the rate limit, the name, the
//! size, whether the text looks like a secret, `workflow::validate`, the step cap, the gate rule,
//! then the directory (canonicalised, so a symlink cannot lead out of it), then for an update
//! the revision. The file is written with the project's atomic write.

use std::fs;
use std::io;
use std::path::Path;
use std::time::Instant;

use aibus_core::persist::fs::{create_private_dir, write_atomic};
use aibus_core::persist::paths::project_dir;
use aibus_core::persist::time::rfc3339;
use aibus_core::workflow::{self, Workflow};
use rmcp::model::{CallToolResult, Tool, ToolAnnotations};
use rmcp::ErrorData;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::limits::WRITES_PER_MINUTE;
use crate::{arguments, schema, text, Server, DRAFT_2020_12};

/// Largest workflow file a remote author may write.
const MAX_BYTES: usize = 64 * 1024;

/// Most steps in a written workflow.
const MAX_STEPS: usize = 32;

/// The first line of a file written through the server.
const PROVENANCE: &str = "# written through aibus mcp, ";

/// SHA-256 of the file's bytes as lower-case hex: the revision a writer must name to overwrite.
pub(crate) fn revision(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn tools() -> Vec<Tool> {
    let scope = json!({ "type": "string", "enum": ["project", "user"] });
    let name = json!({ "type": "string", "pattern": "^[a-z0-9][a-z0-9_-]{0,63}$" });
    let toml = json!({ "type": "string", "maxLength": MAX_BYTES });
    let problem = json!({
        "type": "object",
        "properties": {
            "step": { "type": ["integer", "null"] },
            "field": { "type": ["string", "null"] },
            "message": { "type": "string" },
        },
        "required": ["step", "field", "message"],
        "additionalProperties": false,
    });
    let output = schema(json!({
        "$schema": DRAFT_2020_12,
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "scope": scope,
            "path": { "type": "string" },
            "revision": { "type": "string" },
            "warnings": { "type": "array", "items": problem },
        },
        "required": ["name", "scope", "path", "revision", "warnings"],
        "additionalProperties": false,
    }));
    let create = Tool::new(
        "workflow_create",
        "Write a new workflow file. The text is validated first; an existing name is an error (use workflow_update). A step with a gate (a shell command) needs the server to have been started with --allow gates.",
        schema(json!({
            "$schema": DRAFT_2020_12,
            "type": "object",
            "properties": { "name": name, "scope": scope, "toml": toml },
            "required": ["name", "scope", "toml"],
            "additionalProperties": false,
        })),
    )
    .with_raw_output_schema(output.clone())
    .with_annotations(ToolAnnotations::new().read_only(false).destructive(false).idempotent(true).open_world(false));
    let update = Tool::new(
        "workflow_update",
        "Replace a workflow file. expected_revision is the revision you read; if the file changed since, nothing is written and the error carries the current revision.",
        schema(json!({
            "$schema": DRAFT_2020_12,
            "type": "object",
            "properties": {
                "name": name,
                "scope": scope,
                "toml": toml,
                "expected_revision": { "type": "string", "pattern": "^[0-9a-f]{64}$" },
            },
            "required": ["name", "scope", "toml", "expected_revision"],
            "additionalProperties": false,
        })),
    )
    .with_raw_output_schema(output)
    .with_annotations(ToolAnnotations::new().read_only(false).destructive(true).idempotent(true).open_world(false));
    vec![create, update]
}

impl Server {
    /// One `workflow_create` (`update` false) or `workflow_update`. An `Err` is a call the schema
    /// forbids; everything the caller can fix is an `isError` result with a sentence.
    pub(crate) fn workflow_write(
        &self,
        update: bool,
        args: &Value,
        now: Instant,
    ) -> Result<CallToolResult, ErrorData> {
        let tool = if update {
            "workflow_update"
        } else {
            "workflow_create"
        };
        let required: &[&str] = if update {
            &["name", "scope", "toml", "expected_revision"]
        } else {
            &["name", "scope", "toml"]
        };
        let map = arguments(tool, args, required, &[])?;
        let (name, scope, body) = (
            text(tool, map, "name")?,
            text(tool, map, "scope")?,
            text(tool, map, "toml")?,
        );
        if !matches!(scope, "project" | "user") {
            return Err(ErrorData::invalid_params(
                format!("{tool}: scope is project or user"),
                None,
            ));
        }
        let expected = if update {
            Some(text(tool, map, "expected_revision")?)
        } else {
            None
        };

        // Everything up to the directory is decided from the arguments alone.
        if !workflow::valid_name(name) {
            return Ok(
                self.fail("the name must match ^[a-z0-9][a-z0-9_-]{0,63}$ (a name, not a path)")
            );
        }
        if body.len() > MAX_BYTES {
            return Ok(self.fail(format!(
                "the workflow is {} bytes, the most is {MAX_BYTES}",
                body.len()
            )));
        }
        if self.redactor.redact(body) != body {
            return Ok(self.fail("the workflow text contains something that looks like a credential; nothing was written"));
        }
        let roster = match self.roster() {
            Ok(r) => r,
            Err(e) => return Ok(self.fail(e)),
        };
        let found = workflow::validate(body, name, &roster);
        if !found.ok {
            let lines: Vec<String> = found
                .errors
                .iter()
                .map(|p| {
                    let step = p.step.map_or(String::new(), |n| format!("step {n}: "));
                    let field = p
                        .field
                        .as_deref()
                        .map_or(String::new(), |f| format!("{f}: "));
                    format!("{step}{field}{}", p.message)
                })
                .collect();
            return Ok(self.fail(format!(
                "the workflow does not validate, nothing was written:\n{}",
                lines.join("\n")
            )));
        }
        let parsed = Workflow::parse(name, body).expect("validate said ok, so parse does too");
        if parsed.steps.len() > MAX_STEPS {
            return Ok(self.fail(format!(
                "the workflow has {} steps, the most is {MAX_STEPS}",
                parsed.steps.len()
            )));
        }
        if !self.caps.gates {
            if let Some((i, step)) = parsed
                .steps
                .iter()
                .enumerate()
                .find(|(_, s)| s.gate.is_some())
            {
                return Ok(self.fail(format!(
                    "step {} has a gate, a shell command aibus runs as the user; start the server with --allow gates to write one ({})",
                    i + 1,
                    step.gate.as_deref().unwrap_or_default().trim()
                )));
            }
        }

        // A call the arguments alone refuse never reaches the disk, so it costs no token; the
        // per-call limit still counts it.
        let limited = self
            .write_limiter
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take(now);
        if let Err(secs) = limited {
            return Ok(self.fail(format!(
                "rate limit: at most {WRITES_PER_MINUTE} writes a minute, try again in {secs} s"
            )));
        }

        let _one_writer = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let target = match self.target(name, scope) {
            Ok(t) => t,
            Err(e) => return Ok(self.fail(e)),
        };
        let current = match fs::symlink_metadata(&target) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Ok(self.fail(format!("{name}: {e}"))),
            Ok(md) if md.file_type().is_symlink() || !md.is_file() => {
                return Ok(self.fail(format!(
                    "{name}.toml is not a plain file; refusing to write through it"
                )))
            }
            Ok(_) => match fs::read(&target) {
                Ok(bytes) => Some(bytes),
                Err(e) => return Ok(self.fail(format!("{name}: {e}"))),
            },
        };
        match (&current, expected) {
            (Some(_), None) => {
                return Ok(self.fail(format!(
                    "{name} exists in the {scope} scope, use workflow_update with its revision"
                )))
            }
            (None, Some(_)) => {
                return Ok(self.fail(format!("{name} does not exist in the {scope} scope, use workflow_create")))
            }
            (Some(old), Some(want)) if revision(old) != want => {
                return Ok(self.fail(format!(
                    "{name} changed since you read it: its current revision is {}, you named {want}; read it again and retry",
                    revision(old)
                )))
            }
            _ => {}
        }

        // The provenance line is added once: a copy in the submitted text is dropped, and an
        // update keeps the line the file already had.
        let submitted = body
            .strip_prefix(PROVENANCE)
            .map_or(body, |rest| rest.split_once('\n').map_or("", |(_, r)| r));
        let bytes = if self.caps.no_provenance {
            body.to_owned()
        } else {
            let kept = current
                .as_deref()
                .and_then(|old| std::str::from_utf8(old).ok())
                .and_then(|old| old.lines().next())
                .filter(|l| l.starts_with(PROVENANCE));
            let line = kept.map_or_else(
                || format!("{PROVENANCE}{}", rfc3339(std::time::SystemTime::now())),
                str::to_owned,
            );
            format!("{line}\n{submitted}")
        };
        if let Err(e) = write_atomic(&target, &bytes) {
            return Ok(self.fail(format!("{name}: could not write: {e}")));
        }
        let warnings: Vec<Value> = found
            .warnings
            .iter()
            .map(|p| json!({ "step": p.step, "field": p.field, "message": p.message }))
            .collect();
        Ok(self.done(json!({
            "name": name,
            "scope": scope,
            "path": self.shorten(&target.to_string_lossy().replace('\\', "/")),
            "revision": revision(bytes.as_bytes()),
            "warnings": warnings,
        })))
    }

    /// `<scope's workflows dir>/<name>.toml`, with the directory made if it is missing (the user
    /// scope always, the project scope only when `.aibus` is already there) and then resolved
    /// through symlinks: it has to land exactly where it should, or the write is refused.
    fn target(&self, name: &str, scope: &str) -> Result<std::path::PathBuf, String> {
        let user = scope == "user";
        let dir = workflow::dirs(&self.project, Some(&self.config))
            .into_iter()
            .nth(usize::from(user))
            .ok_or("no user directory for workflows")?;
        let base = if user {
            self.config.clone()
        } else {
            project_dir(&self.project)
        };
        let missing = |p: &Path| matches!(fs::symlink_metadata(p), Err(e) if e.kind() == io::ErrorKind::NotFound);
        if !user && !base.is_dir() {
            return Err("this project has no .aibus directory; set it up with aibus before writing workflows into it".into());
        }
        if user && missing(&base) {
            create_private_dir(&base)
                .map_err(|e| format!("could not make the user config directory: {e}"))?;
        }
        if missing(&dir) {
            create_private_dir(&dir)
                .map_err(|e| format!("could not make the workflows directory: {e}"))?;
        }
        let real_base = fs::canonicalize(&base).map_err(|e| e.to_string())?;
        let real_dir = fs::canonicalize(&dir).map_err(|e| e.to_string())?;
        if !user {
            let real_project = fs::canonicalize(&self.project).map_err(|e| e.to_string())?;
            if real_base != real_project.join(".aibus") {
                return Err(
                    ".aibus resolves outside the project; refusing to write through it".into(),
                );
            }
        }
        if real_dir != real_base.join("workflows") {
            return Err("the workflows directory resolves outside its scope (a symlink?); refusing to write through it".into());
        }
        Ok(real_dir.join(format!("{name}.toml")))
    }
}
