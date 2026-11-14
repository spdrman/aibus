//! Red before green (issue #109): the conformance suite has to fail on a provider that
//! forgets `ToolUseEnd`, swallows a cancel, or logs the key. The wrapper below breaks the real
//! Anthropic adapter one way at a time; the same wrapper with nothing broken passes, so it is
//! the flaw that turns the suite red and not the wrapper.

use std::path::PathBuf;
use std::sync::Arc;

use aibus_provider::{
    AccountInfo, AuthMethodKind, Capabilities, ChatRequest, ChatResponse, Credential, ModelId,
    ModelInfo, ModelRef, Price, Provider, ProviderConfig, ProviderError, ProviderFactory,
    ProviderId, StopReason, StreamChunk, AliasTable,
};
use aibus_testkit::capture_logs;
use aibus_testkit::conformance::{check_case, Case, Expect, FakeServer, Fixture, Fixtures};
use async_trait::async_trait;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy)]
enum Flaw {
    Nothing,
    DropsToolUseEnd,
    SwallowsCancel,
    LogsTheKey,
}

struct Broken {
    inner: Arc<dyn Provider>,
    flaw: Flaw,
}

#[async_trait]
impl Provider for Broken {
    fn id(&self) -> ProviderId {
        self.inner.id()
    }
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
    fn models(&self) -> Vec<ModelInfo> {
        self.inner.models()
    }
    async fn refresh_models(&self, creds: &Credential) -> Result<Vec<ModelInfo>, ProviderError> {
        self.inner.refresh_models(creds).await
    }
    fn resolve_model(&self, r: &ModelRef, aliases: &AliasTable) -> Option<ModelId> {
        self.inner.resolve_model(r, aliases)
    }
    async fn stream(
        &self,
        creds: &Credential,
        req: ChatRequest,
        out: mpsc::Sender<StreamChunk>,
        cancel: CancellationToken,
    ) -> Result<ChatResponse, ProviderError> {
        match self.flaw {
            Flaw::Nothing => self.inner.stream(creds, req, out, cancel).await,
            Flaw::LogsTheKey => {
                tracing::debug!("stream with key {}", creds.secret.expose());
                self.inner.stream(creds, req, out, cancel).await
            }
            // A token nobody ever cancels: the call only ends when the server does.
            Flaw::SwallowsCancel => self.inner.stream(creds, req, out, CancellationToken::new()).await,
            Flaw::DropsToolUseEnd => {
                let (tx, mut rx) = mpsc::channel(256);
                let forward = async {
                    while let Some(c) = rx.recv().await {
                        if !matches!(c, StreamChunk::ToolUseEnd { .. }) && out.send(c).await.is_err() {
                            break;
                        }
                    }
                };
                tokio::join!(self.inner.stream(creds, req, tx, cancel), forward).0
            }
        }
    }
    async fn count_tokens(&self, creds: &Credential, req: &ChatRequest) -> Option<Result<u64, ProviderError>> {
        self.inner.count_tokens(creds, req).await
    }
    async fn verify(&self, creds: &Credential) -> Result<AccountInfo, ProviderError> {
        self.inner.verify(creds).await
    }
    fn price(&self, model: &ModelId) -> Option<Price> {
        self.inner.price(model)
    }
}

fn cassette(name: &str) -> Fixture {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../aibus-provider-anthropic/tests/cassettes");
    Fixture::load(&dir.join(format!("{name}.http")))
}

fn case(which: &str) -> Case {
    match which {
        "tool_use" => Case {
            name: "tool_use",
            fixture: cassette("tool_call"),
            expect: Expect::Tools { calls: vec![("read_file", json!({"path": "src/main.rs"}))], min_fragments: 2 },
        },
        "cancel" => Case { name: "cancel", fixture: cassette("stream_text").hang_after(4), expect: Expect::Cancel },
        _ => Case {
            name: "text",
            fixture: cassette("stream_text"),
            expect: Expect::Stream { text: Some("Hello, world"), stop: StopReason::EndTurn },
        },
    }
}

/// Whether the case panicked (the suite went red) for the real adapter wrapped with `flaw`.
async fn goes_red(flaw: Flaw, which: &'static str) -> bool {
    let server = FakeServer::start().await;
    let inner = aibus_provider_anthropic::Factory
        .build(&ProviderConfig { base_url: Some(server.uri()), ..ProviderConfig::default() })
        .unwrap();
    let p: Arc<dyn Provider> = Arc::new(Broken { inner, flaw });
    let f = Fixtures { model: "claude-sonnet-4-5", kind: AuthMethodKind::ApiKey, auth: Some(("x-api-key", "")), cases: vec![], skipped: vec![] };
    // Spawned so the panic of a red case is caught; the capture lives inside the task so a
    // leaked key fails the case there and does not trip the test's own drop check.
    tokio::spawn(async move {
        let _logs = capture_logs();
        check_case(&p, &server, &f, &case(which)).await;
    })
    .await
    .is_err()
}

#[tokio::test]
async fn a_provider_that_forgets_tool_use_end_fails_the_suite() {
    assert!(goes_red(Flaw::DropsToolUseEnd, "tool_use").await);
}

#[tokio::test]
async fn a_provider_that_swallows_cancel_fails_the_suite() {
    assert!(goes_red(Flaw::SwallowsCancel, "cancel").await);
}

#[tokio::test]
async fn a_provider_that_logs_the_key_fails_the_suite() {
    assert!(goes_red(Flaw::LogsTheKey, "text").await);
}

#[tokio::test]
async fn the_same_wrapper_with_nothing_broken_passes_all_three() {
    for which in ["tool_use", "cancel", "text"] {
        assert!(!goes_red(Flaw::Nothing, which).await, "{which}");
    }
}
