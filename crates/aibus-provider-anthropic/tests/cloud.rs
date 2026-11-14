//! Claude over Bedrock (sigv4) and Vertex (gcloud) against recorded-shape cassettes served by a
//! local mock server. No cloud credentials, no real network.
//!
//! A Bedrock cassette keeps its body as one Messages API event per line, the readable form of
//! what Bedrock sends; `bedrock_template` frames each line the way the service does.

mod common;

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aibus_provider::{
    AccountName, AuthMethodKind, ContentBlock, Credential, ModelId, Provider, ProviderConfig,
    ProviderError, ProviderErrorKind, ProviderFactory, Secret, StopReason, StreamChunk,
};
use aibus_provider_anthropic::{Anthropic, CloudFactory, TokenSource};
use async_trait::async_trait;
use base64::Engine;
use common::*;
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A made-up AWS key pair in the one-string form `ACCESS_KEY_ID:SECRET_ACCESS_KEY`.
const AWS_SECRET: &str = "AKIDEXAMPLE:wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
const VERTEX_TOKEN: &str = "ya29.fake-gcloud-access-token-do-not-log-8a41";

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// One `application/vnd.amazon.eventstream` frame with string headers.
fn eventstream_frame(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
    let mut h = Vec::new();
    for (k, v) in headers {
        h.push(k.len() as u8);
        h.extend_from_slice(k.as_bytes());
        h.push(7);
        h.extend_from_slice(&(v.len() as u16).to_be_bytes());
        h.extend_from_slice(v.as_bytes());
    }
    let mut f = ((16 + h.len() + payload.len()) as u32)
        .to_be_bytes()
        .to_vec();
    f.extend_from_slice(&(h.len() as u32).to_be_bytes());
    f.extend_from_slice(&crc32(&f).to_be_bytes());
    f.extend_from_slice(&h);
    f.extend_from_slice(payload);
    f.extend_from_slice(&crc32(&f).to_be_bytes());
    f
}

/// Each line of the cassette goes out as a `chunk` frame holding `{"bytes": base64(line)}`.
fn bedrock_template(c: &Cassette) -> ResponseTemplate {
    let mut body = Vec::new();
    for line in c.body.lines().filter(|l| !l.is_empty()) {
        let b64 = base64::engine::general_purpose::STANDARD.encode(line);
        body.extend(eventstream_frame(
            &[
                (":event-type", "chunk"),
                (":content-type", "application/json"),
                (":message-type", "event"),
            ],
            format!(r#"{{"bytes":"{b64}"}}"#).as_bytes(),
        ));
    }
    let mut t = ResponseTemplate::new(c.status).set_body_bytes(body);
    for (k, v) in &c.headers {
        t = t.insert_header(k.as_str(), v.as_str());
    }
    t
}

fn cfg(server: &MockServer) -> ProviderConfig {
    ProviderConfig {
        base_url: Some(server.uri()),
        ..ProviderConfig::default()
    }
}

fn aws_cred(secret: &str) -> Credential {
    Credential {
        account: AccountName("bedrock".into()),
        secret: Secret::new(secret.to_owned()),
        kind: AuthMethodKind::Env,
        resolved_at: std::time::Instant::now(),
    }
}

/// 2015-08-30T12:36:00Z, the date of AWS's own examples.
fn fixed_clock() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_440_938_160)
}

fn bedrock(server: &MockServer) -> Anthropic {
    Anthropic::bedrock(&cfg(server), "us-east-1", fixed_clock).unwrap()
}

struct Token(Result<&'static str, &'static str>);

#[async_trait]
impl TokenSource for Token {
    async fn token(&self, _: &Credential) -> Result<Secret<String>, ProviderError> {
        self.0
            .map(|t| Secret::new(t.to_owned()))
            .map_err(|m| ProviderError::new(ProviderErrorKind::Auth, m))
    }
}

fn vertex(server: &MockServer, tokens: Token) -> Anthropic {
    Anthropic::vertex(&cfg(server), "my-project", "us-east5", Arc::new(tokens)).unwrap()
}

