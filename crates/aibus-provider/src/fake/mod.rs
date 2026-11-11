//! The deterministic fake provider (10 section 3): no network, driven by a `Script`.

mod script;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use async_trait::async_trait;
use smol_str::SmolStr;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::debug;
use ulid::Ulid;

pub use script::{Script, Step};

use crate::error::{ConfigError, ProviderError, ProviderErrorKind};
use crate::provider::*;
use crate::secret::{AuthMethodKind, Credential};
use crate::types::*;

/// The credential the fake runs with by default. A test subscriber fails the suite if this
/// string ever shows up in a log line (04 section 7, condition 2).
pub const FAKE_KEY: &str = "fake-key-DO-NOT-LOG-7f3a9c";

/// A request as the fake saw it, with the account that sent it. Known keys are replaced
/// with `***`, so a test can print or snapshot it.
#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub account: AccountName,
    pub request: ChatRequest,
}

pub struct FakeProvider {
    script: Arc<Mutex<Script>>,
    caps: Capabilities,
    /// Account name to its key. Empty means any credential is accepted.
    accounts: BTreeMap<AccountName, String>,
    recorded: Mutex<Vec<RecordedRequest>>,
}

impl FakeProvider {
    pub fn new(script: Script) -> Self {
        let caps = Capabilities {
            tools: true,
            parallel_tool_calls: true,
            streaming: true,
            thinking: true,
            prompt_cache: true,
            logprobs: true,
            effort: true,
            images: true,
            system_role: SystemRole::Separate,
            max_tools: None,
        };
        Self {
            script: Arc::new(Mutex::new(script)),
            caps,
            accounts: BTreeMap::new(),
            recorded: Mutex::new(vec![]),
        }
    }

    pub fn with_capabilities(mut self, caps: Capabilities) -> Self {
        self.caps = caps;
        self
    }

    /// Registers an account with its own key. Once any account is registered, a credential
    /// for an unknown account, or with the wrong key, fails with `Auth`.
    pub fn with_account(mut self, name: &str, key: &str) -> Self {
        self.accounts
            .insert(AccountName(SmolStr::new(name)), key.to_owned());
        self
    }

    /// The live script, for appending steps mid-test. Several concurrent `stream` calls on
    /// one fake share it and interleave, so concurrent tests use one fake per run.
    pub fn script(&self) -> Arc<Mutex<Script>> {
        self.script.clone()
    }

    /// Every request received so far, in order, after capability degradation.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.recorded.lock().unwrap().clone()
    }

    fn check(&self, creds: &Credential) -> Result<(), ProviderError> {
        if self.accounts.is_empty()
            || self.accounts.get(&creds.account) == Some(creds.secret.expose())
        {
            return Ok(());
        }
        Err(ProviderError::new(
            ProviderErrorKind::Auth,
            format!("fake: credential rejected for account {}", creds.account),
        ))
    }

    fn record(&self, account: &AccountName, req: &ChatRequest) {
        let mut json = serde_json::to_string(req).expect("ChatRequest serialises");
        for key in std::iter::once(FAKE_KEY).chain(self.accounts.values().map(String::as_str)) {
            json = json.replace(key, "***");
        }
        let request = serde_json::from_str(&json).expect("redaction keeps the JSON valid");
        self.recorded.lock().unwrap().push(RecordedRequest {
            account: account.clone(),
            request,
        });
    }
}

fn cancelled() -> ProviderError {
    ProviderError::new(ProviderErrorKind::Cancelled, "fake: cancelled")
}

/// Sends a chunk, giving up the moment `cancel` fires or the receiver is gone.
async fn emit(
    out: &mpsc::Sender<StreamChunk>,
    cancel: &CancellationToken,
    chunk: StreamChunk,
) -> Result<(), ProviderError> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(cancelled()),
        sent = out.send(chunk) => sent.map_err(|_| cancelled()),
    }
}

/// Streams `text` in chunks of at most 8 bytes, cutting only on character boundaries.
async fn emit_text(
    out: &mpsc::Sender<StreamChunk>,
    cancel: &CancellationToken,
    text: &str,
) -> Result<(), ProviderError> {
    let mut rest = text;
    while !rest.is_empty() {
        let mut end = rest.len().min(8);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let (head, tail) = rest.split_at(end);
        emit(out, cancel, StreamChunk::TextDelta(head.to_owned())).await?;
        rest = tail;
    }
    Ok(())
}

fn request_text(req: &ChatRequest) -> String {
    let mut text = req.system.clone();
    for m in &req.messages {
        text.push('\n');
        text.push_str(&m.plain_text());
    }
    text
}

#[async_trait]
impl Provider for FakeProvider {
    fn id(&self) -> ProviderId {
        ProviderId("fake")
    }

    fn capabilities(&self) -> Capabilities {
        self.caps.clone()
    }

    fn models(&self) -> Vec<ModelInfo> {
        ["fake-small", "fake-large"]
            .into_iter()
            .map(|id| ModelInfo {
                id: ModelId(SmolStr::new(id)),
                context_window: 200_000,
                max_output: 8_192,
                family: SmolStr::new("fake"),
                deprecated: false,
            })
            .collect()
    }

    async fn refresh_models(&self, creds: &Credential) -> Result<Vec<ModelInfo>, ProviderError> {
        self.check(creds)?;
        Ok(self.models())
    }

