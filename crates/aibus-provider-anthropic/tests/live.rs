//! The nightly suite against a real account (10 section 3.1). It only exists with
//! `--features live-tests`, and runs only when `AIBUS_LIVE_ACCOUNT=<name>` names an account
//! whose key is in `AIBUS_LIVE_KEY_<NAME>` (upper-cased, `-` as `_`). `AIBUS_LIVE_BASE_URL` and
//! `AIBUS_LIVE_MODEL` override the endpoint and the model.
//!
//! `AIBUS_RECORD=1` also writes what the API answered to `tests/cassettes/live/<case>.http`, with
//! the key and the organisation id stripped, for review before it replaces a cassette.
#![cfg(feature = "live-tests")]

use std::time::Instant;

use aibus_provider::{
    AccountName, AuthMethodKind, CacheHint, ChatRequest, Credential, Message, MessageRole, ModelId,
    Provider, ProviderConfig, ProviderErrorKind, ProviderFactory, RequestMetadata, Secret,
    StopReason, StreamChunk, ToolChoice, ToolSpec,
};
use aibus_provider_anthropic::Factory;
use aibus_provider_http::reqwest;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct Live {
    account: String,
    key: String,
    base_url: Option<String>,
    model: String,
}

fn live() -> Option<Live> {
    let account = std::env::var("AIBUS_LIVE_ACCOUNT").ok()?;
    let var = format!("AIBUS_LIVE_KEY_{}", account.to_uppercase().replace('-', "_"));
    let key = std::env::var(&var).unwrap_or_else(|_| panic!("{var} is not set"));
    Some(Live {
        account,
        key,
        base_url: std::env::var("AIBUS_LIVE_BASE_URL").ok(),
        model: std::env::var("AIBUS_LIVE_MODEL").unwrap_or_else(|_| "claude-haiku-4-5".into()),
    })
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

    fn provider(&self) -> std::sync::Arc<dyn Provider> {
        let cfg = ProviderConfig { base_url: self.base_url.clone(), ..ProviderConfig::default() };
        Factory.build(&cfg).unwrap()
    }

    fn base(&self) -> &str {
        self.base_url.as_deref().unwrap_or("https://api.anthropic.com")
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

    /// Sends `body` straight to `path` and, when `AIBUS_RECORD=1`, saves the answer.
    async fn record(&self, case: &str, path: &str, key: &str, body: serde_json::Value) {
        if std::env::var("AIBUS_RECORD").as_deref() != Ok("1") {
            return;
        }
        let resp = reqwest::Client::new()
            .post(format!("{}{path}", self.base()))
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .unwrap();
        let mut out = format!("HTTP {}\n", resp.status().as_u16());
        for h in ["content-type", "retry-after", "request-id"] {
            if let Some(v) = resp.headers().get(h).and_then(|v| v.to_str().ok()) {
                out.push_str(&format!("{h}: {v}\n"));
            }
        }
        let text = resp.text().await.unwrap();
        out.push('\n');
        out.push_str(&text.replace(key, "<key>"));
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cassettes/live");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{case}.http")), out).unwrap();
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
    assert!(resp.usage.input_tokens > 0 && resp.usage.output_tokens > 0);
    l.record(
        "stream_text",
        "/v1/messages",
        &l.key,
        json!({"model": l.model, "max_tokens": 64, "stream": true, "messages": [{"role": "user", "content": "Say hello."}]}),
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
        "/v1/messages",
        &l.key,
        json!({"model": l.model, "max_tokens": 128, "stream": true,
               "tools": [{"name": "read_file", "description": "Read a file", "input_schema": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}}],
               "tool_choice": {"type": "any"},
               "messages": [{"role": "user", "content": "What is in main.rs?"}]}),
    )
    .await;
}

#[tokio::test]
async fn a_wrong_key_is_an_auth_error_that_does_not_retry() {
    let l = live_or_skip!();
    let bad = "sk-ant-api03-not-a-real-key";
    let (tx, _rx) = mpsc::channel(8);
    let e = l.provider().stream(&l.cred(bad), l.request("hi"), tx, CancellationToken::new()).await.unwrap_err();
    assert_eq!((e.kind, e.retryable), (ProviderErrorKind::Auth, false));
    l.record("auth_401", "/v1/messages", bad, json!({"model": l.model, "max_tokens": 8, "messages": [{"role": "user", "content": "hi"}]})).await;
}

#[tokio::test]
async fn an_unknown_model_is_model_not_found() {
    let l = live_or_skip!();
    let mut req = l.request("hi");
    req.model = ModelId("claude-no-such-model-9".into());
    let (tx, _rx) = mpsc::channel(8);
    let e = l.provider().stream(&l.cred(&l.key), req, tx, CancellationToken::new()).await.unwrap_err();
    assert_eq!(e.kind, ProviderErrorKind::ModelNotFound);
    l.record(
        "not_found_404",
        "/v1/messages",
        &l.key,
        json!({"model": "claude-no-such-model-9", "max_tokens": 8, "messages": [{"role": "user", "content": "hi"}]}),
    )
    .await;
}

#[tokio::test]
async fn count_tokens_answers_for_a_real_request() {
    let l = live_or_skip!();
    let n = l.provider().count_tokens(&l.cred(&l.key), &l.request("hello")).await.unwrap().unwrap();
    assert!(n > 0);
}
