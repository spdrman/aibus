//! Tool registry, effective allow-list and the executor that enforces it (01 section 9, 09
//! section 2, D-05). The allow-list is checked here, before dispatch, so a tool never sees a
//! call the run was not granted.

mod policy;
mod readonly;

use std::collections::BTreeMap;
use std::sync::Arc;

use aibus_provider::ToolSpec;
use serde_json::Value;

use crate::agent::{AgentDef, Role, ToolPattern};
use crate::tool::{Tool, ToolCx, ToolError, ToolOutput};

pub use readonly::{Glob, Grep, Ls, Read};

/// Spawn depth past which `delegate` is no longer offered (`limits.max_spawn_depth`, 01 section 4).
const MAX_SPAWN_DEPTH: u8 = 3;

/// The tools a run may call: allow patterns with the denies that still need a runtime check.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct EffectiveTools {
    pub allow: Vec<ToolPattern>,
    /// Denies that carry a specifier. A bare deny has already removed its tool from `allow`.
    pub deny: Vec<ToolPattern>,
}

/// Works out the tool list for one run: the role's tool set, narrowed by the definition's
/// `allow`, with every deny applied before it (deny wins). Background runs lose the interactive
/// tools, and `delegate` disappears at the depth limit.
pub fn effective_tools(
    def: &AgentDef,
    role: Role,
    depth: u8,
    background: bool,
) -> EffectiveTools {
    const COMMON: [&str; 8] = [
        "read",
        "write",
        "edit",
        "glob",
        "grep",
        "ls",
        "bash",
        "report_artifact",
    ];
    const LEAD_ONLY: [&str; 10] = [
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
    ];
    const INTERACTIVE: [&str; 3] = ["ask_user", "enter_plan_mode", "exit_plan_mode"];

    let mut names: Vec<&str> = match role {
        Role::Lead | Role::Worker => COMMON.to_vec(),
        Role::Advisor => vec!["read", "glob", "grep", "ls"],
        Role::Router => vec![],
    };
    if matches!(role, Role::Lead | Role::Worker) {
        if depth < MAX_SPAWN_DEPTH {
            names.push("delegate");
        }
        if def.memory.is_some() {
            names.extend(["memory_read", "memory_write", "memory_append"]);
        }
    }
    if role == Role::Lead {
        names.extend(LEAD_ONLY);
    }
    if background {
        names.retain(|n| !INTERACTIVE.contains(n));
    }

    let mut allow: Vec<ToolPattern> = match &def.tools.allow {
        None => names
            .iter()
            .map(|n| ToolPattern {
                tool: (*n).into(),
                spec: None,
            })
            .collect(),
        Some(list) => list
            .iter()
            .filter(|p| names.contains(&p.tool.as_str()))
            .cloned()
            .collect(),
    };
    allow.retain(|p| !def.tools.deny.iter().any(|d| d.spec.is_none() && d.tool == p.tool));
    let deny = def
        .tools
        .deny
        .iter()
        .filter(|d| d.spec.is_some())
        .cloned()
        .collect();
    EffectiveTools { allow, deny }
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: BTreeMap<&'static str, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// A registry holding `read`, `glob`, `ls` and `grep`.
    pub fn read_only() -> Self {
        let mut r = Self::default();
        r.register(Arc::new(Read));
        r.register(Arc::new(Glob));
        r.register(Arc::new(Ls));
        r.register(Arc::new(Grep));
        r
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name(), tool);
    }

    /// Schemas for the tools the run may call. Names outside `eff` are never shown.
    pub fn specs(&self, eff: &EffectiveTools) -> Vec<ToolSpec> {
        self.tools
            .iter()
            .filter(|(name, _)| eff.allow.iter().any(|p| p.tool == **name))
            .map(|(_, t)| t.spec())
            .collect()
    }
}

pub struct ToolExecutor {
    registry: ToolRegistry,
}

impl ToolExecutor {
    pub fn new(registry: ToolRegistry) -> Self {
        Self { registry }
    }

    pub fn specs(&self, eff: &EffectiveTools) -> Vec<ToolSpec> {
        self.registry.specs(eff)
    }

