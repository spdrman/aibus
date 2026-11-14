//! A run as an actor (02 section 3.3): one tokio task that owns its `Conversation`, talks to
//! the provider and the tool executor, and tells the rest of the engine only through `Msg`.
//!
//! Around the plain tool loop sit the hooks (01 section 6), the retry rules (02 section 6) and
//! the budgets (01 section 1.1). Mail is read at turn boundaries: a user message is appended
//! (through `user_prompt_submit` for a lead), `Compact` compacts, and a permission answer is
//! only ever consumed while the run waits for it. Compaction itself lives in `compact.rs`: this
//! file decides when (a `Compact` mail, `auto_compact_at`, `ContextTooLong`) and calls whatever
//! `RunDeps::compactor` holds. A run also keeps its transcript on disk (`transcript.rs`) and can
//! start from a parent's conversation (a fork) or from its own transcript (a resume).

mod assist;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use aibus_provider::{
    CacheHint, ChatRequest, ChatResponse, Provider, ProviderError, ProviderErrorKind,
    RequestMetadata, StreamChunk, ToolChoice, ToolSpec,
};
use serde_json::{json, Map, Value};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{sleep, sleep_until, Instant};
use tokio_util::sync::CancellationToken;

use super::budget::Budgets;
use super::coalesce::Coalescer;
use super::fallback::Fallback;
use super::result;
use super::transcript::{self, Kind, Line, RunMeta, Transcript};
use super::{
    AccountName, AgentName, ContentBlock, Conversation, Message, MessageRole, ModelId, RunError,
    RunErrorKind, RunId, RunResult, RunStatus,
};
use assist::{LeadAssist, Route};

use crate::agent::{AgentDef, PermissionMode, Role};
use crate::assist::Assist;
use crate::context::{self, ContextInput};
use crate::credentials::CredentialSource;
use crate::hooks::{Fired, HookInput, HookRunner};
use crate::ids::PermissionId;
use crate::msg::{Decision, HookEvent, Msg, PermissionRequest, ToolCall, ToolResult};
use crate::tool::{Signal, ToolCx, ToolError, ToolLimits};
use crate::tools::{effective_tools, EffectiveTools, ToolExecutor};
use crate::worktree::{self, WorktreeInfo};

/// Slots in a run's mailbox (08 section 3).
pub const MAILBOX_CAPACITY: usize = 64;
/// Slots in the channel the provider streams chunks into (08 section 2).
pub const STREAM_CAPACITY: usize = 1024;

#[derive(Debug)]
pub enum RunMail {
    /// Appended to the conversation before the next request.
    UserMessage(String),
    Permission {
        id: PermissionId,
        decision: Decision,
    },
    /// Compacts at the next turn boundary.
    Compact,
    /// A background child ended: its wrapped result (01 section 5.3) goes to the model as a
    /// message before the next request.
    ChildFinished(String),
    /// A tool call the run makes at its next turn boundary in place of a request, with the
    /// result in hand afterwards: `/fix <text>` typed while the lead runs is a `workflow` call.
    Call {
        tool: smol_str::SmolStr,
        input: serde_json::Value,
    },
    /// A fork is starting from this run (01 section 5.2): at the next turn boundary the run
    /// answers with a copy of its conversation.
    Fork {
        reply: oneshot::Sender<Conversation>,
    },
    /// `/model`: from the next request on, the run talks to this model. A request already in
    /// flight finishes on the old one.
    Model(ModelSwitch),
}

/// Where a run's requests go: the provider, the credential, the model and the account. A run
/// starts with the one in its `RunConfig` and `RunDeps`; `RunMail::Model` replaces it. The four
/// change together, because a model only means something on its own provider.
#[derive(Clone)]
pub struct ModelSwitch {
    pub provider: Arc<dyn Provider>,
    pub creds: Arc<dyn CredentialSource>,
    pub model: ModelId,
    pub account: AccountName,
}

impl std::fmt::Debug for ModelSwitch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ModelSwitch({} on {})", self.model.0, self.account.0)
    }
}

pub struct RunHandle {
    pub id: RunId,
    pub cancel: CancellationToken,
    pub mailbox: mpsc::Sender<RunMail>,
    pub join: JoinHandle<()>,
}

/// Where a run's conversation comes from.
pub enum Start {
    /// Built from the definition and the task (01 section 5.1).
    New,
    /// The parent's conversation with the task already added (01 section 5.2).
    Fork(Box<Conversation>),
    /// The run's own transcript, plus `RunConfig::prompt` as a new user message (01 section 5.4).
    Resume,
}

/// Everything that names this run.
pub struct RunConfig {
    pub run: RunId,
    pub def: AgentDef,
    pub prompt: String,
    pub parent: Option<RunId>,
    pub depth: u8,
    pub model: ModelId,
    pub account: AccountName,
    pub cwd: PathBuf,
    /// `<session>/runs`; a capped summary is written to `<runs_dir>/<run id>/result.md`.
    pub runs_dir: PathBuf,
    pub limits: ToolLimits,
    /// The `[limits]` defaults; the definition's own `max_*` win.
    pub budgets: Budgets,
    /// `YYYY-MM-DD` for the environment block.
    pub date: String,
    pub data_home: Option<PathBuf>,
    /// Text that goes in front of the task as it is (`session_start` hook output, already
    /// labelled `[hook <name>]`).
    pub context: Vec<String>,
    /// Runs in the background: no `ask_user`, `enter_plan_mode` or `exit_plan_mode` (01
    /// section 4.3). The definition's `background: true` and the caller's `background` both
    /// end up here.
    pub background: bool,
    /// `limits.max_spawn_depth`: at `depth` this far down the run is not offered `delegate`.
    pub max_spawn_depth: u8,
    /// A tool call the run opens with in place of its first request, and then goes on with the
    /// result in hand: `@agent-<name> <rest>` is a foreground `delegate` (01 section 4.2), `/fix
    /// <text>` and `--workflow` are a `workflow` call (01 section 8.4).
    pub forced: Option<(smol_str::SmolStr, serde_json::Value)>,
    pub start: Start,
    /// The worktree `cwd` points into, removed when the run ends if nothing in it changed.
    pub worktree: Option<WorktreeInfo>,
}

