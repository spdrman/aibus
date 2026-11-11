//! OpenAI and OpenAI-compatible provider implementations (04 section 3): `openai`, `kimi` (the
//! Moonshot endpoint, with its own model table) and `openai-compatible` (Ollama, vLLM, LM
//! Studio). Chat Completions and the Responses API are two dialects of one provider, picked by
//! the account's `api`.
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
use aibus_provider::presets::resolve;
use aibus_provider::{
    AccountInfo, AliasTable, Api, AuthMethodKind, Capabilities, ChatRequest, ChatResponse, Compat,
    ConfigError, Credential, ModelId, ModelInfo, ModelRef, ModelTableSpec, Price, Provider,
    ProviderConfig, ProviderDef, ProviderError, ProviderErrorKind, ProviderFactory, ProviderId,
    StreamChunk, SystemRole,
};
use aibus_provider_http::reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE,
};
use aibus_provider_http::{cancelled, emit, json, request_id, sensitive, Events, Http};
use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Wire {
    Chat,
    Responses,
}

/// Builds an instance for each OpenAI-shaped api from a provider definition (`presets.json`
/// or a `[[provider]]` table). The constructors below name the three vendors this crate has
/// always had; built without a definition, a factory builds its own preset from the account's
/// `base_url` and `api`.
pub struct Factory {
    id: &'static str,
}

impl Factory {
    pub fn openai() -> Self {
        Self { id: "openai" }
    }

    pub fn kimi() -> Self {
        Self { id: "kimi" }
    }

    /// Needs a `base_url` on the account; `auth = none` is allowed.
    pub fn compatible() -> Self {
        Self { id: "openai-compatible" }
    }

    /// The definition to build from: the one the registry resolved, or this factory's preset
    /// with the account-level `base_url` and `api` laid over it.
    fn definition(&self, cfg: &ProviderConfig) -> Result<ProviderDef, ConfigError> {
        if let Some(def) = &cfg.def {
            return Ok(def.clone());
        }
        let mut user = ProviderDef { id: self.id.into(), base_url: cfg.base_url.clone(), ..ProviderDef::default() };
        match cfg.api.as_deref() {
            None | Some("chat") => {}
            Some("responses") => user.api = Some(Api::OpenaiResponses),
            Some(other) => {
                return Err(ConfigError::Invalid(self.id.into(), format!("api \"{other}\" is not \"chat\" or \"responses\"")))
            }
        }
        resolve(self.id, Some(&user))
            .map_err(|e| ConfigError::Invalid(self.id.into(), e))?
            .ok_or_else(|| ConfigError::UnknownProvider(self.id.into()))
    }
}

impl ProviderFactory for Factory {
    fn id(&self) -> ProviderId {
        ProviderId(self.id)
    }

    fn build(&self, cfg: &ProviderConfig) -> Result<Arc<dyn Provider>, ConfigError> {
        Ok(Arc::new(OpenAi::new(self.definition(cfg)?, cfg)?))
    }

    fn auth_methods(&self) -> &'static [AuthMethodKind] {
        if self.id == "openai-compatible" {
            &[AuthMethodKind::ApiKey, AuthMethodKind::Bearer, AuthMethodKind::Env, AuthMethodKind::None]
        } else {
            &[AuthMethodKind::ApiKey, AuthMethodKind::Bearer, AuthMethodKind::Env]
        }
    }

    fn apis(&self) -> &'static [&'static str] {
        &["openai-chat", "openai-responses"]
    }
}

pub struct OpenAi {
    id: &'static str,
    wire: Wire,
    compat: Compat,
    /// No trailing slash; `/chat/completions` and friends go after it.
    base: String,
    /// From the definition: sent on every request, and added to every URL.
    extra_headers: HeaderMap,
    query: String,
    auth_header: Option<HeaderName>,
    table: Mutex<ModelTable>,
    cache_dir: Option<PathBuf>,
    http: Http,
}

