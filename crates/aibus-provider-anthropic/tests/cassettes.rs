//! The provider against recorded responses served by a local mock server (10 section 3.1).
//! Nothing here reaches a real API or needs a real key.

mod common;

use std::sync::Arc;
use std::time::Duration;

use aibus_provider::{
    AuthMethodKind, CacheHint, ContentBlock, Effort, ModelId, ModelRef, Provider, ProviderConfig,
    ProviderErrorKind, ProviderFactory, StopReason, StreamChunk, ThinkingBudget, ToolSpec,
};
use aibus_provider_anthropic::Factory;
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

fn provider(server: &MockServer) -> Arc<dyn Provider> {
    Factory
        .build(&ProviderConfig { base_url: Some(server.uri()), ..ProviderConfig::default() })
        .unwrap()
}

async fn sent(server: &MockServer) -> wiremock::Request {
    server.received_requests().await.unwrap().remove(0)
}

fn body(r: &wiremock::Request) -> Value {
    serde_json::from_slice(&r.body).unwrap()
}

#[tokio::test]
async fn a_text_stream_becomes_deltas_usage_and_a_message() {
    let server = server_with("POST", "/v1/messages", "stream_text").await;
    let (result, chunks) = collect(&*provider(&server), &cred(AuthMethodKind::ApiKey), request("hi")).await;
    let resp = result.unwrap();

    let text: Vec<_> = chunks.iter().filter_map(|c| if let StreamChunk::TextDelta(t) = c { Some(t.as_str()) } else { None }).collect();
    assert_eq!(text, ["Hello", ", world"]);
    assert_eq!(resp.message.blocks, [ContentBlock::Text("Hello, world".into())]);
    assert_eq!(resp.stop, StopReason::EndTurn);
    assert_eq!(resp.request_id.as_deref(), Some("req_011CTx2mQ8bZ1fVjL4pWn7Hs"));
    assert_eq!((resp.usage.input_tokens, resp.usage.output_tokens), (25, 15));
    // 25 in at $3 and 15 out at $15 per million tokens.
    assert!((resp.usage.cost_usd - (25.0 * 3.0 + 15.0 * 15.0) / 1e6).abs() < 1e-12);
    // The usage chunk and Done come last, in that order, and agree with the response.
    let n = chunks.len();
    assert_eq!(chunks[n - 2], StreamChunk::Usage(resp.usage));
    assert_eq!(chunks[n - 1], StreamChunk::Done(StopReason::EndTurn));
}

#[tokio::test]
async fn the_request_carries_the_key_version_and_user_agent_and_nothing_secret_in_the_body() {
    let server = server_with("POST", "/v1/messages", "stream_text").await;
    let _ = collect(&*provider(&server), &cred(AuthMethodKind::ApiKey), request("hi")).await;
    let r = sent(&server).await;
    assert_eq!(r.headers.get("x-api-key").unwrap().to_str().unwrap(), KEY);
    assert!(r.headers.get("authorization").is_none());
    assert_eq!(r.headers.get("anthropic-version").unwrap(), "2023-06-01");
    assert!(r.headers.get("user-agent").unwrap().to_str().unwrap().starts_with("aibus/"));
    assert_eq!(r.headers.get("content-type").unwrap(), "application/json");
    assert!(r.headers.get("anthropic-beta").is_none());
    let b = body(&r);
    assert_eq!((b["model"].as_str(), b["stream"].as_bool()), (Some("claude-sonnet-4-5"), Some(true)));
    assert!(!String::from_utf8_lossy(&r.body).contains(KEY));
}

#[tokio::test]
async fn bearer_auth_uses_authorization_and_none_sends_no_key() {
    let server = server_with("POST", "/v1/messages", "stream_text").await;
    let p = provider(&server);
    let _ = collect(&*p, &cred(AuthMethodKind::Bearer), request("a")).await;
    let _ = collect(&*p, &cred(AuthMethodKind::None), request("b")).await;
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs[0].headers.get("authorization").unwrap().to_str().unwrap(), format!("Bearer {KEY}"));
    assert!(reqs[0].headers.get("x-api-key").is_none());
    assert!(reqs[1].headers.get("authorization").is_none() && reqs[1].headers.get("x-api-key").is_none());
}

#[tokio::test]
async fn an_oauth_token_goes_in_authorization_never_in_x_api_key_or_the_body() {
    let server = server_with("POST", "/v1/messages", "stream_text").await;
    let _ = collect(&*provider(&server), &cred(AuthMethodKind::OAuth), request("a")).await;
    let r = sent(&server).await;
    assert_eq!(r.headers.get("authorization").unwrap().to_str().unwrap(), format!("Bearer {KEY}"));
    assert!(r.headers.get("x-api-key").is_none());
    assert!(!String::from_utf8_lossy(&r.body).contains(KEY));
}

