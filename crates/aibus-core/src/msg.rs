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
use crate::workflow::WorkflowRow;
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

/// What a key or a click in the transcript asks of its view state (05 section 3).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrOp {
    /// Positive towards older rows. Scrolling away from the bottom stops following.
    Scroll(i32),
    Top,
    /// Back to the newest row, and follow again (the `jump to latest` button).
    Bottom,
    /// Select a call (`Next` and `Prev` walk the calls of the run).
    Tool(Selection),
    /// Expand or collapse the selected call.
    Toggle,
    /// Step the view one change back in graph time (`[`).
    Back,
    Forward,
    /// Back to now.
    Live,
}

/// What a mouse gesture or a bound key asks of the UI state (05 section 6.1). The TUI turns a
/// hit on its map into one of these, so a click and the key with the same meaning take the same
/// path through `reduce`, and replay sees both alike.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UiIntent {
    Focus(Pane),
    FocusNext,
    FocusPrev,
    Select(Pane, Selection),
    /// `lines` is positive towards older rows (up). `sideways` scrolls the card row.
    Scroll {
        pane: Pane,
        lines: i32,
        sideways: bool,
    },
    /// Enter, or a double click: open what the pane has selected.
    Open(Pane),
    ToggleRail,
    SetRailWidth(u16),
    SetLogRows(u16),
    /// `f` in the log: follow the newest row, or hold the place.
    ToggleFollow,
    /// Right click on a run's card or row, or `m` on the selected one.
    OpenMenu {
        run: RunId,
        col: u16,
        row: u16,
    },
    CloseMenu,
    /// The menu's highlighted item, by index.
    MenuSelect(i64),
    /// Resident memory in MiB, sampled by the loop under `-v`.
    Mem(u32),
    Transcript(TrOp),
    /// A finished run's transcript came back from disk (a chunk of it, or all).
    Loaded(RunId),
    /// The selected row of the open palette, agents screen or accounts screen.
    Pick(Selection),
    /// Opens the permission modal on the oldest open prompt (`p`, a click on the alert).
    ShowPermission,
}

impl UiIntent {
    pub fn name(self) -> &'static str {
        match self {
            UiIntent::Focus(_) => "focus",
            UiIntent::FocusNext => "focus_next",
            UiIntent::FocusPrev => "focus_prev",
            UiIntent::Select(..) => "select",
            UiIntent::Scroll { .. } => "scroll",
            UiIntent::Open(_) => "open",
            UiIntent::ToggleRail => "toggle_rail",
            UiIntent::SetRailWidth(_) => "set_rail_width",
            UiIntent::SetLogRows(_) => "set_log_rows",
            UiIntent::ToggleFollow => "toggle_follow",
            UiIntent::OpenMenu { .. } => "open_menu",
            UiIntent::CloseMenu => "close_menu",
            UiIntent::MenuSelect(_) => "menu_select",
            UiIntent::Mem(_) => "mem",
            UiIntent::Transcript(_) => "transcript",
            UiIntent::Loaded(_) => "loaded",
            UiIntent::Pick(_) => "pick",
            UiIntent::ShowPermission => "show_permission",
        }
    }
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
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    SubagentStart,
    SubagentStop,
    Stop,
    PreCompact,
    Checkpoint,
    SessionEnd,
}