/// What the run talks to.
pub struct RunDeps {
    pub provider: Arc<dyn Provider>,
    pub creds: Arc<dyn CredentialSource>,
    pub tools: Arc<ToolExecutor>,
    pub tx: mpsc::Sender<Msg>,
    pub hooks: Arc<HookRunner>,
    /// What compacts a conversation (B2.2). Without one a `ContextTooLong` fails the run and
    /// `RunMail::Compact` is dropped.
    pub compactor: Option<Arc<dyn Compactor>>,
    /// The advisor, the router and the session ledger (B2.3). Without it nobody is consulted.
    pub assist: Option<Arc<Assist>>,
    /// The accounts the run moves to when its own fails before the first token of a turn (F9).
    /// Without one a failure ends or retries as it always did.
    pub fallback: Option<Arc<Fallback>>,
}

/// Summarises the start of a conversation into a `compact_boundary` message (01 section 5.5).
#[async_trait::async_trait]
pub trait Compactor: Send + Sync {
    /// Shrinks `conv` in place. `Err` is why it could not.
    async fn compact(
        &self,
        conv: &mut Conversation,
        cancel: &CancellationToken,
    ) -> Result<(), String>;

    /// The auto-handoff of 01 section 5.5, which a lead run writes between `pre_compact` and the
    /// summary. The path of what it wrote, if it wrote one; the default writes nothing.
    async fn handoff(&self, _conv: &Conversation) -> Result<Option<PathBuf>, String> {
        Ok(None)
    }
}

/// Starts the run's task. Must be called inside a tokio runtime.
pub fn spawn(cfg: RunConfig, deps: RunDeps) -> RunHandle {
    let cancel = CancellationToken::new();
    let (mailbox, mail) = mpsc::channel(MAILBOX_CAPACITY);
    let id = cfg.run;
    let join = tokio::spawn(task(cfg, deps, cancel.clone(), mail));
    RunHandle {
        id,
        cancel,
        mailbox,
        join,
    }
}

struct End {
    status: RunStatus,
    error: Option<RunError>,
}

impl End {
    fn of(status: RunStatus) -> Self {
        Self {
            status,
            error: None,
        }
    }
}

/// The three things every step of the run needs.
struct Task<'a> {
    cfg: &'a RunConfig,
    deps: &'a RunDeps,
    cancel: &'a CancellationToken,
    /// `None` when it could not be opened or a write failed (reported once).
    transcript: Mutex<Option<Transcript>>,
    /// A tool the router chose for the next request (`tool_choice`), taken by `request`.
    hint: Mutex<Option<smol_str::SmolStr>>,
    /// Where the next request goes; `RunMail::Model` replaces it at a turn boundary.
    now: Mutex<ModelSwitch>,
}

async fn task(
    cfg: RunConfig,
    deps: RunDeps,
    cancel: CancellationToken,
    mut mail: mpsc::Receiver<RunMail>,
) {
    let started = Instant::now();
    let t = Task {
        cfg: &cfg,
        deps: &deps,
        cancel: &cancel,
        transcript: Mutex::new(None),
        hint: Mutex::new(None),
        now: Mutex::new(ModelSwitch {
            provider: deps.provider.clone(),
            creds: deps.creds.clone(),
            model: cfg.model.clone(),
            account: cfg.account.clone(),
        }),
    };
    t.send(Msg::RunStarted {
        run: cfg.run,
        agent: cfg.def.name.clone(),
        role: cfg.def.role,
        parent: cfg.parent,
        model: cfg.model.clone(),
        account: cfg.account.clone(),
    })
    .await;

    let (cwd, def, prompt, date, data_home) = (
        cfg.cwd.clone(),
        cfg.def.clone(),
        cfg.prompt.clone(),
        cfg.date.clone(),
        cfg.data_home.clone(),
    );
    let path = cfg
        .runs_dir
        .join(cfg.run.to_string())
        .join("conversation.jsonl");
    let kind = match &cfg.start {
        Start::New => Kind::Run,
        Start::Fork(_) => Kind::Fork,
        Start::Resume => Kind::Resume,
    };
    // For a resume: how many messages the file holds, and how many blocks its last one has.
    let mut resumed = None;
    let built: Result<Conversation, String> = match &cfg.start {
        Start::Fork(seed) => Ok((**seed).clone()),
        start => {
            let from = matches!(start, Start::Resume).then(|| path.clone());
            let loaded = tokio::task::spawn_blocking(move || match from {
                Some(path) => {
                    let mut conv = transcript::load(&path)?.conv;
                    let held = (
                        conv.messages.len(),
                        conv.messages.last().map_or(0, |m| m.blocks.len()),
                    );
                    conv.push_user_text(prompt);
                    Ok((conv, Some(held)))
                }
                None => Ok((
                    context::build(&ContextInput {
                        def: &def,
                        prompt: &prompt,
                        cwd: &cwd,
                        platform: std::env::consts::OS,
                        date: &date,
                        git: context::gather_git(&cwd),
                        data_home: data_home.as_deref(),
                    }),
                    None,
                )),
            })
            .await;
            match loaded {
                Ok(Ok((conv, held))) => {
                    resumed = held;
                    Ok(conv)
                }
                Ok(Err(e)) => Err(e),
                Err(e) => Err(e.to_string()),
            }
        }
    };
    let mut conv = match built {
        Ok(conv) => conv,
        Err(e) => {
            let error = RunError {
                kind: RunErrorKind::Internal,
                message: format!("building the context failed: {e}"),
                retryable: false,
            };
            let end = End {
                status: RunStatus::Failed,
                error: Some(error),
            };
            return t.finish(Conversation::default(), 0, started, end).await;
        }
    };

    t.ledger_begin(&conv);
    let mut eff = effective_tools(
        &cfg.def,
        cfg.def.role,
        cfg.depth,
        cfg.background || cfg.def.background,
        cfg.max_spawn_depth,
    );
    // A fork cannot fork (01 section 5.2).
    if kind == Kind::Fork {
        eff.allow.retain(|p| p.tool != "fork");
    }

    // The transcript: who the run is, the system prompt, then the messages as they come. A
    // resumed run only adds to what is there.
    match Transcript::open(&path) {
        Ok(mut tr) => {
            let meta = Line::Start(RunMeta {
                kind,
                agent: cfg.def.name.clone(),
                model: cfg.model.clone(),
                account: cfg.account.clone(),
                parent: cfg.parent,
                depth: cfg.depth,
            });
            let redact = |s: &str| deps.hooks.redact(s).into_owned();
            let head = tr.write(&meta, redact).and_then(|()| {
                if let Some((held, _)) = resumed {
                    // What was loaded is in the file already.
                    tr.mark(held);
                    Ok(())
                } else {
                    tr.write(
                        &Line::System {
                            text: conv.system.clone(),
                        },
                        redact,
                    )
                }
            });
            match head {
                Ok(()) => *t.lock() = Some(tr),
                Err(e) => t.transcript_failed(&e).await,
            }
        }
        Err(e) => t.transcript_failed(&e).await,
    }

    let mut turns = 0;
    let begun = match kind {
        Kind::Fork => None,
        _ => t.begin(&mut conv).await,
    };
    // A resume's message that joined the last message of the file goes in as an extension.
    if let Some((held, blocks)) = resumed {
        if conv.messages.len() == held {
            let added = conv.messages[held - 1].blocks[blocks..].to_vec();
            t.write_line(&Line::Extend { blocks: added }).await;
        }
    }
    let end = match begun {
        Some(end) => end,
        None => drive(&t, &mut mail, &eff, &mut conv, &mut turns).await,
    };
    t.finish(conv, turns, started, end).await;
}

