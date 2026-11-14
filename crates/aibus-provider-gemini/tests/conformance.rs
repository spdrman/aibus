//! The shared provider suite (aibus-testkit, issue #109) against the Gemini adapter, on
//! cassettes written by hand from the shapes in Google's public API reference (no live call).
//! Gemini-specific wire checks stay in wire.rs.

use std::path::PathBuf;
use std::time::Duration;

use aibus_provider::{
    AuthMethodKind, ProviderConfig, ProviderErrorKind::*, ProviderFactory, StopReason,
};
use aibus_provider_gemini::Factory;
use aibus_testkit::conformance::{run, Case, Expect, FakeServer, Fixture, Fixtures};
use serde_json::json;

fn cassette(name: &str) -> Fixture {
    Fixture::load(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/cassettes")
            .join(format!("{name}.http")),
    )
}

fn case(name: &'static str, fixture: &str, expect: Expect) -> Case {
    Case {
        name,
        fixture: cassette(fixture),
        expect,
    }
}

fn fail(
    kind: aibus_provider::ProviderErrorKind,
    retryable: bool,
    retry_after: Option<u64>,
) -> Expect {
    Expect::Fail {
        kind,
        retryable,
        retry_after: retry_after.map(Duration::from_secs),
        delivered: None,
    }
}

#[tokio::test]
async fn gemini_passes_the_conformance_suite() {
    let server = FakeServer::start().await;
    let p = Factory
        .build(&ProviderConfig {
            base_url: Some(server.uri()),
            ..ProviderConfig::default()
        })
        .unwrap();
    let cases = vec![
        case(
            "text",
            "gm_stream_text",
            Expect::Stream {
                text: Some("Hello, world"),
                stop: StopReason::EndTurn,
            },
        ),
        case(
            "tool_use",
            "gm_tool_call",
            Expect::Tools {
                calls: vec![("read_file", json!({"path": "src/main.rs"}))],
                min_fragments: 1,
            },
        ),
        case(
            "parallel_tools",
            "gm_parallel_tools",
            Expect::Tools {
                calls: vec![
                    ("read_file", json!({"path": "a"})),
                    ("read_file", json!({"path": "b"})),
                ],
                min_fragments: 1,
            },
        ),
        case(
            "thinking_with_signature",
            "gm_thinking_signature",
            Expect::Tools {
                calls: vec![("read_file", json!({"path": "src/main.rs"}))],
                min_fragments: 1,
            },
        ),
        case(
            "max_tokens",
            "gm_max_tokens",
            Expect::Stream {
                text: Some("Once upon"),
                stop: StopReason::MaxTokens,
            },
        ),
        case(
            "refusal",
            "gm_safety",
            Expect::Stream {
                text: None,
                stop: StopReason::ContentFilter,
            },
        ),
        case(
            "prompt_blocked",
            "gm_prompt_blocked",
            Expect::Stream {
                text: None,
                stop: StopReason::ContentFilter,
            },
        ),
        case(
            "malformed_function_call",
            "gm_malformed_call",
            fail(InvalidRequest, false, None),
        ),
        case(
            "http_400_invalid",
            "gm_invalid_400",
            fail(InvalidRequest, false, None),
        ),
        case(
            "http_400_api_key",
            "gm_api_key_400",
            fail(Auth, false, None),
        ),
        case(
            "http_400_context",
            "gm_context_400",
            fail(ContextTooLong, false, None),
        ),
        case("http_401", "gm_auth_401", fail(Auth, false, None)),
        case("http_403", "gm_forbidden_403", fail(Auth, false, None)),
        case(
            "http_404",
            "gm_not_found_404",
            fail(ModelNotFound, false, None),
        ),
        case(
            "http_413",
            "gm_too_large_413",
            fail(ContextTooLong, false, None),
        ),
        case(
            "http_429_retry_after",
            "gm_rate_limit_429",
            fail(RateLimit, true, Some(33)),
        ),
        case(
            "http_429",
            "gm_rate_limit_no_delay_429",
            fail(RateLimit, true, None),
        ),
        case("http_500", "gm_server_500", fail(Server, true, None)),
        case(
            "http_503",
            "gm_overloaded_503",
            fail(Overloaded, true, None),
        ),
        case("stream_error", "gm_stream_error", fail(Server, true, None)),
        case(
            "malformed_sse",
            "gm_malformed_sse",
            fail(Unknown, false, None),
        ),
        case(
            "truncated",
            "gm_truncated",
            Expect::Fail {
                kind: Network,
                retryable: true,
                retry_after: None,
                delivered: Some("Hello"),
            },
        ),
        Case {
            name: "cancel",
            fixture: cassette("gm_stream_text").hang_after(2),
            expect: Expect::Cancel,
        },
        case("models", "gm_models_list", Expect::Models { at_least: 3 }),
        // The embedding model in the list cannot generate, so it is not the account's.
        case("verify", "gm_models_list", Expect::Verify { models: 3 }),
        case("count_tokens", "gm_count_tokens", Expect::Tokens(Some(42))),
    ];
    let f = Fixtures {
        model: "gemini-2.5-pro",
        kind: AuthMethodKind::ApiKey,
        auth: Some(("x-goog-api-key", "")),
        cases,
        skipped: vec![(
            "http_529",
            "Google never answers 529; an overloaded model is a 503 UNAVAILABLE (http_503)",
        )],
    };
    run(p, &server, &f).await;
}
