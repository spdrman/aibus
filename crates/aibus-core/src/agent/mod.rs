//! Agent definitions (01 section 1): the types here, `def` parses one file, `discover` walks the
//! scopes, `builtin` holds the compiled-in nine.

pub mod builtin;
pub mod def;
pub mod discover;
pub mod roster;
pub mod validate;

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

pub use aibus_provider::{Effort, ModelRef};

use crate::ids::{AccountName, AgentName};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Lead,
    Worker,
    Advisor,
    Router,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Lead => "lead",
            Role::Worker => "worker",
            Role::Advisor => "advisor",
            Role::Router => "router",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "lead" => Role::Lead,
            "worker" => Role::Worker,
            "advisor" => Role::Advisor,
            "router" => Role::Router,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Isolation {
    #[default]
    None,
    Worktree,
}

impl Isolation {
    pub fn as_str(self) -> &'static str {
        match self {
            Isolation::None => "none",
            Isolation::Worktree => "worktree",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    User,
    Project,
    Local,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    #[default]
    Default,
    AcceptEdits,
    DontAsk,
    Bypass,
    Plan,
}

impl PermissionMode {
    /// The name used in definitions and settings.
    pub fn as_str(self) -> &'static str {
        match self {
            PermissionMode::Default => "default",
            PermissionMode::AcceptEdits => "accept_edits",
            PermissionMode::DontAsk => "dont_ask",
            PermissionMode::Bypass => "bypass",
            PermissionMode::Plan => "plan",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentColor {
    Red,
    Blue,
    Green,
    Yellow,
    Purple,
    Orange,
    Pink,
    Cyan,
}

/// `bash(git *)`: a tool name and an optional glob over its main argument.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ToolPattern {
    pub tool: SmolStr,
    pub spec: Option<SmolStr>,
}

impl std::fmt::Display for ToolPattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.spec {
            Some(spec) => write!(f, "{}({spec})", self.tool),
            None => f.write_str(&self.tool),
        }
    }
}

/// The inverse of `Display`: `bash(git *)` or `read`. Fails on an empty tool or a missing `)`.
impl std::str::FromStr for ToolPattern {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        let (tool, spec) = match s.split_once('(') {
            Some((t, rest)) => (t.trim(), Some(rest.strip_suffix(')').ok_or(())?.trim())),
            None => (s.trim(), None),
        };
        if tool.is_empty() || tool.contains(')') {
            return Err(());
        }
        Ok(ToolPattern {
            tool: tool.to_ascii_lowercase().into(),
            spec: spec.filter(|s| !s.is_empty()).map(SmolStr::from),
        })
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct ToolPolicy {
    /// `None` means every tool available for the role.
    pub allow: Option<Vec<ToolPattern>>,
    pub deny: Vec<ToolPattern>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct HookSpec {
    pub matcher: Option<String>,
    pub command: String,
    pub timeout_secs: Option<u32>,
}

/// Lifecycle hooks scoped to one agent, keyed by event name (01 section 6).
pub type HookSet = BTreeMap<SmolStr, Vec<HookSpec>>;

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct CheckpointSpec {
    pub name: SmolStr,
    pub on: Option<SmolStr>,
    pub matcher: Option<String>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct RouterSpec {
    pub threshold: f32,
    pub questions: Vec<SmolStr>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Scope {
    Cli,
    Project,
    User,
    Extra(PathBuf),
    Builtin,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct DefSource {
    pub path: Option<PathBuf>,
    pub scope: Scope,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct AgentDef {
    pub name: AgentName,
    pub description: String,
    pub role: Role,
    pub tools: ToolPolicy,
    pub model: ModelRef,
    pub account: Option<AccountName>,
    pub permission_mode: PermissionMode,
    pub max_turns: Option<u32>,
    pub max_tokens_total: Option<u64>,
    pub max_cost_usd: Option<f64>,
    pub effort: Option<Effort>,
    pub skills: Vec<String>,
    pub hooks: HookSet,
    pub memory: Option<MemoryScope>,
    pub background: bool,
    pub isolation: Isolation,
    pub omit_project_rules: bool,
    pub color: Option<AgentColor>,
    pub checkpoints: Vec<CheckpointSpec>,
    pub router: Option<RouterSpec>,
    pub initial_prompt: Option<String>,
    pub auto_compact_at: Option<u32>,
    pub system_prompt: String,
    pub source: DefSource,
}
