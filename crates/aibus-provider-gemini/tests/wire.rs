//! What the Gemini adapter puts on the wire and keeps from the answer: the request shape, the
//! thought signature round trip, thinking and usage, the model table and the key's header.
//! Served by the conformance kit's fake server; nothing here reaches Google.

use std::path::PathBuf;
use std::time::{Instant, SystemTime};

use aibus_provider::fake::FAKE_KEY;
use aibus_provider::presets::resolve;
use aibus_provider::{
    AccountName, AliasTable, AuthMethodKind, CacheHint, ChatRequest, ChatResponse, ContentBlock, Credential, Message,
    MessageRole, ModelId, ModelRef, Provider, ProviderConfig, ProviderFactory, RequestMetadata, Secret, StopReason,
    StreamChunk, ThinkingBudget, ToolChoice, ToolSpec,
};
use aibus_provider_gemini::Factory;
use aibus_testkit::conformance::{FakeServer, Fixture};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn cassette(name: &str) -> Fixture {
    Fixture::load(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/cassettes").join(format!("{name}.http")))
}

fn cred() -> Credential {
    Credential {
        account: AccountName("work".into()),
        secret: Secret::new(FAKE_KEY.to_owned()),
        kind: AuthMethodKind::ApiKey,
        resolved_at: Instant::now(),
    }
}

fn request(model: &str, messages: Vec<Message>) -> ChatRequest {
    ChatRequest {
        model: ModelId(model.into()),
        system: "Be terse.".into(),
        messages,
        tools: vec![ToolSpec {
            name: "read_file".into(),
            description: "Read a file".into(),
            schema: json!({
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "additionalProperties": false,
                "properties": {"path": {"type": "string", "default": "."}},
                "required": ["path"]
            }),
        }],
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

fn user(text: &str) -> Message {
    Message { role: MessageRole::User, blocks: vec![ContentBlock::Text(text.into())], at: SystemTime::UNIX_EPOCH }
}

async fn provider(server: &FakeServer) -> std::sync::Arc<dyn Provider> {
    Factory.build(&ProviderConfig { base_url: Some(server.uri()), ..ProviderConfig::default() }).unwrap()
}

async fn collect(p: &dyn Provider, req: ChatRequest) -> (ChatResponse, Vec<StreamChunk>) {
    let (tx, mut rx) = mpsc::channel(256);
    let resp = p.stream(&cred(), req, tx, CancellationToken::new()).await.unwrap();
    let mut chunks = Vec::new();
    while let Ok(c) = rx.try_recv() {
        chunks.push(c);
    }
    (resp, chunks)
}

fn sent(server: &FakeServer, i: usize) -> (String, Value) {
    let r = &server.requests()[i];
    (r.target.clone(), serde_json::from_slice(&r.body).unwrap())
}

#[tokio::test]
async fn the_request_goes_to_the_streaming_path_with_the_key_in_its_header_and_a_clean_schema() {
    let server = FakeServer::start().await;
    server.script(cassette("gm_stream_text"));
    let mut req = request("gemini-2.5-pro", vec![user("hello")]);
    req.thinking = Some(ThinkingBudget { tokens: 2048 });
    req.temperature = Some(0.2);
    collect(provider(&server).await.as_ref(), req).await;

    let (target, body) = sent(&server, 0);
    assert_eq!(target, "/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse");
    let r = &server.requests()[0];
    assert_eq!(r.header("x-goog-api-key"), Some(FAKE_KEY));
    assert!(r.header("authorization").is_none());
    assert_eq!(body["systemInstruction"]["parts"][0]["text"], "Be terse.");
    assert_eq!(body["contents"][0], json!({"role": "user", "parts": [{"text": "hello"}]}));
    assert_eq!(
        body["tools"][0]["functionDeclarations"][0]["parameters"],
        json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]})
    );
    assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "AUTO");
    assert_eq!(
        body["generationConfig"],
        json!({"maxOutputTokens": 1024, "temperature": 0.2, "thinkingConfig": {"thinkingBudget": 2048, "includeThoughts": true}})
    );
}

