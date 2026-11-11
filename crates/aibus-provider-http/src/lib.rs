//! The HTTP plumbing the real provider crates share: one `reqwest` client per provider, sending
//! with cancellation, the error mapping for non-2xx answers, and an SSE event reader over a
//! response. Network code lives in the `aibus-provider-*` crates only (09 section 8).
//!
//! The client is built with rustls (no OpenSSL), the system's root certificates, and `HTTPS_PROXY`
//! and friends from the environment. It has no default headers: every request adds its own
//! `User-Agent` and its provider adds the auth header from the `&Credential` it was handed,
//! marked sensitive, so printing the client or a request cannot show a key (04 section 7).
#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use aibus_provider::sse::{SseEvent, SseParser};
use aibus_provider::error::{parse_retry_after, parse_rfc3339};
use aibus_provider::{ProviderError, ProviderErrorKind, RateLimitInfo, StreamChunk};
use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use reqwest::{Client, RequestBuilder, Response};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub use reqwest;

/// `aibus/<version>` on every request (09 section 8).
const UA: &str = concat!("aibus/", env!("CARGO_PKG_VERSION"));

/// How much of an error body is read. The message is all that is wanted from it.
const ERROR_BODY_MAX: usize = 64 * 1024;

/// A client built on first use, so starting aibus does not load root certificates for providers
/// that are never called.
#[derive(Default)]
pub struct Http {
    client: OnceLock<Result<Client, String>>,
}

pub fn cancelled() -> ProviderError {
    ProviderError::new(ProviderErrorKind::Cancelled, "cancelled")
}

fn network(e: reqwest::Error) -> ProviderError {
    let kind = if e.is_timeout() {
        ProviderErrorKind::Timeout
    } else {
        ProviderErrorKind::Network
    };
    ProviderError::new(kind, e.without_url().to_string())
}

/// The auth header value, marked sensitive so `{:?}` of the header map prints `Sensitive`.
/// A key with characters a header cannot carry is an auth error that does not repeat the key.
pub fn sensitive(value: &str) -> Result<HeaderValue, ProviderError> {
    let mut v = HeaderValue::from_str(value).map_err(|_| {
        ProviderError::new(
            ProviderErrorKind::Auth,
            "the key has characters an HTTP header cannot carry (a stray newline?)",
        )
    })?;
    v.set_sensitive(true);
    Ok(v)
}

/// The provider's message and its error code. The message is `error.message` (both providers'
/// shape), else `message`, else the start of the body. The code is `error.code`, else
/// `error.type` (Anthropic's shape), else empty.
fn error_of(body: &[u8]) -> (String, String) {
    let json: Option<serde_json::Value> = serde_json::from_slice(body).ok();
    let found = json.as_ref().and_then(|j| {
        j.pointer("/error/message")
            .or_else(|| j.get("message"))
            .or_else(|| j.get("error").filter(|e| e.is_string()))
            .and_then(|m| m.as_str())
            .map(str::to_owned)
    });
    let code = json
        .as_ref()
        .and_then(|j| j.pointer("/error/code").or_else(|| j.pointer("/error/type")))
        .and_then(|c| c.as_str())
        .unwrap_or_default()
        .to_owned();
    (found.unwrap_or_else(|| String::from_utf8_lossy(body).trim().to_owned()), code)
}

/// `error.details[].retryDelay` of a Google error body.
fn retry_delay(body: &[u8]) -> Option<Duration> {
    let json: serde_json::Value = serde_json::from_slice(body).ok()?;
    json.pointer("/error/details")?
        .as_array()?
        .iter()
        .find_map(|d| parse_go_duration(d["retryDelay"].as_str()?))
}

/// A Go-style duration, as OpenAI writes its reset headers: `1s`, `6m0s`, `20ms`, `1h2m3.5s`.
fn parse_go_duration(value: &str) -> Option<Duration> {
    let mut rest = value.trim();
    let mut total = Duration::ZERO;
    if rest.is_empty() {
        return None;
    }
    while !rest.is_empty() {
        let digits = rest.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
        let n: f64 = rest[..digits].parse().ok()?;
        rest = &rest[digits..];
        let unit = rest.find(|c: char| c.is_ascii_digit() || c == '.').unwrap_or(rest.len());
        let secs = match &rest[..unit] {
            "h" => 3600.0,
            "m" => 60.0,
            "s" => 1.0,
            "ms" => 1e-3,
            "us" | "\u{b5}s" => 1e-6,
            "ns" => 1e-9,
            _ => return None,
        };
        total += Duration::try_from_secs_f64(n * secs).ok()?;
        rest = &rest[unit..];
    }
    Some(total)
}