    fn resolve_model(&self, r: &ModelRef, aliases: &AliasTable) -> Option<ModelId> {
        let id = match r {
            ModelRef::Alias(a) => aliases.get(a)?.clone(),
            ModelRef::Full(id) => id.0.clone(),
            ModelRef::Inherit => return None,
        };
        self.models().into_iter().map(|m| m.id).find(|m| m.0 == id)
    }

    async fn stream(
        &self,
        creds: &Credential,
        req: ChatRequest,
        out: mpsc::Sender<StreamChunk>,
        cancel: CancellationToken,
    ) -> Result<ChatResponse, ProviderError> {
        self.check(creds)?;
        let req = self.caps.degrade(req);
        debug!(account = %creds.account, model = %req.model, "fake: stream");
        self.record(&creds.account, &req);

        let mut blocks = vec![];
        let mut text = String::new();
        let mut usage = Usage::default();
        let mut stop = None;
        let mut called_tool = false;
        let mut seen = 0usize;
        let mut calls = 0u128;

        loop {
            if cancel.is_cancelled() {
                return Err(cancelled());
            }
            let step = self.script.lock().unwrap().steps.pop_front();
            let Some(step) = step else { break };
            seen += 1;
            match step {
                Step::Text(t) => {
                    emit_text(&out, &cancel, &t).await?;
                    text.push_str(&t);
                }
                Step::ToolCall { name, input } => {
                    if !text.is_empty() {
                        blocks.push(ContentBlock::Text(std::mem::take(&mut text)));
                    }
                    calls += 1;
                    let id = ToolCallId(Ulid::from_parts(0, calls));
                    let name = SmolStr::new(name);
                    emit(
                        &out,
                        &cancel,
                        StreamChunk::ToolUseStart {
                            id,
                            name: name.clone(),
                        },
                    )
                    .await?;
                    let json_fragment = input.to_string();
                    emit(
                        &out,
                        &cancel,
                        StreamChunk::ToolUseInputDelta { id, json_fragment },
                    )
                    .await?;
                    emit(&out, &cancel, StreamChunk::ToolUseEnd { id }).await?;
                    blocks.push(ContentBlock::ToolUse { id, name, input, signature: None });
                    called_tool = true;
                }
                Step::Usage { input, output } => {
                    usage.input_tokens += input;
                    usage.output_tokens += output;
                    emit(
                        &out,
                        &cancel,
                        StreamChunk::Usage(Usage {
                            input_tokens: input,
                            output_tokens: output,
                            ..Usage::default()
                        }),
                    )
                    .await?;
                }
                Step::Error(e) => return Err(e),
                Step::Delay(d) => {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => return Err(cancelled()),
                        _ = tokio::time::sleep(d) => {}
                    }
                }
                Step::MatchPrompt { contains, then } => {
                    if request_text(&req).contains(&contains) {
                        let mut script = self.script.lock().unwrap();
                        for s in then.into_iter().rev() {
                            script.steps.push_front(s);
                        }
                    }
                }
                Step::RouterAnswer { choice, p } => {
                    let answer = serde_json::json!({ "choice": choice, "p": p }).to_string();
                    emit_text(&out, &cancel, &answer).await?;
                    text.push_str(&answer);
                    if req.logprobs {
                        let chunk = StreamChunk::Logprob {
                            token: choice,
                            logprob: p.ln(),
                        };
                        emit(&out, &cancel, chunk).await?;
                    }
                }
                Step::Stop(reason) => {
                    stop = Some(reason);
                    break;
                }
            }
        }
        if seen == 0 {
            return Err(ProviderError::new(
                ProviderErrorKind::Unknown,
                "fake: script exhausted",
            ));
        }

        if !text.is_empty() {
            blocks.push(ContentBlock::Text(text));
        }
        let stop = stop.unwrap_or(if called_tool {
            StopReason::ToolUse
        } else {
            StopReason::EndTurn
        });
        emit(&out, &cancel, StreamChunk::Done(stop.clone())).await?;
        Ok(ChatResponse {
            message: Message {
                role: MessageRole::Assistant,
                blocks,
                at: SystemTime::UNIX_EPOCH,
            },
            usage,
            stop,
            request_id: Some(format!("fake-req-{}", self.recorded.lock().unwrap().len())),
            rate_limit: None,
        })
    }

    async fn count_tokens(
        &self,
        _: &Credential,
        _: &ChatRequest,
    ) -> Option<Result<u64, ProviderError>> {
        None
    }

    async fn verify(&self, creds: &Credential) -> Result<AccountInfo, ProviderError> {
        self.check(creds)?;
        Ok(AccountInfo {
            label: creds.account.to_string(),
            org: None,
            models: self.models().into_iter().map(|m| m.id).collect(),
        })
    }

    fn price(&self, model: &ModelId) -> Option<Price> {
        self.models()
            .iter()
            .any(|m| &m.id == model)
            .then_some(Price {
                input_per_mtok: 1.0,
                output_per_mtok: 5.0,
                cache_read_per_mtok: Some(0.1),
                cache_write_per_mtok: Some(1.25),
            })
    }
}

/// Registered in the binary's `providers.rs` like any other provider.
pub struct Factory;

impl ProviderFactory for Factory {
    fn id(&self) -> ProviderId {
        ProviderId("fake")
    }

    fn build(&self, _cfg: &ProviderConfig) -> Result<Arc<dyn Provider>, ConfigError> {
        Ok(Arc::new(FakeProvider::new(Script::new())))
    }

    fn auth_methods(&self) -> &'static [AuthMethodKind] {
        &[AuthMethodKind::ApiKey, AuthMethodKind::None]
    }
}