#[tokio::test]
async fn a_model_that_does_not_think_gets_no_thinking_config() {
    let server = FakeServer::start().await;
    server.script(cassette("gm_stream_text"));
    let mut req = request("gemini-2.0-flash", vec![user("hello")]);
    req.thinking = Some(ThinkingBudget { tokens: 2048 });
    collect(provider(&server).await.as_ref(), req).await;
    assert!(sent(&server, 0).1["generationConfig"].get("thinkingConfig").is_none());
}

/// The round trip the issue asks for: a call that arrived with a `thoughtSignature` is sent
/// back, byte for byte, in the next request, next to the result that answers it.
#[tokio::test]
async fn a_thought_signature_comes_back_in_the_next_request_unchanged() {
    let server = FakeServer::start().await;
    let p = provider(&server).await;
    server.script(cassette("gm_tool_call"));
    let (first, _) = collect(p.as_ref(), request("gemini-2.5-pro", vec![user("read main")])).await;

    let call = first.message.blocks.iter().find_map(|b| match b {
        ContentBlock::ToolUse { id, signature, .. } => Some((*id, signature.clone())),
        _ => None,
    });
    let (id, signature) = call.expect("a tool call");
    let signature = signature.expect("the signature was kept on the block");
    let recorded: Value = serde_json::from_str(
        cassette("gm_tool_call").body_text().lines().find_map(|l| l.strip_prefix("data: ")).unwrap(),
    )
    .unwrap();
    assert_eq!(json!(signature), recorded["candidates"][0]["content"]["parts"][0]["thoughtSignature"]);

    server.script(cassette("gm_stream_text"));
    let result = Message {
        role: MessageRole::User,
        blocks: vec![ContentBlock::ToolResult { id, content: "fn main() {}".into(), is_error: false }],
        at: SystemTime::UNIX_EPOCH,
    };
    collect(p.as_ref(), request("gemini-2.5-pro", vec![user("read main"), first.message, result])).await;

    let (_, body) = sent(&server, 1);
    assert_eq!(body["contents"][1]["role"], "model");
    assert_eq!(
        body["contents"][1]["parts"][0],
        json!({"functionCall": {"name": "read_file", "args": {"path": "src/main.rs"}}, "thoughtSignature": signature})
    );
    assert_eq!(
        body["contents"][2]["parts"][0],
        json!({"functionResponse": {"name": "read_file", "response": {"output": "fn main() {}"}}})
    );
}

#[tokio::test]
async fn thought_summaries_stream_as_thinking_and_the_usage_is_split_the_way_the_issue_says() {
    let server = FakeServer::start().await;
    server.script(cassette("gm_thinking_signature"));
    let (resp, chunks) = collect(provider(&server).await.as_ref(), request("gemini-2.5-pro", vec![user("read main")])).await;

    let thinking: String = chunks.iter().filter_map(|c| if let StreamChunk::ThinkingDelta(t) = c { Some(t.as_str()) } else { None }).collect();
    assert_eq!(thinking, "The user wants a file, so I should call read_file.");
    assert!(matches!(&resp.message.blocks[0], ContentBlock::Thinking { text, redacted: false, .. } if text == &thinking));
    assert!(matches!(&resp.message.blocks[1], ContentBlock::Text(t) if t == "Reading it."));
    assert!(matches!(&resp.message.blocks[2], ContentBlock::ToolUse { signature: Some(_), .. }));
    assert_eq!(resp.stop, StopReason::ToolUse, "STOP with a call in it is a tool-use stop");

    // promptTokenCount 200 minus 50 cached; candidates 12 plus 40 thoughts.
    let u = resp.usage;
    assert_eq!((u.input_tokens, u.cache_read, u.output_tokens, u.reasoning_tokens), (150, 50, 52, Some(40)));
    // gemini-2.5-pro: $1.25 in, $10 out, $0.31 cache read per million.
    let want = (150.0 * 1.25 + 52.0 * 10.0 + 50.0 * 0.31) / 1_000_000.0;
    assert!((u.cost_usd - want).abs() < 1e-12, "{} vs {want}", u.cost_usd);
}

