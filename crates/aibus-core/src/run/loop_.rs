//! A run as an actor (02 section 3.3): one tokio task that owns its `Conversation`, talks to
//! the provider and the tool executor, and tells the rest of the engine only through `Msg`.
//!
//! Hooks, permission prompts, retries and budgets are not here yet (B1.3), so `RunMail::Permission`
//! and `RunMail::Compact` are accepted and dropped.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use aibus_provider::{
    CacheHint, ChatRequest, ChatResponse, Credential, Provider, ProviderError, ProviderErrorKind,
    RequestMetadata, StreamChunk, ToolChoice,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{sleep_until, Instant};
use tokio_util::sync::CancellationToken;

use super::coalesce::Coalescer;
use super::result;
use super::{
    AccountName, AgentName, ContentBlock, Conversation, Message, MessageRole, ModelId, RunError,
    RunErrorKind, RunId, RunResult, RunStatus,
};
use crate::agent::AgentDef;
use crate::context::{self, ContextInput};
use crate::ids::PermissionId;
use crate::msg::{Decision, Msg, ToolCall, ToolResult};
use crate::tool::{ToolCx, ToolError, ToolLimits};
use crate::tools::{effective_tools, EffectiveTools, ToolExecutor};

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
    Compact,
}

pub struct RunHandle {
    pub id: RunId,
    pub cancel: CancellationToken,
    pub mailbox: mpsc::Sender<RunMail>,
    pub join: JoinHandle<()>,
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
    /// `YYYY-MM-DD` for the environment block.
    pub date: String,
    pub data_home: Option<PathBuf>,
}

/// What the run talks to.
pub struct RunDeps {
    pub provider: Arc<dyn Provider>,
    pub creds: Arc<Credential>,
    pub tools: Arc<ToolExecutor>,
    pub tx: mpsc::Sender<Msg>,
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

async fn task(
    cfg: RunConfig,
    deps: RunDeps,
    cancel: CancellationToken,
    mut mail: mpsc::Receiver<RunMail>,
) {
    let started = Instant::now();
    let run = cfg.run;
    send(
        &deps.tx,
        &cancel,
        Msg::RunStarted {
            run,
            agent: cfg.def.name.clone(),
            role: cfg.def.role,
            parent: cfg.parent,
            model: cfg.model.clone(),
            account: cfg.account.clone(),
        },
    )
    .await;

    let (cwd, def, prompt, date, data_home) = (
        cfg.cwd.clone(),
        cfg.def.clone(),
        cfg.prompt.clone(),
        cfg.date.clone(),
        cfg.data_home.clone(),
    );
    let built = tokio::task::spawn_blocking(move || {
        context::build(&ContextInput {
            def: &def,
            prompt: &prompt,
            cwd: &cwd,
            platform: std::env::consts::OS,
            date: &date,
            git: context::gather_git(&cwd),
            data_home: data_home.as_deref(),
        })
    })
    .await;
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
            return finish(&cfg, &deps, &cancel, Conversation::default(), 0, started, end).await;
        }
    };

    let eff = effective_tools(&cfg.def, cfg.def.role, cfg.depth, cfg.def.background);
    let mut turns = 0;
    let end = drive(&cfg, &deps, &cancel, &mut mail, &eff, &mut conv, &mut turns).await;
    finish(&cfg, &deps, &cancel, conv, turns, started, end).await;
}