#[tokio::test]
async fn a_tool_call_streams_start_input_fragments_and_end_and_costs_the_cache() {
    let server = server_with("POST", "/v1/messages", "tool_call").await;
    let mut req = request("read main");
    req.tools = vec![ToolSpec { name: "read_file".into(), description: "Read".into(), schema: json!({"type": "object"}) }];
    let (result, chunks) = collect(&*provider(&server), &cred(AuthMethodKind::ApiKey), req).await;
    let resp = result.unwrap();

    let starts: Vec<_> = chunks.iter().filter(|c| matches!(c, StreamChunk::ToolUseStart { .. })).collect();
    assert_eq!(starts.len(), 1);
    let StreamChunk::ToolUseStart { id, name } = starts[0] else { unreachable!() };
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
    assert_eq!(
        resp.message.blocks[1],
        ContentBlock::ToolUse { id: *id, name: "read_file".into(), input: json!({"path": "src/main.rs"}), signature: None }
    );
    let u = resp.usage;
    assert_eq!((u.input_tokens, u.output_tokens, u.cache_read, u.cache_write), (1200, 61, 1000, 150));
    let expected = (1200.0 * 3.0 + 61.0 * 15.0 + 1000.0 * 0.3 + 150.0 * 3.75) / 1e6;
    assert!((u.cost_usd - expected).abs() < 1e-12, "{} vs {expected}", u.cost_usd);

    let b = body(&sent(&server).await);
    assert_eq!(b["tools"][0]["input_schema"], json!({"type": "object"}));
}

#[tokio::test]
async fn a_tool_with_no_arguments_gets_an_empty_object() {
    let server = server_with("POST", "/v1/messages", "no_arg_tool").await;
    let (result, _) = collect(&*provider(&server), &cred(AuthMethodKind::ApiKey), request("x")).await;
    let ContentBlock::ToolUse { input, .. } = &result.unwrap().message.blocks[0] else { panic!() };
    assert_eq!(input, &json!({}));
}

#[tokio::test]
async fn thinking_streams_and_keeps_its_signature_for_the_next_request() {
    let server = server_with("POST", "/v1/messages", "thinking").await;
    let mut req = request("think");
    req.thinking = Some(ThinkingBudget { tokens: 2048 });
    req.max_output_tokens = 4096;
    let (result, chunks) = collect(&*provider(&server), &cred(AuthMethodKind::ApiKey), req).await;
    let resp = result.unwrap();
    let thoughts: Vec<_> = chunks.iter().filter_map(|c| if let StreamChunk::ThinkingDelta(t) = c { Some(t.as_str()) } else { None }).collect();
    assert_eq!(thoughts, ["Let me think", " about it."]);
    assert_eq!(
        resp.message.blocks[0],
        ContentBlock::Thinking { text: "Let me think about it.".into(), redacted: false, signature: Some("EqQBCgIYAhIM1gbcDa9GJwZA2b3h".into()) }
    );
    assert_eq!(body(&sent(&server).await)["thinking"], json!({"type": "enabled", "budget_tokens": 2048}));
}

/// The mapping table of 04 section 2, one row per recorded error.
#[tokio::test]
async fn http_errors_map_to_provider_errors() {
    use ProviderErrorKind::*;
    let rows = [
        // cassette, kind, retryable, retry_after seconds, request id
        ("auth_401", Auth, false, None, "req_011CTx2n9aTq7LmB2sRe5YdX"),
        ("rate_limit_429", RateLimit, true, Some(12), "req_011CTx2n1d8rQmZk4Hc9Vw3P"),
        ("overloaded_529", Overloaded, true, None, "req_011CTx2nF3KpWz6Nu1Jh8GcA"),
        ("server_500", Server, true, None, "req_011CTx2nK7MvXe9Rb4Dq2SfT"),
        ("context_too_long_400", ContextTooLong, false, None, "req_011CTx2nQ2HdYa5Cw8Lt6VnE"),
        ("not_found_404", ModelNotFound, false, None, "req_011CTx2nU6BrZf3Gx7Ks1PmW"),
        ("too_large_413", ContextTooLong, false, None, "req_011CTx2nY4NtAg8Hv2Jw5RqD"),
        ("billing_402", Quota, false, None, "req_ant_billing"),
        ("credit_balance_400", Quota, false, None, "req_ant_credit"),
    ];
    for (name, kind, retryable, retry_after, id) in rows {
        let server = server_with("POST", "/v1/messages", name).await;
        let (result, chunks) = collect(&*provider(&server), &cred(AuthMethodKind::ApiKey), request("x")).await;
        let e = result.unwrap_err();
        assert_eq!((e.kind, e.retryable), (kind, retryable), "{name}: {e}");
        assert_eq!(e.retry_after, retry_after.map(Duration::from_secs), "{name}");
        assert_eq!(e.request_id.as_deref(), Some(id), "{name}");
        assert!(!e.message.is_empty() && !e.message.contains(KEY), "{name}");
        assert!(chunks.is_empty(), "{name}: nothing streamed before an HTTP error");
    }
}