fn text_of(chunks: &[StreamChunk]) -> Vec<&str> {
    chunks
        .iter()
        .filter_map(|c| {
            if let StreamChunk::TextDelta(t) = c {
                Some(t.as_str())
            } else {
                None
            }
        })
        .collect()
}

#[tokio::test]
async fn bedrock_streams_a_signed_request_and_decodes_the_event_stream() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/model/claude-sonnet-4-5/invoke-with-response-stream"))
        .respond_with(bedrock_template(&cassette("bedrock_stream")))
        .mount(&server)
        .await;
    let (result, chunks) = collect(&bedrock(&server), &aws_cred(AWS_SECRET), request("hi")).await;
    let resp = result.unwrap();
    assert_eq!(text_of(&chunks), ["Hello", ", world"]);
    assert_eq!(
        resp.message.blocks,
        [ContentBlock::Text("Hello, world".into())]
    );
    assert_eq!(resp.stop, StopReason::EndTurn);
    assert_eq!(
        resp.request_id.as_deref(),
        Some("6f1e7a52-3b0c-4d58-9a41-2c7d0e5b8f13")
    );
    assert_eq!(
        (resp.usage.input_tokens, resp.usage.output_tokens),
        (25, 15)
    );

    let r = server.received_requests().await.unwrap().remove(0);
    let auth = r.headers.get("authorization").unwrap().to_str().unwrap();
    assert!(
        auth.starts_with(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/bedrock/aws4_request, \
             SignedHeaders=content-type;host;x-amz-date, Signature="
        ),
        "{auth}"
    );
    assert_eq!(r.headers.get("x-amz-date").unwrap(), "20150830T123600Z");
    assert!(r.headers.get("x-api-key").is_none() && r.headers.get("anthropic-version").is_none());
    assert!(r
        .headers
        .get("user-agent")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("aibus/"));
    // The model moved into the URL and the stream flag into the endpoint's name.
    let b: Value = serde_json::from_slice(&r.body).unwrap();
    assert!(b.get("model").is_none() && b.get("stream").is_none());
    assert_eq!(b["anthropic_version"], "bedrock-2023-05-31");
    assert_eq!(b["max_tokens"], 1024);
    // The secret half of the key is in the signature's input only, never on the wire.
    let wire = format!("{:?}{}", r.headers, String::from_utf8_lossy(&r.body));
    assert!(!wire.contains("wJalrXUtnFEMI"), "{wire}");
}

#[tokio::test]
async fn bedrock_signs_the_same_request_the_same_way_and_a_new_time_changes_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(bedrock_template(&cassette("bedrock_stream")))
        .mount(&server)
        .await;
    for secs in [1_440_938_160, 1_440_938_160, 1_440_938_161] {
        let p = Anthropic::bedrock(&cfg(&server), "us-east-1", move || {
            UNIX_EPOCH + Duration::from_secs(secs)
        })
        .unwrap();
        let _ = collect(&p, &aws_cred(AWS_SECRET), request("hi")).await;
    }
    let sigs: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| {
            r.headers
                .get("authorization")
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(sigs[0], sigs[1]);
    assert_ne!(sigs[0], sigs[2]);
}

#[tokio::test]
async fn a_session_token_is_sent_and_signed_and_a_colon_in_the_model_id_is_encoded() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(bedrock_template(&cassette("bedrock_stream")))
        .mount(&server)
        .await;
    let mut req = request("hi");
    req.model = ModelId("anthropic.claude-sonnet-4-5-20250929-v1:0".into());
    let secret = format!("{AWS_SECRET}:FwoGZXIvYXdzEXAMPLETOKEN");
    let (result, _) = collect(&bedrock(&server), &aws_cred(&secret), req).await;
    result.unwrap();
    let r = server.received_requests().await.unwrap().remove(0);
    assert_eq!(
        r.url.path(),
        "/model/anthropic.claude-sonnet-4-5-20250929-v1%3A0/invoke-with-response-stream"
    );
    assert_eq!(
        r.headers.get("x-amz-security-token").unwrap(),
        "FwoGZXIvYXdzEXAMPLETOKEN"
    );
    let auth = r.headers.get("authorization").unwrap().to_str().unwrap();
    assert!(
        auth.contains("SignedHeaders=content-type;host;x-amz-date;x-amz-security-token,"),
        "{auth}"
    );
}