impl Task<'_> {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Transcript>> {
        self.transcript
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The transcript could not be written: say so once and stop trying. The run goes on, it
    /// just cannot be resumed from the file.
    async fn transcript_failed(&self, e: &std::io::Error) {
        *self.lock() = None;
        self.send(Msg::IoError {
            context: "writing conversation.jsonl".into(),
            error: e.to_string(),
        })
        .await;
    }

    /// Appends `line` to the transcript.
    async fn write_line(&self, line: &Line) {
        let done = self
            .lock()
            .as_mut()
            .map(|t| t.write(line, |s| self.deps.hooks.redact(s).into_owned()));
        if let Some(Err(e)) = done {
            self.transcript_failed(&e).await;
        }
    }

    /// Appends the messages the conversation gained since the last call.
    async fn save(&self, conv: &Conversation) {
        let done = self
            .lock()
            .as_mut()
            .map(|t| t.sync(conv, |s| self.deps.hooks.redact(s).into_owned()));
        if let Some(Err(e)) = done {
            self.transcript_failed(&e).await;
        }
    }

    /// A run a person talks to: the lead, not a fork of it. Only its user messages go through
    /// `user_prompt_submit`, and it is the one that ends with `stop`.
    fn user_facing(&self) -> bool {
        self.cfg.def.role == Role::Lead && !matches!(self.cfg.start, Start::Fork(_))
    }

    /// Sends `msg`; a closed channel means nobody is listening any more, so the run stops.
    async fn send(&self, msg: Msg) {
        if self.deps.tx.send(msg).await.is_err() {
            self.cancel.cancel();
        }
    }

    /// Runs the hooks of `event` for this run and reports each one that ran as `HookFired`.
    async fn fire(
        &self,
        event: HookEvent,
        matcher: Option<&str>,
        mode: PermissionMode,
        extra: Map<String, Value>,
    ) -> Fired {
        let cfg = self.cfg;
        let input = HookInput {
            event,
            matcher,
            run: Some(cfg.run),
            agent: cfg.def.name.as_str(),
            role: cfg.def.role,
            cwd: &cfg.cwd,
            transcript_path: Some(
                cfg.runs_dir
                    .join(cfg.run.to_string())
                    .join("transcript.jsonl"),
            ),
            permission_mode: mode,
            extra,
        };
        let fired = self
            .deps
            .hooks
            .fire(&input, &cfg.def.hooks, self.cancel)
            .await;
        for ran in &fired.ran {
            self.send(Msg::HookFired {
                run: Some(cfg.run),
                event,
                outcome: ran.outcome.clone(),
            })
            .await;
        }
        fired
    }

    /// `subagent_start` for a worker, then `user_prompt_submit` and the `session_start` text
    /// for the lead's first prompt. `Some` when a hook refused the prompt.
    async fn begin(&self, conv: &mut Conversation) -> Option<End> {
        let cfg = self.cfg;
        let mode = cfg.def.permission_mode;
        let mut context = cfg.context.clone();
        if cfg.def.role == Role::Worker {
            let name = cfg.def.name.to_string();
            self.fire(
                HookEvent::SubagentStart,
                Some(name.as_str()),
                mode,
                Map::new(),
            )
            .await;
        }
        if self.user_facing() {
            let extra = Map::from_iter([("prompt".to_owned(), json!(cfg.prompt))]);
            let fired = self
                .fire(HookEvent::UserPromptSubmit, None, mode, extra)
                .await;
            if let Some(b) = fired.block {
                return Some(End {
                    status: RunStatus::Denied,
                    error: Some(RunError {
                        kind: RunErrorKind::Denied,
                        message: format!("prompt refused by hook {}: {}", b.hook, b.reason),
                        retryable: false,
                    }),
                });
            }
            context.extend(fired.context);
        }
        if let Some(task) = conv.messages.last_mut() {
            // The task is the last block; the hook output goes in front of it.
            let at = task.blocks.len().saturating_sub(1);
            task.blocks
                .splice(at..at, context.into_iter().map(ContentBlock::Text));
        }
        None
    }

    /// Reads the mailbox at a turn boundary: `queued` first (mail that arrived while the run
    /// waited for a permission answer), then whatever is waiting now.
    async fn boundary(
        &self,
        mail: &mut mpsc::Receiver<RunMail>,
        queued: &mut Vec<RunMail>,
        conv: &mut Conversation,
        mode: PermissionMode,
        pre_tokens: u64,
    ) -> Option<(smol_str::SmolStr, serde_json::Value)> {
        while let Ok(m) = mail.try_recv() {
            queued.push(m);
        }
        let mut compact = false;
        let mut call = None;
        for m in queued.drain(..) {
            match m {
                RunMail::UserMessage(text) => {
                    let mut blocks = Vec::new();
                    // Only the user's own prompt goes past `user_prompt_submit`; a worker's
                    // `send_message` text comes from another run.
                    if self.user_facing() {
                        let extra = Map::from_iter([("prompt".to_owned(), json!(text))]);
                        let fired = self
                            .fire(HookEvent::UserPromptSubmit, None, mode, extra)
                            .await;
                        if fired.block.is_some() {
                            continue;
                        }
                        blocks.extend(fired.context.into_iter().map(ContentBlock::Text));
                    }
                    blocks.push(ContentBlock::Text(text));
                    conv.messages.push(Message {
                        role: MessageRole::User,
                        blocks,
                        at: SystemTime::UNIX_EPOCH,
                    });
                }
                RunMail::Compact => compact = true,
                RunMail::Model(to) => *self.now.lock().unwrap_or_else(PoisonError::into_inner) = to,
                RunMail::Call { tool, input } => call = Some((tool, input)),
                RunMail::ChildFinished(report) => {
                    conv.messages
                        .push(Message::text(MessageRole::User, &report));
                }
                RunMail::Fork { reply } => {
                    // The fork starts from the conversation as it stands, mail so far included.
                    let _ = reply.send(conv.clone());
                }
                // Only meaningful while a prompt is open; a late answer is stale.
                RunMail::Permission { .. } => {}
            }
        }
        if compact {
            if let Err(why) = self.compact(conv, "manual", mode, pre_tokens).await {
                self.send(Msg::IoError {
                    context: format!("compacting {}", self.cfg.run),
                    error: why,
                })
                .await;
            }
        }
        call
    }

    /// The order of 01 section 5.5: `pre_compact` hooks, the lead's auto-handoff, the summary,
    /// the `compact_boundary` line, then `Compacted`. The transcript keeps every message that
    /// was there; only the live context shrinks.
    async fn compact(
        &self,
        conv: &mut Conversation,
        trigger: &str,
        mode: PermissionMode,
        pre_tokens: u64,
    ) -> Result<(), String> {
        let Some(compactor) = &self.deps.compactor else {
            return Err("nothing is set up to compact this conversation".into());
        };
        let extra = Map::from_iter([
            ("trigger".to_owned(), json!(trigger)),
            ("pre_tokens".to_owned(), json!(pre_tokens)),
        ]);
        self.fire(HookEvent::PreCompact, Some(trigger), mode, extra)
            .await;
        self.save(conv).await;
        if self.user_facing() {
            match compactor.handoff(conv).await {
                Ok(Some(path)) => self.send(Msg::HandoffWritten { path }).await,
                Ok(None) => {}
                Err(why) => {
                    self.send(Msg::IoError {
                        context: "writing the auto-handoff".into(),
                        error: why,
                    })
                    .await
                }
            }
        }
        compactor.compact(conv, self.cancel).await?;
        self.write_line(&Line::CompactBoundary {
            pre_tokens,
            messages: conv.messages.clone(),
        })
        .await;
        if let Some(t) = self.lock().as_mut() {
            t.mark(conv.messages.len());
        }
        self.send(Msg::Compacted {
            run: self.cfg.run,
            pre_tokens,
        })
        .await;
        Ok(())
    }

    /// What goes to the provider for `conv`.
    fn chat_request(
        &self,
        conv: &Conversation,
        specs: &[ToolSpec],
        max_output: u32,
        tool_choice: ToolChoice,
    ) -> ChatRequest {
        let cfg = self.cfg;
        ChatRequest {
            model: self
                .now
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .model
                .clone(),
            system: conv.system.clone(),
            messages: conv.messages.clone(),
            tools: specs.to_vec(),
            tool_choice,
            max_output_tokens: max_output,
            temperature: None,
            effort: cfg.def.effort,
            thinking: None,
            cache_hint: CacheHint::AfterSystem,
            logprobs: false,
            metadata: RequestMetadata {
                run_id: Some(cfg.run.to_string()),
                session_id: None,
            },
        }
    }

    /// One provider request, with the retry rules of 02 section 6: a retryable error waits
    /// 500 ms, 1 s, 2 s ... (30 s at most, longer when the provider says so) and tries again up
    /// to five times; `ContextTooLong` compacts once and tries again; `Auth` never retries.
    async fn request(
        &self,
        conv: &mut Conversation,
        specs: &[ToolSpec],
        max_output: u32,
        mode: PermissionMode,
        pre_tokens: u64,
    ) -> Result<ChatResponse, End> {
        // A retryable provider error is retried this many times (02 section 6) ...
        const MAX_RETRIES: u32 = 5;
        // ... after 500 ms, doubling each time, never waiting more than 30 s ...
        const BACKOFF_BASE: Duration = Duration::from_millis(500);
        const BACKOFF_CAP: Duration = Duration::from_secs(30);
        // ... unless the provider asked for longer. A server that asks for an hour gets five
        // minutes.
        const RETRY_AFTER_CAP: Duration = Duration::from_secs(300);

        let cfg = self.cfg;
        let mut retries = 0;
        let mut compacted = false;
        let mut waiting = false;
        // The accounts this turn has used, so a fallback never goes back to one.
        let mut tried = vec![self
            .now
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .account
            .clone()];
        // The router may have named the tool to call; it holds for every attempt of this turn.
        let choice = match self
            .hint
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            Some(name) => ToolChoice::Named(name),
            None => ToolChoice::Auto,
        };
        loop {
            let req = self.chat_request(conv, specs, max_output, choice.clone());
            if waiting {
                // The card stops saying "retrying" once the next attempt starts.
                self.note_waiting("").await;
                waiting = false;
            }
            // Taken now: a switch that lands while this request runs applies to the next one.
            let to = self
                .now
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            let (result, emitted) = stream_turn(self.deps, &to, cfg.run, req, self.cancel).await;
            let e = match result {
                Ok(resp) => return Ok(resp),
                Err(e) => e,
            };
            if e.kind == ProviderErrorKind::Cancelled || self.cancel.is_cancelled() {
                return Err(End::of(RunStatus::Cancelled));
            }
            match e.kind {
                ProviderErrorKind::Auth | ProviderErrorKind::Quota
                    if self.fall_back(&e, emitted, &mut tried).await =>
                {
                    retries = 0;
                }
                ProviderErrorKind::Auth => {
                    let who = &self
                        .now
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .account
                        .clone();
                    let what = if e.message.is_empty() {
                        "authentication failed"
                    } else {
                        &e.message
                    };
                    let banner = format!("account {who}: {what}, run aibus account test {who}");
                    let mut end = failed(conv, banner, false);
                    if let Some(e) = end.error.as_mut() {
                        e.kind = RunErrorKind::Auth;
                    }
                    return Err(end);
                }
                ProviderErrorKind::Quota => {
                    let who = &self
                        .now
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .account
                        .clone();
                    let banner = format!(
                        "account {who}: quota or billing cap reached ({}), top up the plan or switch account, then run aibus account test {who}",
                        e.message
                    );
                    let mut end = failed(conv, banner, false);
                    if let Some(e) = end.error.as_mut() {
                        e.kind = RunErrorKind::Quota;
                    }
                    return Err(end);
                }
                ProviderErrorKind::ContentFilter => {
                    let mut end = failed(
                        conv,
                        format!("run ended early: the provider's content filter refused the request ({})", e.message),
                        false,
                    );
                    if let Some(e) = end.error.as_mut() {
                        e.kind = RunErrorKind::ContentFilter;
                    }
                    return Err(end);
                }
                ProviderErrorKind::ContextTooLong if !compacted => {
                    compacted = true;
                    match self.compact(conv, "auto", mode, pre_tokens).await {
                        Ok(()) => continue,
                        Err(why) => {
                            return Err(failed(
                                conv,
                                format!(
                                    "run ended early: {} (compacting failed: {why})",
                                    e.message
                                ),
                                false,
                            ))
                        }
                    }
                }
                _ if e.retryable && retries < MAX_RETRIES => {
                    retries += 1;
                    let backoff = BACKOFF_BASE
                        .saturating_mul(1 << (retries - 1))
                        .min(BACKOFF_CAP);
                    let wait = backoff.max(e.retry_after.unwrap_or_default().min(RETRY_AFTER_CAP));
                    self.note_waiting(&format!(
                        "retrying in {} s ({retries}/{MAX_RETRIES})",
                        wait.as_secs_f32().ceil()
                    ))
                    .await;
                    waiting = true;
                    tokio::select! {
                        biased;
                        () = self.cancel.cancelled() => return Err(End::of(RunStatus::Cancelled)),
                        () = sleep(wait) => {}
                    }
                }
                // The retries on this account are spent: an overload (or, if asked for, a rate
                // limit) moves on to the next account.
                _ if self.fall_back(&e, emitted, &mut tried).await => retries = 0,
                _ => {
                    return Err(failed(
                        conv,
                        format!("run ended early: {}", e.message),
                        e.retryable,
                    ))
                }
            }
        }
    }

    /// F9: `e` came from the account the run is on. When nothing of the turn has reached the
    /// user yet and the fallback chain has an account for this kind of failure, the run moves
    /// there (the transcript and the graph record it) and this answers true: the caller asks
    /// again, with a fresh retry budget. Never after the first `TextDelta`, `ThinkingDelta` or
    /// `ToolUseStart`.
    async fn fall_back(
        &self,
        e: &ProviderError,
        emitted: bool,
        tried: &mut Vec<AccountName>,
    ) -> bool {
        let Some(fb) = self.deps.fallback.as_ref().filter(|_| !emitted) else {
            return false;
        };
        let from = self
            .now
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .account
            .clone();
        let Some(to) = fb.next(&from, tried, e) else {
            return false;
        };
        tried.push(to.account.clone());
        self.write_line(&Line::Switch {
            account: to.account.clone(),
            model: to.model.clone(),
        })
        .await;
        self.send(Msg::RunSwitched {
            run: self.cfg.run,
            from,
            to: to.account.clone(),
            model: to.model.clone(),
        })
        .await;
        *self.now.lock().unwrap_or_else(PoisonError::into_inner) = to;
        true
    }

    /// The card's "waiting for ..." note (empty clears it).
    async fn note_waiting(&self, note: &str) {
        self.send(Msg::Stream {
            run: self.cfg.run,
            chunks: vec![StreamChunk::Waiting(note.to_owned())],
        })
        .await;
    }

    /// Puts a `PermissionNeeded` into the graph and waits for the answer. A headless run (no
    /// terminal) is refused on the spot with `PermissionDecided { Deny, "no tty" }`. Mail that
    /// arrives meanwhile is kept for the next turn.
    async fn ask(
        &self,
        mail: &mut mpsc::Receiver<RunMail>,
        queued: &mut Vec<RunMail>,
        call: aibus_provider::ToolCallId,
        tool: &str,
        summary: String,
    ) -> Decision {
        let perm = PermissionId::new();
        let request = PermissionRequest {
            id: perm,
            agent: self.cfg.def.name.clone(),
            run: self.cfg.run,
            call,
            tool: tool.into(),
            summary,
        };
        self.send(Msg::PermissionNeeded { request }).await;
        let decided = |decision, reason| Msg::PermissionDecided {
            id: perm,
            decision,
            reason,
        };
        if !self.deps.tools.tty() {
            self.send(decided(Decision::Deny, Some("no tty".into())))
                .await;
            return Decision::Deny;
        }
        loop {
            tokio::select! {
                biased;
                () = self.cancel.cancelled() => return Decision::Deny,
                m = mail.recv() => match m {
                    Some(RunMail::Permission { id: got, decision }) if got == perm => {
                        self.send(decided(decision, None)).await;
                        return decision;
                    }
                    Some(m @ (RunMail::UserMessage(_) | RunMail::Compact | RunMail::Call { .. } | RunMail::ChildFinished(_) | RunMail::Fork { .. } | RunMail::Model(_))) => queued.push(m),
                    Some(RunMail::Permission { .. }) => {}
                    None => return Decision::Deny,
                },
            }
        }
    }

    /// Drops the conversation, then sends the one `RunFinished` (after a `RunFailed` when the
    /// provider is why the run stopped).
    async fn finish(&self, conv: Conversation, turns: u32, started: Instant, end: End) {
        let (cfg, deps) = (self.cfg, self.deps);
        self.save(&conv).await;
        let usage = conv.usage;
        let text = last_text(&conv);
        drop(conv);
        // A clean worktree goes; one with changes stays and is reported (09 section 7).
        let worktree = match cfg.worktree.clone() {
            Some(w) => tokio::task::spawn_blocking(move || worktree::release(w))
                .await
                .ok()
                .flatten(),
            None => None,
        };
        if let Some(a) = &deps.assist {
            a.ledger.finish(cfg.run, end.status);
        }

        let dir = cfg.runs_dir.join(cfg.run.to_string());
        let (summary, summary_file) =
            match result::cap(text.clone(), cfg.limits.result_max_bytes, &dir) {
                Ok(capped) => capped,
                Err(e) => {
                    self.send(Msg::IoError {
                        context: format!("writing {}", dir.join("result.md").display()),
                        error: e.to_string(),
                    })
                    .await;
                    let mut end = cfg.limits.result_max_bytes.min(text.len());
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    (text[..end].to_owned(), None)
                }
            };
        // A run that had text still reports the provider failure (the account banner for an
        // auth error, for one); a budget stop is not a failure.
        let provider_error = end.error.as_ref().filter(|e| {
            matches!(
                e.kind,
                RunErrorKind::Provider
                    | RunErrorKind::Auth
                    | RunErrorKind::Quota
                    | RunErrorKind::ContentFilter
            )
        });
        if end.status == RunStatus::Failed || provider_error.is_some() {
            if let Some(error) = &end.error {
                self.send(Msg::RunFailed {
                    run: cfg.run,
                    error: error.clone(),
                })
                .await;
            }
        }
        let agent: AgentName = cfg.def.name.clone();
        // The final send ignores a closed channel: there is nothing left to stop.
        let _ = deps
            .tx
            .send(Msg::RunFinished {
                run: cfg.run,
                result: RunResult {
                    run_id: cfg.run,
                    agent,
                    status: end.status,
                    turns,
                    usage,
                    summary,
                    summary_file,
                    duration: started.elapsed(),
                    error: end.error,
                    worktree,
                },
            })
            .await;
    }
}

