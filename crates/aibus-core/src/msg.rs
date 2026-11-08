//! `Msg` and `Command` (02 section 3.1) with the payload types they need.

use std::path::PathBuf;
use std::time::Duration;

use aibus_graph::Value;
use aibus_provider::StreamChunk;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::agent::{AgentDef, Isolation, Role};
use crate::ids::{AccountName, AgentName, ForkId, ModelId, PermissionId, RunId, ToolCallId};
use crate::run::{Progress, RunError, RunResult, Usage};
use crate::tool::Artifact;
use crate::state::value::{map, n, s};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyCode {
    Char(char),
    Enter,
    Esc,
    Tab,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    F(u8),
}

/// Terminal-neutral key event: the engine must compile without crossterm, so the TUI converts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseKind {
    Down,
    Up,
    Move,
    Scroll(i16),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MouseEvent {
    pub kind: MouseKind,
    pub col: u16,
    pub row: u16,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Pane {
    Advisor,
    Forks,
    Cards,
    Log,
    Input,
}

impl Pane {
    pub fn as_str(self) -> &'static str {
        match self {
            Pane::Advisor => "advisor",
            Pane::Forks => "forks",
            Pane::Cards => "cards",
            Pane::Log => "log",
            Pane::Input => "input",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Selection {
    Index(i64),
    Next,
    Prev,
    Clear,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Outcome {
    Succeeded,
    PartialPlus,
    PartialMinus,
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Decision {
    Allow,
    AllowSession,
    Deny,
}

#[derive(Clone, PartialEq, Debug)]
pub struct PermissionRequest {
    pub id: PermissionId,
    pub agent: AgentName,
    pub run: RunId,
    pub call: ToolCallId,
    pub tool: SmolStr,
    /// Short human line for the modal, already redacted.
    pub summary: String,
}

#[derive(Clone, PartialEq, Debug)]
pub struct ToolCall {
    pub id: ToolCallId,
    pub name: SmolStr,
    pub input: serde_json::Value,
    /// Decided by the run task from `Tool::needs_permission`. A call that needs it is not
    /// executed until `PermissionDecided` allows it.
    pub needs_permission: bool,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
    /// Files the call reported; they land in the run's `meta`.
    pub artifacts: Vec<Artifact>,
    /// Set by `report_artifact`; becomes the card's percentage.
    pub progress: Option<Progress>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum HookEvent {
    SessionStart,
    SessionEnd,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    SubagentStop,
    Stop,
    PreCompact,
    Checkpoint,
}

impl HookEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            HookEvent::SessionStart => "session_start",
            HookEvent::SessionEnd => "session_end",
            HookEvent::UserPromptSubmit => "user_prompt_submit",
            HookEvent::PreToolUse => "pre_tool_use",
            HookEvent::PostToolUse => "post_tool_use",
            HookEvent::SubagentStop => "subagent_stop",
            HookEvent::Stop => "stop",
            HookEvent::PreCompact => "pre_compact",
            HookEvent::Checkpoint => "checkpoint",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HookOutcome {
    pub exit_code: i32,
    pub message: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RouterQuestion {
    pub kind: SmolStr,
    pub prompt: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Verdict {
    Sharp,
    Soft,
    Split,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Sharp => "sharp",
            Verdict::Soft => "soft",
            Verdict::Split => "split",
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct RouterAnswer {
    pub choice: String,
    pub p: f64,
    pub verdict: Verdict,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Checkpoint {
    BeforePlan,
    SameErrorX2,
    BeforeDone,
    Custom(SmolStr),
}

impl Checkpoint {
    pub fn name(&self) -> &str {
        match self {
            Checkpoint::BeforePlan => "before_plan",
            Checkpoint::SameErrorX2 => "same_error_x2",
            Checkpoint::BeforeDone => "before_done",
            Checkpoint::Custom(n) => n,
        }
    }
}

/// What the graph may know about an account. There is deliberately no secret-shaped field:
/// `key_ref` is `keyring:aibus/<name>`, `env:AIBUS_ACCOUNT_<NAME>_KEY` or `file:<path>`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct AccountView {
    pub name: AccountName,
    pub provider: SmolStr,
    pub label: String,
    pub default_model: Option<ModelId>,
    pub key_ref: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ShutdownReason {
    UserQuit,
    Forced,
    Signal,
    Error,
}

impl ShutdownReason {
    pub fn as_str(self) -> &'static str {
        match self {
            ShutdownReason::UserQuit => "user_quit",
            ShutdownReason::Forced => "forced",
            ShutdownReason::Signal => "signal",
            ShutdownReason::Error => "error",
        }
    }
}

/// The user-facing verb set the TUI and CLI both produce.
#[derive(Clone, PartialEq, Debug)]
pub enum Command {
    /// `parent` is the run that asked (None for the user).
    Delegate {
        agent: AgentName,
        prompt: String,
        background: bool,
        isolation: Isolation,
        parent: Option<RunId>,
    },
    Fork {
        prompt: String,
        parent: Option<RunId>,
    },
    Cancel {
        run: RunId,
    },
    Resume {
        run: RunId,
        message: String,
    },
    SendMessage {
        run: RunId,
        text: String,
    },
    Permission {
        id: PermissionId,
        decision: Decision,
    },
    Retry {
        run: RunId,
    },
    Focus(Pane),
    Scroll(Pane, i32),
    Select(Pane, Selection),
    OpenTranscript(RunId),
    CloseModal,
    OpenPalette,
    OpenHelp,
    Workflow {
        name: String,
        input: String,
    },
    LedgerUpdate {
        section: String,
        text: String,
    },
    CreateHandoff {
        outcome: Option<Outcome>,
    },
    Compact {
        run: RunId,
    },
    Clear,
    Quit {
        force: bool,
    },
}

#[derive(Clone, Debug)]
pub enum Msg {
    // terminal
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize {
        cols: u16,
        rows: u16,
    },
    /// 30 Hz while `view:animating`, else not sent at all.
    Tick,
    // user intent
    SubmitPrompt {
        text: String,
    },
    Command(Command),
    // engine facts
    RunStarted {
        run: RunId,
        agent: AgentName,
        role: Role,
        parent: Option<RunId>,
        model: ModelId,
        account: AccountName,
    },
    /// Coalesced chunks of one frame.
    Stream {
        run: RunId,
        chunks: Vec<StreamChunk>,
    },
    ToolCallRequested {
        run: RunId,
        call: ToolCall,
    },
    ToolCallFinished {
        run: RunId,
        call_id: ToolCallId,
        result: ToolResult,
        duration: Duration,
    },
    PermissionNeeded {
        request: PermissionRequest,
    },
    PermissionDecided {
        id: PermissionId,
        decision: Decision,
        /// Why, when nobody was asked (`"no tty"`).
        reason: Option<String>,
    },
    RunFinished {
        run: RunId,
        result: RunResult,
    },
    RunFailed {
        run: RunId,
        error: RunError,
    },
    UsageUpdate {
        run: RunId,
        usage: Usage,
    },
    HookFired {
        run: Option<RunId>,
        event: HookEvent,
        outcome: HookOutcome,
    },
    RouterAnswered {
        fork: ForkId,
        question: RouterQuestion,
        answer: RouterAnswer,
    },
    AdvisorSpoke {
        checkpoint: Checkpoint,
        advice: String,
        tokens_read: u64,
    },
    CheckpointHit {
        checkpoint: Checkpoint,
        detail: String,
    },
    Compacted {
        run: RunId,
        pre_tokens: u64,
    },
    HandoffWritten {
        path: PathBuf,
    },
    // infra
    AgentsReloaded {
        defs: Vec<AgentDef>,
        warnings: Vec<String>,
    },
    AccountsReloaded {
        accounts: Vec<AccountView>,
    },
    IoError {
        context: String,
        error: String,
    },
    Shutdown {
        reason: ShutdownReason,
    },
    /// The persist effect finished a snapshot. Not in 02 section 3.1: something has to reset
    /// `model:persist:due`, and only a commit can do it.
    Persisted,
}

impl Msg {
    /// The commit intent. Stable names; they end up in the commit log and in tests.
    pub fn name(&self) -> &'static str {
        match self {
            Msg::Key(_) => "Key",
            Msg::Mouse(_) => "Mouse",
            Msg::Resize { .. } => "Resize",
            Msg::Tick => "Tick",
            Msg::SubmitPrompt { .. } => "SubmitPrompt",
            Msg::Command(_) => "Command",
            Msg::RunStarted { .. } => "RunStarted",
            Msg::Stream { .. } => "Stream",
            Msg::ToolCallRequested { .. } => "ToolCallRequested",
            Msg::ToolCallFinished { .. } => "ToolCallFinished",
            Msg::PermissionNeeded { .. } => "PermissionNeeded",
            Msg::PermissionDecided { .. } => "PermissionDecided",
            Msg::RunFinished { .. } => "RunFinished",
            Msg::RunFailed { .. } => "RunFailed",
            Msg::UsageUpdate { .. } => "UsageUpdate",
            Msg::HookFired { .. } => "HookFired",
            Msg::RouterAnswered { .. } => "RouterAnswered",
            Msg::AdvisorSpoke { .. } => "AdvisorSpoke",
            Msg::CheckpointHit { .. } => "CheckpointHit",
            Msg::Compacted { .. } => "Compacted",
            Msg::HandoffWritten { .. } => "HandoffWritten",
            Msg::AgentsReloaded { .. } => "AgentsReloaded",
            Msg::AccountsReloaded { .. } => "AccountsReloaded",
            Msg::IoError { .. } => "IoError",
            Msg::Shutdown { .. } => "Shutdown",
            Msg::Persisted => "Persisted",
        }
    }

    /// The redacted payload for `model:last_msg` and `msgs.jsonl`: ids, counts and lengths,
    /// never text content. A key press carries nothing at all, since it may be a password.
    pub fn summary(&self) -> Value {
        let name = s(self.name());
        let len = |t: &str| n(t.len());
        match self {
            Msg::Key(_) | Msg::Mouse(_) | Msg::Tick | Msg::Persisted => map([("msg", name)]),
            Msg::Resize { cols, rows } => {
                map([("msg", name), ("cols", n(*cols)), ("rows", n(*rows))])
            }
            Msg::SubmitPrompt { text } => map([("msg", name), ("len", len(text))]),
            Msg::Command(c) => map([("msg", name), ("command", s(command_name(c)))]),
            Msg::RunStarted { run, .. } => map([("msg", name), ("run", s(&run.to_string()))]),
            Msg::Stream { run, chunks } => {
                let bytes: usize = chunks
                    .iter()
                    .map(|c| match c {
                        StreamChunk::TextDelta(t) | StreamChunk::ThinkingDelta(t) => t.len(),
                        StreamChunk::ToolUseInputDelta { json_fragment, .. } => json_fragment.len(),
                        _ => 0,
                    })
                    .sum();
                map([
                    ("msg", name),
                    ("run", s(&run.to_string())),
                    ("chunks", n(chunks.len())),
                    ("bytes", n(bytes)),
                ])
            }
            Msg::ToolCallRequested { run, call } => map([
                ("msg", name),
                ("run", s(&run.to_string())),
                ("call", s(&call.id.0.to_string())),
                ("input_bytes", n(call.input.to_string().len())),
            ]),
            Msg::ToolCallFinished {
                run,
                call_id,
                result,
                duration,
            } => map([
                ("msg", name),
                ("run", s(&run.to_string())),
                ("call", s(&call_id.0.to_string())),
                ("bytes", len(&result.content)),
                ("ok", Value::Bool(!result.is_error)),
                ("ms", n(duration.as_millis())),
            ]),
            Msg::PermissionNeeded { request } => map([
                ("msg", name),
                ("id", s(&request.id.to_string())),
                ("run", s(&request.run.to_string())),
            ]),
            Msg::PermissionDecided { id, decision, .. } => map([
                ("msg", name),
                ("id", s(&id.to_string())),
                ("allowed", Value::Bool(*decision != Decision::Deny)),
            ]),
            Msg::RunFinished { run, result } => map([
                ("msg", name),
                ("run", s(&run.to_string())),
                ("turns", n(result.turns)),
                ("summary_len", len(&result.summary)),
            ]),
            Msg::RunFailed { run, error } => map([
                ("msg", name),
                ("run", s(&run.to_string())),
                ("kind", s(error.kind.as_str())),
            ]),
            Msg::UsageUpdate { run, usage } => map([
                ("msg", name),
                ("run", s(&run.to_string())),
                (
                    "tokens",
                    n(usage.input_tokens.saturating_add(usage.output_tokens)),
                ),
            ]),
            Msg::HookFired {
                run,
                event,
                outcome,
            } => map([
                ("msg", name),
                ("run", run.map(|r| s(&r.to_string())).unwrap_or(Value::Null)),
                ("event", s(event.as_str())),
                ("exit_code", n(outcome.exit_code)),
            ]),
            Msg::RouterAnswered { fork, question, .. } => map([
                ("msg", name),
                ("fork", n(fork.0)),
                ("kind", s(&question.kind)),
            ]),
            Msg::AdvisorSpoke {
                checkpoint,
                advice,
                tokens_read,
            } => map([
                ("msg", name),
                ("checkpoint", s(checkpoint.name())),
                ("advice_len", len(advice)),
                ("tokens_read", n(*tokens_read)),
            ]),
            Msg::CheckpointHit { checkpoint, detail } => map([
                ("msg", name),
                ("checkpoint", s(checkpoint.name())),
                ("detail_len", len(detail)),
            ]),
            Msg::Compacted { run, pre_tokens } => map([
                ("msg", name),
                ("run", s(&run.to_string())),
                ("pre_tokens", n(*pre_tokens)),
            ]),
            Msg::HandoffWritten { path } => {
                map([("msg", name), ("path_len", n(path.as_os_str().len()))])
            }
            Msg::AgentsReloaded { defs, warnings } => map([
                ("msg", name),
                ("defs", n(defs.len())),
                ("warnings", n(warnings.len())),
            ]),
            Msg::AccountsReloaded { accounts } => {
                map([("msg", name), ("accounts", n(accounts.len()))])
            }
            Msg::IoError { context, error } => map([
                ("msg", name),
                ("context_len", len(context)),
                ("error_len", len(error)),
            ]),
            Msg::Shutdown { reason } => map([("msg", name), ("reason", s(reason.as_str()))]),
        }
    }
}

/// The verb alone, no arguments.
pub fn command_name(c: &Command) -> &'static str {
    match c {
        Command::Delegate { .. } => "delegate",
        Command::Fork { .. } => "fork",
        Command::Cancel { .. } => "cancel",
        Command::Resume { .. } => "resume",
        Command::SendMessage { .. } => "send_message",
        Command::Permission { .. } => "permission",
        Command::Retry { .. } => "retry",
        Command::Focus(_) => "focus",
        Command::Scroll(..) => "scroll",
        Command::Select(..) => "select",
        Command::OpenTranscript(_) => "open_transcript",
        Command::CloseModal => "close_modal",
        Command::OpenPalette => "open_palette",
        Command::OpenHelp => "open_help",
        Command::Workflow { .. } => "workflow",
        Command::LedgerUpdate { .. } => "ledger_update",
        Command::CreateHandoff { .. } => "create_handoff",
        Command::Compact { .. } => "compact",
        Command::Clear => "clear",
        Command::Quit { .. } => "quit",
    }
}
