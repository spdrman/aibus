//! The fake-key log grep (04 section 7) with the real provider crates in the loop: whatever
//! they log at debug level, on success and on failure, the key is not in it, and the key only
//! ever leaves in the one auth header.

use std::sync::Arc;

use aibus_provider::fake::FAKE_KEY;
use aibus_provider::{
    CacheHint, ChatRequest, Message, MessageRole, ModelId, Provider, ProviderConfig, ProviderFactory,
    RequestMetadata, ToolChoice,
};
use aibus_testkit::{capture_logs, credential, fake_credential};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};

const ANTHROPIC_SSE: &str = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n\n\
event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n\
event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n\
event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";

const OPENAI_SSE: &str = "data: {\"id\":\"c\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n\
data: {\"id\":\"c\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
data: {\"id\":\"c\",\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1}}\n\n\
data: [DONE]\n\n";

fn request(model: &str) -> ChatRequest {
    ChatRequest {
        model: ModelId(model.into()),
        system: "s".into(),
        messages: vec![Message::text(MessageRole::User, "hello")],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: 64,
        temperature: None,
        effort: None,
        thinking: None,
        cache_hint: CacheHint::LastMessage,
        logprobs: false,
        metadata: RequestMetadata { run_id: Some("r".into()), session_id: Some("s".into()) },
    }
}

async fn run(p: Arc<dyn Provider>, model: &str, key: &aibus_provider::Credential) {
    let (tx, mut rx) = mpsc::channel(64);
    let result = p.stream(key, request(model), tx, CancellationToken::new()).await;
    while rx.try_recv().is_ok() {}
    // Success or a mapped error: either way it went through the provider's code paths.
    if let Err(e) = &result {
        assert!(!e.to_string().contains(FAKE_KEY), "{e}");
    }
    let _ = p.count_tokens(key, &request(model)).await;
    let _ = p.verify(key).await;
    let _ = p.refresh_models(key).await;
}

async fn serve(status: u16, body: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(any()).respond_with(ResponseTemplate::new(status).set_body_string(body)).mount(&server).await;
    server
}

fn all_providers(base_url: &str) -> Vec<(Arc<dyn Provider>, &'static str)> {
    let cfg = ProviderConfig { base_url: Some(base_url.to_owned()), ..ProviderConfig::default() };
    vec![
        (aibus_provider_anthropic::Factory.build(&cfg).unwrap(), "claude-sonnet-4-5"),
        (aibus_provider_openai::Factory::openai().build(&cfg).unwrap(), "gpt-4.1"),
        (aibus_provider_openai::Factory::kimi().build(&cfg).unwrap(), "kimi-k2-thinking"),
        (aibus_provider_openai::Factory::compatible().build(&cfg).unwrap(), "local"),
    ]
}

#[tokio::test]
async fn the_fake_key_never_reaches_the_logs_on_success() {
    let logs = capture_logs();
    let account_key = credential("work", "sk-live-account-key-7d1e44");
    logs.forbid("sk-live-account-key-7d1e44");
    for (body, which) in [(ANTHROPIC_SSE, 0), (OPENAI_SSE, 1), (OPENAI_SSE, 2), (OPENAI_SSE, 3)] {
        let server = serve(200, body).await;
        let (p, model) = all_providers(&server.uri()).swap_remove(which);
        run(p.clone(), model, &fake_credential()).await;
        run(p, model, &account_key).await;
        for r in server.received_requests().await.unwrap() {
            let mut sent = String::from_utf8_lossy(&r.body).into_owned();
            sent.push_str(r.url.as_ref());
            assert!(!sent.contains(FAKE_KEY) && !sent.contains("sk-live-account-key"), "the key went into a body or a URL");
            let in_headers: Vec<_> = r
                .headers
                .iter()
                .filter(|(_, v)| v.to_str().is_ok_and(|v| v.contains(FAKE_KEY) || v.contains("sk-live-account-key")))
                .map(|(k, _)| k.as_str().to_owned())
                .collect();
            assert!(in_headers.len() <= 1 && in_headers.iter().all(|h| h == "x-api-key" || h == "authorization"), "{in_headers:?}");
        }
    }
    let text = logs.text();
    // The providers did log (at debug level), so the grep above had something to find.
    assert!(text.contains("anthropic: stream") && text.contains("openai: stream"), "{text}");
    assert!(!text.contains(FAKE_KEY));
}

#[tokio::test]
async fn the_fake_key_never_reaches_the_logs_when_the_server_fails() {
    let logs = capture_logs();
    for (status, body) in [
        (401, r#"{"error":{"message":"bad key","type":"invalid_request_error"}}"#),
        (429, r#"{"error":{"message":"slow down"}}"#),
        (529, "overloaded"),
        (200, "data: {broken\n\n"),
    ] {
        let server = serve(status, body).await;
        for (p, model) in all_providers(&server.uri()) {
            run(p, model, &fake_credential()).await;
        }
    }
    assert!(!logs.text().contains(FAKE_KEY));
}

struct Token;

#[async_trait::async_trait]
impl aibus_provider_anthropic::TokenSource for Token {
    async fn token(&self, _: &aibus_provider::Credential) -> Result<aibus_provider::Secret<String>, aibus_provider::ProviderError> {
        Ok(aibus_provider::Secret::new("ya29.fake-token-5c0e91".to_owned()))
    }
}

/// Bedrock and Vertex build their auth headers from a secret and a token; neither may reach a
/// log or a body, on success or on a server error, and each leaves in its one auth header.
#[tokio::test]
async fn the_bedrock_and_vertex_secrets_never_reach_the_logs() {
    let logs = capture_logs();
    let aws = credential("aws", "AKIDEXAMPLE:wJalrXUtnFEMI-secret-half-7d1e44");
    for (status, body) in [(200, ANTHROPIC_SSE), (403, r#"{"message":"denied"}"#)] {
        let server = serve(status, body).await;
        let cfg = ProviderConfig { base_url: Some(server.uri()), ..ProviderConfig::default() };
        let bedrock = aibus_provider_anthropic::Anthropic::bedrock(&cfg, "us-east-1", std::time::SystemTime::now).unwrap();
        let vertex = aibus_provider_anthropic::Anthropic::vertex(&cfg, "proj", "us-east5", Arc::new(Token)).unwrap();
        run(Arc::new(bedrock), "claude-sonnet-4-5", &aws).await;
        run(Arc::new(vertex), "claude-sonnet-4-5", &fake_credential()).await;
        for r in server.received_requests().await.unwrap() {
            let mut sent = String::from_utf8_lossy(&r.body).into_owned();
            sent.push_str(r.url.as_ref());
            assert!(!sent.contains("secret-half") && !sent.contains("ya29."), "a secret went into a body or a URL");
            let in_headers: Vec<_> = r
                .headers
                .iter()
                .filter(|(_, v)| v.to_str().is_ok_and(|v| v.contains("secret-half") || v.contains("ya29.")))
                .map(|(k, _)| k.as_str().to_owned())
                .collect();
            assert!(in_headers.iter().all(|h| h == "authorization"), "{in_headers:?}");
        }
    }
    let text = logs.text();
    assert!(text.contains("anthropic: stream"), "{text}");
    assert!(!text.contains("secret-half") && !text.contains("ya29.") && !text.contains(FAKE_KEY));
}