#[tokio::test]
async fn bedrock_errors_keep_the_providers_message_and_never_the_secret() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(cassette("bedrock_denied_403").template())
        .mount(&server)
        .await;
    let (result, _) = collect(&bedrock(&server), &aws_cred(AWS_SECRET), request("hi")).await;
    let e = result.unwrap_err();
    assert_eq!(e.kind, ProviderErrorKind::Auth);
    assert!(
        e.message
            .contains("not authorized to perform: bedrock:InvokeModelWithResponseStream"),
        "{}",
        e.message
    );
    assert_eq!(
        e.request_id.as_deref(),
        Some("0d9c4b1e-77a3-4f0a-8e26-5b13a9c2d7f4")
    );
    assert!(!format!("{e:?}").contains("wJalrXUtnFEMI"));
}

#[tokio::test]
async fn an_exception_frame_mid_stream_is_a_rate_limit_error() {
    let server = MockServer::start().await;
    let frame = eventstream_frame(
        &[
            (":exception-type", "throttlingException"),
            (":message-type", "exception"),
        ],
        br#"{"message":"Too many requests, please wait before trying again."}"#,
    );
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(frame))
        .mount(&server)
        .await;
    let (result, _) = collect(&bedrock(&server), &aws_cred(AWS_SECRET), request("hi")).await;
    let e = result.unwrap_err();
    assert_eq!((e.kind, e.retryable), (ProviderErrorKind::RateLimit, true));
}

#[tokio::test]
async fn a_malformed_sigv4_secret_fails_before_any_request() {
    let server = MockServer::start().await;
    let (result, _) = collect(&bedrock(&server), &aws_cred("SECRETVALUE:"), request("hi")).await;
    let e = result.unwrap_err();
    assert_eq!(e.kind, ProviderErrorKind::Auth);
    assert!(!e.message.contains("SECRETVALUE"));
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(bedrock(&server).verify(&aws_cred("nope:")).await.is_err());
}

#[tokio::test]
async fn vertex_takes_its_token_from_the_source_and_posts_to_stream_raw_predict() {
    let server = MockServer::start().await;
    let p = "/v1/projects/my-project/locations/us-east5/publishers/anthropic/models/claude-sonnet-4-5@20250929:streamRawPredict";
    Mock::given(method("POST"))
        .and(path(p))
        .respond_with(cassette("vertex_stream").template())
        .mount(&server)
        .await;
    let mut req = request("hi");
    req.model = ModelId("claude-sonnet-4-5@20250929".into());
    // The account's own secret is not what Vertex is sent: the token source decides.
    let (result, chunks) = collect(
        &vertex(&server, Token(Ok(VERTEX_TOKEN))),
        &cred(AuthMethodKind::Env),
        req,
    )
    .await;
    let resp = result.unwrap();
    assert_eq!(text_of(&chunks), ["Hello", ", world"]);
    assert_eq!(
        resp.request_id.as_deref(),
        Some("8c2f6d0a4b7e19f35d6a0c1e72b49f58")
    );

    let r = server.received_requests().await.unwrap().remove(0);
    assert_eq!(
        r.headers.get("authorization").unwrap().to_str().unwrap(),
        format!("Bearer {VERTEX_TOKEN}")
    );
    assert!(r.headers.get("x-api-key").is_none() && r.headers.get("anthropic-version").is_none());
    let b: Value = serde_json::from_slice(&r.body).unwrap();
    assert!(b.get("model").is_none());
    assert_eq!(
        (b["anthropic_version"].as_str(), b["stream"].as_bool()),
        (Some("vertex-2023-10-16"), Some(true))
    );
    let body = String::from_utf8_lossy(&r.body);
    assert!(!body.contains(VERTEX_TOKEN) && !body.contains(KEY));
}