/// What the response headers say about the account's windows: `retry-after` (seconds or date)
/// and `retry-after-ms`, OpenAI's `x-ratelimit-remaining-*` and `x-ratelimit-reset-*`, and
/// Anthropic's `anthropic-ratelimit-*` family. `None` when there is no such header at all.
/// `now` anchors the date forms, so a test can pin it.
pub fn rate_limit(headers: &HeaderMap, now: SystemTime) -> Option<RateLimitInfo> {
    let get = |n: &str| headers.get(n).and_then(|v| v.to_str().ok());
    let count = |names: &[&str]| names.iter().filter_map(|n| get(n)?.trim().parse::<u64>().ok()).min();
    // Anthropic sends a timestamp, OpenAI a span.
    let reset = |names: &[&str]| {
        names
            .iter()
            .filter_map(|n| {
                let v = get(n)?;
                parse_go_duration(v).or_else(|| parse_rfc3339(v)?.duration_since(now).ok().or(Some(Duration::ZERO)))
            })
            .min()
    };
    let retry_after = get("retry-after-ms")
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|ms| ms.is_finite() && *ms >= 0.0)
        .map(|ms| Duration::from_secs_f64(ms.min(3_600_000.0) / 1000.0))
        .or_else(|| get("retry-after").and_then(|v| parse_retry_after(v, now)));
    let requests_remaining = count(&["x-ratelimit-remaining-requests", "anthropic-ratelimit-requests-remaining"]);
    let tokens_remaining = count(&[
        "x-ratelimit-remaining-tokens",
        "anthropic-ratelimit-tokens-remaining",
        "anthropic-ratelimit-input-tokens-remaining",
        "anthropic-ratelimit-output-tokens-remaining",
    ]);
    let requests_reset = reset(&["x-ratelimit-reset-requests", "anthropic-ratelimit-requests-reset"]);
    let tokens_reset = reset(&[
        "x-ratelimit-reset-tokens",
        "anthropic-ratelimit-tokens-reset",
        "anthropic-ratelimit-input-tokens-reset",
        "anthropic-ratelimit-output-tokens-reset",
    ]);
    // The reset that matters is the one of the window that ran out; both when both did.
    let resets_in = match (requests_remaining == Some(0), tokens_remaining == Some(0)) {
        (true, true) => requests_reset.max(tokens_reset),
        (true, false) => requests_reset,
        (false, true) => tokens_reset,
        (false, false) => requests_reset.into_iter().chain(tokens_reset).min(),
    };
    let info = RateLimitInfo { retry_after, requests_remaining, tokens_remaining, resets_in };
    (info != RateLimitInfo::default()).then_some(info)
}

pub fn request_id(resp: &Response) -> Option<String> {
    ["request-id", "x-request-id", "x-amzn-requestid"]
        .iter()
        .find_map(|h| resp.headers().get(*h))
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

impl Http {
    pub fn new() -> Self {
        Self::default()
    }

    fn client(&self) -> Result<&Client, ProviderError> {
        self.client
            .get_or_init(|| {
                Client::builder()
                    .use_rustls_tls()
                    .connect_timeout(Duration::from_secs(10))
                    // Between two chunks of the answer. A reply can think for a while before the
                    // first token, so this is long.
                    .read_timeout(Duration::from_secs(300))
                    .build()
                    .map_err(|e| e.to_string())
            })
            .as_ref()
            .map_err(|e| {
                ProviderError::new(ProviderErrorKind::Network, format!("http client: {e}"))
            })
    }

    /// Sends what `build` makes, and returns the response when it is a 2xx. Anything else is
    /// mapped by status (`ProviderError::from_http`) with the provider's own message. Returns
    /// `Cancelled` as soon as `cancel` fires, whatever the request is waiting on.
    pub async fn send(
        &self,
        cancel: &CancellationToken,
        build: impl FnOnce(&Client) -> RequestBuilder,
    ) -> Result<Response, ProviderError> {
        let request = build(self.client()?).header(USER_AGENT, UA);
        let mut resp = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(cancelled()),
            r = request.send() => r.map_err(network)?,
        };
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status().as_u16();
        let info = rate_limit(resp.headers(), SystemTime::now());
        let id = request_id(&resp);
        let mut body = Vec::new();
        while body.len() < ERROR_BODY_MAX {
            let chunk = tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(cancelled()),
                c = resp.chunk() => c,
            };
            match chunk {
                Ok(Some(c)) => body.extend_from_slice(&c),
                // A body that breaks off still leaves the status to go on.
                Ok(None) | Err(_) => break,
            }
        }
        let (message, code) = error_of(&body);
        // Google sends the wait in the body (`error.details[].retryDelay`, "33s"), not a header.
        let retry_after = info.and_then(|i| i.retry_after).or_else(|| retry_delay(&body));
        let mut e = ProviderError::from_http(status, &message, retry_after, id, &code);
        e.rate_limit = info;
        Err(e)
    }
}

