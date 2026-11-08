//! `aibus agent validate` (01 section 1.4): what a definition promises against what exists.
//! `def::parse` has already checked the shape of the fields; this checks them against the tools,
//! accounts, models and hook commands the session really has.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::def::{parse, DefError};
use super::discover::{list_md, MAX_FILES_PER_SCOPE};
use super::{AgentDef, DefSource, ModelRef, Role, Scope, ToolPattern};
use crate::ids::{AccountName, AgentName};

/// What the definitions are checked against.
pub struct ValidateCx<'a> {
    /// Every account a definition may name, enabled or not.
    pub accounts: &'a [AccountName],
    /// Whether some account's provider can serve this model. Aliases and ids come here;
    /// `inherit` and the reserved `defaults.*` aliases never do.
    pub model_resolves: &'a dyn Fn(&ModelRef) -> bool,
    /// Hook commands with a `/` in them are relative to this directory.
    pub project_dir: &'a Path,
}

/// One problem, located by file and frontmatter field.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct Finding {
    pub path: Option<PathBuf>,
    pub field: String,
    pub message: String,
}

/// The `--json` shape of `agent validate`: `{ok, errors, warnings}`.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize)]
pub struct Report {
    pub ok: bool,
    pub errors: Vec<Finding>,
    pub warnings: Vec<Finding>,
}

impl Report {
    pub fn merge(&mut self, other: Report) {
        self.errors.extend(other.errors);
        self.warnings.extend(other.warnings);
        self.ok = self.errors.is_empty();
    }
}

/// The tools a definition may name (01 section 9), besides `mcp:*` patterns.
const KNOWN_TOOLS: [&str; 22] = [
    "read",
    "write",
    "edit",
    "glob",
    "grep",
    "ls",
    "bash",
    "report_artifact",
    "delegate",
    "fork",
    "resume_run",
    "send_message",
    "ledger_update",
    "create_handoff",
    "recall",
    "ask_user",
    "plan",
    "enter_plan_mode",
    "exit_plan_mode",
    "memory_read",
    "memory_write",
    "memory_append",
];

/// 01 section 6.1.
const HOOK_EVENTS: [&str; 11] = [
    "session_start",
    "user_prompt_submit",
    "pre_tool_use",
    "post_tool_use",
    "post_tool_use_failure",
    "subagent_start",
    "subagent_stop",
    "stop",
    "pre_compact",
    "checkpoint",
    "session_end",
];

/// The reserved aliases the built-ins use for the `defaults.*` config keys.
const RESERVED_MODELS: [&str; 4] = ["default_model", "advisor_model", "router_model", "swarm_model"];

/// Checks one parsed definition. Findings carry the definition's file path.
pub fn validate(def: &AgentDef, cx: &ValidateCx) -> Report {
    let mut r = Report::default();
    let path = def.source.path.clone();
    let mut error = |field: &str, message: String| {
        r.errors.push(Finding {
            path: path.clone(),
            field: field.into(),
            message,
        })
    };

    let mut tool_errors = Vec::new();
    let mut check_tools = |field: &'static str, list: &[ToolPattern]| {
        for p in list {
            if !KNOWN_TOOLS.contains(&p.tool.as_str()) && !p.tool.starts_with("mcp") {
                tool_errors.push((field, format!("unknown tool `{}`", p.tool)));
            }
        }
    };
    check_tools("tools", def.tools.allow.as_deref().unwrap_or_default());
    check_tools("disallowed_tools", &def.tools.deny);
    for (field, message) in tool_errors {
        error(field, message);
    }

    let reserved = matches!(&def.model, ModelRef::Alias(a) if RESERVED_MODELS.contains(&a.as_str()));
    if def.model != ModelRef::Inherit && !reserved && !(cx.model_resolves)(&def.model) {
        error(
            "model",
            format!(
                "model `{}` does not resolve on any account",
                super::def::model_str(&def.model)
            ),
        );
    }
    if let Some(a) = def.account.as_ref().filter(|a| !cx.accounts.contains(a)) {
        error("account", format!("account `{a}` does not exist"));
    }
    if !def.checkpoints.is_empty() && def.role != Role::Advisor {
        error(
            "checkpoints",
            "checkpoints are only for advisor agents".into(),
        );
    }
    if def.router.is_some() && def.role != Role::Router {
        error("router", "router settings are only for router agents".into());
    }

    let mut warnings = Vec::new();
    for (event, specs) in &def.hooks {
        if !HOOK_EVENTS.contains(&event.as_str()) {
            warnings.push(("hooks", format!("unknown hook event `{event}`")));
        }
        for h in specs {
            if let Some(problem) = hook_problem(&h.command, cx.project_dir) {
                error("hooks", format!("{event}: {problem}"));
            }
        }
    }
    r.warnings.extend(warnings.into_iter().map(|(field, message)| Finding {
        path: def.source.path.clone(),
        field: field.into(),
        message,
    }));
    r.ok = r.errors.is_empty();
    r
}

