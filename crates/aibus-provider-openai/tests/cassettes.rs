//! The three providers against recorded responses served by a local mock server (10 section 3.1).
//! Nothing here reaches a real API or needs a real key.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use aibus_provider::{
    AuthMethodKind, ConfigError, ContentBlock, Effort, Message, MessageRole, ModelId, ModelRef,
    Provider, ProviderConfig, ProviderErrorKind, ProviderFactory, ProviderRegistry, StopReason,
    StreamChunk, ToolSpec,
};
use aibus_provider_openai::Factory;
use common::*;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer};

async fn server_with(verb: &str, p: &str, name: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method(verb)).and(path(p)).respond_with(cassette(name).template()).mount(&server).await;
    server
}

fn cfg(server: &MockServer) -> ProviderConfig {
    ProviderConfig { base_url: Some(server.uri()), ..ProviderConfig::default() }
}

fn build(f: Factory, server: &MockServer) -> Arc<dyn Provider> {
    f.build(&cfg(server)).unwrap()
}

async fn sent(server: &MockServer) -> wiremock::Request {
    server.received_requests().await.unwrap().remove(0)
}

fn body(r: &wiremock::Request) -> Value {
    serde_json::from_slice(&r.body).unwrap()
}

fn texts(chunks: &[StreamChunk]) -> Vec<&str> {
    chunks.iter().filter_map(|c| if let StreamChunk::TextDelta(t) = c { Some(t.as_str()) } else { None }).collect()
}

fn read_file_tool() -> Vec<ToolSpec> {
    vec![ToolSpec { name: "read_file".into(), description: "Read".into(), schema: json!({"type": "object"}) }]
}

// ---- OpenAI, Chat Completions ----------------------------------------------------------

#[tokio::test]
async fn openai_text_stream_with_cached_prompt_tokens_costs_each_bucket() {
    let server = server_with("POST", "/chat/completions", "oa_stream_text").await;
    let (result, chunks) = collect(&*build(Factory::openai(), &server), &cred(AuthMethodKind::ApiKey), request("hi")).await;
    let resp = result.unwrap();
    assert_eq!(texts(&chunks), ["Hello", ", world"]);
    assert_eq!(resp.message.blocks, [ContentBlock::Text("Hello, world".into())]);
    assert_eq!(resp.stop, StopReason::EndTurn);
    assert_eq!(resp.request_id.as_deref(), Some("req_6f1d0c7a4b8e49c2a1d3e5f7a9b2c4d6"));
    // 125 prompt tokens, 100 of them cached: 25 at the full rate, 100 at the cache rate.
    let u = resp.usage;
    assert_eq!((u.input_tokens, u.cache_read, u.cache_write, u.output_tokens), (25, 100, 0, 15));
    assert!((u.cost_usd - (25.0 * 2.0 + 15.0 * 8.0 + 100.0 * 0.5) / 1e6).abs() < 1e-12);
    let n = chunks.len();
    assert_eq!(chunks[n - 2], StreamChunk::Usage(u));
    assert_eq!(chunks[n - 1], StreamChunk::Done(StopReason::EndTurn));
}

#[tokio::test]
async fn openai_request_has_bearer_auth_user_agent_and_the_right_body() {
    let server = server_with("POST", "/chat/completions", "oa_stream_text").await;
    let mut req = request("hi");
    req.temperature = Some(0.2);
    req.metadata.run_id = Some("RUN".into());
    let _ = collect(&*build(Factory::openai(), &server), &cred(AuthMethodKind::ApiKey), req).await;
    let r = sent(&server).await;
    assert_eq!(r.headers.get("authorization").unwrap().to_str().unwrap(), format!("Bearer {KEY}"));
    assert!(r.headers.get("x-api-key").is_none());
    assert!(r.headers.get("user-agent").unwrap().to_str().unwrap().starts_with("aibus/"));
    let b = body(&r);
    assert_eq!(b["model"], "gpt-4.1");
    assert_eq!(b["stream_options"], json!({"include_usage": true}));
    assert_eq!(b["max_completion_tokens"], 1024);
    assert_eq!(b["user"], "aibus:RUN");
    assert!((b["temperature"].as_f64().unwrap() - 0.2).abs() < 1e-6);
    assert!(!String::from_utf8_lossy(&r.body).contains(KEY));
}