/// Every recorded error answer has a row in the table above (the cloud ones are asserted in
/// tests/cloud.rs), so a new cassette cannot land without saying which kind it maps to.
#[test]
fn every_error_cassette_has_an_expected_kind() {
    let known = [
        "auth_401", "rate_limit_429", "overloaded_529", "server_500", "context_too_long_400", "not_found_404",
        "too_large_413", "billing_402", "forbidden_403", "invalid_400", "overloaded_503",
        "rate_limit_429_no_retry_after", "credit_balance_400", "bedrock_denied_403", "vertex_unauthenticated_401",
    ];
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cassettes");
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        let status: u16 = text.lines().next().unwrap().trim_start_matches("HTTP ").parse().unwrap();
        let name = path.file_stem().unwrap().to_str().unwrap();
        assert!(status < 400 || known.contains(&name), "{name} answers {status} but has no row");
    }
}

#[tokio::test]
async fn window_headers_land_in_rate_limit_info_on_a_success() {
    let server = server_with("POST", "/v1/messages", "stream_text_windows").await;
    let (result, _) = collect(&*provider(&server), &cred(AuthMethodKind::ApiKey), request("x")).await;
    let info = result.unwrap().rate_limit.expect("the headers were there");
    assert_eq!((info.requests_remaining, info.tokens_remaining), (Some(0), Some(48_000)));
    assert!(info.resets_in.is_some(), "the exhausted window's reset parses from a timestamp");
    let server = server_with("POST", "/v1/messages", "stream_text").await;
    let (result, _) = collect(&*provider(&server), &cred(AuthMethodKind::ApiKey), request("x")).await;
    assert_eq!(result.unwrap().rate_limit, None);
}

#[tokio::test]
async fn the_providers_own_words_are_the_message() {
    let server = server_with("POST", "/v1/messages", "context_too_long_400").await;
    let (result, _) = collect(&*provider(&server), &cred(AuthMethodKind::ApiKey), request("x")).await;
    assert_eq!(result.unwrap_err().message, "prompt is too long: 213482 tokens > 200000 maximum");
}

#[tokio::test]
async fn a_cache_hint_puts_one_breakpoint_on_the_wire() {
    for (hint, at) in [
        (CacheHint::AfterSystem, "/system/0"),
        (CacheHint::AfterTools, "/tools/0"),
        (CacheHint::LastMessage, "/messages/0/content/0"),
    ] {
        let server = server_with("POST", "/v1/messages", "stream_text").await;
        let mut req = request("hi");
        req.tools = vec![ToolSpec { name: "t".into(), description: "d".into(), schema: json!({}) }];
        req.cache_hint = hint;
        let _ = collect(&*provider(&server), &cred(AuthMethodKind::ApiKey), req).await;
        let b = body(&sent(&server).await);
        assert_eq!(b.pointer(&format!("{at}/cache_control/type")).and_then(Value::as_str), Some("ephemeral"), "{hint:?}");
        assert_eq!(b.to_string().matches("cache_control").count(), 1, "{hint:?}");
    }
}

#[tokio::test]
async fn effort_goes_out_only_for_a_model_that_takes_it() {
    let server = server_with("POST", "/v1/messages", "stream_text").await;
    let p = provider(&server);
    let mut req = request("hi");
    req.effort = Some(Effort::Medium);
    let _ = collect(&*p, &cred(AuthMethodKind::ApiKey), req.clone()).await;
    req.model = ModelId("claude-opus-4-5".into());
    let _ = collect(&*p, &cred(AuthMethodKind::ApiKey), req).await;
    let reqs = server.received_requests().await.unwrap();
    assert!(body(&reqs[0]).get("output_config").is_none() && reqs[0].headers.get("anthropic-beta").is_none());
    assert_eq!(body(&reqs[1])["output_config"], json!({"effort": "medium"}));
    assert_eq!(reqs[1].headers.get("anthropic-beta").unwrap(), "effort-2025-11-24");
}

