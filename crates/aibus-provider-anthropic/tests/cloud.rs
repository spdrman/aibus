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
    ProviderError, ProviderErrorKind, Secret, StopReason, StreamChunk,
};
use aibus_provider_anthropic::{Anthropic, TokenSource};
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
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
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
    let mut f = ((16 + h.len() + payload.len()) as u32).to_be_bytes().to_vec();
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
            &[(":event-type", "chunk"), (":content-type", "application/json"), (":message-type", "event")],
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
    ProviderConfig { base_url: Some(server.uri()), ..ProviderConfig::default() }
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
    async fn token(&self) -> Result<Secret<String>, ProviderError> {
        self.0
            .map(|t| Secret::new(t.to_owned()))
            .map_err(|m| ProviderError::new(ProviderErrorKind::Auth, m))
    }
}

fn vertex(server: &MockServer, tokens: Token) -> Anthropic {
    Anthropic::vertex(&cfg(server), "my-project", "us-east5", Arc::new(tokens)).unwrap()
}

fn text_of(chunks: &[StreamChunk]) -> Vec<&str> {
    chunks.iter().filter_map(|c| if let StreamChunk::TextDelta(t) = c { Some(t.as_str()) } else { None }).collect()
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
    assert_eq!(resp.message.blocks, [ContentBlock::Text("Hello, world".into())]);
    assert_eq!(resp.stop, StopReason::EndTurn);
    assert_eq!(resp.request_id.as_deref(), Some("6f1e7a52-3b0c-4d58-9a41-2c7d0e5b8f13"));
    assert_eq!((resp.usage.input_tokens, resp.usage.output_tokens), (25, 15));

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
    assert!(r.headers.get("user-agent").unwrap().to_str().unwrap().starts_with("aibus/"));
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
    Mock::given(method("POST")).respond_with(bedrock_template(&cassette("bedrock_stream"))).mount(&server).await;
    for secs in [1_440_938_160, 1_440_938_160, 1_440_938_161] {
        let p = Anthropic::bedrock(&cfg(&server), "us-east-1", move || UNIX_EPOCH + Duration::from_secs(secs)).unwrap();
        let _ = collect(&p, &aws_cred(AWS_SECRET), request("hi")).await;
    }
    let sigs: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| r.headers.get("authorization").unwrap().to_str().unwrap().to_owned())
        .collect();
    assert_eq!(sigs[0], sigs[1]);
    assert_ne!(sigs[0], sigs[2]);
}

#[tokio::test]
async fn a_session_token_is_sent_and_signed_and_a_colon_in_the_model_id_is_encoded() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(bedrock_template(&cassette("bedrock_stream"))).mount(&server).await;
    let mut req = request("hi");
    req.model = ModelId("anthropic.claude-sonnet-4-5-20250929-v1:0".into());
    let secret = format!("{AWS_SECRET}:FwoGZXIvYXdzEXAMPLETOKEN");
    let (result, _) = collect(&bedrock(&server), &aws_cred(&secret), req).await;
    result.unwrap();
    let r = server.received_requests().await.unwrap().remove(0);
    assert_eq!(r.url.path(), "/model/anthropic.claude-sonnet-4-5-20250929-v1%3A0/invoke-with-response-stream");
    assert_eq!(r.headers.get("x-amz-security-token").unwrap(), "FwoGZXIvYXdzEXAMPLETOKEN");
    let auth = r.headers.get("authorization").unwrap().to_str().unwrap();
    assert!(auth.contains("SignedHeaders=content-type;host;x-amz-date;x-amz-security-token,"), "{auth}");
}

#[tokio::test]
async fn bedrock_errors_keep_the_providers_message_and_never_the_secret() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(cassette("bedrock_denied_403").template()).mount(&server).await;
    let (result, _) = collect(&bedrock(&server), &aws_cred(AWS_SECRET), request("hi")).await;
    let e = result.unwrap_err();
    assert_eq!(e.kind, ProviderErrorKind::Auth);
    assert!(e.message.contains("not authorized to perform: bedrock:InvokeModelWithResponseStream"), "{}", e.message);
    assert_eq!(e.request_id.as_deref(), Some("0d9c4b1e-77a3-4f0a-8e26-5b13a9c2d7f4"));
    assert!(!format!("{e:?}").contains("wJalrXUtnFEMI"));
}

#[tokio::test]
async fn an_exception_frame_mid_stream_is_a_rate_limit_error() {
    let server = MockServer::start().await;
    let frame = eventstream_frame(
        &[(":exception-type", "throttlingException"), (":message-type", "exception")],
        br#"{"message":"Too many requests, please wait before trying again."}"#,
    );
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200).set_body_bytes(frame)).mount(&server).await;
    let (result, _) = collect(&bedrock(&server), &aws_cred(AWS_SECRET), request("hi")).await;
    let e = result.unwrap_err();
    assert_eq!((e.kind, e.retryable), (ProviderErrorKind::RateLimit, true));
}

