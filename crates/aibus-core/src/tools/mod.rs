//! Tool registry, effective allow-list and the executor that enforces it (01 section 9, 09
//! section 2, D-05). The allow-list is checked here, before dispatch, so a tool never sees a
//! call the run was not granted.

mod bash;
mod continuity;
mod delegate;
mod lifecycle;
mod memory;
mod policy;
mod readonly;
mod sandbox;
mod shell;
mod user;
mod web;
mod workflow;
mod write;

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use aibus_provider::ToolSpec;
use serde_json::Value;

use crate::agent::{AgentDef, PermissionMode, Role, ToolPattern};
use crate::msg::Decision;
use crate::permission::{self, Verdict};
use crate::redact::Redactor;
use crate::tool::{Tool, ToolCx, ToolError, ToolOutput};

pub(crate) use bash::{kill_group, scrubbed_env, Tail};
pub use bash::{Bash, JobView};
pub use continuity::{CreateHandoff, LedgerUpdate, Recall};
pub use delegate::Delegate;
pub use lifecycle::{Fork, ResumeRun, SendMessage};
pub use memory::{MemoryAppend, MemoryRead, MemoryWrite};
pub use readonly::{Glob, Grep, Ls, Read};
pub use policy::{normalise_command, spec_matches};
pub use sandbox::Sandbox;
pub use shell::split;
pub use shell::ShellDeny;
pub use user::{AskUser, EnterPlanMode, ExitPlanMode, Plan, ReportArtifact};
pub use web::{Fetched, WebFetch, WebFetcher};
pub use workflow::RunWorkflow;
pub use write::{Edit, Write};

/// The tools a run may call: allow patterns with the denies that still need a runtime check.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct EffectiveTools {
    pub allow: Vec<ToolPattern>,
    /// Denies that carry a specifier. A bare deny has already removed its tool from `allow`.
    pub deny: Vec<ToolPattern>,
}