impl OpenAi {
    fn new(def: ProviderDef, cfg: &ProviderConfig) -> Result<Self, ConfigError> {
        let id = def.static_id();
        let compat = def.compat();
        let wire = match def.api {
            Some(Api::OpenaiChat) => Wire::Chat,
            Some(Api::OpenaiResponses) if compat.responses => Wire::Responses,
            Some(Api::OpenaiResponses) => {
                return Err(ConfigError::Invalid(id.into(), format!("{id} has no Responses API; use api = \"chat\"")))
            }
            other => return Err(ConfigError::Invalid(id.into(), format!("api {other:?} is not an OpenAI api"))),
        };
        let base = def
            .base_url
            .as_deref()
            .ok_or_else(|| {
                ConfigError::NotConfigured(
                    id.into(),
                    "base_url is required: set it on the account or the provider, e.g. http://127.0.0.1:11434/v1".into(),
                )
            })?
            .trim_end_matches('/');
        if !(base.starts_with("https://") || base.starts_with("http://")) {
            return Err(ConfigError::Invalid(id.into(), "base_url must start with https:// or http://".into()));
        }
        let bad = |what: &str, e: String| ConfigError::Invalid(id.into(), format!("{what}: {e}"));
        let mut extra_headers = HeaderMap::new();
        for (k, v) in &def.headers {
            let name = HeaderName::from_bytes(k.as_bytes()).map_err(|e| bad("headers", format!("{k}: {e}")))?;
            extra_headers.insert(name, HeaderValue::from_str(v).map_err(|e| bad("headers", format!("{k}: {e}")))?);
        }
        let auth_header = compat
            .auth_header
            .as_deref()
            .map(|h| HeaderName::from_bytes(h.as_bytes()).map_err(|e| bad("compat.auth_header", e.to_string())))
            .transpose()?;
        let query = def.query.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");
        let empty = r#"{"models": []}"#;
        let builtin = match &def.model_table {
            Some(ModelTableSpec::Inline(v)) => v.to_string(),
            Some(ModelTableSpec::Named(n)) => match n.as_str() {
                "openai" => include_str!("openai.json").to_owned(),
                "kimi" => include_str!("kimi.json").to_owned(),
                "empty" => empty.to_owned(),
                other => return Err(bad("model_table", format!("no built-in table called {other}"))),
            },
            None => empty.to_owned(),
        };
        let prefixes: Vec<&str> = def.prefixes.iter().map(String::as_str).collect();
        Ok(Self {
            id,
            wire,
            base: base.to_owned(),
            extra_headers,
            query,
            auth_header,
            table: Mutex::new(ModelTable::load(id, &builtin, cfg, &prefixes, compat.open_aliases)?),
            compat,
            cache_dir: cfg.cache_dir.clone(),
            http: Http::new(),
        })
    }

    /// `base/path`, plus the definition's query parameters.
    fn url(&self, path: &str) -> String {
        match self.query.is_empty() {
            true => format!("{}/{path}", self.base),
            false => format!("{}/{path}?{}", self.base, self.query),
        }
    }

    fn table(&self) -> std::sync::MutexGuard<'_, ModelTable> {
        self.table.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The definition's headers, then the key: `Authorization: Bearer`, or the definition's own
    /// header (`api-key` on Azure), unless the account needs no key.
    fn headers(&self, creds: &Credential) -> Result<HeaderMap, ProviderError> {
        let mut h = self.extra_headers.clone();
        if creds.kind != AuthMethodKind::None {
            match &self.auth_header {
                Some(name) => {
                    h.insert(name.clone(), sensitive(creds.secret.expose())?);
                }
                None => {
                    let value = Zeroizing::new(format!("Bearer {}", creds.secret.expose()));
                    h.insert(AUTHORIZATION, sensitive(&value)?);
                }
            }
        }
        Ok(h)
    }

