//! Anthropic provider implementation: the Messages API over SSE (04 section 3).
//!
//! The key reaches the wire only through the `&Credential` a call is handed, as a sensitive
//! header, and nothing here logs a header, a body or the credential.
#![forbid(unsafe_code)]

mod cloud;
mod eventstream;
mod sigv4;
mod stream;
mod wire;

pub use cloud::TokenSource;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use aibus_provider::models::ModelTable;
use aibus_provider::{
    AccountInfo, AliasTable, Api, AuthMethodKind, Capabilities, ChatRequest, ChatResponse,
    ConfigError, Credential, ModelId, ModelInfo, ModelRef, Price, Provider, ProviderConfig,
    ProviderDef, ProviderError, ProviderErrorKind, ProviderFactory, ProviderId, StreamChunk,
    SystemRole,
};
use aibus_provider_http::reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE,
};
use aibus_provider_http::{cancelled, chunk, emit, json, rate_limit, request_id, sensitive, Events, Http};
use async_trait::async_trait;
use cloud::Transport;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";
const EFFORT_BETA: &str = "effort-2025-11-24";

pub struct Factory;

impl ProviderFactory for Factory {
    fn id(&self) -> ProviderId {
        ProviderId("anthropic")
    }

    fn build(&self, cfg: &ProviderConfig) -> Result<Arc<dyn Provider>, ConfigError> {
        Ok(Arc::new(Anthropic::new(cfg)?))
    }

    fn auth_methods(&self) -> &'static [AuthMethodKind] {
        &[AuthMethodKind::ApiKey, AuthMethodKind::Bearer, AuthMethodKind::Env]
    }

    fn apis(&self) -> &'static [&'static str] {
        &["anthropic-messages"]
    }
}

pub struct Anthropic {
    /// `anthropic`, or the id of the `[[provider]]` this instance was built for.
    id: &'static str,
    /// From the definition: sent on every request.
    extra_headers: HeaderMap,
    /// No trailing slash, no `/v1`.
    base: String,
    table: Mutex<ModelTable>,
    cache_dir: Option<PathBuf>,
    http: Http,
    transport: Transport,
}

impl Anthropic {
    pub fn new(cfg: &ProviderConfig) -> Result<Self, ConfigError> {
        Self::with_transport(cfg, DEFAULT_BASE_URL.to_owned(), Transport::Direct, &["claude-"])
    }

