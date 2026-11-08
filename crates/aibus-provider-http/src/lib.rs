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
use std::time::Duration;

use aibus_provider::sse::{SseEvent, SseParser};
use aibus_provider::{ProviderError, ProviderErrorKind, StreamChunk};
use reqwest::header::{HeaderValue, USER_AGENT};
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

/// `error.message` (both providers' shape), else `message`, else the start of the body.
fn message_of(body: &[u8]) -> String {
    let json: Option<serde_json::Value> = serde_json::from_slice(body).ok();
    let found = json.as_ref().and_then(|j| {
        j.pointer("/error/message")
            .or_else(|| j.get("message"))
            .or_else(|| j.get("error").filter(|e| e.is_string()))
            .and_then(|m| m.as_str())
            .map(str::to_owned)
    });
    found.unwrap_or_else(|| String::from_utf8_lossy(body).trim().to_owned())
}

pub fn request_id(resp: &Response) -> Option<String> {
    ["request-id", "x-request-id"]
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
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(aibus_provider::error::parse_retry_after);
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
        Err(ProviderError::from_http(
            status,
            &message_of(&body),
            retry_after,
            id,
        ))
    }
}

/// A response body that is one JSON document (the model list, a token count).
pub async fn json(resp: Response) -> Result<serde_json::Value, ProviderError> {
    let bytes = resp.bytes().await.map_err(network)?;
    serde_json::from_slice(&bytes).map_err(|e| {
        ProviderError::new(ProviderErrorKind::Unknown, format!("answer is not JSON: {e}"))
    })
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