#[tokio::test]
async fn vertex_errors_map_by_status_and_a_failing_token_source_stops_the_call() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(cassette("vertex_unauthenticated_401").template())
        .mount(&server)
        .await;
    let (result, _) = collect(
        &vertex(&server, Token(Ok(VERTEX_TOKEN))),
        &cred(AuthMethodKind::None),
        request("hi"),
    )
    .await;
    let e = result.unwrap_err();
    assert_eq!(e.kind, ProviderErrorKind::Auth);
    assert!(
        e.message
            .starts_with("Request had invalid authentication credentials"),
        "{}",
        e.message
    );
    assert!(!e.message.contains(VERTEX_TOKEN));

    let p = vertex(&server, Token(Err("gcloud: not logged in")));
    let before = server.received_requests().await.unwrap().len();
    let (result, _) = collect(&p, &cred(AuthMethodKind::None), request("hi")).await;
    assert_eq!(result.unwrap_err().message, "gcloud: not logged in");
    assert_eq!(server.received_requests().await.unwrap().len(), before);
    assert!(p.verify(&cred(AuthMethodKind::None)).await.is_err());
}

#[tokio::test]
async fn verify_makes_one_real_request_on_the_smallest_model_and_reports_its_cost() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(bedrock_template(&cassette("bedrock_stream")))
        .mount(&server)
        .await;
    let b = bedrock(&server);
    assert!(b
        .count_tokens(&aws_cred(AWS_SECRET), &request("hi"))
        .await
        .is_none());
    let info = b.verify(&aws_cred(AWS_SECRET)).await.unwrap();
    assert!(info.models.iter().any(|m| m.0.contains("claude-")));
    let seen = server.received_requests().await.unwrap();
    assert_eq!(seen.len(), 1, "one request, no more");
    let body: Value = serde_json::from_slice(&seen[0].body).unwrap();
    assert_eq!(body["max_tokens"], 1);
    assert!(
        seen[0].url.path().contains("haiku"),
        "the smallest model: {}",
        seen[0].url.path()
    );
    // 25 input and 15 output tokens at the plain haiku price, found through the profile's `price_of`.
    let cost = info.cost_usd.expect("a cost, not n/a");
    assert!((cost - 0.0001).abs() < 1e-9, "{cost}");

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(cassette("vertex_stream").template())
        .mount(&server)
        .await;
    let v = vertex(&server, Token(Ok(VERTEX_TOKEN)));
    assert!(v
        .count_tokens(&cred(AuthMethodKind::None), &request("hi"))
        .await
        .is_none());
    let info = v.verify(&cred(AuthMethodKind::None)).await.unwrap();
    assert!(info.cost_usd.is_some());
    let seen = server.received_requests().await.unwrap();
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0]
            .url
            .path()
            .ends_with("/models/claude-haiku-4-5@20251001:streamRawPredict"),
        "{}",
        seen[0].url.path()
    );
}

#[tokio::test]
async fn verify_fails_with_the_providers_error_and_no_secret_when_the_request_is_refused() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(cassette("bedrock_denied_403").template())
        .mount(&server)
        .await;
    let e = bedrock(&server)
        .verify(&aws_cred(AWS_SECRET))
        .await
        .unwrap_err();
    assert_eq!(e.kind, ProviderErrorKind::Auth);
    assert!(!format!("{e:?}").contains("wJalrXUtnFEMI"));
}

/// The token source is asked on behalf of the calling account, so a cache can key on it.
struct Spy(std::sync::Mutex<Vec<String>>);

#[async_trait]
impl TokenSource for Spy {
    async fn token(&self, creds: &Credential) -> Result<Secret<String>, ProviderError> {
        self.0.lock().unwrap().push(creds.account.to_string());
        Ok(Secret::new(VERTEX_TOKEN.to_owned()))
    }
}

