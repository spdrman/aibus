//! Cassettes and helpers for the tests that run the provider against a local mock server.
//!
//! A cassette is `tests/cassettes/<name>.http`: `HTTP <status>`, header lines, a blank line and
//! the body. `AIBUS_RECORD=1` with the live suite writes them from a real account (tests/live.rs);
//! secrets are stripped on the way in, so a cassette never holds a key.
#![allow(dead_code)]

use std::path::PathBuf;
use std::time::{Instant, SystemTime};

use aibus_provider::{
    AccountName, AuthMethodKind, CacheHint, ChatRequest, ChatResponse, Credential, Message,
    MessageRole, ModelId, Provider, ProviderError, RequestMetadata, Secret, StreamChunk, ToolChoice,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use wiremock::ResponseTemplate;

pub const KEY: &str = "sk-proj-TEST-KEY-do-not-log-8e4b02";

pub struct Cassette {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

pub fn cassette(name: &str) -> Cassette {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/cassettes").join(format!("{name}.http"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let (head, body) = text.split_once("\n\n").expect("a blank line after the headers");
    let mut lines = head.lines();
    let status = lines.next().and_then(|l| l.strip_prefix("HTTP ")).expect("HTTP <status>").parse().unwrap();
    let headers = lines
        .map(|l| {
            let (k, v) = l.split_once(": ").expect("header line");
            (k.to_owned(), v.to_owned())
        })
        .collect();
    Cassette { status, headers, body: body.to_owned() }
}

impl Cassette {
    pub fn template(&self) -> ResponseTemplate {
        let mut t = ResponseTemplate::new(self.status).set_body_bytes(self.body.clone().into_bytes());
        for (k, v) in &self.headers {
            t = t.insert_header(k.as_str(), v.as_str());
        }
        t
    }
}

pub fn cred(kind: AuthMethodKind) -> Credential {
    Credential {
        account: AccountName("work".into()),
        secret: Secret::new(KEY.to_owned()),
        kind,
        resolved_at: Instant::now(),
    }
}

pub fn request(text: &str) -> ChatRequest {
    ChatRequest {
        model: ModelId("gpt-4.1".into()),
        system: "Be terse.".into(),
        messages: vec![Message { role: MessageRole::User, blocks: vec![aibus_provider::ContentBlock::Text(text.into())], at: SystemTime::UNIX_EPOCH }],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: 1024,
        temperature: None,
        effort: None,
        thinking: None,
        cache_hint: CacheHint::None,
        logprobs: false,
        metadata: RequestMetadata::default(),
    }
}

/// Runs one stream and returns its result with every chunk it sent.
pub async fn collect(
    p: &dyn Provider,
    creds: &Credential,
    req: ChatRequest,
) -> (Result<ChatResponse, ProviderError>, Vec<StreamChunk>) {
    let (tx, mut rx) = mpsc::channel(256);
    let result = p.stream(creds, req, tx, CancellationToken::new()).await;
    let mut chunks = Vec::new();
    while let Ok(c) = rx.try_recv() {
        chunks.push(c);
    }
    (result, chunks)
}
