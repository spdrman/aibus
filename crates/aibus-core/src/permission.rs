//! The permission layer (09 section 3): which calls run, which ask, which are refused.
//!
//! `decide` is the mode table and nothing else. The tool says how much confirmation a call
//! wants in the current mode (`Tool::needs_permission`), the executor says whether the call
//! was pre-approved, and the answer is one of three verdicts.

use std::io::Write;
use std::path::Path;

use crate::agent::{PermissionMode, ToolPattern};
use crate::error::AibusError;
use crate::tool::Need;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    Allow,
    Ask,
    Deny,
}

/// One cell of the 09 section 3 table. `need` is what the tool reports for `mode`, so
/// `write` is `Ask` in `default` and `Never` in `accept_edits`. `pre_approved` is a match in
/// `[tools] allow`, `--allow` or a session-scoped "always"; it turns `Ask` into `Allow` but
/// never `Always`, which prompts every time.
pub fn decide(mode: PermissionMode, tool: &str, need: Need, pre_approved: bool) -> Verdict {
    match mode {
        PermissionMode::Bypass => Verdict::Allow,
        PermissionMode::Plan if need == Need::Never || tool == "plan" => Verdict::Allow,
        PermissionMode::Plan => Verdict::Deny,
        _ => match need {
            Need::Never => Verdict::Allow,
            Need::Ask if pre_approved => Verdict::Allow,
            _ if mode == PermissionMode::DontAsk => Verdict::Deny,
            _ => Verdict::Ask,
        },
    }
}

/// `bypass` needs `AIBUS_I_KNOW=1` as well as the flag, so a flag alone cannot turn it on
/// (06 section 2). `i_know` is the variable's value. The error is a usage error, exit code 2.
pub fn bypass_allowed(mode: PermissionMode, i_know: Option<&str>) -> Result<bool, AibusError> {
    match (mode, i_know) {
        (PermissionMode::Bypass, Some("1")) => Ok(true),
        (PermissionMode::Bypass, _) => Err(AibusError::Usage(
            "bypass also needs AIBUS_I_KNOW=1 in the environment".into(),
        )),
        _ => Ok(false),
    }
}

/// Adds `patterns` to `[tools] allow` in the project's `settings.toml`, keeping the other keys.
/// Called only after the user confirms "persist to .aibus/settings.toml"; a session-scoped
/// "always" never reaches the file by itself. Comments in the file are not kept.
pub fn persist_allow(settings: &Path, patterns: &[ToolPattern]) -> std::io::Result<()> {
    let mut doc: toml::Table = match std::fs::read_to_string(settings) {
        Ok(text) => text.parse().map_err(std::io::Error::other)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(e) => return Err(e),
    };
    let tools = doc
        .entry("tools")
        .or_insert_with(|| toml::Table::new().into())
        .as_table_mut()
        .ok_or_else(|| std::io::Error::other("[tools] is not a table"))?;
    let allow = tools
        .entry("allow")
        .or_insert_with(|| toml::Value::Array(vec![]))
        .as_array_mut()
        .ok_or_else(|| std::io::Error::other("tools.allow is not an array"))?;
    for p in patterns {
        let text = p.to_string();
        if !allow.iter().any(|v| v.as_str() == Some(&text)) {
            allow.push(text.into());
        }
    }
    let body = toml::to_string_pretty(&doc).map_err(std::io::Error::other)?;
    if let Some(dir) = settings.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = settings.with_extension("toml.tmp");
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(body.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, settings)
}