/// Works out the tool list for one run: the role's tool set, narrowed by the definition's
/// `allow`, with every deny applied before it (deny wins). Background runs lose the interactive
/// tools, and `delegate` disappears at `max_spawn_depth` (01 section 4.4).
pub fn effective_tools(
    def: &AgentDef,
    role: Role,
    depth: u8,
    background: bool,
    max_spawn_depth: u8,
) -> EffectiveTools {
    const COMMON: [&str; 9] = [
        "read",
        "write",
        "edit",
        "glob",
        "grep",
        "ls",
        "bash",
        "report_artifact",
        "web_fetch",
    ];
    const LEAD_ONLY: [&str; 11] = [
        "fork",
        "workflow",
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

    let mcp = matches!(role, Role::Lead | Role::Worker);
    let mut names: Vec<&str> = match role {
        Role::Lead | Role::Worker => COMMON.to_vec(),
        Role::Advisor => vec!["read", "glob", "grep", "ls"],
        Role::Router => vec![],
    };
    if matches!(role, Role::Lead | Role::Worker) {
        if depth < max_spawn_depth {
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
            .filter(|p| names.contains(&p.tool.as_str()) || (mcp && p.tool.starts_with("mcp:")))
            .cloned()
            .collect(),
    };
    // MCP tools are the server's, not part of any role's set: an agent that lists no tools
    // gets whatever servers are configured, and a list reaches them with `mcp:*` or a name.
    if mcp && def.tools.allow.is_none() {
        allow.push(ToolPattern {
            tool: "mcp:*".into(),
            spec: None,
        });
    }
    allow.retain(|p| !def.tools.deny.iter().any(|d| d.spec.is_none() && d.tool == p.tool));
    // A bare deny of an MCP tool stays, since `mcp:*` in the allow-list still covers the name.
    let deny = def
        .tools
        .deny
        .iter()
        .filter(|d| d.spec.is_some() || d.tool.starts_with("mcp:"))
        .cloned()
        .collect();
    EffectiveTools { allow, deny }
}

/// Whether an allow-list entry names the tool `name`. MCP entries are globs over
/// `mcp:server:tool` (`mcp:*`); every other entry is the tool's own name.
fn names_tool(entry: &str, name: &str) -> bool {
    entry == name
        || (entry.starts_with("mcp:")
            && globset::Glob::new(entry)
                .is_ok_and(|g| g.compile_matcher().is_match(&*crate::mcp::allow_name(name))))
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
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

    /// The read-only tools plus `write`, `edit`, `bash`, `report_artifact`, `ask_user`, `plan`,
    /// `enter_plan_mode` and `exit_plan_mode`. The caller keeps its own `Arc<Bash>` to look at
    /// background jobs.
    pub fn standard(bash: Arc<Bash>) -> Self {
        let mut r = Self::read_only();
        r.register(Arc::new(Write));
        r.register(Arc::new(Edit));
        r.register(bash);
        r.register(Arc::new(ReportArtifact));
        r.register(Arc::new(AskUser));
        r.register(Arc::new(Plan));
        r.register(Arc::new(EnterPlanMode));
        r.register(Arc::new(ExitPlanMode));
        r
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_owned(), tool);
    }

    /// Schemas for the tools the run may call. Names outside `eff` are never shown.
    pub fn specs(&self, eff: &EffectiveTools) -> Vec<ToolSpec> {
        self.tools
            .iter()
            .filter(|(name, _)| eff.allow.iter().any(|p| names_tool(&p.tool, name)))
            .map(|(_, t)| t.spec())
            .collect()
    }
}

/// What the confirmation step needs besides the call itself.
#[derive(Clone, Default)]
pub struct Gate {
    /// `[tools] allow` and `--allow`: calls that match skip the prompt.
    pub allow: Vec<ToolPattern>,
    /// `[tools] deny_patterns`, on top of `tools/shell_deny.txt`.
    pub deny_patterns: Vec<String>,
    /// Somebody can answer a prompt. Without one an `ask` is refused as "no tty".
    pub tty: bool,
    /// Scrubs the summary a prompt shows.
    pub redactor: Option<Redactor>,
    /// The user's home, for the `~/.config/aibus/**` entry of the write deny-list.
    pub home: Option<PathBuf>,
}

pub struct ToolExecutor {
    registry: ToolRegistry,
    gate: Gate,
    deny: ShellDeny,
    /// Patterns added by "always for this pattern" in this session.
    session: Mutex<Vec<ToolPattern>>,
}

pub(super) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What `ToolExecutor::admit` worked out about one call.
struct Admitted {
    input: Value,
    /// The strings specifiers are matched against: the parts of a command, or a path in its
    /// absolute and project-relative forms.
    subjects: Vec<String>,
    /// The call reaches outside the project.
    outside: bool,
    /// A context rooted where an outside path lives, for the tool to confine itself to.
    cx: Option<ToolCx>,
}

/// Whether `pats` cover a call. A bare pattern covers a call inside the project; a specifier
/// covers what it matches. Every part of a `bash` command has to be covered by some pattern,
/// for the others one subject is enough.
fn covers(pats: &[&ToolPattern], name: &str, subjects: &[String], outside: bool) -> bool {
    let of_tool = || pats.iter().filter(|p| names_tool(&p.tool, name));
    let matches = |p: &ToolPattern, s: &str| {
        p.spec.as_deref().is_none_or(|spec| policy::spec_matches(spec, s))
    };
    if name == "bash" {
        !subjects.is_empty() && subjects.iter().all(|s| of_tool().any(|p| matches(p, s)))
    } else {
        of_tool().any(|p| match &p.spec {
            None => !outside,
            Some(_) => subjects.iter().any(|s| matches(p, s)),
        })
    }
}

fn denied(reason: &str) -> ToolError {
    ToolError::Denied {
        reason: reason.into(),
    }
}

impl ToolExecutor {
    /// An executor with no pre-approved patterns, no terminal and only the built-in shell
    /// deny-list.
    pub fn new(registry: ToolRegistry) -> Self {
        Self::with_gate(registry, Gate::default()).expect("the built-in deny list is valid")
    }

    /// `Err` names a `deny_patterns` entry that is not a valid glob.
    pub fn with_gate(registry: ToolRegistry, gate: Gate) -> Result<Self, String> {
        Ok(Self {
            deny: ShellDeny::new(&gate.deny_patterns)?,
            registry,
            gate,
            session: Mutex::new(Vec::new()),
        })
    }

    pub fn specs(&self, eff: &EffectiveTools) -> Vec<ToolSpec> {
        self.registry.specs(eff)
    }

    /// Whether a prompt can be answered at all (headless runs cannot).
    pub fn tty(&self) -> bool {
        self.gate.tty
    }

    /// The patterns "always for this pattern" added so far. Persisting them is a separate step
    /// the user confirms (`permission::persist_allow`).
    pub fn session_allow(&self) -> Vec<ToolPattern> {
        lock(&self.session).clone()
    }

    /// Checks the call against `eff`, the shell deny-list and the write deny-list, then the
    /// permission layer, then dispatches it. Unknown names and specifier mismatches come back
    /// as `Denied { reason: "not in allow-list" }` without the tool being touched. When the
    /// mode says `Ask`, `ask` gets the (redacted) summary line and returns the user's answer;
    /// `AllowSession` also adds a session-scoped allow entry. Cancelling `cx.cancel` ends the
    /// call with `Cancelled`; output beyond `bash_max_output_bytes` is cut to its tail behind a
    /// head marker.
    pub async fn call_gated<F, Fut>(
        &self,
        eff: &EffectiveTools,
        cx: &ToolCx,
        mode: PermissionMode,
        name: &str,
        input: Value,
        ask: F,
    ) -> Result<ToolOutput, ToolError>
    where
        F: FnOnce(String) -> Fut,
        Fut: Future<Output = Decision>,
    {
        let adm = self.admit(eff, cx, name, input)?;
        let tool = self
            .registry
            .tools
            .get(name)
            .ok_or_else(|| ToolError::Invalid(format!("unknown tool {name}")))?;

        let need = tool.needs_permission(&adm.input, mode);
        let pre = {
            let session = lock(&self.session);
            let pats: Vec<&ToolPattern> = self.gate.allow.iter().chain(session.iter()).collect();
            covers(&pats, name, &adm.subjects, adm.outside)
        };
        match permission::decide(mode, name, need, pre) {
            Verdict::Allow => {}
            Verdict::Deny => {
                return Err(denied(&format!("{} mode does not allow {name}", mode.as_str())))
            }
            Verdict::Ask => {
                // The one line a prompt shows: scrubbed, and cut to 200 characters.
                let main = match name {
                    "bash" => adm.input.get("command"),
                    "write" | "edit" => adm.input.get("path"),
                    _ => None,
                };
                let mut summary = match main.and_then(Value::as_str) {
                    Some(s) => format!("{name}: {s}"),
                    None => format!("{name}: {}", adm.input),
                };
                if let Some(r) = &self.gate.redactor {
                    summary = r.redact(&summary).into_owned();
                }
                if summary.chars().count() > 200 {
                    summary = summary.chars().take(200).collect::<String>() + "...";
                }
                let answer = tokio::select! {
                    biased;
                    () = cx.cancel.cancelled() => return Err(ToolError::Cancelled),
                    d = ask(summary) => d,
                };
                match answer {
                    Decision::Deny => return Err(denied("permission denied")),
                    Decision::Allow => {}
                    Decision::AllowSession => {
                        // The first word of every part of a command (`bash(cargo *)`), the
                        // exact path for `write` and `edit`, the bare tool otherwise.
                        let pattern = |spec: Option<String>| ToolPattern {
                            tool: name.into(),
                            spec: spec.map(Into::into),
                        };
                        let added: Vec<ToolPattern> = match name {
                            "bash" => {
                                let mut words: Vec<&str> =
                                    adm.subjects.iter().filter_map(|s| s.split(' ').next()).collect();
                                words.sort_unstable();
                                words.dedup();
                                words.into_iter().map(|w| pattern(Some(format!("{w} *")))).collect()
                            }
                            "write" | "edit" => vec![pattern(adm.subjects.first().cloned())],
                            _ => vec![pattern(None)],
                        };
                        lock(&self.session).extend(added);
                    }
                }
            }
        }

        let tool_cx = adm.cx.as_ref().unwrap_or(cx);
        let out = tokio::select! {
            biased;
            () = cx.cancel.cancelled() => return Err(ToolError::Cancelled),
            r = tool.call(tool_cx, adm.input) => r?,
        };
        Ok(cap_output(out, cx.limits.bash_max_output_bytes))
    }

    /// The allow-list boundary (09 section 2). Subjects are the strings specifiers are matched
    /// against. Path tools are resolved here so a symlink is judged by where it lands, and the
    /// canonical path replaces the model's string before dispatch. A `bash` command is split
    /// into its simple commands first, and every part has to pass.
    fn admit(
        &self,
        eff: &EffectiveTools,
        cx: &ToolCx,
        name: &str,
        mut input: Value,
    ) -> Result<Admitted, ToolError> {
        let allows: Vec<&ToolPattern> =
            eff.allow.iter().filter(|p| names_tool(&p.tool, name)).collect();
        if allows.is_empty() {
            return Err(denied("not in allow-list"));
        }

        let mut tool_cx = None;
        let mut outside = false;
        let subjects: Vec<String> = match name {
            "bash" => {
                let cmd = input.get("command").and_then(Value::as_str);
                let cmd = cmd.ok_or_else(|| ToolError::Invalid("missing command".into()))?;
                let flat = cmd.split_whitespace().collect::<Vec<_>>().join(" ");
                self.refuse_destructive(&flat)?;
                let parts = shell::split(cmd)
                    .map_err(|why| denied(&format!("ambiguous command: {why}")))?;
                if parts.is_empty() {
                    return Err(ToolError::Invalid("empty command".into()));
                }
                for part in &parts {
                    self.refuse_destructive(part)?;
                }
                parts
            }
            "web_fetch" => {
                let url = input.get("url").and_then(Value::as_str);
                vec![url.ok_or_else(|| ToolError::Invalid("missing url".into()))?.to_string()]
            }
            "glob" => {
                let pat = input.get("pattern").and_then(Value::as_str);
                vec![pat
                    .ok_or_else(|| ToolError::Invalid("missing pattern".into()))?
                    .to_string()]
            }
            "read" | "ls" | "grep" | "write" | "edit" => {
                let writes = matches!(name, "write" | "edit");
                let given = match input.get("path") {
                    Some(Value::String(p)) => p.as_str(),
                    Some(_) => return Err(ToolError::Invalid("path must be a string".into())),
                    None if name == "read" || writes => {
                        return Err(ToolError::Invalid("missing path".into()))
                    }
                    None => ".",
                };
                let (canon, inside) = if writes {
                    policy::resolve_for_write(&cx.cwd, given)?
                } else {
                    policy::resolve(&cx.cwd, given)?
                };
                let root = cx.cwd.canonicalize()?;
                let mut subjects = vec![canon.to_string_lossy().into_owned()];
                if inside {
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
                        canon
                            .ancestors()
                            .skip(1)
                            .find(|a| a.exists())
                            .map_or_else(|| PathBuf::from("/"), std::path::Path::to_path_buf)
                    };
                    tool_cx = Some(ToolCx {
                        run: cx.run,
                        cwd: base,
                        cancel: cx.cancel.clone(),
                        limits: cx.limits,
                        tx: cx.tx.clone(),
                        memory: cx.memory.clone(),
                    });
                }
                if writes {
                    // Anything inside a `.git` directory, the project's own settings file and
                    // the user's config directory (09 section 2).
                    let rel = canon.strip_prefix(&root).unwrap_or(&canon);
                    let names: Vec<String> = rel
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase())
                        .collect();
                    let protected = if names.iter().any(|n| n == ".git") {
                        Some(".git/**")
                    } else if inside && names == [".aibus", "settings.toml"] {
                        Some(".aibus/settings.toml")
                    } else if self
                        .gate
                        .home
                        .as_deref()
                        .is_some_and(|h| canon.starts_with(h.join(".config/aibus")))
                    {
                        Some("~/.config/aibus/**")
                    } else {
                        None
                    };
                    if let Some(entry) = protected {
                        // Only a specifier that names the path lifts the deny-list.
                        let session = lock(&self.session);
                        let lifted = allows
                            .iter()
                            .copied()
                            .chain(self.gate.allow.iter())
                            .chain(session.iter())
                            .filter(|p| p.tool == name)
                            .filter_map(|p| p.spec.as_deref())
                            .any(|spec| subjects.iter().any(|s| policy::spec_matches(spec, s)));
                        if !lifted {
                            return Err(denied(&format!("{entry} is protected")));
                        }
                    }
                }
                input["path"] = Value::String(subjects[0].clone());
                subjects
            }
            _ => vec![],
        };

        if eff.deny.iter().any(|d| {
            names_tool(&d.tool, name)
                && (subjects.is_empty() && d.spec.is_none()
                    || subjects
                        .iter()
                        .any(|s| d.spec.as_deref().is_none_or(|spec| policy::spec_matches(spec, s))))
        }) {
            return Err(denied("denied by tool policy"));
        }
        // A bare allow covers the project only; reaching outside takes a matching specifier.
        if !covers(&allows, name, &subjects, outside) {
            return Err(denied("not in allow-list"));
        }
        Ok(Admitted {
            input,
            subjects,
            outside,
            cx: tool_cx,
        })
    }

    /// `Denied` when `text` trips a destructive-command pattern, logged with the pattern name.
    fn refuse_destructive(&self, text: &str) -> Result<(), ToolError> {
        match self.deny.hit(text) {
            Some(rule) => {
                tracing::warn!(pattern = rule, "bash command refused by the shell deny-list");
                Err(denied(&format!("matches shell deny pattern {rule}")))
            }
            None => Ok(()),
        }
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
