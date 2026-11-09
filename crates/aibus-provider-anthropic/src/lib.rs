//! Anthropic provider implementation: the Messages API over SSE (04 section 3).
//!
//! The key reaches the wire only through the `&Credential` a call is handed, as a sensitive
//! header, and nothing here logs a header, a body or the credential.
#![forbid(unsafe_code)]

mod stream;
mod wire;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use aibus_provider::models::ModelTable;
use aibus_provider::{
    AccountInfo, AliasTable, AuthMethodKind, Capabilities, ChatRequest, ChatResponse, ConfigError,
    Credential, ModelId, ModelInfo, ModelRef, Price, Provider, ProviderConfig, ProviderError,
    ProviderFactory, ProviderId, StreamChunk, SystemRole,
};
use aibus_provider_http::reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use aibus_provider_http::{cancelled, emit, json, request_id, sensitive, Events, Http};
use async_trait::async_trait;
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
}

pub struct Anthropic {
    /// No trailing slash, no `/v1`.
    base: String,
    table: Mutex<ModelTable>,
    cache_dir: Option<PathBuf>,
    http: Http,
}

impl Anthropic {
    pub fn new(cfg: &ProviderConfig) -> Result<Self, ConfigError> {
        let base = cfg.base_url.as_deref().unwrap_or(DEFAULT_BASE_URL).trim_end_matches('/');
        if !(base.starts_with("https://") || base.starts_with("http://")) {
            return Err(ConfigError::Invalid(
                "anthropic".into(),
                "base_url must start with https:// or http://".into(),
            ));
        }
        Ok(Self {
            base: base.to_owned(),
            table: Mutex::new(ModelTable::load(
                "anthropic",
                include_str!("models.json"),
                cfg,
                &["claude-"],
                false,
            )?),
            cache_dir: cfg.cache_dir.clone(),
            http: Http::new(),
        })
    }

    fn table(&self) -> std::sync::MutexGuard<'_, ModelTable> {
        self.table.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The version header and the auth header for this credential. `None` auth sends no key (a
    /// gateway that authenticates some other way).
    fn headers(creds: &Credential) -> Result<HeaderMap, ProviderError> {
        let mut h = HeaderMap::new();
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
        let headers = Self::headers(creds)?;
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
        ProviderId("anthropic")
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
        let (ids, _) = self.list_models(creds).await?;
        Ok(self.table().refreshed(
            "anthropic",
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

        let url = format!("{}/v1/messages", self.base);
        let mut headers = Self::headers(creds)?;
        if beta {
            headers.insert("anthropic-beta", HeaderValue::from_static(EFFORT_BETA));
        }
        let resp = self
            .http
            .send(&cancel, |c| {
                c.post(&url)
                    .headers(headers)
                    .header(CONTENT_TYPE, "application/json")
                    .header(ACCEPT, "text/event-stream")
                    .body(body.to_string())
            })
            .await?;
        let request_id = request_id(&resp);

        let mut events = Events::new(resp, cancel.clone());
        let mut turn = stream::Turn::new(price);
        while let Some(ev) = events.next().await? {
            for chunk in turn.event(&ev)? {
                emit(&out, &cancel, chunk).await?;
            }
        }
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        turn.finish(request_id)
    }

    async fn count_tokens(
        &self,
        creds: &Credential,
        req: &ChatRequest,
    ) -> Option<Result<u64, ProviderError>> {
        let body = wire::count_body(&self.capabilities().degrade(req.clone()));
        let url = format!("{}/v1/messages/count_tokens", self.base);
        let result = async {
            let headers = Self::headers(creds)?;
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