#[tokio::test]
async fn a_malformed_sigv4_secret_fails_before_any_request() {
    let server = MockServer::start().await;
    let (result, _) = collect(&bedrock(&server), &aws_cred("not-a-pair-SECRETVALUE"), request("hi")).await;
    let e = result.unwrap_err();
    assert_eq!(e.kind, ProviderErrorKind::Auth);
    assert!(!e.message.contains("SECRETVALUE"));
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(bedrock(&server).verify(&aws_cred("nope")).await.is_err());
}

#[tokio::test]
async fn vertex_takes_its_token_from_the_source_and_posts_to_stream_raw_predict() {
    let server = MockServer::start().await;
    let p = "/v1/projects/my-project/locations/us-east5/publishers/anthropic/models/claude-sonnet-4-5@20250929:streamRawPredict";
    Mock::given(method("POST")).and(path(p)).respond_with(cassette("vertex_stream").template()).mount(&server).await;
    let mut req = request("hi");
    req.model = ModelId("claude-sonnet-4-5@20250929".into());
    // The account's own secret is not what Vertex is sent: the token source decides.
    let (result, chunks) = collect(&vertex(&server, Token(Ok(VERTEX_TOKEN))), &cred(AuthMethodKind::Env), req).await;
    let resp = result.unwrap();
    assert_eq!(text_of(&chunks), ["Hello", ", world"]);
    assert_eq!(resp.request_id.as_deref(), Some("8c2f6d0a4b7e19f35d6a0c1e72b49f58"));

    let r = server.received_requests().await.unwrap().remove(0);
    assert_eq!(r.headers.get("authorization").unwrap().to_str().unwrap(), format!("Bearer {VERTEX_TOKEN}"));
    assert!(r.headers.get("x-api-key").is_none() && r.headers.get("anthropic-version").is_none());
    let b: Value = serde_json::from_slice(&r.body).unwrap();
    assert!(b.get("model").is_none());
    assert_eq!((b["anthropic_version"].as_str(), b["stream"].as_bool()), (Some("vertex-2023-10-16"), Some(true)));
    let body = String::from_utf8_lossy(&r.body);
    assert!(!body.contains(VERTEX_TOKEN) && !body.contains(KEY));
}

#[tokio::test]
async fn vertex_errors_map_by_status_and_a_failing_token_source_stops_the_call() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(cassette("vertex_unauthenticated_401").template()).mount(&server).await;
    let (result, _) = collect(&vertex(&server, Token(Ok(VERTEX_TOKEN))), &cred(AuthMethodKind::None), request("hi")).await;
    let e = result.unwrap_err();
    assert_eq!(e.kind, ProviderErrorKind::Auth);
    assert!(e.message.starts_with("Request had invalid authentication credentials"), "{}", e.message);
    assert!(!e.message.contains(VERTEX_TOKEN));

    let p = vertex(&server, Token(Err("gcloud: not logged in")));
    let before = server.received_requests().await.unwrap().len();
    let (result, _) = collect(&p, &cred(AuthMethodKind::None), request("hi")).await;
    assert_eq!(result.unwrap_err().message, "gcloud: not logged in");
    assert_eq!(server.received_requests().await.unwrap().len(), before);
    assert!(p.verify(&cred(AuthMethodKind::None)).await.is_err());
}

#[tokio::test]
async fn the_cloud_transports_have_no_counting_endpoint_and_verify_offline() {
    let server = MockServer::start().await;
    let b = bedrock(&server);
    assert!(b.count_tokens(&aws_cred(AWS_SECRET), &request("hi")).await.is_none());
    let info = b.verify(&aws_cred(AWS_SECRET)).await.unwrap();
    assert!(info.models.iter().any(|m| m.0.starts_with("claude-")));
    let v = vertex(&server, Token(Ok(VERTEX_TOKEN)));
    assert!(v.count_tokens(&cred(AuthMethodKind::None), &request("hi")).await.is_none());
    assert!(v.verify(&cred(AuthMethodKind::None)).await.is_ok());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[test]
fn bad_regions_and_projects_are_config_errors() {
    let c = ProviderConfig::default();
    assert!(Anthropic::bedrock(&c, "us-east-1.evil.example", SystemTime::now).is_err());
    assert!(Anthropic::bedrock(&c, "", SystemTime::now).is_err());
    assert!(Anthropic::vertex(&c, "p/../x", "us-east5", Arc::new(Token(Ok("t")))).is_err());
    assert!(Anthropic::vertex(&c, "my-project", "global", Arc::new(Token(Ok("t")))).is_ok());
}