/// The assistant turn a forced call stands for: one tool call, made before the first request.
fn opening_call(name: &str, input: serde_json::Value) -> ChatResponse {
    ChatResponse {
        message: Message {
            role: MessageRole::Assistant,
            blocks: vec![ContentBlock::ToolUse {
                id: aibus_provider::ToolCallId(ulid::Ulid::new()),
                name: name.into(),
                input,
                signature: None,
            }],
            at: SystemTime::UNIX_EPOCH,
        },
        usage: Default::default(),
        stop: aibus_provider::StopReason::ToolUse,
        request_id: None,
        rate_limit: None,
    }
}

/// A run the provider stopped: `Partial` with the text it had, or `Failed` without any.
fn failed(conv: &Conversation, message: String, retryable: bool) -> End {
    let status = if last_text(conv).is_empty() {
        RunStatus::Failed
    } else {
        RunStatus::Partial
    };
    End {
        status,
        error: Some(RunError {
            kind: RunErrorKind::Provider,
            message,
            retryable,
        }),
    }
}

/// The tool loop. `turns` counts provider requests.
async fn drive(
    t: &Task<'_>,
    mail: &mut mpsc::Receiver<RunMail>,
    eff: &EffectiveTools,
    conv: &mut Conversation,
    turns: &mut u32,
) -> End {
    let (cfg, deps, cancel) = (t.cfg, t.deps, t.cancel);
    let budgets = Budgets {
        max_turns: cfg.def.max_turns.unwrap_or(cfg.budgets.max_turns),
        max_tokens_total: cfg
            .def
            .max_tokens_total
            .unwrap_or(cfg.budgets.max_tokens_total),
        max_cost_usd: cfg.def.max_cost_usd.unwrap_or(cfg.budgets.max_cost_usd),
    };
    let specs = deps.tools.specs(eff);
    // The directory the `memory_*` tools are rooted at, found the way the run's first message
    // finds `MEMORY.md`.
    let memory = crate::continuity::memory::memory_dir(
        &cfg.def,
        crate::agent::discover::project_root(&cfg.cwd).unwrap_or(&cfg.cwd),
        cfg.data_home.as_deref(),
    );
    // `enter_plan_mode` switches `mode` to plan and `exit_plan_mode` goes back to `base_mode`.
    let base_mode = cfg.def.permission_mode;
    let mut mode = base_mode;
    // Mail that arrived while the run was waiting for a permission answer.
    let mut queued: Vec<RunMail> = Vec::new();
    // Input tokens of the latest request: what a compaction started now would be shrinking.
    let mut pre_tokens = 0;
    let mut forced = cfg.forced.clone();
    let mut lead = LeadAssist::new(t);

    loop {
        if cancel.is_cancelled() {
            return End::of(RunStatus::Cancelled);
        }
        if let Some(call) = t.boundary(mail, &mut queued, conv, mode, pre_tokens).await {
            forced = Some(call);
        }
        // What the model this turn talks to allows and costs: read again every turn, because
        // `/model` may have changed it at the boundary just now.
        let (max_output, price, auto_limit) = {
            let at = t.now.lock().unwrap_or_else(PoisonError::into_inner).clone();
            let known = at.provider.models();
            let info = known.iter().find(|m| m.id == at.model);
            let window = info.map(|m| u64::from(m.context_window));
            let limit = cfg
                .def
                .auto_compact_at
                .map(u64::from)
                .or(window.map(|w| w / 10 * 8))
                .filter(|_| deps.compactor.is_some());
            (
                info.map_or(8_192, |m| m.max_output),
                at.provider.price(&at.model),
                limit,
            )
        };
        // `auto_compact_at`, else 80 % of the window (01 section 5.5). The count is what the
        // last response said the request was; before there is one, the heuristic's estimate
        // (which already carries a 10 % margin).
        if let Some(limit) = auto_limit {
            let used = if pre_tokens > 0 {
                pre_tokens
            } else {
                aibus_provider::tokens::estimate(
                    &t.chat_request(conv, &specs, max_output, ToolChoice::Auto),
                    0.0,
                )
            };
            if used >= limit && conv.messages.len() > 1 {
                match t.compact(conv, "auto", mode, used).await {
                    Ok(()) => pre_tokens = 0,
                    Err(why) => {
                        // Not fatal: the request may still fit, and `ContextTooLong` tries once more.
                        t.send(Msg::IoError {
                            context: format!("compacting {}", cfg.run),
                            error: why,
                        })
                        .await;
                    }
                }
            }
        }
        t.save(conv).await;
        t.sync(conv);
        if let Some(la) = lead.as_mut() {
            la.before_request(t, conv).await;
        }

        // A forced delegate opens the run in place of the first request.
        let (resp, seeded) = match forced.take() {
            Some((name, input)) => (opening_call(&name, input), true),
            None => match t.request(conv, &specs, max_output, mode, pre_tokens).await {
                Ok(resp) => (resp, false),
                Err(end) => return end,
            },
        };
        if !seeded {
            *turns += 1;
        }
        pre_tokens = resp.usage.input_tokens + resp.usage.cache_read;
        {
            let (total, turn) = (&mut conv.usage, &resp.usage);
            total.input_tokens += turn.input_tokens;
            total.output_tokens += turn.output_tokens;
            total.cache_read += turn.cache_read;
            total.cache_write += turn.cache_write;
            if turn.cost_source == aibus_provider::CostSource::Provider {
                total.cost_source = aibus_provider::CostSource::Provider;
            }
            // The provider's own figure when it has one, otherwise the price table's.
            total.cost_usd += match price {
                Some(p) if turn.cost_usd == 0.0 => {
                    let m = 1_000_000.0;
                    turn.input_tokens as f64 * p.input_per_mtok / m
                        + turn.output_tokens as f64 * p.output_per_mtok / m
                        + turn.cache_read as f64 * p.cache_read_per_mtok.unwrap_or(p.input_per_mtok)
                            / m
                        + turn.cache_write as f64
                            * p.cache_write_per_mtok.unwrap_or(p.input_per_mtok)
                            / m
                }
                _ => turn.cost_usd,
            };
        }
        t.send(Msg::UsageUpdate {
            run: cfg.run,
            usage: conv.usage,
        })
        .await;

        let calls: Vec<(
            aibus_provider::ToolCallId,
            smol_str::SmolStr,
            serde_json::Value,
        )> = resp
            .message
            .blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse {
                    id, name, input, ..
                } => Some((*id, name.clone(), input.clone())),
                _ => None,
            })
            .collect();
        conv.messages.push(resp.message);
        t.save(conv).await;
        if let Some(la) = lead.as_mut() {
            la.after_reply(t, conv, mode).await;
        }

        if calls.is_empty() {
            // The run would end here: the `stop` hooks may send it round again.
            let stop = match cfg.def.role {
                Role::Lead if t.user_facing() => Some((HookEvent::Stop, None)),
                Role::Lead | Role::Worker => {
                    Some((HookEvent::SubagentStop, Some(cfg.def.name.to_string())))
                }
                Role::Advisor | Role::Router => None,
            };
            let mut again = false;
            if let Some((event, matcher)) = stop {
                let extra =
                    Map::from_iter([("last_assistant_message".to_owned(), json!(last_text(conv)))]);
                let fired = t.fire(event, matcher.as_deref(), mode, extra).await;
                if let Some(b) = fired.block {
                    conv.messages.push(Message::text(
                        MessageRole::User,
                        &format!("[hook {}] {}", b.hook, b.reason),
                    ));
                    again = true;
                }
            }
            if let (false, Some(la)) = (again, lead.as_mut()) {
                // The router may nudge a lead that ended on an intention; the advisor speaks
                // before it is done.
                again = la.on_idle(t, conv, mode, &specs).await;
            }
            if !again {
                // A message that came in during the last turn is answered rather than dropped.
                while let Ok(m) = mail.try_recv() {
                    match m {
                        // The conversation will not change again, so the fork can have it now.
                        RunMail::Fork { reply } => {
                            let _ = reply.send(conv.clone());
                        }
                        m => {
                            again |= matches!(
                                m,
                                RunMail::UserMessage(_)
                                    | RunMail::Call { .. }
                                    | RunMail::ChildFinished(_)
                            );
                            queued.push(m);
                        }
                    }
                }
            }
            if !again {
                return End::of(RunStatus::Completed);
            }
        }

        // Only a run that wants to go on is stopped by its budget: a final answer is kept.
        let spent = conv.usage.input_tokens + conv.usage.output_tokens;
        let over = if spent >= budgets.max_tokens_total {
            Some(format!(
                "max_tokens_total reached: {spent} of {} tokens",
                budgets.max_tokens_total
            ))
        } else if conv.usage.cost_usd >= budgets.max_cost_usd {
            Some(format!(
                "max_cost_usd reached: ${:.4} of ${:.2}",
                conv.usage.cost_usd, budgets.max_cost_usd
            ))
        } else {
            None
        };
        if let Some(message) = over {
            // Every tool use needs a result, or the conversation could not be resumed.
            if !calls.is_empty() {
                conv.messages.push(Message {
                    role: MessageRole::User,
                    blocks: calls
                        .iter()
                        .map(|(id, _, _)| ContentBlock::ToolResult {
                            id: *id,
                            content: "not run: the run's budget was used up".into(),
                            is_error: true,
                        })
                        .collect(),
                    at: SystemTime::UNIX_EPOCH,
                });
            }
            return End {
                status: RunStatus::Partial,
                error: Some(RunError {
                    kind: RunErrorKind::Budget,
                    message,
                    retryable: false,
                }),
            };
        }
        if calls.is_empty() {
            if *turns >= budgets.max_turns {
                return End::of(RunStatus::Partial);
            }
            continue;
        }

        let mut results = Vec::with_capacity(calls.len());
        // Each call's tool, content and failure, for the checkpoints; and a router `stop + ask`.
        let mut done = Vec::with_capacity(calls.len());
        let mut stop_after: Option<String> = None;
        for (id, name, mut input) in calls {
            let tool_event = |input: &Value| {
                Map::from_iter([
                    ("tool_name".to_owned(), json!(name)),
                    ("tool_input".to_owned(), input.clone()),
                    ("tool_use_id".to_owned(), json!(id.0.to_string())),
                ])
            };
            let fired = t
                .fire(
                    HookEvent::PreToolUse,
                    Some(name.as_str()),
                    mode,
                    tool_event(&input),
                )
                .await;
            if let Some(updated) = fired.updated_input {
                input = updated;
            }
            let call = ToolCall {
                id,
                name: name.clone(),
                input: input.clone(),
                needs_permission: false,
            };
            t.send(Msg::ToolCallRequested { run: cfg.run, call }).await;
            let cx = ToolCx {
                run: cfg.run,
                cwd: cfg.cwd.clone(),
                cancel: cancel.clone(),
                limits: cfg.limits,
                tx: deps.tx.clone(),
                memory: memory.clone(),
            };
            let t0 = Instant::now();
            let mut result = ToolResult::default();

            // A hook can force a prompt; its answer then also answers the executor's own, so
            // the user is asked once.
            let mut prior = None;
            let mut called = None;
            if let Some(b) = &fired.block {
                called = Some(Err(ToolError::Blocked {
                    hook: b.hook.clone(),
                    reason: b.reason.clone(),
                }));
            } else if fired.ask {
                // The same one line the executor's own prompt shows: scrubbed, 200 characters.
                let summary = deps.hooks.redact(&format!("{name}: {input}")).into_owned();
                let summary: String = summary.chars().take(200).collect();
                match t.ask(mail, &mut queued, id, &name, summary).await {
                    Decision::Deny => {
                        called = Some(Err(ToolError::Denied {
                            reason: "permission denied".into(),
                        }))
                    }
                    d => prior = Some(d),
                }
            }
            let called = match called {
                Some(denied) => denied,
                None => {
                    let (mail_ref, queue_ref, tool_name) = (&mut *mail, &mut queued, &name);
                    let ask = move |summary: String| async move {
                        match prior {
                            Some(d) => d,
                            None => t.ask(mail_ref, queue_ref, id, tool_name, summary).await,
                        }
                    };
                    let _span = tracing::trace_span!(
                        target: "aibus_trace", "tool_call", tool = name.as_str()
                    );
                    deps.tools
                        .call_gated(eff, &cx, mode, &name, input.clone(), ask)
                        .await
                }
            };
            let mut event = HookEvent::PostToolUse;
            let mut response = Map::new();
            match called {
                Ok(out) => {
                    result.content = out.content;
                    result.artifacts = out.artifacts;
                    result.progress = out.progress;
                    response.insert("tool_response".into(), json!(result.content));
                    match out.signal {
                        Some(Signal::EnterPlan) => mode = PermissionMode::Plan,
                        Some(Signal::ExitPlan) => mode = base_mode,
                        Some(Signal::AskUser { .. }) => {
                            // The next user message is the answer.
                            let answer = loop {
                                tokio::select! {
                                    biased;
                                    () = cancel.cancelled() => break None,
                                    m = mail.recv() => match m {
                                        Some(RunMail::UserMessage(text)) => break Some(text),
                                        Some(m @ (RunMail::ChildFinished(_) | RunMail::Call { .. } | RunMail::Fork { .. })) => queued.push(m),
                                        Some(_) => {}
                                        None => break None,
                                    },
                                }
                            };
                            match answer {
                                Some(answer) => result.content = answer,
                                None => return End::of(RunStatus::Cancelled),
                            }
                        }
                        None => {}
                    }
                }
                Err(ToolError::Cancelled) => return End::of(RunStatus::Cancelled),
                Err(e) => {
                    result.content = e.to_string();
                    result.is_error = true;
                    event = HookEvent::PostToolUseFailure;
                    response.insert("error".into(), json!(result.content));
                    // A call that failed on its own (not one that was refused) may be worth
                    // a second go, or the end of the turn: the router answers `retry_or_stop`.
                    let refused = matches!(e, ToolError::Denied { .. } | ToolError::Blocked { .. });
                    if let (false, Some(la)) = (refused, lead.as_mut()) {
                        match la.retry_or_stop(t, &name, &result.content).await {
                            Route::Retry => {
                                let no = |_: String| async { Decision::Deny };
                                let again = deps
                                    .tools
                                    .call_gated(eff, &cx, mode, &name, input.clone(), no)
                                    .await;
                                if let Ok(out) = again {
                                    result.content = out.content;
                                    result.is_error = false;
                                    result.artifacts = out.artifacts;
                                    result.progress = out.progress;
                                    event = HookEvent::PostToolUse;
                                    response.clear();
                                    response.insert("tool_response".into(), json!(result.content));
                                }
                            }
                            Route::Stop => stop_after = Some(result.content.clone()),
                            Route::Carry => {}
                        }
                    }
                }
            }
            if cancel.is_cancelled() {
                return End::of(RunStatus::Cancelled);
            }
            // A call a hook blocked never ran, so there is nothing to follow up on.
            if fired.block.is_none() {
                let mut extra = tool_event(&input);
                extra.extend(response);
                let after = t.fire(event, Some(name.as_str()), mode, extra).await;
                for note in after.notes {
                    result.content.push('\n');
                    result.content.push_str(&note);
                }
            }
            let (content, is_error) = (result.content.clone(), result.is_error);
            t.send(Msg::ToolCallFinished {
                run: cfg.run,
                call_id: id,
                result,
                duration: t0.elapsed(),
            })
            .await;
            done.push((name.clone(), content.clone(), is_error));
            results.push(ContentBlock::ToolResult {
                id,
                content,
                is_error,
            });
        }
        conv.messages.push(Message {
            role: MessageRole::User,
            blocks: results,
            at: SystemTime::UNIX_EPOCH,
        });
        if let Some(la) = lead.as_mut() {
            la.after_results(t, conv, mode, &done).await;
        }
        if let Some(error) = stop_after {
            return End {
                status: RunStatus::Partial,
                error: Some(RunError {
                    kind: RunErrorKind::Tool,
                    message: format!("the router chose to stop and ask: {error}"),
                    retryable: false,
                }),
            };
        }

        if *turns >= budgets.max_turns {
            return End::of(RunStatus::Partial);
        }
    }
}

