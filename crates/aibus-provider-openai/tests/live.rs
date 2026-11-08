//! The nightly suite against a real account (10 section 3.1). It only exists with
//! `--features live-tests`, and runs only when `AIBUS_LIVE_ACCOUNT=<name>` names an account
//! whose key is in `AIBUS_LIVE_KEY_<NAME>` (upper-cased, `-` as `_`). `AIBUS_LIVE_PROVIDER` is
//! `openai` (the default), `kimi` or `openai-compatible`; `AIBUS_LIVE_BASE_URL` and
//! `AIBUS_LIVE_MODEL` override the endpoint and the model (a local server needs both).
//!
//! `AIBUS_RECORD=1` also writes what the API answered to `tests/cassettes/live/<case>.http`, with
//! the key stripped, for review before it replaces a cassette.
#![cfg(feature = "live-tests")]

use std::time::Instant;

use aibus_provider::{
    AccountName, AuthMethodKind, CacheHint, ChatRequest, Credential, Message, MessageRole, ModelId,
    Provider, ProviderConfig, ProviderErrorKind, ProviderFactory, RequestMetadata, Secret,
    StopReason, StreamChunk, ToolChoice, ToolSpec,
};
use aibus_provider_http::reqwest;
use aibus_provider_openai::Factory;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct Live {
    account: String,
    provider: String,
    key: String,
    base_url: Option<String>,
    model: String,
}

fn live() -> Option<Live> {
    let account = std::env::var("AIBUS_LIVE_ACCOUNT").ok()?;
    let var = format!("AIBUS_LIVE_KEY_{}", account.to_uppercase().replace('-', "_"));
    let key = std::env::var(&var).unwrap_or_else(|_| panic!("{var} is not set"));
    let provider = std::env::var("AIBUS_LIVE_PROVIDER").unwrap_or_else(|_| "openai".into());
    let default_model = match provider.as_str() {
        "kimi" => "kimi-k2-0905-preview",
        "openai-compatible" => "",
        _ => "gpt-4.1-mini",
    };
    let model = std::env::var("AIBUS_LIVE_MODEL").unwrap_or_else(|_| default_model.into());
    assert!(!model.is_empty(), "AIBUS_LIVE_MODEL is needed for {provider}");
    Some(Live { account, provider, key, base_url: std::env::var("AIBUS_LIVE_BASE_URL").ok(), model })
}

impl Live {
    fn cred(&self, key: &str) -> Credential {
        Credential {
            account: AccountName(self.account.as_str().into()),
            secret: Secret::new(key.to_owned()),
            kind: AuthMethodKind::ApiKey,
            resolved_at: Instant::now(),
        }
    }

    fn factory(&self) -> Factory {
        match self.provider.as_str() {
            "kimi" => Factory::kimi(),
            "openai-compatible" => Factory::compatible(),
            _ => Factory::openai(),
        }
    }

    fn provider(&self) -> std::sync::Arc<dyn Provider> {
        let cfg = ProviderConfig { base_url: self.base_url.clone(), ..ProviderConfig::default() };
        self.factory().build(&cfg).unwrap()
    }

    fn base(&self) -> String {
        self.base_url.clone().unwrap_or_else(|| {
            if self.provider == "kimi" { "https://api.moonshot.ai/v1" } else { "https://api.openai.com/v1" }.into()
        })
    }

    fn request(&self, text: &str) -> ChatRequest {
        ChatRequest {
            model: ModelId(self.model.as_str().into()),
            system: "Answer in one short sentence.".into(),
            messages: vec![Message::text(MessageRole::User, text)],
            tools: vec![],
            tool_choice: ToolChoice::Auto,
            max_output_tokens: 256,
            temperature: None,
            effort: None,
            thinking: None,
            cache_hint: CacheHint::None,
            logprobs: false,
            metadata: RequestMetadata::default(),
        }
    }

