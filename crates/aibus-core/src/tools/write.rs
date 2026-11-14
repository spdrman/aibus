//! `write` and `edit`. Both replace a file atomically (temp file in the same directory, fsync,
//! rename) so a crash or a cancel never leaves half a file behind.

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::Path;

use aibus_provider::ToolSpec;
use serde::Deserialize;
use serde_json::{json, Value};
use ulid::Ulid;

use super::policy::resolve_for_write;
use super::readonly::{blocking, input};
use crate::agent::PermissionMode;
use crate::tool::{Need, Tool, ToolCx, ToolError, ToolOutput};

/// `Ask` until the mode says edits are fine.
fn edit_need(mode: PermissionMode) -> Need {
    match mode {
        PermissionMode::AcceptEdits | PermissionMode::Bypass => Need::Never,
        PermissionMode::Default | PermissionMode::DontAsk | PermissionMode::Plan => Need::Ask,
    }
}

/// Replaces `path` with `data`. A file that exists keeps its permissions; the parent
/// directories of a new file are created.
fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("path has no parent directory"))?;
    fs::create_dir_all(dir)?;
    let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let tmp = dir.join(format!(".{name}.aibus-{}", Ulid::new()));
    let result = (|| {
        let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        if let Ok(meta) = fs::metadata(path) {
            f.set_permissions(meta.permissions())?;
        }
        f.write_all(data)?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    } else if let Ok(d) = File::open(dir) {
        // Make the rename itself durable. Best effort: some filesystems refuse to sync a directory.
        let _ = d.sync_all();
    }
    result
}

fn confined(cx: &ToolCx, given: &str) -> Result<std::path::PathBuf, ToolError> {
    let (canon, inside) = resolve_for_write(&cx.cwd, given)?;
    if inside {
        Ok(canon)
    } else {
        Err(ToolError::Denied {
            reason: "path outside project".into(),
        })
    }
}

pub struct Write;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteInput {
    path: String,
    content: String,
}

#[async_trait::async_trait]
impl Tool for Write {
    fn name(&self) -> &'static str {
        "write"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write".into(),
            description: "Create a file or replace it with `content`. Parent directories are created.".into(),
            schema: json!({"type":"object","required":["path","content"],"additionalProperties":false,"properties":{
                "path":{"type":"string","minLength":1},
                "content":{"type":"string"}}}),
        }
    }

    fn needs_permission(&self, _: &Value, mode: PermissionMode) -> Need {
        edit_need(mode)
    }

    async fn call(&self, cx: &ToolCx, v: Value) -> Result<ToolOutput, ToolError> {
        let i: WriteInput = input(v)?;
        let path = confined(cx, &i.path)?;
        if cx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        blocking(move || {
            atomic_write(&path, i.content.as_bytes())?;
            Ok(ToolOutput {
                content: format!("wrote {} bytes to {}", i.content.len(), path.display()),
                ..Default::default()
            })
        })
        .await
    }
}

pub struct Edit;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditInput {
    path: String,
    old: String,
    new: String,
    #[serde(default)]
    all: bool,
}

#[async_trait::async_trait]
impl Tool for Edit {
    fn name(&self) -> &'static str {
        "edit"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit".into(),
            description: "Replace `old` with `new` in a file. `old` must match exactly once, \
                unless `all` is set, which replaces every match."
                .into(),
            schema: json!({"type":"object","required":["path","old","new"],"additionalProperties":false,"properties":{
                "path":{"type":"string","minLength":1},
                "old":{"type":"string","minLength":1},
                "new":{"type":"string"},
                "all":{"type":"boolean"}}}),
        }
    }

    fn needs_permission(&self, _: &Value, mode: PermissionMode) -> Need {
        edit_need(mode)
    }

    async fn call(&self, cx: &ToolCx, v: Value) -> Result<ToolOutput, ToolError> {
        let i: EditInput = input(v)?;
        if i.old.is_empty() {
            return Err(ToolError::Invalid("`old` must not be empty".into()));
        }
        let path = confined(cx, &i.path)?;
        if cx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        blocking(move || {
            let text = fs::read_to_string(&path)?;
            let found = text.matches(&i.old).count();
            match (found, i.all) {
                (0, _) => return Err(ToolError::Invalid("`old` was not found".into())),
                (n, false) if n > 1 => {
                    return Err(ToolError::Invalid(format!(
                        "`old` matches {n} times; make it longer or set `all`"
                    )))
                }
                _ => {}
            }
            let replaced = if i.all {
                text.replace(&i.old, &i.new)
            } else {
                text.replacen(&i.old, &i.new, 1)
            };
            atomic_write(&path, replaced.as_bytes())?;
            Ok(ToolOutput {
                content: format!("replaced {found} occurrence(s) in {}", path.display()),
                ..Default::default()
            })
        })
        .await
    }
}