/// One provider request. Chunks are coalesced into `Msg::Stream` as they arrive.
async fn stream_turn(
    deps: &RunDeps,
    to: &ModelSwitch,
    run: RunId,
    req: ChatRequest,
    cancel: &CancellationToken,
) -> (Result<ChatResponse, ProviderError>, bool) {
    let (out, mut chunks) = mpsc::channel::<StreamChunk>(STREAM_CAPACITY);
    // Resolved for every request, so a rotated key is picked up within the store's cache time.
    let creds = match to.creds.credential() {
        Ok(c) => c,
        Err(e) => {
            return (
                Err(ProviderError::new(ProviderErrorKind::Auth, e.to_string())),
                false,
            )
        }
    };
    let span = tracing::trace_span!(
        target: "aibus_trace",
        "provider_call",
        ttfb_ms = tracing::field::Empty,
        tokens = tracing::field::Empty,
    );
    let began = Instant::now();
    let mut first = span.is_disabled();
    let call = to.provider.stream(&creds, req, out, cancel.clone());
    tokio::pin!(call);

    // Whether anything the user could see has come out: after that the turn cannot move.
    let mut emitted = false;
    let mut seen = |c: &StreamChunk| {
        emitted |= matches!(
            c,
            StreamChunk::TextDelta(_)
                | StreamChunk::ThinkingDelta(_)
                | StreamChunk::ToolUseStart { .. }
        );
    };
    let mut co = Coalescer::default();
    let send = |batch: Vec<StreamChunk>| async move {
        if deps
            .tx
            .send(Msg::Stream { run, chunks: batch })
            .await
            .is_err()
        {
            cancel.cancel();
        }
    };
    let result = loop {
        let due = co.due();
        tokio::select! {
            biased;
            () = cancel.cancelled() => {
                break Err(ProviderError::new(ProviderErrorKind::Cancelled, "run cancelled"));
            }
            r = &mut call => break r,
            Some(chunk) = chunks.recv() => {
                seen(&chunk);
                if !std::mem::replace(&mut first, true) {
                    span.record("ttfb_ms", began.elapsed().as_millis() as u64);
                }
                if let Some(batch) = co.push(chunk, Instant::now()) {
                    send(batch).await;
                }
            }
            () = async { sleep_until(due.expect("guarded")).await }, if due.is_some() => {
                if let Some(batch) = co.take() {
                    send(batch).await;
                }
            }
        }
    };
    while let Ok(chunk) = chunks.try_recv() {
        seen(&chunk);
        if !std::mem::replace(&mut first, true) {
            span.record("ttfb_ms", began.elapsed().as_millis() as u64);
        }
        if let Some(batch) = co.push(chunk, Instant::now()) {
            send(batch).await;
        }
    }
    if let Some(batch) = co.take() {
        send(batch).await;
    }
    if let Ok(resp) = &result {
        span.record("tokens", resp.usage.input_tokens + resp.usage.output_tokens);
    }
    (result, emitted)
}

fn last_text(conv: &Conversation) -> String {
    conv.messages
        .iter()
        .rev()
        .filter(|m| m.role == MessageRole::Assistant)
        .map(|m| {
            m.blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text(t) => Some(t.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("")
        })
        .find(|t| !t.is_empty())
        .unwrap_or_default()
}