impl HookEvent {
    /// Every event, in the order of the table in 01 section 6.1.
    pub const ALL: [HookEvent; 11] = [
        HookEvent::SessionStart,
        HookEvent::UserPromptSubmit,
        HookEvent::PreToolUse,
        HookEvent::PostToolUse,
        HookEvent::PostToolUseFailure,
        HookEvent::SubagentStart,
        HookEvent::SubagentStop,
        HookEvent::Stop,
        HookEvent::PreCompact,
        HookEvent::Checkpoint,
        HookEvent::SessionEnd,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            HookEvent::SessionStart => "session_start",
            HookEvent::UserPromptSubmit => "user_prompt_submit",
            HookEvent::PreToolUse => "pre_tool_use",
            HookEvent::PostToolUse => "post_tool_use",
            HookEvent::PostToolUseFailure => "post_tool_use_failure",
            HookEvent::SubagentStart => "subagent_start",
            HookEvent::SubagentStop => "subagent_stop",
            HookEvent::Stop => "stop",
            HookEvent::PreCompact => "pre_compact",
            HookEvent::Checkpoint => "checkpoint",
            HookEvent::SessionEnd => "session_end",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.as_str() == s)
    }

    /// The "Can block" column of 01 section 6.1. A timeout on one of these blocks (fail closed).
    pub fn can_block(self) -> bool {
        matches!(
            self,
            HookEvent::UserPromptSubmit
                | HookEvent::PreToolUse
                | HookEvent::SubagentStop
                | HookEvent::Stop
                | HookEvent::Checkpoint
        )
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HookOutcome {
    pub exit_code: i32,
    pub message: Option<String>,
}

/// What the router is asked (01 section 7.2).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RouterKind {
    WhichFile,
    WhichTool,
    RetryOrStop,
    Custom(String),
}

impl RouterKind {
    pub fn as_str(&self) -> &str {
        match self {
            RouterKind::WhichFile => "which_file",
            RouterKind::WhichTool => "which_tool",
            RouterKind::RetryOrStop => "retry_or_stop",
            RouterKind::Custom(name) => name,
        }
    }
}