    /// Sends `body` straight to `/chat/completions` and, when `AIBUS_RECORD=1`, saves the answer.
    async fn record(&self, case: &str, key: &str, body: serde_json::Value) {
        if std::env::var("AIBUS_RECORD").as_deref() != Ok("1") {
            return;
        }
        let resp = reqwest::Client::new()
            .post(format!("{}/chat/completions", self.base()))
            .header("authorization", format!("Bearer {key}"))
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .unwrap();
        let mut out = format!("HTTP {}\n", resp.status().as_u16());
        for h in ["content-type", "retry-after", "x-request-id"] {
            if let Some(v) = resp.headers().get(h).and_then(|v| v.to_str().ok()) {
                out.push_str(&format!("{h}: {v}\n"));
            }
        }
        let text = resp.text().await.unwrap();
        out.push('\n');
        out.push_str(&text.replace(key, "<key>"));
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cassettes/live");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{}_{case}.http", self.provider)), out).unwrap();
    }
}

macro_rules! live_or_skip {
    () => {
        match live() {
            Some(l) => l,
            None => {
                eprintln!("AIBUS_LIVE_ACCOUNT is not set: skipped");
                return;
            }
        }
    };
}

#[tokio::test]
async fn verify_lists_models() {
    let l = live_or_skip!();
    let info = l.provider().verify(&l.cred(&l.key)).await.unwrap();
    assert!(!info.models.is_empty());
}

#[tokio::test]
async fn a_short_completion_streams_text_usage_and_done() {
    let l = live_or_skip!();
    let (tx, mut rx) = mpsc::channel(256);
    let resp = l.provider().stream(&l.cred(&l.key), l.request("Say hello."), tx, CancellationToken::new()).await.unwrap();
    let mut chunks = Vec::new();
    while let Ok(c) = rx.try_recv() {
        chunks.push(c);
    }
    assert!(chunks.iter().any(|c| matches!(c, StreamChunk::TextDelta(_))));
    assert_eq!(resp.stop, StopReason::EndTurn);
    assert!(resp.usage.output_tokens > 0);
    l.record(
        "stream_text",
        &l.key,
        json!({"model": l.model, "max_tokens": 64, "stream": true, "stream_options": {"include_usage": true},
               "messages": [{"role": "user", "content": "Say hello."}]}),
    )
    .await;
}

#[tokio::test]
async fn a_forced_tool_call_stops_for_tool_use() {
    let l = live_or_skip!();
    let mut req = l.request("What is in main.rs?");
    req.tools = vec![ToolSpec {
        name: "read_file".into(),
        description: "Read a file".into(),
        schema: json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
    }];
    req.tool_choice = ToolChoice::Required;
    let (tx, _rx) = mpsc::channel(256);
    let resp = l.provider().stream(&l.cred(&l.key), req, tx, CancellationToken::new()).await.unwrap();
    assert_eq!(resp.stop, StopReason::ToolUse);
    l.record(
        "tool_call",
        &l.key,
        json!({"model": l.model, "max_tokens": 128, "stream": true, "stream_options": {"include_usage": true},
               "tools": [{"type": "function", "function": {"name": "read_file", "description": "Read a file",
                   "parameters": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}}}],
               "tool_choice": "required",
               "messages": [{"role": "user", "content": "What is in main.rs?"}]}),
    )
    .await;
}

#[tokio::test]
async fn a_wrong_key_is_an_auth_error_that_does_not_retry() {
    let l = live_or_skip!();
    let bad = "sk-not-a-real-key";
    let (tx, _rx) = mpsc::channel(8);
    let e = l.provider().stream(&l.cred(bad), l.request("hi"), tx, CancellationToken::new()).await.unwrap_err();
    assert_eq!((e.kind, e.retryable), (ProviderErrorKind::Auth, false));
    l.record("auth_401", bad, json!({"model": l.model, "max_tokens": 8, "messages": [{"role": "user", "content": "hi"}]})).await;
}

#[tokio::test]
async fn an_unknown_model_is_model_not_found() {
    let l = live_or_skip!();
    let mut req = l.request("hi");
    req.model = ModelId("gpt-no-such-model-9".into());
    let (tx, _rx) = mpsc::channel(8);
    let e = l.provider().stream(&l.cred(&l.key), req, tx, CancellationToken::new()).await.unwrap_err();
    assert_eq!(e.kind, ProviderErrorKind::ModelNotFound);
    l.record(
        "not_found_404",
        &l.key,
        json!({"model": "gpt-no-such-model-9", "max_tokens": 8, "messages": [{"role": "user", "content": "hi"}]}),
    )
    .await;
}