#[tokio::test]
async fn the_cloud_factory_builds_both_from_a_provider_config_and_asks_the_token_source_for_the_account(
) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(cassette("vertex_stream").template())
        .mount(&server)
        .await;
    let spy = Arc::new(Spy(Default::default()));
    let factory = CloudFactory {
        tokens: spy.clone(),
    };
    assert_eq!(factory.apis(), ["anthropic-bedrock", "anthropic-vertex"]);

    let mut def = aibus_provider::presets::preset("vertex").unwrap().clone();
    def.base_url = Some(server.uri());
    def.project = Some("my-project".into());
    def.region = Some("us-east5".into());
    let provider = factory
        .build(&ProviderConfig {
            def: Some(def),
            ..ProviderConfig::default()
        })
        .unwrap();
    assert_eq!(provider.id().0, "vertex");
    let (result, _) = collect(&*provider, &cred(AuthMethodKind::None), request("hi")).await;
    result.unwrap();
    assert_eq!(*spy.0.lock().unwrap(), ["work"]);
    let seen = server.received_requests().await.unwrap();
    assert!(seen[0]
        .url
        .path()
        .starts_with("/v1/projects/my-project/locations/us-east5/publishers/anthropic/models/"));

    let mut def = aibus_provider::presets::preset("bedrock").unwrap().clone();
    def.base_url = Some(server.uri());
    def.region = Some("eu-west-1".into());
    assert_eq!(
        factory
            .build(&ProviderConfig {
                def: Some(def),
                ..ProviderConfig::default()
            })
            .unwrap()
            .id()
            .0,
        "bedrock"
    );
}

#[test]
fn the_cloud_factory_needs_a_definition_a_region_and_for_vertex_a_project() {
    let factory = CloudFactory {
        tokens: Arc::new(Token(Ok("t"))),
    };
    // Built as itself with nothing to go on: left out of the registry, not an error for everyone else.
    assert!(matches!(
        factory.build(&ProviderConfig::default()),
        Err(aibus_provider::ConfigError::NotConfigured(..))
    ));
    let preset = |id: &str| aibus_provider::presets::preset(id).unwrap().clone();
    let no_region = ProviderConfig {
        def: Some(preset("bedrock")),
        ..ProviderConfig::default()
    };
    assert!(factory.build(&no_region).is_err());
    let mut v = preset("vertex");
    v.region = Some("us-east5".into());
    assert!(
        factory
            .build(&ProviderConfig {
                def: Some(v),
                ..ProviderConfig::default()
            })
            .is_err(),
        "no project"
    );
}

#[tokio::test]
async fn a_bearer_token_without_a_colon_is_sent_as_a_bearer_and_not_signed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(bedrock_template(&cassette("bedrock_stream")))
        .mount(&server)
        .await;
    let token = "ABSKQmVkcm9ja0FQSUtleS1leGFtcGxl";
    let (result, _) = collect(&bedrock(&server), &aws_cred(token), request("hi")).await;
    result.unwrap();
    let r = server.received_requests().await.unwrap().remove(0);
    assert_eq!(
        r.headers.get("authorization").unwrap().to_str().unwrap(),
        format!("Bearer {token}")
    );
    assert!(r.headers.get("x-amz-date").is_none());
}

#[test]
fn bad_regions_and_projects_are_config_errors() {
    let c = ProviderConfig::default();
    assert!(Anthropic::bedrock(&c, "us-east-1.evil.example", SystemTime::now).is_err());
    assert!(Anthropic::bedrock(&c, "", SystemTime::now).is_err());
    assert!(Anthropic::vertex(&c, "p/../x", "us-east5", Arc::new(Token(Ok("t")))).is_err());
    assert!(Anthropic::vertex(&c, "my-project", "global", Arc::new(Token(Ok("t")))).is_ok());
}

// ---- the recorded cassettes through the conformance kit ----------------------------------------

use std::path::PathBuf;

use aibus_provider::ProviderErrorKind::*;
use aibus_testkit::conformance::{run, Body, Case, Expect, FakeServer, Fixture, Fixtures};
use serde_json::json;

fn load(sub: &str, name: &str, ext: &str) -> Fixture {
    Fixture::load(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join(sub)
            .join(format!("{name}.{ext}")),
    )
}

