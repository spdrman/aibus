//! The shared provider suite (aibus-testkit, issue #109) against the Anthropic adapter, with
//! its recorded cassettes plus the scripted fixtures under `tests/fixtures/`. Provider-specific
//! wire checks (cache breakpoints, thinking signatures, effort headers) stay in cassettes.rs.

use std::path::PathBuf;
use std::time::Duration;

use aibus_provider::{AuthMethodKind, ProviderConfig, ProviderErrorKind::*, ProviderFactory, StopReason};
use aibus_provider_anthropic::Factory;
use aibus_testkit::conformance::{run, Case, Expect, FakeServer, Fixture, Fixtures};
use serde_json::json;

fn load(sub: &str, name: &str, ext: &str) -> Fixture {
    Fixture::load(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join(sub).join(format!("{name}.{ext}")))
}

fn cassette(name: &str) -> Fixture {
    load("cassettes", name, "http")
}

#[tokio::test]
async fn anthropic_passes_the_conformance_suite() {
    let server = FakeServer::start().await;
    let p = Factory.build(&ProviderConfig { base_url: Some(server.uri()), ..ProviderConfig::default() }).unwrap();
    let fail = |kind, retryable, retry_after: Option<u64>| Expect::Fail {
        kind,
        retryable,
        retry_after: retry_after.map(Duration::from_secs),
        delivered: None,
    };
    let case = |name, fixture, expect| Case { name, fixture, expect };
    let read_a_file = json!({"path": "src/main.rs"});

    let fixtures = Fixtures {
        model: "claude-sonnet-4-5",
        kind: AuthMethodKind::ApiKey,
        auth: Some(("x-api-key", "")),
        skipped: vec![],
        cases: vec![
            case("text", cassette("stream_text"), Expect::Stream { text: Some("Hello, world"), stop: StopReason::EndTurn }),
            case("tool_use", cassette("tool_call"), Expect::Tools { calls: vec![("read_file", read_a_file)], min_fragments: 2 }),
            case(
                "parallel_tools",
                load("fixtures", "parallel_tools", "json"),
                Expect::Tools { calls: vec![("read_file", json!({"path": "a"})), ("read_file", json!({"path": "b"}))], min_fragments: 2 },
            ),
            case("max_tokens", cassette("max_tokens"), Expect::Stream { text: None, stop: StopReason::MaxTokens }),
            case("refusal", cassette("refusal"), Expect::Stream { text: None, stop: StopReason::Refusal }),
            case("http_400_invalid", cassette("invalid_400"), fail(InvalidRequest, false, None)),
            case("http_400_context", cassette("context_too_long_400"), fail(ContextTooLong, false, None)),
            case("http_401", cassette("auth_401"), fail(Auth, false, None)),
            case("http_403", cassette("forbidden_403"), fail(Auth, false, None)),
            case("http_404", cassette("not_found_404"), fail(ModelNotFound, false, None)),
            case("http_413", cassette("too_large_413"), fail(ContextTooLong, false, None)),
            case("http_429_retry_after", cassette("rate_limit_429"), fail(RateLimit, true, Some(12))),
            case("http_429", cassette("rate_limit_429_no_retry_after"), fail(RateLimit, true, None)),
            case("http_402_billing", cassette("billing_402"), fail(Quota, false, None)),
            case("http_400_credit_balance", cassette("credit_balance_400"), fail(Quota, false, None)),
            case("http_500", cassette("server_500"), fail(Server, true, None)),
            case("http_503", cassette("overloaded_503"), fail(Overloaded, true, None)),
            case("http_529", cassette("overloaded_529"), fail(Overloaded, true, None)),
            case("stream_error", cassette("stream_error_overloaded"), fail(Overloaded, true, None)),
            case("malformed_sse", cassette("malformed_sse"), fail(Unknown, false, None)),
            case(
                "truncated",
                cassette("truncated"),
                Expect::Fail { kind: Network, retryable: true, retry_after: None, delivered: Some("Hello") },
            ),
            // Not required, but the scripted form earns its keep here: a connection that dies
            // mid-body, which a recorded .http file cannot say.
            case(
                "dropped_connection",
                load("fixtures", "dropped", "json"),
                Expect::Fail { kind: Network, retryable: true, retry_after: None, delivered: Some("Hello") },
            ),
            // Frames up to the first text delta, then silence with the connection open.
            case("cancel", cassette("stream_text").hang_after(4), Expect::Cancel),
            case("models", cassette("models_list"), Expect::Models { at_least: 3 }),
            case("verify", cassette("models_list"), Expect::Verify { models: 3 }),
            case("count_tokens", cassette("count_tokens"), Expect::Tokens(Some(2095))),
        ],
    };
    run(p, &server, &fixtures).await;
}