#[tokio::test]
async fn a_reasoning_model_gets_effort_and_no_temperature() {
    let server = server_with("POST", "/chat/completions", "oa_stream_text").await;
    let mut req = request("hi");
    req.model = ModelId("gpt-5".into());
    req.temperature = Some(0.2);
    req.effort = Some(Effort::Low);
    let _ = collect(&*build(Factory::openai(), &server), &cred(AuthMethodKind::ApiKey), req).await;
    let b = body(&sent(&server).await);
    assert_eq!(b["reasoning_effort"], "low");
    assert!(b.get("temperature").is_none());
}

#[tokio::test]
async fn openai_tool_call_streams_start_fragments_and_end() {
    let server = server_with("POST", "/chat/completions", "oa_tool_call").await;
    let mut req = request("read main");
    req.tools = read_file_tool();
    let (result, chunks) = collect(&*build(Factory::openai(), &server), &cred(AuthMethodKind::ApiKey), req).await;
    let resp = result.unwrap();
    let StreamChunk::ToolUseStart { id, name } = chunks.iter().find(|c| matches!(c, StreamChunk::ToolUseStart { .. })).unwrap() else { unreachable!() };
    assert_eq!(name, "read_file");
    let fragments: String = chunks
        .iter()
        .filter_map(|c| match c {
            StreamChunk::ToolUseInputDelta { id: i, json_fragment } if i == id => Some(json_fragment.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(fragments, "{\"path\": \"src/main.rs\"}");
    assert!(chunks.contains(&StreamChunk::ToolUseEnd { id: *id }));
    assert_eq!(resp.stop, StopReason::ToolUse);
    assert_eq!(resp.message.blocks, [ContentBlock::ToolUse { id: *id, name: "read_file".into(), input: json!({"path": "src/main.rs"}) }]);
    let b = body(&sent(&server).await);
    assert_eq!(b["tools"][0]["function"]["name"], "read_file");
    assert_eq!(b["tool_choice"], "auto");
}

#[tokio::test]
async fn parallel_tool_calls_keep_their_own_arguments() {
    let server = server_with("POST", "/chat/completions", "oa_parallel_tools").await;
    let (result, chunks) = collect(&*build(Factory::openai(), &server), &cred(AuthMethodKind::ApiKey), request("x")).await;
    let blocks = result.unwrap().message.blocks;
    let inputs: Vec<_> = blocks.iter().map(|b| if let ContentBlock::ToolUse { input, .. } = b { input.clone() } else { panic!() }).collect();
    assert_eq!(inputs, [json!({"path": "a"}), json!({"path": "b"})]);
    assert_eq!(chunks.iter().filter(|c| matches!(c, StreamChunk::ToolUseEnd { .. })).count(), 2);
}

#[tokio::test]
async fn logprobs_arrive_when_asked_and_the_request_says_so() {
    let server = server_with("POST", "/chat/completions", "oa_logprobs").await;
    let mut req = request("pick");
    req.logprobs = true;
    let (result, chunks) = collect(&*build(Factory::openai(), &server), &cred(AuthMethodKind::ApiKey), req).await;
    result.unwrap();
    let lps: Vec<_> = chunks.iter().filter_map(|c| if let StreamChunk::Logprob { token, logprob } = c { Some((token.as_str(), *logprob)) } else { None }).collect();
    assert_eq!(lps, [("sharp", -0.105), ("!", -1.5)]);
    let b = body(&sent(&server).await);
    assert_eq!((b["logprobs"].clone(), b["top_logprobs"].clone()), (json!(true), json!(5)));
}

#[tokio::test]
async fn kimi_cannot_do_logprobs_so_the_request_drops_them() {
    let server = server_with("POST", "/chat/completions", "kimi_stream_text").await;
    let mut req = request("pick");
    req.model = ModelId("kimi-k2-thinking".into());
    req.logprobs = true;
    let _ = collect(&*build(Factory::kimi(), &server), &cred(AuthMethodKind::ApiKey), req).await;
    assert!(body(&sent(&server).await).get("logprobs").is_none());
}

#[tokio::test]
async fn length_and_content_filter_are_stop_reasons() {
    for (name, stop) in [("oa_length", StopReason::MaxTokens), ("oa_content_filter", StopReason::Refusal)] {
        let server = server_with("POST", "/chat/completions", name).await;
        let (result, _) = collect(&*build(Factory::openai(), &server), &cred(AuthMethodKind::ApiKey), request("x")).await;
        assert_eq!(result.unwrap().stop, stop, "{name}");
    }
}

/// The mapping table of 04 section 2, one row per recorded error, for each provider.
#[tokio::test]
async fn http_errors_map_to_provider_errors() {
    use ProviderErrorKind::*;
    let rows = [
        // factory, model, cassette, kind, retryable, retry_after seconds, request id
        ("openai", "oa_auth_401", Auth, false, None, "req_oa_auth"),
        ("openai", "oa_rate_limit_429", RateLimit, true, Some(20), "req_oa_rate"),
        ("openai", "oa_quota_429", RateLimit, false, None, "req_oa_quota"),
        ("openai", "oa_overloaded_503", Overloaded, true, None, "req_oa_503"),
        ("openai", "oa_server_500", Server, true, None, "req_oa_500"),
        ("openai", "oa_context_too_long_400", ContextTooLong, false, None, "req_oa_ctx"),
        ("openai", "oa_not_found_404", ModelNotFound, false, None, "req_oa_404"),
        ("kimi", "kimi_auth_401", Auth, false, None, "kimi_auth"),
        ("kimi", "kimi_rate_limit_429", RateLimit, true, Some(1), "kimi_rate"),
        ("kimi", "kimi_overloaded_503", Overloaded, true, None, "kimi_503"),
        ("kimi", "kimi_server_500", Server, true, None, "kimi_500"),
        ("kimi", "kimi_context_too_long_400", ContextTooLong, false, None, "kimi_ctx"),
        ("kimi", "kimi_not_found_404", ModelNotFound, false, None, "kimi_404"),
    ];
    for (which, name, kind, retryable, retry_after, id) in rows {
        let server = server_with("POST", "/chat/completions", name).await;
        let f = if which == "kimi" { Factory::kimi() } else { Factory::openai() };
        let (result, chunks) = collect(&*build(f, &server), &cred(AuthMethodKind::ApiKey), request("x")).await;
        let e = result.unwrap_err();
        assert_eq!((e.kind, e.retryable), (kind, retryable), "{name}: {e}");
        assert_eq!(e.retry_after, retry_after.map(Duration::from_secs), "{name}");
        assert_eq!(e.request_id.as_deref(), Some(id), "{name}");
        assert!(!e.message.is_empty() && !e.message.contains(KEY), "{name}");
        assert!(chunks.is_empty(), "{name}");
    }
}

#[tokio::test]
async fn malformed_sse_is_an_error_not_a_panic_for_each_provider() {
    for (f, name) in [(Factory::openai(), "oa_malformed_sse"), (Factory::kimi(), "kimi_malformed_sse")] {
        let server = server_with("POST", "/chat/completions", name).await;
        let (result, chunks) = collect(&*build(f, &server), &cred(AuthMethodKind::ApiKey), request("x")).await;
        let e = result.unwrap_err();
        assert_eq!((e.kind, e.retryable), (ProviderErrorKind::Unknown, false), "{name}");
        assert!(e.message.starts_with("malformed SSE"), "{}", e.message);
        assert!(!chunks.iter().any(|c| matches!(c, StreamChunk::Done(_))), "{name}");
    }
}

#[tokio::test]
async fn a_stream_cut_before_it_finishes_is_a_retryable_network_error() {
    let server = server_with("POST", "/chat/completions", "oa_truncated").await;
    let (result, chunks) = collect(&*build(Factory::openai(), &server), &cred(AuthMethodKind::ApiKey), request("x")).await;
    let e = result.unwrap_err();
    assert_eq!((e.kind, e.retryable), (ProviderErrorKind::Network, true));
    assert_eq!(texts(&chunks), ["Hello"]);
}

#[tokio::test]
async fn an_error_object_mid_stream_maps_like_the_http_error() {
    let server = server_with("POST", "/chat/completions", "oa_stream_error").await;
    let (result, _) = collect(&*build(Factory::openai(), &server), &cred(AuthMethodKind::ApiKey), request("x")).await;
    let e = result.unwrap_err();
    assert_eq!((e.kind, e.retryable), (ProviderErrorKind::Server, true));
}

// ---- OpenAI, Responses -----------------------------------------------------------------

fn responses(server: &MockServer) -> Arc<dyn Provider> {
    Factory::openai()
        .build(&ProviderConfig { base_url: Some(server.uri()), api: Some("responses".into()), ..ProviderConfig::default() })
        .unwrap()
}

#[tokio::test]
async fn the_responses_dialect_streams_text_reasoning_and_logprobs() {
    let server = server_with("POST", "/responses", "rs_stream_text").await;
    let mut req = request("hi");
    req.model = ModelId("gpt-5".into());
    req.logprobs = true;
    req.effort = Some(Effort::High);
    let (result, chunks) = collect(&*responses(&server), &cred(AuthMethodKind::ApiKey), req).await;
    let resp = result.unwrap();
    assert_eq!(texts(&chunks), ["Hello", ", world"]);
    assert!(chunks.contains(&StreamChunk::ThinkingDelta("Greeting the user.".into())));
    assert!(chunks.contains(&StreamChunk::Logprob { token: "Hello".into(), logprob: -0.2 }));
    assert_eq!(resp.stop, StopReason::EndTurn);
    let u = resp.usage;
    assert_eq!((u.input_tokens, u.cache_read, u.output_tokens), (25, 100, 15));
    assert!((u.cost_usd - (25.0 * 1.25 + 15.0 * 10.0 + 100.0 * 0.125) / 1e6).abs() < 1e-12);
    assert_eq!(resp.request_id.as_deref(), Some("req_6f1d0c7a4b8e49c2a1d3e5f7a9b2c4d6"));

    let b = body(&sent(&server).await);
    assert_eq!(b["instructions"], "Be terse.");
    assert_eq!(b["reasoning"], json!({"effort": "high"}));
    assert_eq!(b["store"], false);
    assert_eq!(b["input"][0]["content"][0], json!({"type": "input_text", "text": "hi"}));
}

#[tokio::test]
async fn the_responses_dialect_streams_a_function_call() {
    let server = server_with("POST", "/responses", "rs_tool_call").await;
    let mut req = request("read main");
    req.tools = read_file_tool();
    let (result, chunks) = collect(&*responses(&server), &cred(AuthMethodKind::ApiKey), req).await;
    let resp = result.unwrap();
    let StreamChunk::ToolUseStart { id, name } = chunks.iter().find(|c| matches!(c, StreamChunk::ToolUseStart { .. })).unwrap() else { unreachable!() };
    assert_eq!(name, "read_file");
    assert_eq!(chunks.iter().filter(|c| matches!(c, StreamChunk::ToolUseEnd { .. })).count(), 1);
    assert_eq!(resp.stop, StopReason::ToolUse);
    assert_eq!(resp.message.blocks, [ContentBlock::ToolUse { id: *id, name: "read_file".into(), input: json!({"path": "src/main.rs"}) }]);
    assert_eq!(body(&sent(&server).await)["tools"][0]["name"], "read_file");
}

#[tokio::test]
async fn responses_incomplete_and_failed_map_to_a_stop_reason_and_an_error() {
    let server = server_with("POST", "/responses", "rs_incomplete").await;
    let (result, _) = collect(&*responses(&server), &cred(AuthMethodKind::ApiKey), request("x")).await;
    assert_eq!(result.unwrap().stop, StopReason::MaxTokens);
    let server = server_with("POST", "/responses", "rs_failed").await;
    let e = collect(&*responses(&server), &cred(AuthMethodKind::ApiKey), request("x")).await.0.unwrap_err();
    assert_eq!((e.kind, e.retryable), (ProviderErrorKind::Server, true));
}

#[tokio::test]
async fn responses_errors_use_the_same_http_table() {
    let server = server_with("POST", "/responses", "oa_rate_limit_429").await;
    let e = collect(&*responses(&server), &cred(AuthMethodKind::ApiKey), request("x")).await.0.unwrap_err();
    assert_eq!((e.kind, e.retry_after), (ProviderErrorKind::RateLimit, Some(Duration::from_secs(20))));
}

#[tokio::test]
async fn an_unknown_api_is_a_config_error_and_kimi_has_no_responses() {
    let bad = |f: Factory, api: &str| f.build(&ProviderConfig { api: Some(api.into()), ..ProviderConfig::default() }).err().map(|e| e.to_string());
    assert!(bad(Factory::openai(), "grpc").unwrap().contains("grpc"));
    assert!(bad(Factory::kimi(), "responses").unwrap().contains("kimi"));
    assert!(bad(Factory::openai(), "chat").is_none() && bad(Factory::openai(), "responses").is_none());
}

// ---- Kimi ------------------------------------------------------------------------------

#[tokio::test]
async fn kimi_streams_reasoning_content_as_thinking_and_reads_usage_from_the_choice() {
    let server = server_with("POST", "/chat/completions", "kimi_stream_text").await;
    let mut req = request("hi");
    req.model = ModelId("kimi-k2-thinking".into());
    let (result, chunks) = collect(&*build(Factory::kimi(), &server), &cred(AuthMethodKind::ApiKey), req).await;
    let resp = result.unwrap();
    let thoughts: Vec<_> = chunks.iter().filter_map(|c| if let StreamChunk::ThinkingDelta(t) = c { Some(t.as_str()) } else { None }).collect();
    assert_eq!(thoughts, ["The user says hi.", " Reply briefly."]);
    assert_eq!(texts(&chunks), ["Hello", ", world"]);
    assert_eq!(
        resp.message.blocks[0],
        ContentBlock::Thinking { text: "The user says hi. Reply briefly.".into(), redacted: false, signature: None }
    );
    let u = resp.usage;
    assert_eq!((u.input_tokens, u.cache_read, u.output_tokens), (25, 100, 40));
    assert!((u.cost_usd - (25.0 * 0.6 + 40.0 * 2.5 + 100.0 * 0.15) / 1e6).abs() < 1e-12);

    let b = body(&sent(&server).await);
    assert_eq!(b["max_tokens"], 1024);
    assert!(b.get("max_completion_tokens").is_none());
}

#[tokio::test]
async fn kimi_gets_its_reasoning_back_in_the_next_request() {
    let server = server_with("POST", "/chat/completions", "kimi_stream_text").await;
    let mut req = request("hi");
    req.model = ModelId("kimi-k2-thinking".into());
    req.messages.push(Message {
        role: MessageRole::Assistant,
        blocks: vec![
            ContentBlock::Thinking { text: "mulling".into(), redacted: false, signature: None },
            ContentBlock::Text("answer".into()),
        ],
        at: std::time::SystemTime::UNIX_EPOCH,
    });
    req.messages.push(Message::text(MessageRole::User, "more"));
    let _ = collect(&*build(Factory::kimi(), &server), &cred(AuthMethodKind::ApiKey), req).await;
    assert_eq!(body(&sent(&server).await)["messages"][2]["reasoning_content"], "mulling");
}

#[tokio::test]
async fn kimi_tool_call_ids_are_replaced_by_ours_and_come_back_the_same() {
    let server = server_with("POST", "/chat/completions", "kimi_tool_call").await;
    let mut req = request("x");
    req.model = ModelId("kimi-k2-0905-preview".into());
    req.tools = read_file_tool();
    let (result, _) = collect(&*build(Factory::kimi(), &server), &cred(AuthMethodKind::ApiKey), req.clone()).await;
    let resp = result.unwrap();
    let ContentBlock::ToolUse { id, .. } = resp.message.blocks[0] else { panic!() };
    // The next request carries the call and its result under one wire id.
    req.messages.push(resp.message);
    req.messages.push(Message {
        role: MessageRole::User,
        blocks: vec![ContentBlock::ToolResult { id, content: "fn main() {}".into(), is_error: false }],
        at: std::time::SystemTime::UNIX_EPOCH,
    });
    let server = server_with("POST", "/chat/completions", "kimi_stream_text").await;
    let _ = collect(&*build(Factory::kimi(), &server), &cred(AuthMethodKind::ApiKey), req).await;
    let b = body(&sent(&server).await);
    assert_eq!(b["messages"][2]["tool_calls"][0]["id"], b["messages"][3]["tool_call_id"]);
}

// ---- openai-compatible -----------------------------------------------------------------

#[tokio::test]
async fn compatible_needs_a_base_url_and_is_left_out_of_a_registry_nobody_configured_it_in() {
    let Err(e) = Factory::compatible().build(&ProviderConfig::default()) else { panic!("expected an error") };
    assert!(matches!(e, ConfigError::NotConfigured(ref id, _) if id == "openai-compatible"), "{e}");
    assert!(e.to_string().contains("base_url"));

    let factories: Vec<Box<dyn ProviderFactory>> = vec![Box::new(Factory::openai()), Box::new(Factory::compatible())];
    let none = ProviderRegistry::new(&factories, &BTreeMap::new()).unwrap();
    assert_eq!(none.ids(), ["openai"]);
    // An account that uses it without a base_url is told so.
    let asked = BTreeMap::from([("openai-compatible".to_owned(), ProviderConfig::default())]);
    assert!(ProviderRegistry::new(&factories, &asked).is_err());
    let given = BTreeMap::from([("openai-compatible".to_owned(), ProviderConfig { base_url: Some("http://127.0.0.1:11434/v1".into()), ..ProviderConfig::default() })]);
    assert_eq!(ProviderRegistry::new(&factories, &given).unwrap().ids(), ["openai", "openai-compatible"]);
}

#[tokio::test]
async fn compatible_with_no_auth_sends_no_key_and_copes_with_a_server_that_sends_no_usage_and_no_done() {
    let server = server_with("POST", "/chat/completions", "local_stream_text").await;
    let p = build(Factory::compatible(), &server);
    let mut req = request("hi");
    req.model = ModelId("llama3.2".into());
    let (result, chunks) = collect(&*p, &cred(AuthMethodKind::None), req).await;
    let resp = result.unwrap();
    assert_eq!(texts(&chunks), ["Hi", " there"]);
    assert_eq!(resp.usage.cost_usd, 0.0);
    assert_eq!(chunks.last(), Some(&StreamChunk::Done(StopReason::EndTurn)));
    let r = sent(&server).await;
    assert!(r.headers.get("authorization").is_none());
    let b = body(&r);
    assert_eq!(b["max_tokens"], 1024);
    assert!(b.get("user").is_none() && b.get("max_completion_tokens").is_none());
}

#[tokio::test]
async fn compatible_tool_calls_without_an_index_or_a_tool_calls_finish_reason_still_stop_for_tools() {
    let server = server_with("POST", "/chat/completions", "local_tool_call").await;
    let mut req = request("x");
    req.tools = read_file_tool();
    let (result, _) = collect(&*build(Factory::compatible(), &server), &cred(AuthMethodKind::None), req).await;
    let resp = result.unwrap();
    assert_eq!(resp.stop, StopReason::ToolUse);
    assert!(matches!(&resp.message.blocks[0], ContentBlock::ToolUse { input, .. } if input == &json!({"path": "a.txt"})));
}

#[tokio::test]
async fn compatible_learns_its_models_from_the_server_and_then_resolves_them() {
    let server = server_with("GET", "/models", "local_models_list").await;
    let dir = tempfile::tempdir().unwrap();
    let c = ProviderConfig { cache_dir: Some(dir.path().to_owned()), ..cfg(&server) };
    let p = Factory::compatible().build(&c).unwrap();
    let local = ModelId("llama3.2:latest".into());
    let aliases = [("local".into(), "anything-i-run".into())].into();
    assert_eq!(p.resolve_model(&ModelRef::Full(local.clone()), &aliases), None, "not listed yet");
    assert_eq!(p.resolve_model(&ModelRef::Alias("local".into()), &aliases), Some(ModelId("anything-i-run".into())));

    p.refresh_models(&cred(AuthMethodKind::None)).await.unwrap();
    assert_eq!(p.resolve_model(&ModelRef::Full(local.clone()), &aliases), Some(local.clone()));
    assert_eq!(p.price(&local), None);
    // And the next start knows them from the cache.
    assert_eq!(Factory::compatible().build(&c).unwrap().models().len(), 1);
}

// ---- tables, tokens, verify ------------------------------------------------------------

#[tokio::test]
async fn the_three_providers_have_their_own_models_and_prices() {
    let cfg = ProviderConfig { base_url: Some("http://127.0.0.1:1".into()), ..ProviderConfig::default() };
    let (o, k, c) = (Factory::openai().build(&cfg).unwrap(), Factory::kimi().build(&cfg).unwrap(), Factory::compatible().build(&cfg).unwrap());
    let id = |s: &str| ModelId(s.into());
    assert_eq!(o.price(&id("gpt-5")).unwrap().cache_read_per_mtok, Some(0.125));
    assert_eq!(k.price(&id("gpt-5")), None);
    assert_eq!(k.price(&id("kimi-k2-turbo-preview")).unwrap().output_per_mtok, 8.0);
    assert_eq!(o.price(&id("kimi-k2-turbo-preview")), None);
    assert_eq!(o.price(&id("gpt-4.1-2025-04-14")).unwrap().input_per_mtok, 2.0, "a snapshot uses its model's price");
    assert_eq!(k.price(&id("moonshot-v1-8k")), None, "listed without a price: n/a");
    assert!(c.models().is_empty());
    let size = |p: &Arc<dyn Provider>, m: &str| p.models().into_iter().find(|x| x.id.0 == m).map(|x| (x.context_window, x.max_output, x.family.to_string(), x.deprecated));
    assert_eq!(size(&o, "gpt-4.1"), Some((1_047_576, 32_768, "gpt-4.1".into(), false)));
    assert_eq!(size(&k, "kimi-k2-thinking").unwrap().0, 262_144);
    let none = Default::default();
    assert_eq!(o.resolve_model(&ModelRef::Full(id("gpt-9-future")), &none), Some(id("gpt-9-future")));
    assert_eq!(o.resolve_model(&ModelRef::Full(id("kimi-k2-thinking")), &none), None);
    assert_eq!(k.resolve_model(&ModelRef::Full(id("kimi-k3-preview")), &none), Some(id("kimi-k3-preview")));
}

#[tokio::test]
async fn capabilities_follow_04_per_provider() {
    let cfg = ProviderConfig { base_url: Some("http://127.0.0.1:1".into()), ..ProviderConfig::default() };
    let caps = |f: Factory| f.build(&cfg).unwrap().capabilities();
    let (o, k, c) = (caps(Factory::openai()), caps(Factory::kimi()), caps(Factory::compatible()));
    assert!(o.tools && o.streaming && o.logprobs && o.effort && o.images && o.prompt_cache);
    assert!(k.tools && !k.logprobs && !k.effort && !k.images);
    assert!(c.tools && c.logprobs && !c.prompt_cache && c.max_tools.is_none());
    assert_eq!(o.max_tools, Some(128));
    assert_eq!(
        [Factory::openai().id().0, Factory::kimi().id().0, Factory::compatible().id().0],
        ["openai", "kimi", "openai-compatible"]
    );
    assert!(Factory::compatible().auth_methods().contains(&AuthMethodKind::None));
    assert!(!Factory::openai().auth_methods().contains(&AuthMethodKind::None));
}

#[tokio::test]
async fn there_is_no_counting_endpoint_and_the_estimate_follows_the_family_override() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("openai")).unwrap();
    std::fs::write(dir.path().join("openai/models.json"), r#"{"families": {"gpt-4.1": 2.0}}"#).unwrap();
    let plain = build(Factory::openai(), &server);
    let tuned = Factory::openai().build(&ProviderConfig { overrides_dir: Some(dir.path().to_owned()), ..cfg(&server) }).unwrap();
    assert!(plain.count_tokens(&cred(AuthMethodKind::ApiKey), &request("x")).await.is_none());
    let req = request(&"a".repeat(400));
    // 400 characters and a 9 character system prompt, at 4 and at 2 per token, plus 10 %.
    assert_eq!(plain.estimate_tokens(&req), 113);
    assert_eq!(tuned.estimate_tokens(&req), 225);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn verify_lists_models_and_reports_the_org_and_a_bad_key_is_auth() {
    let server = server_with("GET", "/models", "oa_models_list").await;
    let info = build(Factory::openai(), &server).verify(&cred(AuthMethodKind::ApiKey)).await.unwrap();
    assert_eq!((info.label.as_str(), info.org.as_deref(), info.models.len()), ("work", Some("org-TestOrg"), 3));
    assert_eq!(sent(&server).await.headers.get("authorization").unwrap().to_str().unwrap(), format!("Bearer {KEY}"));

    let server = server_with("GET", "/models", "kimi_auth_401").await;
    let e = build(Factory::kimi(), &server).verify(&cred(AuthMethodKind::ApiKey)).await.unwrap_err();
    assert_eq!((e.kind, e.retryable), (ProviderErrorKind::Auth, false));
}

#[tokio::test]
async fn refresh_keeps_known_prices_and_a_new_model_has_none() {
    let server = server_with("GET", "/models", "oa_models_list").await;
    let p = build(Factory::openai(), &server);
    p.refresh_models(&cred(AuthMethodKind::ApiKey)).await.unwrap();
    assert_eq!(p.price(&ModelId("gpt-5".into())).unwrap().input_per_mtok, 1.25);
    assert_eq!(p.price(&ModelId("gpt-6-preview".into())), None);
    assert!(p.models().iter().any(|m| m.id.0 == "gpt-6-preview"));
}

// ---- cancel ----------------------------------------------------------------------------

/// Waits until the mock server has seen `n` requests, so a cancel is sent while the provider is
/// really waiting on the server and not before it started. No clock bound on the cancel itself:
/// the guard timeout only turns a hang into a failure.
async fn requests_seen(server: &MockServer, n: usize) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while server.received_requests().await.unwrap().len() < n {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the request reached the server");
}

#[tokio::test]
async fn cancel_ends_a_call_waiting_for_the_server() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(cassette("oa_stream_text").template().set_delay(Duration::from_secs(600)))
        .mount(&server)
        .await;
    let p = build(Factory::openai(), &server);
    let cancel = CancellationToken::new();
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    let run = tokio::spawn({
        let cancel = cancel.clone();
        async move { p.stream(&cred(AuthMethodKind::ApiKey), request("x"), tx, cancel).await }
    });
    requests_seen(&server, 1).await;
    cancel.cancel();
    // The server will not answer for ten minutes, so only the cancel can end this.
    let e = tokio::time::timeout(Duration::from_secs(30), run).await.expect("cancel ends the call").unwrap().unwrap_err();
    assert_eq!(e.kind, ProviderErrorKind::Cancelled);
}

#[tokio::test]
async fn cancel_wins_over_a_full_output_channel() {
    let server = server_with("POST", "/chat/completions", "oa_stream_text").await;
    let p = build(Factory::openai(), &server);
    let cancel = CancellationToken::new();
    // Capacity one and nobody reading: the second chunk blocks.
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let watch = tx.clone();
    let run = tokio::spawn({
        let cancel = cancel.clone();
        async move { p.stream(&cred(AuthMethodKind::ApiKey), request("x"), tx, cancel).await }
    });
    tokio::time::timeout(Duration::from_secs(30), async {
        while watch.capacity() > 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the channel filled");
    cancel.cancel();
    let e = tokio::time::timeout(Duration::from_secs(30), run).await.expect("cancel ends the call").unwrap().unwrap_err();
    assert_eq!(e.kind, ProviderErrorKind::Cancelled);
}
