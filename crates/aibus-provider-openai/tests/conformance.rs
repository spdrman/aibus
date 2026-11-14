//! The shared provider suite (aibus-testkit, issue #109) against every dialect this crate
//! serves: OpenAI Chat Completions, OpenAI Responses, Kimi and the generic compatible server.
//! Recorded cassettes where there is one, the shared OpenAI error bodies where a dialect has
//! no recording of its own (the HTTP error shape is the same on all of them). Provider-specific
//! wire checks stay in cassettes.rs.

use std::path::PathBuf;
use std::time::Duration;

use aibus_provider::{
    AuthMethodKind, ProviderConfig, ProviderErrorKind::*, ProviderFactory, StopReason,
};
use aibus_provider_openai::Factory;
use aibus_testkit::conformance::{run, Case, Expect, FakeServer, Fixture, Fixtures};
use serde_json::json;

fn load(sub: &str, name: &str, ext: &str) -> Fixture {
    Fixture::load(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join(sub)
            .join(format!("{name}.{ext}")),
    )
}

fn cassette(name: &str) -> Fixture {
    load("cassettes", name, "http")
}

fn case(name: &'static str, fixture: Fixture, expect: Expect) -> Case {
    Case {
        name,
        fixture,
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

fn truncated(kind: aibus_provider::ProviderErrorKind) -> Expect {
    Expect::Fail {
        kind,
        retryable: true,
        retry_after: None,
        delivered: Some("Hello"),
    }
}

/// The HTTP error rows. `p` is the cassette prefix of the dialect that has its own recordings
/// (`oa` or `kimi`); the rows it has none for come from the OpenAI ones.
fn http_errors(p: &str, retry_after: u64) -> Vec<Case> {
    vec![
        case(
            "http_400_invalid",
            cassette("oa_invalid_400"),
            fail(InvalidRequest, false, None),
        ),
        case(
            "http_400_context",
            cassette(&format!("{p}_context_too_long_400")),
            fail(ContextTooLong, false, None),
        ),
        case(
            "http_401",
            cassette(&format!("{p}_auth_401")),
            fail(Auth, false, None),
        ),
        case(
            "http_403",
            cassette("oa_forbidden_403"),
            fail(Auth, false, None),
        ),
        case(
            "http_404",
            cassette(&format!("{p}_not_found_404")),
            fail(ModelNotFound, false, None),
        ),
        case(
            "http_413",
            cassette("oa_too_large_413"),
            fail(ContextTooLong, false, None),
        ),
        case(
            "http_429_retry_after",
            cassette(&format!("{p}_rate_limit_429")),
            fail(RateLimit, true, Some(retry_after)),
        ),
        // OpenAI's quota 429 carries no retry-after, and waiting does not help: it is Quota.
        case(
            "http_429",
            cassette("oa_quota_429"),
            fail(Quota, false, None),
        ),
        case(
            "http_400_content_policy",
            cassette("oa_content_filter_400"),
            fail(ContentFilter, false, None),
        ),
        case(
            "http_500",
            cassette(&format!("{p}_server_500")),
            fail(Server, true, None),
        ),
        case(
            "http_503",
            cassette(&format!("{p}_overloaded_503")),
            fail(Overloaded, true, None),
        ),
        case(
            "http_529",
            cassette("oa_overloaded_529"),
            fail(Overloaded, true, None),
        ),
    ]
}

struct Chat {
    p: &'static str,
    factory: Factory,
    model: &'static str,
    kind: AuthMethodKind,
    text: &'static str,
    text_fixture: &'static str,
    tool_fixture: &'static str,
    tool_input: serde_json::Value,
    tool_fragments: usize,
    retry_after: u64,
    models: (&'static str, usize),
    cancel_after: usize,
}

async fn run_chat(c: Chat) {
    let server = FakeServer::start().await;
    let p = c
        .factory
        .build(&ProviderConfig {
            base_url: Some(server.uri()),
            ..ProviderConfig::default()
        })
        .unwrap();
    let mut cases = http_errors(c.p, c.retry_after);
    cases.extend([
        case(
            "text",
            cassette(c.text_fixture),
            Expect::Stream {
                text: Some(c.text),
                stop: StopReason::EndTurn,
            },
        ),
        case(
            "tool_use",
            cassette(c.tool_fixture),
            Expect::Tools {
                calls: vec![("read_file", c.tool_input)],
                min_fragments: c.tool_fragments,
            },
        ),
        case(
            "parallel_tools",
            cassette("oa_parallel_tools"),
            Expect::Tools {
                calls: vec![
                    ("read_file", json!({"path": "a"})),
                    ("read_file", json!({"path": "b"})),
                ],
                min_fragments: 1,
            },
        ),
        case(
            "max_tokens",
            cassette("oa_length"),
            Expect::Stream {
                text: None,
                stop: StopReason::MaxTokens,
            },
        ),
        case(
            "refusal",
            cassette("oa_content_filter"),
            Expect::Stream {
                text: None,
                stop: StopReason::ContentFilter,
            },
        ),
        case(
            "stream_error",
            cassette("oa_stream_error"),
            fail(Server, true, None),
        ),
        case(
            "malformed_sse",
            cassette(&format!("{}_malformed_sse", c.p)),
            fail(Unknown, false, None),
        ),
        case("truncated", cassette("oa_truncated"), truncated(Network)),
        case(
            "cancel",
            cassette(c.text_fixture).hang_after(c.cancel_after),
            Expect::Cancel,
        ),
        case(
            "models",
            cassette(c.models.0),
            Expect::Models {
                at_least: c.models.1,
            },
        ),
        case(
            "verify",
            cassette(c.models.0),
            Expect::Verify { models: c.models.1 },
        ),
        case("count_tokens", cassette(c.models.0), Expect::Tokens(None)),
    ]);
    let auth = (c.kind != AuthMethodKind::None).then_some(("authorization", "Bearer "));
    run(
        p,
        &server,
        &Fixtures {
            model: c.model,
            kind: c.kind,
            auth,
            cases,
            skipped: vec![],
        },
    )
    .await;
}

#[tokio::test]
async fn openai_chat_passes_the_conformance_suite() {
    run_chat(Chat {
        p: "oa",
        factory: Factory::openai(),
        model: "gpt-4.1",
        kind: AuthMethodKind::ApiKey,
        text: "Hello, world",
        text_fixture: "oa_stream_text",
        tool_fixture: "oa_tool_call",
        tool_input: json!({"path": "src/main.rs"}),
        tool_fragments: 2,
        retry_after: 20,
        models: ("oa_models_list", 3),
        cancel_after: 3,
    })
    .await;
}

#[tokio::test]
async fn kimi_passes_the_conformance_suite() {
    run_chat(Chat {
        p: "kimi",
        factory: Factory::kimi(),
        model: "kimi-k2-thinking",
        kind: AuthMethodKind::ApiKey,
        text: "Hello, world",
        text_fixture: "kimi_stream_text",
        tool_fixture: "kimi_tool_call",
        tool_input: json!({"path": "src/main.rs"}),
        tool_fragments: 1,
        retry_after: 1,
        models: ("kimi_models_list", 3),
        cancel_after: 4,
    })
    .await;
}

/// A local server: no key at all, no `[DONE]`, tool calls that finish with `stop`.
#[tokio::test]
async fn the_compatible_server_passes_the_conformance_suite() {
    run_chat(Chat {
        p: "oa",
        factory: Factory::compatible(),
        model: "llama3.2",
        kind: AuthMethodKind::None,
        text: "Hi there",
        text_fixture: "local_stream_text",
        tool_fixture: "local_tool_call",
        tool_input: json!({"path": "a.txt"}),
        tool_fragments: 1,
        retry_after: 20,
        models: ("local_models_list", 1),
        cancel_after: 2,
    })
    .await;
}

#[tokio::test]
async fn openai_responses_passes_the_conformance_suite() {
    let server = FakeServer::start().await;
    let p = Factory::openai()
        .build(&ProviderConfig {
            base_url: Some(server.uri()),
            api: Some("responses".into()),
            ..ProviderConfig::default()
        })
        .unwrap();
    let mut cases = http_errors("oa", 20);
    cases.extend([
        case(
            "text",
            cassette("rs_stream_text"),
            Expect::Stream {
                text: Some("Hello, world"),
                stop: StopReason::EndTurn,
            },
        ),
        case(
            "tool_use",
            cassette("rs_tool_call"),
            Expect::Tools {
                calls: vec![("read_file", json!({"path": "src/main.rs"}))],
                min_fragments: 2,
            },
        ),
        case(
            "parallel_tools",
            load("fixtures", "rs_parallel_tools", "json"),
            Expect::Tools {
                calls: vec![
                    ("read_file", json!({"path": "a"})),
                    ("read_file", json!({"path": "b"})),
                ],
                min_fragments: 2,
            },
        ),
        case(
            "max_tokens",
            cassette("rs_incomplete"),
            Expect::Stream {
                text: Some("Once"),
                stop: StopReason::MaxTokens,
            },
        ),
        case(
            "refusal",
            load("fixtures", "rs_refusal", "json"),
            Expect::Stream {
                text: None,
                stop: StopReason::ContentFilter,
            },
        ),
        case(
            "stream_error",
            cassette("rs_failed"),
            fail(Server, true, None),
        ),
        case(
            "malformed_sse",
            cassette("rs_malformed_sse"),
            fail(Unknown, false, None),
        ),
        case("truncated", cassette("rs_truncated"), truncated(Network)),
        case(
            "cancel",
            cassette("rs_stream_text").hang_after(5),
            Expect::Cancel,
        ),
        case(
            "models",
            cassette("oa_models_list"),
            Expect::Models { at_least: 3 },
        ),
        case(
            "verify",
            cassette("oa_models_list"),
            Expect::Verify { models: 3 },
        ),
        case(
            "count_tokens",
            cassette("oa_models_list"),
            Expect::Tokens(None),
        ),
    ]);
    let f = Fixtures {
        model: "gpt-5",
        kind: AuthMethodKind::ApiKey,
        auth: Some(("authorization", "Bearer ")),
        cases,
        skipped: vec![],
    };
    run(p, &server, &f).await;
}

/// A connection that dies mid-body, which a recorded .http file cannot say.
#[tokio::test]
async fn a_dropped_connection_is_a_retryable_network_error() {
    let server = FakeServer::start().await;
    let p = Factory::openai()
        .build(&ProviderConfig {
            base_url: Some(server.uri()),
            ..ProviderConfig::default()
        })
        .unwrap();
    let logs = aibus_testkit::capture_logs();
    let c = case(
        "dropped_connection",
        load("fixtures", "dropped", "json"),
        truncated(Network),
    );
    let f = Fixtures {
        model: "gpt-4.1",
        kind: AuthMethodKind::ApiKey,
        auth: Some(("authorization", "Bearer ")),
        cases: vec![],
        skipped: vec![],
    };
    aibus_testkit::conformance::check_case(&p, &server, &f, &c).await;
    drop(logs);
}