/// A stream the kit plays to Bedrock: the Messages events of an SSE fixture (its `data:` lines,
/// the same JSON Bedrock puts in a `chunk`) framed as `application/vnd.amazon.eventstream`,
/// the way `bedrock_template` does for a cassette. The fixture's end (close, drop, hang) stays.
fn bedrock_fixture(sse: Fixture) -> Fixture {
    let events: Vec<String> = sse
        .body_text()
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .map(str::to_owned)
        .collect();
    assert!(!events.is_empty(), "no data lines to frame");
    let mut f = sse;
    f.headers = vec![(
        "content-type".into(),
        "application/vnd.amazon.eventstream".into(),
    )];
    f.body = Body::Bytes(
        events
            .iter()
            .flat_map(|line| {
                let b64 = base64::engine::general_purpose::STANDARD.encode(line);
                eventstream_frame(
                    &[
                        (":event-type", "chunk"),
                        (":content-type", "application/json"),
                        (":message-type", "event"),
                    ],
                    format!(r#"{{"bytes":"{b64}"}}"#).as_bytes(),
                )
            })
            .collect(),
    );
    f
}

/// A Bedrock cassette that already keeps one event per line (`bedrock_stream.http`).
fn bedrock_cassette(c: &Cassette) -> Fixture {
    let sse: String = c
        .body
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| format!("data: {l}\n\n"))
        .collect();
    bedrock_fixture(Fixture::http(c.status, &[], &sse))
}

fn from_preset(id: &str, server: &FakeServer, tokens: Arc<dyn TokenSource>) -> Arc<dyn Provider> {
    let mut def = aibus_provider::presets::preset(id).unwrap().clone();
    def.base_url = Some(server.uri());
    def.region = Some("us-east5".into());
    def.project = Some("my-project".into());
    CloudFactory { tokens }
        .build(&ProviderConfig {
            def: Some(def),
            ..ProviderConfig::default()
        })
        .unwrap()
}

/// The cases both cloud adapters share. `stream` turns an SSE fixture into what the adapter's
/// wire carries (Vertex: itself, Bedrock: event stream frames). Error bodies are plain HTTP on
/// both, so they pass through as they are.
fn cloud_cases(
    stream: fn(Fixture) -> Fixture,
    text: Fixture,
    auth_401: Fixture,
    forbidden_403: Fixture,
) -> Vec<Case> {
    let fail = |kind, retryable, retry_after: Option<u64>| Expect::Fail {
        kind,
        retryable,
        retry_after: retry_after.map(Duration::from_secs),
        delivered: None,
    };
    let case = |name, fixture, expect| Case {
        name,
        fixture,
        expect,
    };
    let sse = |name: &str| load("cassettes", name, "http");
    vec![
        case(
            "text",
            text.clone(),
            Expect::Stream {
                text: Some("Hello, world"),
                stop: StopReason::EndTurn,
            },
        ),
        case(
            "tool_use",
            stream(sse("tool_call")),
            Expect::Tools {
                calls: vec![("read_file", json!({"path": "src/main.rs"}))],
                min_fragments: 2,
            },
        ),
        case(
            "parallel_tools",
            stream(load("fixtures", "parallel_tools", "json")),
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
            stream(sse("max_tokens")),
            Expect::Stream {
                text: None,
                stop: StopReason::MaxTokens,
            },
        ),
        case(
            "refusal",
            stream(sse("refusal")),
            Expect::Stream {
                text: None,
                stop: StopReason::Refusal,
            },
        ),
        case(
            "http_400_invalid",
            sse("invalid_400"),
            fail(InvalidRequest, false, None),
        ),
        case(
            "http_400_context",
            sse("context_too_long_400"),
            fail(ContextTooLong, false, None),
        ),
        case("http_401", auth_401, fail(Auth, false, None)),
        case("http_403", forbidden_403, fail(Auth, false, None)),
        case(
            "http_404",
            sse("not_found_404"),
            fail(ModelNotFound, false, None),
        ),
        case(
            "http_413",
            sse("too_large_413"),
            fail(ContextTooLong, false, None),
        ),
        case(
            "http_429_retry_after",
            sse("rate_limit_429"),
            fail(RateLimit, true, Some(12)),
        ),
        case(
            "http_429",
            sse("rate_limit_429_no_retry_after"),
            fail(RateLimit, true, None),
        ),
        case("http_500", sse("server_500"), fail(Server, true, None)),
        case(
            "http_503",
            sse("overloaded_503"),
            fail(Overloaded, true, None),
        ),
        case(
            "http_529",
            sse("overloaded_529"),
            fail(Overloaded, true, None),
        ),
        case(
            "stream_error",
            stream(sse("stream_error_overloaded")),
            fail(Overloaded, true, None),
        ),
        case(
            "truncated",
            stream(sse("truncated")),
            Expect::Fail {
                kind: Network,
                retryable: true,
                retry_after: None,
                delivered: Some("Hello"),
            },
        ),
        // The frames up to the first text delta, then silence with the connection open.
        case(
            "cancel",
            stream_hang(stream, sse("stream_text")),
            Expect::Cancel,
        ),
        // Neither cloud has a model list or a counting endpoint here: the table is static and
        // `count_tokens` is `None`, so these two fixtures are never asked for. `verify` is the
        // one real request (a one token probe), played from the text stream.
        case("models", text.clone(), Expect::Models { at_least: 3 }),
        case("count_tokens", text.clone(), Expect::Tokens(None)),
    ]
}

