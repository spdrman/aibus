//! The sockets the core engine asks for without owning (09 section 8): `web_fetch` pages and
//! `http` hooks, over the same rustls client stack the providers use (`aibus-provider-http`
//! brings `reqwest` with rustls and the system roots; `HTTPS_PROXY` and friends are read by it).
//! `prompt` hooks also come here, because the model call needs the session's provider.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use aibus_core::assist::{complete, Seat};
use aibus_core::tools::{Fetched, WebFetcher};
use aibus_core::HookTransport;
use aibus_provider::{
    CacheHint, ChatRequest, ContentBlock, Message, MessageRole, RequestMetadata, ToolChoice,
};
use aibus_provider_http::reqwest::header::{CONTENT_TYPE, USER_AGENT};
use aibus_provider_http::reqwest::{Client, Response};
use tokio_util::sync::CancellationToken;

/// `aibus/<version>` on every request (09 section 8).
const UA: &str = concat!("aibus/", env!("CARGO_PKG_VERSION"));
/// A fetched page is read this far.
const PAGE_BYTES: usize = 2 * 1024 * 1024;
/// A hook's answer is read this far.
const HOOK_REPLY_BYTES: usize = 64 * 1024;
/// A whole fetch or POST, from connect to the last byte; a hook's own timeout is shorter or equal.
const TOTAL: Duration = Duration::from_secs(60);
/// What a prompt hook's model may write: a verdict is a line.
const VERDICT_TOKENS: u32 = 512;

#[derive(Default)]
pub struct Net {
    client: OnceLock<Result<Client, String>>,
    /// The model `prompt` hooks ask; set once the session knows its default model.
    pub seat: OnceLock<Seat>,
}

impl Net {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn client(&self) -> Result<&Client, String> {
        self.client
            .get_or_init(|| {
                Client::builder()
                    .use_rustls_tls()
                    .connect_timeout(Duration::from_secs(10))
                    .timeout(TOTAL)
                    .build()
                    .map_err(|e| e.to_string())
            })
            .as_ref()
            .map_err(|e| format!("http client: {e}"))
    }
}

/// The body of `resp` up to `max` bytes.
async fn read(
    mut resp: Response,
    max: usize,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    while body.len() < max {
        let chunk = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err("cancelled".into()),
            c = resp.chunk() => c.map_err(|e| e.without_url().to_string())?,
        };
        match chunk {
            Some(c) => body.extend_from_slice(&c),
            None => break,
        }
    }
    body.truncate(max);
    Ok(body)
}

#[async_trait::async_trait]
impl WebFetcher for Net {
    async fn get(&self, url: &str, cancel: &CancellationToken) -> Result<Fetched, String> {
        let request = self.client()?.get(url).header(USER_AGENT, UA);
        let resp = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err("cancelled".into()),
            r = request.send() => r.map_err(|e| e.without_url().to_string())?,
        };
        let status = resp.status().as_u16();
        let content_type = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let final_url = resp.url().to_string();
        let body = read(resp, PAGE_BYTES, cancel).await?;
        Ok(Fetched {
            status,
            content_type,
            url: final_url,
            body,
        })
    }
}

#[async_trait::async_trait]
impl HookTransport for Net {
    async fn ask(
        &self,
        system: &str,
        user: &str,
        cancel: &CancellationToken,
    ) -> Result<String, String> {
        let seat = self
            .seat
            .get()
            .ok_or("no model is set up to answer a prompt hook")?;
        let req = ChatRequest {
            model: seat.model.clone(),
            system: system.into(),
            messages: vec![Message::text(MessageRole::User, user)],
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            max_output_tokens: VERDICT_TOKENS,
            temperature: None,
            effort: None,
            thinking: None,
            cache_hint: CacheHint::None,
            logprobs: false,
            metadata: RequestMetadata::default(),
        };
        let (resp, _) = complete(seat, req, cancel).await.map_err(|e| e.message)?;
        Ok(resp
            .message
            .blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect())
    }

    async fn post(
        &self,
        url: &str,
        body: &str,
        cancel: &CancellationToken,
    ) -> Result<(u16, String), String> {
        let request = self
            .client()?
            .post(url)
            .header(USER_AGENT, UA)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_owned());
        let resp = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err("cancelled".into()),
            r = request.send() => r.map_err(|e| e.without_url().to_string())?,
        };
        let status = resp.status().as_u16();
        let reply = read(resp, HOOK_REPLY_BYTES, cancel).await?;
        Ok((status, String::from_utf8_lossy(&reply).into_owned()))
    }
}
