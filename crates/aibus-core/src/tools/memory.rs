//! `memory_read`, `memory_write` and `memory_append` (01 section 8.3): plain Markdown files in
//! the agent's own memory directory, and nowhere else. A run gets them only when its definition
//! has `memory`; the directory comes from `ToolCx::memory`.

use std::fs;
use std::io;
use std::path::PathBuf;

use aibus_provider::ToolSpec;
use serde::Deserialize;
use serde_json::{json, Value};

use super::readonly::{blocking, input};
use crate::agent::PermissionMode;
use crate::continuity::memory::{confine, ensure_local_ignored, local_project};
use crate::persist::fs::{update_locked, write_atomic};
use crate::redact::Redactor;
use crate::tool::{Need, Tool, ToolCx, ToolError, ToolOutput};

/// The most one write or append may carry.
const MAX_TEXT: usize = 1 << 20;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathInput {
    path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextInput {
    path: String,
    text: String,
}

fn memory_dir(cx: &ToolCx) -> Result<PathBuf, ToolError> {
    cx.memory.clone().ok_or_else(|| ToolError::Denied {
        reason: "this agent has no memory directory".into(),
    })
}

fn invalid(e: String) -> ToolError {
    ToolError::Invalid(e)
}

fn schema(text: bool) -> Value {
    let mut props = json!({"path":{"type":"string","minLength":1,"description":"relative to your memory directory"}});
    let mut required = vec!["path"];
    if text {
        props["text"] = json!({"type":"string"});
        required.push("text");
    }
    json!({"type":"object","required":required,"additionalProperties":false,"properties":props})
}

pub struct MemoryRead;

#[async_trait::async_trait]
impl Tool for MemoryRead {
    fn name(&self) -> &'static str {
        "memory_read"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "memory_read".into(),
            description: "Read a file in your memory directory.".into(),
            schema: schema(false),
        }
    }

    fn needs_permission(&self, _: &Value, _: PermissionMode) -> Need {
        Need::Never
    }

    async fn call(&self, cx: &ToolCx, v: Value) -> Result<ToolOutput, ToolError> {
        let i: PathInput = input(v)?;
        let (dir, max) = (memory_dir(cx)?, cx.limits.result_max_bytes);
        blocking(move || {
            let path = confine(&dir, &i.path).map_err(invalid)?;
            let mut text = fs::read_to_string(&path).map_err(|e| match e.kind() {
                io::ErrorKind::NotFound => ToolError::Invalid(format!("{}: no such file", i.path)),
                _ => ToolError::Io(e),
            })?;
            let truncated = text.len() > max;
            if truncated {
                let mut end = max;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                text.truncate(end);
            }
            Ok(ToolOutput {
                content: text,
                truncated,
                ..Default::default()
            })
        })
        .await
    }
}

/// Prepares a write: the directory exists, the path stays inside it, its folder exists, and a
/// local-scope directory is gitignored.
fn target(dir: &std::path::Path, rel: &str) -> Result<PathBuf, ToolError> {
    fs::create_dir_all(dir)?;
    if let Some(project) = local_project(dir) {
        ensure_local_ignored(project)?;
    }
    let path = confine(dir, rel).map_err(invalid)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // The folder may be new, so the path is checked again now that it exists.
    confine(dir, rel).map_err(invalid)
}

fn too_long(text: &str) -> Result<(), ToolError> {
    if text.len() > MAX_TEXT {
        return Err(ToolError::Invalid(format!("`text` is over {MAX_TEXT} bytes")));
    }
    Ok(())
}

pub struct MemoryWrite {
    redactor: Redactor,
}

impl MemoryWrite {
    pub fn new(redactor: Redactor) -> Self {
        Self { redactor }
    }
}

#[async_trait::async_trait]
impl Tool for MemoryWrite {
    fn name(&self) -> &'static str {
        "memory_write"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "memory_write".into(),
            description: "Create or replace a Markdown file in your memory directory. `MEMORY.md` is \
                the index that is shown to you at the start of every run."
                .into(),
            schema: schema(true),
        }
    }

    fn needs_permission(&self, _: &Value, _: PermissionMode) -> Need {
        Need::Never
    }

    async fn call(&self, cx: &ToolCx, v: Value) -> Result<ToolOutput, ToolError> {
        let i: TextInput = input(v)?;
        too_long(&i.text)?;
        let (dir, text) = (memory_dir(cx)?, self.redactor.redact(&i.text).into_owned());
        blocking(move || {
            let path = target(&dir, &i.path)?;
            write_atomic(&path, &text)?;
            Ok(ToolOutput {
                content: format!("wrote {} bytes to {}", text.len(), i.path),
                ..Default::default()
            })
        })
        .await
    }
}

pub struct MemoryAppend {
    redactor: Redactor,
}

impl MemoryAppend {
    pub fn new(redactor: Redactor) -> Self {
        Self { redactor }
    }
}

#[async_trait::async_trait]
impl Tool for MemoryAppend {
    fn name(&self) -> &'static str {
        "memory_append"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "memory_append".into(),
            description: "Add text to the end of a Markdown file in your memory directory, creating it if needed."
                .into(),
            schema: schema(true),
        }
    }

    fn needs_permission(&self, _: &Value, _: PermissionMode) -> Need {
        Need::Never
    }

    async fn call(&self, cx: &ToolCx, v: Value) -> Result<ToolOutput, ToolError> {
        let i: TextInput = input(v)?;
        too_long(&i.text)?;
        let (dir, text) = (memory_dir(cx)?, self.redactor.redact(&i.text).into_owned());
        blocking(move || {
            let path = target(&dir, &i.path)?;
            update_locked(&path, |old| {
                let sep = if old.is_empty() || old.ends_with('\n') { "" } else { "\n" };
                Ok(format!("{old}{sep}{text}{}", if text.ends_with('\n') { "" } else { "\n" }))
            })?;
            Ok(ToolOutput {
                content: format!("appended to {}", i.path),
                ..Default::default()
            })
        })
        .await
    }
}
