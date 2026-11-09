//! The `Tool` trait and its supporting types (02 section 5).

use std::path::PathBuf;

use aibus_provider::ToolSpec;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::PermissionMode;
use crate::ids::RunId;
use crate::msg::Msg;
use crate::run::Progress;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Need {
    Never,
    Ask,
    Always,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ToolLimits {
    pub result_max_bytes: usize,
    pub bash_max_output_bytes: usize,
}

impl Default for ToolLimits {
    fn default() -> Self {
        Self {
            result_max_bytes: 32 * 1024,
            bash_max_output_bytes: 256 * 1024,
        }
    }
}

pub struct ToolCx {
    pub run: RunId,
    pub cwd: PathBuf,
    pub cancel: CancellationToken,
    pub limits: ToolLimits,
    pub tx: mpsc::Sender<Msg>,
    /// The run's memory directory, when its definition has `memory` (01 section 8.3); what the
    /// `memory_*` tools are rooted at.
    pub memory: Option<PathBuf>,
}

/// A file a run marked as one of its outputs (`report_artifact`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Artifact {
    pub name: String,
    /// What the file is: `report`, `patch`, `data`, whatever the model called it.
    pub kind: String,
    pub path: PathBuf,
}

/// Something a tool wants the run loop to do, because only the loop owns the mailbox and the
/// effective permission mode.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Signal {
    /// Park the run in `WaitingUser` and use the next user message as the tool result.
    AskUser {
        question: String,
        choices: Vec<String>,
    },
    /// The effective mode becomes `plan`.
    EnterPlan,
    /// The effective mode goes back to what the definition asked for.
    ExitPlan,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct ToolOutput {
    pub content: String,
    pub artifacts: Vec<Artifact>,
    pub truncated: bool,
    pub progress: Option<Progress>,
    pub signal: Option<Signal>,
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("denied: {reason}")]
    Denied { reason: String },
    #[error("timeout")]
    Timeout,
    #[error("cancelled")]
    Cancelled,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("blocked by hook {hook}: {reason}")]
    Blocked { hook: String, reason: String },
}

#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    /// Name, description and JSON schema, as the model sees it.
    fn spec(&self) -> ToolSpec;
    fn needs_permission(&self, input: &serde_json::Value, mode: PermissionMode) -> Need;
    async fn call(&self, cx: &ToolCx, input: serde_json::Value) -> Result<ToolOutput, ToolError>;
}