impl From<&str> for RouterKind {
    fn from(name: &str) -> Self {
        match name {
            "which_file" => RouterKind::WhichFile,
            "which_tool" => RouterKind::WhichTool,
            "retry_or_stop" => RouterKind::RetryOrStop,
            other => RouterKind::Custom(other.to_owned()),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RouterQuestion {
    pub kind: RouterKind,
    /// A short, templated view: the file list, the last error, the available tools.
    pub context: String,
    pub choices: Vec<String>,
}

/// Where a router answer's `p` came from (Q4).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum PSource {
    Logprob,
    SelfReport,
}

impl PSource {
    pub fn as_str(self) -> &'static str {
        match self {
            PSource::Logprob => "logprob",
            PSource::SelfReport => "self_report",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Verdict {
    Sharp,
    Split,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Sharp => "sharp",
            Verdict::Split => "split",
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct RouterAnswer {
    pub choice: String,
    /// 0.0..=1.0, from logprobs when the provider gives them, else self-reported.
    pub p: f32,
    pub verdict: Verdict,
    pub latency: Duration,
    /// Not in the spec's struct: the log and `model:router:last:<kind>` show it (Q4).
    pub source: PSource,
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
/// `key_ref` is `keyring:aibus/<name>`, `env:AIBUS_ACCOUNT_<NAME>_KEY`, `file:<path>` or `none`.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct AccountView {
    pub name: AccountName,
    pub provider: SmolStr,
    pub label: String,
    pub default_model: Option<ModelId>,
    pub key_ref: String,
    /// What the account's provider serves, read from the tables at startup (the `/model` picker
    /// draws from this; it never asks the network). Empty when the table lists nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ModelView>,
}

/// One model of an account as the picker shows it. The fields vary together per model: the price
/// and the capability flags come from the provider's tables for this id.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct ModelView {
    pub id: ModelId,
    /// The `[models]` or account alias that names this model, when there is one.
    pub alias: Option<String>,
    pub context_window: u32,
    /// USD per million tokens; `None` when the table has no price (shown as `n/a`).
    pub input_per_mtok: Option<f64>,
    pub output_per_mtok: Option<f64>,
    pub tools: bool,
    pub thinking: bool,
    pub images: bool,
    pub cache: bool,
    pub logprobs: bool,
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

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Screen {
    Agents,
    Accounts,
    /// `/model` with no reference: the models of the connected accounts, for this target.
    Models(ModelTarget),
    /// `/login` with no provider: the providers an account can be added for.
    Providers,
    /// `/logout <account>` asks first.
    ConfirmLogout(AccountName),
    /// `/clear` asks first.
    ConfirmClear,
    /// `/cancel` on the lead asks first.
    ConfirmCancel(RunId),
    /// `/status`: one run's numbers.
    RunStatus(RunId),
    /// `/status @advisor` and `/status @router`: the seat's checkpoints or answers so far.
    AdvisorStatus,
    RouterStatus,
    /// `/cost` with no target: the per-run table and the session total.
    Cost,
    /// `/workflow list`: the workflows of the project and the user, from `model:workflows`.
    Workflows,
    /// `/workflow show <name>` and a refused `/workflow save`: one text and its summary line,
    /// from `model:workflow:shown`.
    WorkflowShow,
}

/// What `Command::SetModel` changes: the lead's model, a seat's, or one run's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModelTarget {
    Lead,
    Advisor,
    Router,
    Run(RunId),
}

impl ModelTarget {
    /// The word the graph and the log use for it (`lead`, `advisor`, `router`, `run <id>`).
    pub fn label(self) -> String {
        match self {
            ModelTarget::Lead => "lead".into(),
            ModelTarget::Advisor => "advisor".into(),
            ModelTarget::Router => "router".into(),
            ModelTarget::Run(r) => format!("run {r}"),
        }
    }

    /// The inverse of `label`, for what the graph holds.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "lead" => Some(ModelTarget::Lead),
            "advisor" => Some(ModelTarget::Advisor),
            "router" => Some(ModelTarget::Router),
            _ => s.strip_prefix("run ")?.parse().ok().map(ModelTarget::Run),
        }
    }
}

/// The user-facing verb set the TUI and CLI both produce.
#[derive(Clone, PartialEq, Debug)]
pub enum Command {
    /// `parent` is the run that asked (None for the user). `run` is the id the caller already
    /// handed out (the `delegate` tool answers with it before the loop has seen the command);
    /// without one the reducer makes one. `model` and `account` override the definition's.
    Delegate {
        agent: AgentName,
        prompt: String,
        background: bool,
        isolation: Isolation,
        parent: Option<RunId>,
        run: Option<RunId>,
        model: Option<String>,
        account: Option<AccountName>,
    },
    /// Moves a foreground wait to the background (`Ctrl+b`).
    Background {
        run: RunId,
    },
    /// A run that starts from `parent`'s conversation (the lead's when `None`), plus `prompt`.
    /// `run` is the id the caller already handed out.
    Fork {
        prompt: String,
        parent: Option<RunId>,
        run: Option<RunId>,
    },
    Cancel {
        run: RunId,
    },
    /// Continues a finished run with `message`; `parent` is the run that asked, who gets the
    /// result (None when a surface did).
    Resume {
        run: RunId,
        message: String,
        parent: Option<RunId>,
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
    /// The text typed into the palette.
    PaletteText(String),
    OpenHelp,
    /// The agents screen (`A`) or the accounts screen (`K`).
    OpenScreen(Screen),
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
    /// The text typed after `/` in the session log; empty shows every row.
    LogFilter(String),
    Clear,
    Quit {
        force: bool,
    },
    /// Changes the model of `target` from its next request on. `model` is an alias or a full id,
    /// or `off` for a seat. The reducer cannot resolve it (it has no accounts), so it queues the
    /// request and the executor answers with `Msg::ModelSwitched`.
    SetModel {
        target: ModelTarget,
        model: String,
    },
    /// A filesystem step of `/workflow list`, `show` or `save`. The reducer does nothing with it:
    /// the terminal loop does the step (it owns the directories) and answers with
    /// `Msg::WorkflowsLoaded`.
    WorkflowFile(WorkflowFile),
    /// `/workflow new <goal>`: the lead delegates `goal` to the `workflow-author` in the
    /// foreground. The reducer does nothing with it; the binary sends the lead the call, because
    /// it holds the connection to the server the author talks to.
    WorkflowAuthor {
        goal: String,
    },
}

/// What `Command::WorkflowFile` asks the loop to do with the workflow files.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum WorkflowFile {
    List,
    Show(String),
    /// Writes `model:workflow:draft` as `name`, in the user's directory with `user`.
    Save { name: String, user: bool, force: bool },
}

/// The text `/workflow show` opens: the file (or the validator's findings), and one line under it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WorkflowShown {
    pub name: String,
    pub text: String,
    pub summary: String,
}

#[derive(Clone, Debug)]
pub enum Msg {
    // terminal
    Key(KeyEvent),
    Mouse(MouseEvent),
    /// A gesture or key that only changes UI state.
    Ui(UiIntent),
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
    /// An account operation the terminal ran off the UI thread has finished. `op` is `add`,
    /// `test`, `disable` or `remove`; `text` is the line the Accounts screen shows. The graph
    /// logs the outcome only, never the text.
    AccountOpDone {
        op: String,
        account: String,
        ok: bool,
        text: String,
    },
    /// A browser login the terminal started has something to show: the authorization address, or
    /// the device code. `line` is for the modal only; the graph and the logs never keep it.
    AccountPrompt {
        account: String,
        line: String,
    },
    /// The executor's answer to a queued `Command::SetModel` (`id` is the request's). `Ok(None)`
    /// is a seat turned off; `Err` is why nothing changed.
    ModelSwitched {
        id: u64,
        target: ModelTarget,
        to: Result<Option<(ModelId, AccountName)>, String>,
    },
    /// The workflow files as the loop found them, after a list, a show or a save. `rows` replaces
    /// `model:workflows` (the prompt's `/fix` family reads the names), `shown` replaces
    /// `model:workflow:shown` when there is one, and `said` goes to the session log.
    WorkflowsLoaded {
        rows: Vec<WorkflowRow>,
        shown: Option<WorkflowShown>,
        said: Option<String>,
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
            Msg::Ui(_) => "Ui",
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
            Msg::AccountOpDone { .. } => "AccountOpDone",
            Msg::AccountPrompt { .. } => "AccountPrompt",
            Msg::IoError { .. } => "IoError",
            Msg::Shutdown { .. } => "Shutdown",
            Msg::ModelSwitched { .. } => "ModelSwitched",
            Msg::WorkflowsLoaded { .. } => "WorkflowsLoaded",
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
            Msg::Ui(i) => map([("msg", name), ("intent", s(i.name()))]),
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
                ("kind", s(question.kind.as_str())),
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
            Msg::AccountPrompt { line, .. } => map([("msg", name), ("line_len", len(line))]),
            Msg::AccountOpDone { ok, text, .. } => map([("msg", name), ("ok", Value::Bool(*ok)), ("text_len", len(text))]),
            Msg::IoError { context, error } => map([
                ("msg", name),
                ("context_len", len(context)),
                ("error_len", len(error)),
            ]),
            Msg::Shutdown { reason } => map([("msg", name), ("reason", s(reason.as_str()))]),
            Msg::ModelSwitched { id, to, .. } => map([("msg", name), ("id", n(*id)), ("ok", Value::Bool(to.is_ok()))]),
            Msg::WorkflowsLoaded { rows, shown, .. } => {
                map([("msg", name), ("rows", n(rows.len())), ("shown", Value::Bool(shown.is_some()))])
            }
        }
    }
}

/// The verb alone, no arguments.
pub fn command_name(c: &Command) -> &'static str {
    match c {
        Command::Delegate { .. } => "delegate",
        Command::Background { .. } => "background",
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
        Command::PaletteText(_) => "palette_text",
        Command::OpenScreen(_) => "open_screen",
        Command::OpenHelp => "open_help",
        Command::Workflow { .. } => "workflow",
        Command::LedgerUpdate { .. } => "ledger_update",
        Command::CreateHandoff { .. } => "create_handoff",
        Command::Compact { .. } => "compact",
        Command::LogFilter(_) => "log_filter",
        Command::Clear => "clear",
        Command::Quit { .. } => "quit",
        Command::SetModel { .. } => "set_model",
        Command::WorkflowFile(_) => "workflow_file",
        Command::WorkflowAuthor { .. } => "workflow_author",
    }
}
