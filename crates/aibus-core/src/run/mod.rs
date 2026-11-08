//! Runs and conversations (02 section 5), and the task that drives one (02 section 3.3).

mod budget;
mod coalesce;
mod loop_;
mod model;
pub mod result;
pub mod transcript;

mod compact;

pub use budget::Budgets;
pub use compact::ModelCompactor;
pub use loop_::{
    spawn, Compactor, RunConfig, RunDeps, RunHandle, RunMail, Start, MAILBOX_CAPACITY,
    STREAM_CAPACITY,
};
pub use model::{expand, resolve_model, ModelDefaults};

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use aibus_graph::StoreHandle;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

pub use aibus_provider::{ContentBlock, Message, MessageRole, Usage};

use crate::agent::{Isolation, Role};
use crate::worktree::WorktreeInfo;
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

/// What a finished run hands back (01 section 5.3). Artifacts and the worktree join it with the
/// tools and isolation that produce them.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct RunResult {
    pub run_id: RunId,
    pub agent: AgentName,
    pub status: RunStatus,
    pub turns: u32,
    pub usage: Usage,
    /// The final assistant text, capped at `ToolLimits::result_max_bytes`.
    pub summary: String,
    /// The full text when `summary` was capped.
    pub summary_file: Option<PathBuf>,
    pub duration: Duration,
    pub error: Option<RunError>,
    /// The run's worktree when it was dirty and so was kept (09 section 7); a clean one is
    /// removed at the end of the run and not reported.
    pub worktree: Option<WorktreeInfo>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum RunErrorKind {
    Provider,
    /// The credential was rejected (06 section 3, exit 4). Never retried.
    Auth,
    Tool,
    Budget,
    Denied,
    Internal,
}

impl RunErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RunErrorKind::Provider => "provider",
            RunErrorKind::Auth => "auth",
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

impl Conversation {
    /// Adds a user turn: every `tool_use` still waiting for its result gets an error one, and
    /// when the last message is already a user turn the text joins it as a block (two user
    /// turns in a row are not valid for every provider). A fork's task and a resumed run's
    /// message both go in this way.
    pub fn push_user_text(&mut self, text: String) {
        let waiting: Vec<_> = match self.messages.last() {
            Some(m) if m.role == MessageRole::Assistant => m
                .blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolUse { id, .. } => Some(*id),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        let mut blocks: Vec<ContentBlock> = waiting
            .into_iter()
            .map(|id| ContentBlock::ToolResult {
                id,
                content: "not run: the run stopped before this call".into(),
                is_error: true,
            })
            .collect();
        blocks.push(ContentBlock::Text(text));
        match self.messages.last_mut() {
            Some(m) if m.role == MessageRole::User => m.blocks.extend(blocks),
            _ => self.messages.push(Message {
                role: MessageRole::User,
                blocks,
                at: SystemTime::UNIX_EPOCH,
            }),
        }
    }
}