/// `stream` of the first four SSE frames, with the connection left open.
fn stream_hang(stream: fn(Fixture) -> Fixture, sse: Fixture) -> Fixture {
    let mut f = stream(sse.hang_after(4));
    f.end = aibus_testkit::conformance::End::Hang;
    f
}

#[tokio::test]
async fn bedrock_passes_the_conformance_suite() {
    let server = FakeServer::start().await;
    // A Bedrock API key (a bearer, no colon) so the key travels in one header the kit can see;
    // the sigv4 form has its own tests above.
    let p = from_preset("bedrock", &server, Arc::new(Token(Ok("unused"))));
    let mut cases = cloud_cases(
        bedrock_fixture,
        bedrock_cassette(&cassette("bedrock_stream")),
        load("cassettes", "auth_401", "http"),
        load("cassettes", "bedrock_denied_403", "http"),
    );
    cases.push(Case {
        name: "verify",
        fixture: bedrock_cassette(&cassette("bedrock_stream")),
        expect: Expect::Verify { models: 11 },
    });
    let f = Fixtures {
        model: "claude-sonnet-4-5",
        kind: AuthMethodKind::Bearer,
        auth: Some(("authorization", "Bearer ")),
        cases,
        skipped: vec![
            ("malformed_sse", "Bedrock has no SSE: a bad frame is a broken event stream (a frame that stops short is `truncated`), not a malformed line"),
        ],
    };
    run(p, &server, &f).await;
}

#[tokio::test]
async fn vertex_passes_the_conformance_suite() {
    let server = FakeServer::start().await;
    // The token source is what Vertex sends, so it is the fake key the kit looks for.
    let p = from_preset(
        "vertex",
        &server,
        Arc::new(Token(Ok(aibus_provider::fake::FAKE_KEY))),
    );
    let mut cases = cloud_cases(
        std::convert::identity,
        load("cassettes", "stream_text", "http"),
        load("cassettes", "vertex_unauthenticated_401", "http"),
        load("cassettes", "forbidden_403", "http"),
    );
    cases.push(Case {
        name: "verify",
        fixture: load("cassettes", "vertex_stream", "http"),
        expect: Expect::Verify { models: 4 },
    });
    cases.push(Case {
        name: "malformed_sse",
        fixture: load("cassettes", "malformed_sse", "http"),
        expect: Expect::Fail {
            kind: Unknown,
            retryable: false,
            retry_after: None,
            delivered: None,
        },
    });
    let f = Fixtures {
        model: "claude-sonnet-4-5@20250929",
        kind: AuthMethodKind::None,
        auth: Some(("authorization", "Bearer ")),
        cases,
        skipped: vec![],
    };
    run(p, &server, &f).await;
}
