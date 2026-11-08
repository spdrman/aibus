//! Runs and conversations (02 section 5).

use std::time::SystemTime;

use aibus_graph::StoreHandle;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

pub use aibus_provider::{ContentBlock, Message, MessageRole, Usage};

use crate::agent::{Isolation, Role};
use crate::ids::{AccountName, AgentName, ModelId, PermissionId, RunId, SessionId};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum RunStatus {
    Queued,
    Starting,
    Running,
    WaitingPermission(PermissionId),
    WaitingUser,
    Compacting,
    Completed,
    Partial,
    Failed,
    Cancelled,
    Denied,
}

impl RunStatus {
    /// The string stored in `model:run:<id>:status`.
    pub fn as_value(self) -> String {
        match self {
            RunStatus::WaitingPermission(id) => format!("waiting_permission:{id}"),
            other => other.name().to_owned(),
        }
    }

    fn name(self) -> &'static str {
        match self {
            RunStatus::Queued => "queued",
            RunStatus::Starting => "starting",
            RunStatus::Running => "running",
            RunStatus::WaitingPermission(_) => "waiting_permission",
            RunStatus::WaitingUser => "waiting_user",
            RunStatus::Compacting => "compacting",
            RunStatus::Completed => "completed",
            RunStatus::Partial => "partial",
            RunStatus::Failed => "failed",
            RunStatus::Cancelled => "cancelled",
            RunStatus::Denied => "denied",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        if let Some(id) = s.strip_prefix("waiting_permission:") {
            return id.parse().ok().map(RunStatus::WaitingPermission);
        }
        Some(match s {
            "queued" => RunStatus::Queued,
            "starting" => RunStatus::Starting,
            "running" => RunStatus::Running,
            "waiting_user" => RunStatus::WaitingUser,
            "compacting" => RunStatus::Compacting,
            "completed" => RunStatus::Completed,
            "partial" => RunStatus::Partial,
            "failed" => RunStatus::Failed,
            "cancelled" => RunStatus::Cancelled,
            "denied" => RunStatus::Denied,
            _ => return None,
        })
    }

    /// Holds an orchestrator slot: started and not yet finished.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            RunStatus::Starting
                | RunStatus::Running
                | RunStatus::WaitingPermission(_)
                | RunStatus::WaitingUser
                | RunStatus::Compacting
        )
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            RunStatus::Completed
                | RunStatus::Partial
                | RunStatus::Failed
                | RunStatus::Cancelled
                | RunStatus::Denied
        )
    }
}

/// The card's percentage (05 section 2.6).
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum Progress {
    Indeterminate,
    Fraction(f32),
    Done,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Run {
    pub id: RunId,
    pub session: SessionId,
    pub parent: Option<RunId>,
    pub depth: u8,
    pub agent: AgentName,
    pub role: Role,
    pub model: ModelId,
    pub account: AccountName,
    pub status: RunStatus,
    pub background: bool,
    pub isolation: Isolation,
    pub started_at: Option<SystemTime>,
    pub ended_at: Option<SystemTime>,
    pub turns: u32,
    pub usage: Usage,
    pub progress: Progress,
    /// "maps the repo": from the definition or the prompt's first line.
    pub label: SmolStr,
    /// Handle into the side store (03 section 8).
    pub transcript: StoreHandle,
}

/// What a finished run hands back to the loop.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct RunResult {
    pub status: RunStatus,
    pub turns: u32,
    pub usage: Usage,
    pub summary: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum RunErrorKind {
    Provider,
    Tool,
    Budget,
    Denied,
    Internal,
}

impl RunErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RunErrorKind::Provider => "provider",
            RunErrorKind::Tool => "tool",
            RunErrorKind::Budget => "budget",
            RunErrorKind::Denied => "denied",
            RunErrorKind::Internal => "internal",
        }
    }
}

/// Why a run ended `Failed`. The message is already redacted.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, thiserror::Error)]
#[error("{}: {message}", kind.as_str())]
pub struct RunError {
    pub kind: RunErrorKind,
    pub message: String,
    pub retryable: bool,
}

/// The conversation a run task owns. The graph holds only a handle to its transcript.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct Conversation {
    pub system: String,
    pub messages: Vec<Message>,
    /// Index of the summary message.
    pub compact_boundary: Option<usize>,
    pub usage: Usage,
}