/// A response body that is one JSON document (the model list, a token count).
pub async fn json(resp: Response) -> Result<serde_json::Value, ProviderError> {
    let bytes = resp.bytes().await.map_err(network)?;
    serde_json::from_slice(&bytes).map_err(|e| {
        ProviderError::new(ProviderErrorKind::Unknown, format!("answer is not JSON: {e}"))
    })
}

/// The next piece of a response body that is not SSE (Bedrock's binary event stream), `None` at
/// the end. Gives up when `cancel` fires.
pub async fn chunk(resp: &mut Response, cancel: &CancellationToken) -> Result<Option<Vec<u8>>, ProviderError> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(cancelled()),
        c = resp.chunk() => Ok(c.map_err(network)?.map(|b| b.to_vec())),
    }
}

/// The events of a streaming response, one at a time.
pub struct Events {
    resp: Response,
    parser: SseParser,
    ready: VecDeque<SseEvent>,
    cancel: CancellationToken,
    ended: bool,
}

impl Events {
    pub fn new(resp: Response, cancel: CancellationToken) -> Self {
        Self {
            resp,
            parser: SseParser::new(),
            ready: VecDeque::new(),
            cancel,
            ended: false,
        }
    }

    /// The next event, `None` when the server closed the stream cleanly. A broken connection
    /// or a malformed line is an error.
    pub async fn next(&mut self) -> Result<Option<SseEvent>, ProviderError> {
        let malformed =
            |e: aibus_provider::sse::SseError| ProviderError::new(ProviderErrorKind::Unknown, format!("malformed SSE: {e}"));
        loop {
            if let Some(ev) = self.ready.pop_front() {
                return Ok(Some(ev));
            }
            if self.ended {
                return Ok(None);
            }
            let chunk = tokio::select! {
                biased;
                () = self.cancel.cancelled() => return Err(cancelled()),
                c = self.resp.chunk() => c.map_err(network)?,
            };
            match chunk {
                Some(bytes) => self.ready.extend(self.parser.push(&bytes).map_err(malformed)?),
                None => {
                    self.ended = true;
                    self.ready.extend(self.parser.finish().map_err(malformed)?);
                }
            }
        }
    }
}