/// Validates a definition file, or every `*.md` in a directory (at most 1,000). Files that do not
/// parse are errors here, where discovery would skip them with a warning; two files in one
/// directory with one name are an error on the second.
pub fn validate_path(path: &Path, cx: &ValidateCx) -> Report {
    let mut r = Report::default();
    let files = if path.is_dir() {
        let (files, more) = list_md(path);
        if more {
            r.warnings.push(Finding {
                path: Some(path.to_owned()),
                field: "path".into(),
                message: format!("more than {MAX_FILES_PER_SCOPE} files, checked the first {MAX_FILES_PER_SCOPE}"),
            });
        }
        files
    } else {
        vec![path.to_owned()]
    };
    let mut seen: BTreeMap<AgentName, PathBuf> = BTreeMap::new();
    for file in files {
        let at = Some(file.clone());
        let finding = |field: &str, message: String| Finding {
            path: at.clone(),
            field: field.into(),
            message,
        };
        let text = match fs::read_to_string(&file) {
            Ok(t) => t,
            Err(e) => {
                r.errors.push(finding("path", format!("cannot read: {e}")));
                continue;
            }
        };
        let source = DefSource {
            path: at.clone(),
            scope: Scope::Project,
        };
        match parse(&text, source) {
            Err(e) => {
                let field = match &e {
                    DefError::Missing(f) => *f,
                    DefError::Field { field, .. } => field.as_str(),
                    DefError::Name(_) => "name",
                    _ => "frontmatter",
                };
                r.errors.push(finding(field, e.to_string()));
            }
            Ok((def, warnings)) => {
                for w in warnings {
                    // Parse warnings name their key in backticks.
                    let field = w.split('`').nth(1).unwrap_or("frontmatter");
                    r.warnings.push(finding(field, w.clone()));
                }
                if let Some(first) = seen.get(&def.name) {
                    r.errors.push(finding(
                        "name",
                        format!("`{}` is already defined in {}", def.name, first.display()),
                    ));
                } else {
                    seen.insert(def.name.clone(), file.clone());
                }
                r.merge(validate(&def, cx));
            }
        }
    }
    r.ok = r.errors.is_empty();
    r
}

/// Why the first word of a hook command cannot run, if it cannot.
fn hook_problem(command: &str, project_dir: &Path) -> Option<String> {
    let Some(program) = command.split_whitespace().next() else {
        return Some("the command is empty".into());
    };
    let found = if program.contains('/') {
        let p = project_dir.join(program);
        if !p.is_file() {
            return Some(format!("`{program}` does not exist"));
        }
        p
    } else {
        let path = std::env::var_os("PATH").unwrap_or_default();
        match std::env::split_paths(&path)
            .map(|d| d.join(program))
            .find(|p| p.is_file())
        {
            Some(p) => p,
            None => return Some(format!("`{program}` is not on PATH")),
        }
    };
    (!is_executable(&found)).then(|| format!("`{program}` is not executable"))
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_: &Path) -> bool {
    true
}