    /// Checks the call against `eff`, then dispatches it. Unknown names and specifier
    /// mismatches come back as `Denied { reason: "not in allow-list" }` without the tool being
    /// touched. Cancelling `cx.cancel` ends the call with `Cancelled`; output beyond
    /// `bash_max_output_bytes` is cut to its tail behind a head marker.
    pub async fn call(
        &self,
        eff: &EffectiveTools,
        cx: &ToolCx,
        name: &str,
        mut input: Value,
    ) -> Result<ToolOutput, ToolError> {
        let denied = |reason: &str| ToolError::Denied {
            reason: reason.into(),
        };
        let allows: Vec<&ToolPattern> = eff.allow.iter().filter(|p| p.tool == name).collect();
        if allows.is_empty() {
            return Err(denied("not in allow-list"));
        }

        // Subjects are the strings specifiers are matched against. Path tools are resolved
        // here so a symlink is judged by where it lands, and the canonical path replaces the
        // model's string before dispatch.
        let mut tool_cx = None;
        let mut outside = false;
        let subjects: Vec<String> = match name {
            "bash" => {
                let cmd = input.get("command").and_then(Value::as_str);
                let cmd = cmd.ok_or_else(|| ToolError::Invalid("missing command".into()))?;
                vec![policy::normalise_command(cmd)]
            }
            "glob" => {
                let pat = input.get("pattern").and_then(Value::as_str);
                vec![pat
                    .ok_or_else(|| ToolError::Invalid("missing pattern".into()))?
                    .to_string()]
            }
            "read" | "ls" | "grep" => {
                let given = match input.get("path").and_then(Value::as_str) {
                    Some(p) => p,
                    None if name == "read" => {
                        return Err(ToolError::Invalid("missing path".into()))
                    }
                    None => ".",
                };
                let (canon, inside) = policy::resolve(&cx.cwd, given)?;
                let mut subjects = vec![canon.to_string_lossy().into_owned()];
                if inside {
                    let root = cx.cwd.canonicalize()?;
                    if let Ok(rel) = canon.strip_prefix(&root) {
                        subjects.push(rel.to_string_lossy().into_owned());
                    }
                } else {
                    outside = true;
                    // The tool confines itself to its cwd, so an admitted outside path
                    // becomes the cwd of this one call.
                    let base = if canon.is_dir() {
                        canon.clone()
                    } else {
                        canon.parent().map(Into::into).unwrap_or_else(|| canon.clone())
                    };
                    tool_cx = Some(ToolCx {
                        run: cx.run,
                        cwd: base,
                        cancel: cx.cancel.clone(),
                        limits: cx.limits,
                        tx: cx.tx.clone(),
                    });
                }
                input["path"] = Value::String(subjects[0].clone());
                subjects
            }
            _ => vec![],
        };

        let hit = |p: &ToolPattern| match &p.spec {
            None => true,
            Some(spec) => subjects.iter().any(|s| policy::spec_matches(spec, s)),
        };
        if eff.deny.iter().any(|d| d.tool == name && hit(d)) {
            return Err(denied("denied by tool policy"));
        }
        // A bare allow covers the project only; reaching outside takes a matching specifier.
        let admitted = allows
            .iter()
            .any(|p| if p.spec.is_none() { !outside } else { hit(p) });
        if !admitted {
            return Err(denied("not in allow-list"));
        }

        let tool = self
            .registry
            .tools
            .get(name)
            .ok_or_else(|| ToolError::Invalid(format!("unknown tool {name}")))?;
        let tool_cx = tool_cx.as_ref().unwrap_or(cx);
        let out = tokio::select! {
            biased;
            () = cx.cancel.cancelled() => return Err(ToolError::Cancelled),
            r = tool.call(tool_cx, input) => r?,
        };
        Ok(cap_output(out, cx.limits.bash_max_output_bytes))
    }
}

/// Keeps the last `max` bytes (on a char boundary) behind a marker saying how much was cut.
fn cap_output(mut out: ToolOutput, max: usize) -> ToolOutput {
    if out.content.len() > max {
        let mut start = out.content.len() - max;
        while !out.content.is_char_boundary(start) {
            start += 1;
        }
        out.content = format!(
            "[... {start} bytes truncated ...]\n{}",
            &out.content[start..]
        );
        out.truncated = true;
    }
    out
}