/// Sends one chunk, giving up when `cancel` fires while the channel is full.
pub async fn emit(
    out: &mpsc::Sender<StreamChunk>,
    cancel: &CancellationToken,
    chunk: StreamChunk,
) -> Result<(), ProviderError> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(cancelled()),
        // A closed channel means the run is gone; there is nobody to tell.
        _ = out.send(chunk) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn token() -> CancellationToken {
        CancellationToken::new()
    }

    #[tokio::test]
    async fn every_request_carries_the_user_agent_and_nothing_else_by_default() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .and(header("user-agent", UA))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let http = Http::new();
        let url = format!("{}/x", server.uri());
        http.send(&token(), |c| c.get(&url)).await.unwrap();
        let seen = &server.received_requests().await.unwrap()[0];
        let names: Vec<_> = seen.headers.keys().map(|k| k.as_str().to_owned()).collect();
        for n in &names {
            assert!(!n.contains("auth") && !n.contains("key"), "unexpected header {n}");
        }
        assert!(UA.starts_with("aibus/"));
    }

    #[tokio::test]
    async fn the_client_has_no_default_headers_to_print() {
        let http = Http::new();
        let text = format!("{:?}", http.client().unwrap());
        assert!(!text.contains("authorization") && !text.contains("x-api-key"), "{text}");
    }

    #[tokio::test]
    async fn a_sensitive_header_prints_as_sensitive() {
        let v = sensitive("sk-secret-value").unwrap();
        assert!(!format!("{v:?}").contains("sk-secret-value"));
        let err = sensitive("bad\nkey").unwrap_err();
        assert_eq!(err.kind, ProviderErrorKind::Auth);
        assert!(!err.message.contains("bad"));
    }

    #[tokio::test]
    async fn a_429_carries_retry_after_request_id_and_the_providers_message() {
        let server = MockServer::start().await;
        Mock::given(path("/x"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "7")
                    .insert_header("request-id", "req_1")
                    .set_body_string(r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#),
            )
            .mount(&server)
            .await;
        let http = Http::new();
        let url = format!("{}/x", server.uri());
        let e = http.send(&token(), |c| c.get(&url)).await.unwrap_err();
        assert_eq!(e.kind, ProviderErrorKind::RateLimit);
        assert!(e.retryable);
        assert_eq!(e.retry_after, Some(Duration::from_secs(7)));
        assert_eq!(e.request_id.as_deref(), Some("req_1"));
        assert_eq!(e.message, "slow down");
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(reqwest::header::HeaderName::from_bytes(k.as_bytes()).unwrap(), v.parse().unwrap());
        }
        h
    }

    #[test]
    fn google_puts_the_wait_in_the_error_body() {
        let body = br#"{"error":{"code":429,"status":"RESOURCE_EXHAUSTED","details":[{"@type":"x/ErrorInfo"},{"@type":"x/RetryInfo","retryDelay":"33s"}]}}"#;
        assert_eq!(retry_delay(body), Some(Duration::from_secs(33)));
        assert_eq!(retry_delay(br#"{"error":{"message":"no details"}}"#), None);
        assert_eq!(retry_delay(b"not json"), None);
    }

    #[test]
    fn go_durations_parse() {
        assert_eq!(parse_go_duration("1s"), Some(Duration::from_secs(1)));
        assert_eq!(parse_go_duration("6m0s"), Some(Duration::from_secs(360)));
        assert_eq!(parse_go_duration("20ms"), Some(Duration::from_millis(20)));
        assert_eq!(parse_go_duration("1h2m3.5s"), Some(Duration::from_millis(3_723_500)));
        assert_eq!(parse_go_duration(""), None);
        assert_eq!(parse_go_duration("soon"), None);
        assert_eq!(parse_go_duration("5"), None);
    }

    #[test]
    fn retry_after_ms_wins_and_the_http_date_counts_from_now() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_792_567_590);
        let both = rate_limit(&headers(&[("retry-after", "20"), ("retry-after-ms", "1500")]), now).unwrap();
        assert_eq!(both.retry_after, Some(Duration::from_millis(1500)));
        let date = rate_limit(&headers(&[("retry-after", "Wed, 21 Oct 2026 07:28:00 GMT")]), now).unwrap();
        assert_eq!(date.retry_after, Some(Duration::from_secs(90)));
        assert_eq!(rate_limit(&headers(&[("content-type", "text/plain")]), now), None);
    }

    #[test]
    fn openai_window_headers_land_in_the_info() {
        let h = headers(&[
            ("x-ratelimit-remaining-requests", "0"),
            ("x-ratelimit-remaining-tokens", "29000"),
            ("x-ratelimit-reset-requests", "6m0s"),
            ("x-ratelimit-reset-tokens", "1s"),
        ]);
        let i = rate_limit(&h, SystemTime::now()).unwrap();
        assert_eq!((i.requests_remaining, i.tokens_remaining), (Some(0), Some(29_000)));
        assert_eq!(i.resets_in, Some(Duration::from_secs(360)), "the exhausted window's reset");
    }

    #[test]
    fn anthropic_window_headers_land_in_the_info() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_792_567_680 - 45);
        let h = headers(&[
            ("anthropic-ratelimit-requests-remaining", "12"),
            ("anthropic-ratelimit-tokens-remaining", "0"),
            ("anthropic-ratelimit-input-tokens-remaining", "400"),
            ("anthropic-ratelimit-tokens-reset", "2026-10-21T07:28:00Z"),
            ("anthropic-ratelimit-requests-reset", "2026-10-21T07:27:20Z"),
        ]);
        let i = rate_limit(&h, now).unwrap();
        assert_eq!(i.requests_remaining, Some(12));
        assert_eq!(i.tokens_remaining, Some(0), "the smallest of the token counts");
        assert_eq!(i.resets_in, Some(Duration::from_secs(45)));
    }

    #[tokio::test]
    async fn the_window_headers_ride_on_the_error_and_the_body_code_picks_the_kind() {
        let server = MockServer::start().await;
        Mock::given(path("/quota"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("x-ratelimit-remaining-requests", "0")
                    .insert_header("x-ratelimit-reset-requests", "9s")
                    .set_body_string(r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota","code":"insufficient_quota"}}"#),
            )
            .mount(&server)
            .await;
        Mock::given(path("/filter"))
            .respond_with(ResponseTemplate::new(400).set_body_string(
                r#"{"error":{"message":"blocked","type":"invalid_request_error","code":"content_policy_violation"}}"#,
            ))
            .mount(&server)
            .await;
        let http = Http::new();
        let url = format!("{}/quota", server.uri());
        let e = http.send(&token(), |c| c.get(&url)).await.unwrap_err();
        assert_eq!((e.kind, e.retryable), (ProviderErrorKind::Quota, false));
        let info = e.rate_limit.unwrap();
        assert_eq!((info.requests_remaining, info.resets_in), (Some(0), Some(Duration::from_secs(9))));
        let url = format!("{}/filter", server.uri());
        let e = http.send(&token(), |c| c.get(&url)).await.unwrap_err();
        assert_eq!((e.kind, e.retryable), (ProviderErrorKind::ContentFilter, false));
        assert_eq!(e.rate_limit, None);
    }

    #[tokio::test]
    async fn cancel_wins_over_a_slow_server() {
        let server = MockServer::start().await;
        Mock::given(path("/slow"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(600)))
            .mount(&server)
            .await;
        let http = Http::new();
        let url = format!("{}/slow", server.uri());
        let cancel = token();
        let c2 = cancel.clone();
        // Cancel once the server has the request; it will not answer for ten minutes, so only
        // the cancel can end the call. The outer timeout turns a hang into a failure.
        let watch = tokio::spawn(async move {
            while server.received_requests().await.unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            c2.cancel();
            server
        });
        let e = tokio::time::timeout(Duration::from_secs(30), http.send(&cancel, |c| c.get(&url)))
            .await
            .expect("cancel ends the call")
            .unwrap_err();
        assert_eq!(e.kind, ProviderErrorKind::Cancelled);
        let _server = watch.await.unwrap();
    }

    #[tokio::test]
    async fn a_refused_connection_is_a_retryable_network_error() {
        let http = Http::new();
        let e = http.send(&token(), |c| c.get("http://127.0.0.1:1/")).await.unwrap_err();
        assert_eq!(e.kind, ProviderErrorKind::Network);
        assert!(e.retryable);
    }

    #[tokio::test]
    async fn events_come_out_one_by_one_and_a_bad_line_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(path("/ok"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("event: a\ndata: 1\n\ndata: 2\n\n", "text/event-stream"))
            .mount(&server)
            .await;
        Mock::given(path("/bad"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"data: \xff\n\n".to_vec()))
            .mount(&server)
            .await;
        let http = Http::new();
        let url = format!("{}/ok", server.uri());
        let mut ev = Events::new(http.send(&token(), |c| c.get(&url)).await.unwrap(), token());
        assert_eq!(ev.next().await.unwrap().unwrap().event, "a");
        assert_eq!(ev.next().await.unwrap().unwrap().data, "2");
        assert!(ev.next().await.unwrap().is_none());

        let url = format!("{}/bad", server.uri());
        let mut ev = Events::new(http.send(&token(), |c| c.get(&url)).await.unwrap(), token());
        let e = ev.next().await.unwrap_err();
        assert_eq!(e.kind, ProviderErrorKind::Unknown);
        assert!(e.message.starts_with("malformed SSE"));
    }

    #[tokio::test]
    async fn emit_gives_up_on_cancel_while_the_channel_is_full() {
        let (tx, _rx) = mpsc::channel(1);
        let cancel = token();
        emit(&tx, &cancel, StreamChunk::TextDelta("a".into())).await.unwrap();
        cancel.cancel();
        let e = emit(&tx, &cancel, StreamChunk::TextDelta("b".into())).await.unwrap_err();
        assert_eq!(e.kind, ProviderErrorKind::Cancelled);
    }
}
