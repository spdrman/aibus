//! The `Tool` trait and its supporting types (02 section 5). Concrete tools land with A4.

use std::path::PathBuf;

use aibus_provider::ToolSpec;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::PermissionMode;
use crate::ids::RunId;
use crate::msg::Msg;

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
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Artifact {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ToolOutput {
    pub content: String,
    pub artifacts: Vec<Artifact>,
    pub truncated: bool,
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
    fn name(&self) -> &'static str;
    /// Name, description and JSON schema, as the model sees it.
    fn spec(&self) -> ToolSpec;
    fn needs_permission(&self, input: &serde_json::Value, mode: PermissionMode) -> Need;
    async fn call(&self, cx: &ToolCx, input: serde_json::Value) -> Result<ToolOutput, ToolError>;
}