    /// A 429 that is a spent quota will not pass by waiting.
    fn not_retryable_quota(mut e: ProviderError) -> ProviderError {
        if e.kind == ProviderErrorKind::RateLimit && e.message.contains("exceeded your current quota") {
            e.retryable = false;
        }
        e
    }

    async fn list_models(&self, creds: &Credential) -> Result<(Vec<ModelId>, Option<String>), ProviderError> {
        let url = self.url("models");
        let headers = self.headers(creds)?;
        let resp = self
            .http
            .send(&CancellationToken::new(), |c| c.get(&url).headers(headers))
            .await
            .map_err(Self::not_retryable_quota)?;
        let org = resp
            .headers()
            .get("openai-organization")
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
impl Provider for OpenAi {
    fn id(&self) -> ProviderId {
        ProviderId(self.id)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            tools: true,
            parallel_tool_calls: true,
            streaming: true,
            thinking: false,
            prompt_cache: self.compat.prompt_cache,
            logprobs: self.compat.logprobs,
            effort: self.compat.effort,
            images: self.compat.images,
            system_role: SystemRole::Separate,
            max_tools: self.compat.max_tools,
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        self.table().models()
    }

    async fn refresh_models(&self, creds: &Credential) -> Result<Vec<ModelInfo>, ProviderError> {
        let (ids, _) = self.list_models(creds).await?;
        // A local server lists what it has and says nothing of sizes; these are only a start,
        // and `providers/<id>/models.json` is where to put the real numbers.
        let (context_window, max_output) = if self.compat.open_aliases { (32_768, 4_096) } else { (128_000, 8_192) };
        Ok(self.table().refreshed(
            self.id,
            ids,
            |id| ModelInfo {
                id: id.clone(),
                context_window,
                max_output,
                family: id.0.split(['-', ':']).next().unwrap_or_default().into(),
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
        let (price, reasoning) = {
            let t = self.table();
            // A model that reasons takes `reasoning_effort` and refuses `temperature`. The table
            // says so for the ones it lists, the name for the ones released since.
            let reasoning = t.entry(&req.model).map_or_else(
                || ["gpt-5", "o1", "o3", "o4"].iter().any(|p| req.model.0.starts_with(p)),
                |e| e.effort,
            );
            (t.price(&req.model), reasoning)
        };
        let opts = wire::Opts {
            completion_tokens: self.compat.completion_tokens,
            reasoning,
            user: self.compat.user,
            keep_reasoning: self.compat.keep_reasoning,
            logprobs: req.logprobs,
        };
        let (path, body) = match self.wire {
            Wire::Chat => ("chat/completions", wire::chat_body(&req, opts)),
            Wire::Responses => ("responses", wire::responses_body(&req, opts)),
        };
        tracing::debug!(account = %creds.account, model = %req.model, provider = self.id, "openai: stream");

        let url = self.url(path);
        let headers = self.headers(creds)?;
        let resp = self
            .http
            .send(&cancel, |c| {
                c.post(&url)
                    .headers(headers)
                    .header(CONTENT_TYPE, "application/json")
                    .header(ACCEPT, "text/event-stream")
                    .body(body.to_string())
            })
            .await
            .map_err(Self::not_retryable_quota)?;
        let request_id = request_id(&resp);

        let mut events = Events::new(resp, cancel.clone());
        let mut turn = stream::Turn::new(price);
        while let Some(ev) = events.next().await? {
            let chunks = match self.wire {
                Wire::Chat => turn.chat_event(&ev.data)?,
                Wire::Responses => turn.responses_event(&ev.data)?,
            };
            for chunk in chunks {
                emit(&out, &cancel, chunk).await?;
            }
        }
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        for chunk in turn.close()? {
            emit(&out, &cancel, chunk).await?;
        }
        turn.into_response(request_id)
    }

    /// OpenAI has no counting endpoint: the caller uses `estimate_tokens`.
    async fn count_tokens(
        &self,
        _: &Credential,
        _: &ChatRequest,
    ) -> Option<Result<u64, ProviderError>> {
        None
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