    pub(crate) fn with_transport(
        cfg: &ProviderConfig,
        default_base: String,
        transport: Transport,
        prefixes: &'static [&'static str],
    ) -> Result<Self, ConfigError> {
        // The definition the registry resolved, or just the account's `base_url` (a factory
        // built on its own, and the cloud transports, whose default host is not Anthropic's).
        let def = cfg.def.clone().unwrap_or_else(|| ProviderDef {
            id: "anthropic".into(),
            api: Some(Api::AnthropicMessages),
            base_url: cfg.base_url.clone(),
            ..ProviderDef::default()
        });
        if def.api != Some(Api::AnthropicMessages) {
            return Err(ConfigError::Invalid(def.id.clone(), "this adapter speaks anthropic-messages only".into()));
        }
        let id = def.static_id();
        let base = def.base_url.as_deref().unwrap_or(&default_base).trim_end_matches('/');
        if !(base.starts_with("https://") || base.starts_with("http://")) {
            return Err(ConfigError::Invalid(id.into(), "base_url must start with https:// or http://".into()));
        }
        let mut extra_headers = HeaderMap::new();
        for (k, v) in &def.headers {
            let bad = |e: String| ConfigError::Invalid(id.into(), format!("headers: {k}: {e}"));
            extra_headers.insert(
                HeaderName::from_bytes(k.as_bytes()).map_err(|e| bad(e.to_string()))?,
                HeaderValue::from_str(v).map_err(|e| bad(e.to_string()))?,
            );
        }
        // A definition's own prefixes win over the transport's (Bedrock has its own).
        let own: Vec<&str> = def.prefixes.iter().map(String::as_str).collect();
        let prefixes = if own.is_empty() || !matches!(transport, Transport::Direct) { prefixes } else { &own[..] };
        Ok(Self {
            id,
            extra_headers,
            base: base.to_owned(),
            table: Mutex::new(ModelTable::load(id, aibus_provider::catalog::table("anthropic").unwrap_or_default(), cfg, prefixes, false)?),
            cache_dir: cfg.cache_dir.clone(),
            http: Http::new(),
            transport,
        })
    }

    fn table(&self) -> std::sync::MutexGuard<'_, ModelTable> {
        self.table.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The version header and the auth header for this credential. `None` auth sends no key (a
    /// gateway that authenticates some other way).
    fn headers(&self, creds: &Credential) -> Result<HeaderMap, ProviderError> {
        let mut h = self.extra_headers.clone();
        h.insert("anthropic-version", HeaderValue::from_static(API_VERSION));
        match creds.kind {
            AuthMethodKind::None => {}
            AuthMethodKind::Bearer | AuthMethodKind::OAuth => {
                let value = Zeroizing::new(format!("Bearer {}", creds.secret.expose()));
                h.insert(AUTHORIZATION, sensitive(&value)?);
            }
            AuthMethodKind::ApiKey | AuthMethodKind::Env => {
                h.insert("x-api-key", sensitive(creds.secret.expose())?);
            }
        }
        Ok(h)
    }

    async fn list_models(&self, creds: &Credential) -> Result<(Vec<ModelId>, Option<String>), ProviderError> {
        let url = format!("{}/v1/models?limit=1000", self.base);
        let headers = self.headers(creds)?;
        let resp = self
            .http
            .send(&CancellationToken::new(), |c| c.get(&url).headers(headers))
            .await?;
        let org = resp
            .headers()
            .get("anthropic-organization-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let body = json(resp).await?;
        let ids = body["data"]
            .as_array()
            .map(|a| a.iter().filter_map(|m| m["id"].as_str()).map(|id| ModelId(id.into())).collect())
            .unwrap_or_default();
        Ok((ids, org))
    }
}

#[async_trait]
impl Provider for Anthropic {
    fn id(&self) -> ProviderId {
        ProviderId(self.id)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            tools: true,
            parallel_tool_calls: true,
            streaming: true,
            thinking: true,
            prompt_cache: true,
            logprobs: false,
            effort: true,
            images: true,
            system_role: SystemRole::Separate,
            max_tools: None,
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        self.table().models()
    }

    async fn refresh_models(&self, creds: &Credential) -> Result<Vec<ModelInfo>, ProviderError> {
        if !matches!(self.transport, Transport::Direct) {
            return Ok(self.models());
        }
        let (ids, _) = self.list_models(creds).await?;
        Ok(self.table().refreshed(
            self.id,
            ids,
            |id| ModelInfo {
                id: id.clone(),
                context_window: 200_000,
                max_output: 8_192,
                // The model's line is its family, for the token-ratio overrides.
                family: ["opus", "sonnet", "haiku"]
                    .into_iter()
                    .find(|f| id.0.contains(f))
                    .unwrap_or("claude")
                    .into(),
                deprecated: false,
            },
            self.cache_dir.as_deref(),
            SystemTime::now(),
        ))
    }

    fn resolve_model(&self, r: &ModelRef, aliases: &AliasTable) -> Option<ModelId> {
        self.table().resolve(r, aliases)
    }

    async fn stream(
        &self,
        creds: &Credential,
        req: ChatRequest,
        out: mpsc::Sender<StreamChunk>,
        cancel: CancellationToken,
    ) -> Result<ChatResponse, ProviderError> {
        let req = self.capabilities().degrade(req);
        let (price, effort_ok) = {
            let t = self.table();
            (t.price(&req.model), t.entry(&req.model).is_some_and(|e| e.effort))
        };
        let body = wire::messages_body(&req, effort_ok);
        let beta = req.effort.is_some() && effort_ok;
        tracing::debug!(account = %creds.account, model = %req.model, "anthropic: stream");

        let (url, headers, body) = self.prepare(creds, &req.model, body, beta).await?;
        let mut resp = self.http.send(&cancel, |c| c.post(&url).headers(headers).body(body)).await?;
        let request_id = request_id(&resp);
        let window = rate_limit(resp.headers(), SystemTime::now());

        let mut turn = stream::Turn::new(price);
        if matches!(self.transport, Transport::Bedrock { .. }) {
            let mut frames = eventstream::Frames::default();
            while let Some(bytes) = chunk(&mut resp, &cancel).await? {
                frames.push(&bytes);
                while let Some(ev) = frames.next()? {
                    for c in turn.event(&ev)? {
                        emit(&out, &cancel, c).await?;
                    }
                }
            }
            if frames.is_partial() {
                return Err(ProviderError::new(ProviderErrorKind::Network, "the event stream broke off inside a frame"));
            }
        } else {
            let mut events = Events::new(resp, cancel.clone());
            while let Some(ev) = events.next().await? {
                for c in turn.event(&ev)? {
                    emit(&out, &cancel, c).await?;
                }
            }
        }
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        let mut done = turn.finish(request_id)?;
        done.rate_limit = window;
        Ok(done)
    }

    async fn count_tokens(
        &self,
        creds: &Credential,
        req: &ChatRequest,
    ) -> Option<Result<u64, ProviderError>> {
        // Bedrock and Vertex have no counting endpoint here: the caller falls back to its estimate.
        if !matches!(self.transport, Transport::Direct) {
            return None;
        }
        let body = wire::count_body(&self.capabilities().degrade(req.clone()));
        let url = format!("{}/v1/messages/count_tokens", self.base);
        let result = async {
            let headers = self.headers(creds)?;
            let resp = self
                .http
                .send(&CancellationToken::new(), |c| {
                    c.post(&url)
                        .headers(headers)
                        .header(CONTENT_TYPE, "application/json")
                        .body(body.to_string())
                })
                .await?;
            json(resp).await?["input_tokens"].as_u64().ok_or_else(|| {
                ProviderError::new(aibus_provider::ProviderErrorKind::Unknown, "count_tokens: no input_tokens in the answer")
            })
        }
        .await;
        Some(result)
    }

    async fn verify(&self, creds: &Credential) -> Result<AccountInfo, ProviderError> {
        if !matches!(self.transport, Transport::Direct) {
            self.check_cloud_auth(creds).await?;
            let models = self.models().into_iter().map(|m| m.id).collect();
            return Ok(AccountInfo { label: creds.account.to_string(), org: None, models });
        }
        let (models, org) = self.list_models(creds).await?;
        Ok(AccountInfo { label: creds.account.to_string(), org, models })
    }

    fn price(&self, model: &ModelId) -> Option<Price> {
        self.table().price(model)
    }

    fn estimate_tokens(&self, req: &ChatRequest) -> u64 {
        aibus_provider::tokens::estimate(req, self.table().chars_per_token(&req.model))
    }
}