#[tokio::test]
async fn the_models_resolve_by_prefix_and_a_dated_snapshot_has_the_undated_price() {
    let server = FakeServer::start().await;
    let p = provider(&server).await;
    let price = p.price(&ModelId("gemini-2.5-flash".into())).expect("a price");
    assert_eq!((price.input_per_mtok, price.output_per_mtok), (0.3, 2.5));
    assert_eq!(p.price(&ModelId("gemini-2.5-flash-20250617".into())), Some(price));
    assert_eq!(p.price(&ModelId("gemini-2.5-flash-2025-06-17".into())), Some(price));
    assert!(p.models().iter().any(|m| m.id.0 == "gemini-2.5-pro" && m.context_window == 1_048_576 && m.max_output == 65_536));

    let aliases = AliasTable::default();
    let known = |id: &str| p.resolve_model(&ModelRef::Full(ModelId(id.into())), &aliases);
    assert!(known("gemini-9-future").is_some(), "the gemini- prefix resolves a model released after the table");
    assert!(known("gpt-5").is_none());
}

#[tokio::test]
async fn a_refresh_keeps_known_prices_takes_new_sizes_and_drops_what_cannot_generate() {
    let server = FakeServer::start().await;
    server.script(cassette("gm_models_list"));
    let p = provider(&server).await;
    let listed = p.refresh_models(&cred()).await.unwrap();
    let ids: Vec<&str> = listed.iter().map(|m| m.id.0.as_str()).collect();
    assert_eq!(ids, ["gemini-2.5-pro", "gemini-2.5-flash", "gemini-9-preview"], "no `models/` prefix, no embedding model");
    let new = listed.iter().find(|m| m.id.0 == "gemini-9-preview").unwrap();
    assert_eq!((new.context_window, new.max_output), (2_000_000, 100_000));
    assert!(p.price(&ModelId("gemini-9-preview".into())).is_none(), "a new model has no guessed price");
    assert!(p.price(&ModelId("gemini-2.5-pro".into())).is_some());
    assert_eq!(server.requests()[0].target, "/v1beta/models?pageSize=1000");
    assert_eq!(server.requests()[0].header("x-goog-api-key"), Some(FAKE_KEY));
}

#[tokio::test]
async fn count_tokens_posts_the_request_to_count_tokens() {
    let server = FakeServer::start().await;
    server.script(cassette("gm_count_tokens"));
    let p = provider(&server).await;
    let n = p.count_tokens(&cred(), &request("gemini-2.5-pro", vec![user("hello")])).await.unwrap().unwrap();
    assert_eq!(n, 42);
    let (target, body) = sent(&server, 0);
    assert_eq!(target, "/v1beta/models/gemini-2.5-pro:countTokens");
    assert_eq!(body["generateContentRequest"]["model"], "models/gemini-2.5-pro");
    assert_eq!(body["generateContentRequest"]["contents"][0]["parts"][0]["text"], "hello");
}

#[tokio::test]
async fn a_model_id_that_would_leave_the_path_is_refused_before_anything_is_sent() {
    let server = FakeServer::start().await;
    let p = provider(&server).await;
    let (tx, _rx) = mpsc::channel(8);
    let e = p
        .stream(&cred(), request("gemini-2.5-pro/../../x", vec![user("hi")]), tx, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(e.kind, aibus_provider::ProviderErrorKind::InvalidRequest);
    assert!(server.requests().is_empty());
    assert!(!e.message.contains(FAKE_KEY));
}

#[test]
fn the_preset_builds_this_adapter_and_a_user_provider_of_this_api_starts_from_it() {
    let def = resolve("gemini", None).unwrap().unwrap();
    assert_eq!(def.api.map(aibus_provider::Api::as_str), Some("gemini-generate-content"));
    assert_eq!(def.base_url.as_deref(), Some("https://generativelanguage.googleapis.com"));
    assert_eq!(def.env_vars, ["GEMINI_API_KEY"]);
    assert_eq!(Factory.apis(), ["gemini-generate-content"]);

    let user = aibus_provider::ProviderDef {
        id: "gemini-eu".into(),
        api: Some(aibus_provider::Api::GeminiGenerateContent),
        base_url: Some("https://eu.example/".into()),
        ..aibus_provider::ProviderDef::default()
    };
    let mine = resolve("gemini-eu", Some(&user)).unwrap().unwrap();
    assert_eq!(mine.prefixes, ["gemini-"], "started from the gemini preset");
    Factory.build(&ProviderConfig { def: Some(mine), ..ProviderConfig::default() }).unwrap();
}