#[tokio::test]
async fn cancel_wins_over_a_full_output_channel() {
    let server = server_with("POST", "/v1/messages", "stream_text").await;
    let p = provider(&server);
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

#[tokio::test]
async fn count_tokens_asks_the_endpoint() {
    let server = server_with("POST", "/v1/messages/count_tokens", "count_tokens").await;
    let n = provider(&server).count_tokens(&cred(AuthMethodKind::ApiKey), &request("hi")).await.unwrap().unwrap();
    assert_eq!(n, 2095);
    let b = body(&sent(&server).await);
    assert!(b.get("max_tokens").is_none() && b.get("stream").is_none());
    assert_eq!(b["messages"][0]["content"][0]["text"], "hi");
}

#[tokio::test]
async fn verify_lists_models_and_reports_the_org() {
    let server = server_with("GET", "/v1/models", "models_list").await;
    let info = provider(&server).verify(&cred(AuthMethodKind::ApiKey)).await.unwrap();
    assert_eq!(info.label, "work");
    assert_eq!(info.org.as_deref(), Some("3f5a1c52-6c1d-4d1e-9d0e-0b6f5f1a2c77"));
    assert_eq!(info.models.len(), 3);
    assert_eq!(sent(&server).await.headers.get("x-api-key").unwrap().to_str().unwrap(), KEY);
}

#[tokio::test]
async fn verify_with_a_bad_key_is_an_auth_error() {
    let server = server_with("GET", "/v1/models", "auth_401").await;
    let e = provider(&server).verify(&cred(AuthMethodKind::ApiKey)).await.unwrap_err();
    assert_eq!((e.kind, e.retryable), (ProviderErrorKind::Auth, false));
}

#[tokio::test]
async fn refresh_models_keeps_known_prices_marks_new_models_unpriced_and_fills_the_cache() {
    let server = server_with("GET", "/v1/models", "models_list").await;
    let dir = tempfile::tempdir().unwrap();
    let cfg = ProviderConfig { base_url: Some(server.uri()), cache_dir: Some(dir.path().to_owned()), ..ProviderConfig::default() };
    let p = Factory.build(&cfg).unwrap();
    let listed = p.refresh_models(&cred(AuthMethodKind::ApiKey)).await.unwrap();
    assert_eq!(listed.len(), 3);

    let snapshot = ModelId("claude-sonnet-4-5-20250929".into());
    let future = ModelId("claude-future-9".into());
    assert_eq!(p.price(&snapshot).unwrap().input_per_mtok, 3.0, "a dated snapshot has its model's price");
    assert_eq!(p.price(&future), None, "a new model has no price: cost is n/a, not a guess");
    assert!(p.models().iter().any(|m| m.id == future));

    // A provider built later starts from the cache.
    let later = Factory.build(&cfg).unwrap();
    assert!(later.models().iter().any(|m| m.id == future));
    assert!(aibus_provider::models::cache_age(dir.path(), "anthropic", std::time::SystemTime::now()).is_some());
}

#[tokio::test]
async fn a_model_without_a_price_streams_with_cost_zero_and_resolves_by_prefix() {
    let server = server_with("POST", "/v1/messages", "stream_text").await;
    let p = provider(&server);
    let mut req = request("hi");
    req.model = ModelId("claude-future-9".into());
    let resp = collect(&*p, &cred(AuthMethodKind::ApiKey), req).await.0.unwrap();
    assert_eq!(resp.usage.cost_usd, 0.0);
    assert_eq!(resp.usage.input_tokens, 25);
    let aliases = [("big".into(), "claude-opus-4-1".into()), ("odd".into(), "gpt-4".into())].into();
    assert_eq!(p.resolve_model(&ModelRef::Full(ModelId("claude-future-9".into())), &aliases), Some(ModelId("claude-future-9".into())));
    assert_eq!(p.resolve_model(&ModelRef::Alias("big".into()), &aliases), Some(ModelId("claude-opus-4-1".into())));
    assert_eq!(p.resolve_model(&ModelRef::Alias("odd".into()), &aliases), None);
    assert_eq!(p.resolve_model(&ModelRef::Full(ModelId("gpt-5".into())), &aliases), None);
}

#[tokio::test]
async fn capabilities_follow_04() {
    let p = Factory.build(&ProviderConfig::default()).unwrap();
    let c = p.capabilities();
    assert!(c.tools && c.parallel_tool_calls && c.streaming && c.thinking && c.prompt_cache && c.effort && c.images);
    assert!(!c.logprobs);
    assert_eq!(c.system_role, aibus_provider::SystemRole::Separate);
    assert_eq!(Factory.id().0, "anthropic");
    assert!(Factory.auth_methods().contains(&AuthMethodKind::ApiKey));
    assert_eq!(p.models().iter().find(|m| m.id.0 == "claude-sonnet-4-5").unwrap().context_window, 200_000);
    assert!(p.models().iter().any(|m| m.deprecated));
}

#[tokio::test]
async fn a_bad_base_url_is_a_config_error() {
    let Err(e) = Factory.build(&ProviderConfig { base_url: Some("ftp://x".into()), ..ProviderConfig::default() }) else {
        panic!("expected an error");
    };
    assert!(e.to_string().contains("base_url"), "{e}");
}