/// The tool loop. `turns` counts provider requests.
async fn drive(
    cfg: &RunConfig,
    deps: &RunDeps,
    cancel: &CancellationToken,
    mail: &mut mpsc::Receiver<RunMail>,
    eff: &EffectiveTools,
    conv: &mut Conversation,
    turns: &mut u32,
) -> End {
    // 50 is the `max_turns` default from config (01 section 1.1).
    let max_turns = cfg.def.max_turns.unwrap_or(50);
    let max_output = deps
        .provider
        .models()
        .iter()
        .find(|m| m.id == cfg.model)
        .map_or(8_192, |m| m.max_output);
    let price = deps.provider.price(&cfg.model);
    let specs = deps.tools.specs(eff);

    loop {
        if cancel.is_cancelled() {
            return End::of(RunStatus::Cancelled);
        }
        while let Ok(m) = mail.try_recv() {
            if let RunMail::UserMessage(text) = m {
                conv.messages.push(Message::text(MessageRole::User, &text));
            }
        }

        let req = ChatRequest {
            model: cfg.model.clone(),
            system: conv.system.clone(),
            messages: conv.messages.clone(),
            tools: specs.clone(),
            tool_choice: ToolChoice::Auto,
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
        };
        let resp = match stream_turn(deps, cfg.run, req, cancel).await {
            Ok(resp) => resp,
            Err(e) if e.kind == ProviderErrorKind::Cancelled || cancel.is_cancelled() => {
                return End::of(RunStatus::Cancelled)
            }
            Err(e) => {
                let error = RunError {
                    kind: RunErrorKind::Provider,
                    message: format!("run ended early: {}", e.message),
                    retryable: e.retryable,
                };
                let status = if last_text(conv).is_empty() {
                    RunStatus::Failed
                } else {
                    RunStatus::Partial
                };
                return End {
                    status,
                    error: Some(error),
                };
            }
        };
        *turns += 1;
        {
            let (total, turn) = (&mut conv.usage, &resp.usage);
            total.input_tokens += turn.input_tokens;
            total.output_tokens += turn.output_tokens;
            total.cache_read += turn.cache_read;
            total.cache_write += turn.cache_write;
            total.cost_usd += turn.cost_usd;
            if let Some(p) = price {
                let m = 1_000_000.0;
                total.cost_usd += turn.input_tokens as f64 * p.input_per_mtok / m
                    + turn.output_tokens as f64 * p.output_per_mtok / m
                    + turn.cache_read as f64 * p.cache_read_per_mtok.unwrap_or(p.input_per_mtok) / m
                    + turn.cache_write as f64
                        * p.cache_write_per_mtok.unwrap_or(p.input_per_mtok)
                        / m;
            }
        }

        let calls: Vec<(aibus_provider::ToolCallId, smol_str::SmolStr, serde_json::Value)> = resp
            .message
            .blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, name, input } => {
                    Some((*id, name.clone(), input.clone()))
                }
                _ => None,
            })
            .collect();
        conv.messages.push(resp.message);
        if calls.is_empty() {
            return End::of(RunStatus::Completed);
        }

        let mut results = Vec::with_capacity(calls.len());
        for (id, name, input) in calls {
            let call = ToolCall {
                id,
                name: name.clone(),
                input: input.clone(),
                needs_permission: false,
            };
            send(
                &deps.tx,
                cancel,
                Msg::ToolCallRequested {
                    run: cfg.run,
                    call,
                },
            )
            .await;
            let cx = ToolCx {
                run: cfg.run,
                cwd: cfg.cwd.clone(),
                cancel: cancel.clone(),
                limits: cfg.limits,
                tx: deps.tx.clone(),
            };
            let t0 = Instant::now();
            let (content, is_error) = match deps.tools.call(eff, &cx, &name, input).await {
                Ok(out) => (out.content, false),
                Err(ToolError::Cancelled) => return End::of(RunStatus::Cancelled),
                Err(e) => (e.to_string(), true),
            };
            send(
                &deps.tx,
                cancel,
                Msg::ToolCallFinished {
                    run: cfg.run,
                    call_id: id,
                    result: ToolResult {
                        content: content.clone(),
                        is_error,
                    },
                    duration: t0.elapsed(),
                },
            )
            .await;
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

        if *turns >= max_turns {
            return End::of(RunStatus::Partial);
        }
    }
}

/// One provider request. Chunks are coalesced into `Msg::Stream` as they arrive.
async fn stream_turn(
    deps: &RunDeps,
    run: RunId,
    req: ChatRequest,
    cancel: &CancellationToken,
) -> Result<ChatResponse, ProviderError> {
    let (out, mut chunks) = mpsc::channel::<StreamChunk>(STREAM_CAPACITY);
    let call = deps.provider.stream(&deps.creds, req, out, cancel.clone());
    tokio::pin!(call);

    let mut co = Coalescer::default();
    let emit = |batch: Vec<StreamChunk>| send(&deps.tx, cancel, Msg::Stream { run, chunks: batch });
    let result = loop {
        let due = co.due();
        tokio::select! {
            biased;
            () = cancel.cancelled() => {
                break Err(ProviderError::new(ProviderErrorKind::Cancelled, "run cancelled"));
            }
            r = &mut call => break r,
            Some(chunk) = chunks.recv() => {
                if let Some(batch) = co.push(chunk, Instant::now()) {
                    emit(batch).await;
                }
            }
            () = async { sleep_until(due.expect("guarded")).await }, if due.is_some() => {
                if let Some(batch) = co.take() {
                    emit(batch).await;
                }
            }
        }
    };
    while let Ok(chunk) = chunks.try_recv() {
        if let Some(batch) = co.push(chunk, Instant::now()) {
            emit(batch).await;
        }
    }
    if let Some(batch) = co.take() {
        emit(batch).await;
    }
    result
}

/// Sends `msg`; a closed channel means nobody is listening any more, so the run stops.
async fn send(tx: &mpsc::Sender<Msg>, cancel: &CancellationToken, msg: Msg) {
    if tx.send(msg).await.is_err() {
        cancel.cancel();
    }
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

/// Drops the conversation, then sends the one `RunFinished` (after a `RunFailed` when the run
/// produced nothing).
async fn finish(
    cfg: &RunConfig,
    deps: &RunDeps,
    cancel: &CancellationToken,
    conv: Conversation,
    turns: u32,
    started: Instant,
    end: End,
) {
    let usage = conv.usage;
    let text = last_text(&conv);
    drop(conv);

    let dir = cfg.runs_dir.join(cfg.run.to_string());
    let (summary, summary_file) = match result::cap(text.clone(), cfg.limits.result_max_bytes, &dir)
    {
        Ok(capped) => capped,
        Err(e) => {
            send(
                &deps.tx,
                cancel,
                Msg::IoError {
                    context: format!("writing {}", dir.join("result.md").display()),
                    error: e.to_string(),
                },
            )
            .await;
            let mut end = cfg.limits.result_max_bytes.min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            (text[..end].to_owned(), None)
        }
    };
    if let (RunStatus::Failed, Some(error)) = (end.status, &end.error) {
        send(
            &deps.tx,
            cancel,
            Msg::RunFailed {
                run: cfg.run,
                error: error.clone(),
            },
        )
        .await;
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
            },
        })
        .await;
}
