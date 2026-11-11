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
use aibus_provider::{
    AccountInfo, AliasTable, AuthMethodKind, Capabilities, ChatRequest, ChatResponse, ConfigError,
    Credential, ModelId, ModelInfo, ModelRef, Price, Provider, ProviderConfig, ProviderError,
    ProviderFactory, ProviderId, StreamChunk, SystemRole,
};
use aibus_provider_http::reqwest::header::{HeaderMap, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use aibus_provider_http::{cancelled, emit, json, rate_limit, request_id, sensitive, Events, Http};
use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Flavor {
    OpenAi,
    Kimi,
    Compatible,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Api {
    Chat,
    Responses,
}

impl Flavor {
    fn id(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Kimi => "kimi",
            Self::Compatible => "openai-compatible",
        }
    }

    /// `None` for a server only the account can name.
    fn default_base(self) -> Option<&'static str> {
        match self {
            Self::OpenAi => Some("https://api.openai.com/v1"),
            Self::Kimi => Some("https://api.moonshot.ai/v1"),
            Self::Compatible => None,
        }
    }

    /// The built-in table, the ids that resolve without being listed, and whether any alias
    /// target does.
    fn table(self) -> (&'static str, &'static [&'static str], bool) {
        match self {
            Self::OpenAi => (
                include_str!("../../../catalog/openai.json"),
                &["gpt-", "chatgpt-", "o1", "o3", "o4"],
                false,
            ),
            Self::Kimi => (include_str!("../../../catalog/kimi.json"), &["kimi-", "moonshot-"], false),
            Self::Compatible => (r#"{"models": []}"#, &[], true),
        }
    }

    fn capabilities(self) -> Capabilities {
        let openai = self == Self::OpenAi;
        Capabilities {
            tools: true,
            parallel_tool_calls: true,
            streaming: true,
            thinking: false,
            // OpenAI and Moonshot cache prefixes on their own; a local server does not.
            prompt_cache: self != Self::Compatible,
            logprobs: self != Self::Kimi,
            effort: openai,
            images: openai,
            system_role: SystemRole::Separate,
            max_tools: (self != Self::Compatible).then_some(128),
        }
    }
}

pub struct Factory(Flavor);

impl Factory {
    pub fn openai() -> Self {
        Self(Flavor::OpenAi)
    }

    pub fn kimi() -> Self {
        Self(Flavor::Kimi)
    }

    /// Needs a `base_url` on the account; `auth = none` is allowed.
    pub fn compatible() -> Self {
        Self(Flavor::Compatible)
    }
}

impl ProviderFactory for Factory {
    fn id(&self) -> ProviderId {
        ProviderId(self.0.id())
    }

    fn build(&self, cfg: &ProviderConfig) -> Result<Arc<dyn Provider>, ConfigError> {
        Ok(Arc::new(OpenAi::new(self.0, cfg)?))
    }

    fn auth_methods(&self) -> &'static [AuthMethodKind] {
        match self.0 {
            Flavor::Compatible => &[
                AuthMethodKind::ApiKey,
                AuthMethodKind::Bearer,
                AuthMethodKind::Env,
                AuthMethodKind::None,
            ],
            _ => &[AuthMethodKind::ApiKey, AuthMethodKind::Bearer, AuthMethodKind::Env],
        }
    }
}

pub struct OpenAi {
    flavor: Flavor,
    api: Api,
    /// No trailing slash; `/chat/completions` and friends go after it.
    base: String,
    table: Mutex<ModelTable>,
    cache_dir: Option<PathBuf>,
    http: Http,
    /// The gateway puts what a request cost in `usage.cost` (OpenRouter), and that replaces the
    /// table's estimate.
    reports_cost: bool,
}

impl OpenAi {
    fn new(flavor: Flavor, cfg: &ProviderConfig) -> Result<Self, ConfigError> {
        let id = flavor.id();
        let base = match (cfg.base_url.as_deref(), flavor.default_base()) {
            (Some(url), _) => url,
            (None, Some(url)) => url,
            (None, None) => {
                return Err(ConfigError::NotConfigured(
                    id.into(),
                    "base_url is required: set it on the account, e.g. http://127.0.0.1:11434/v1".into(),
                ))
            }
        }
        .trim_end_matches('/');
        if !(base.starts_with("https://") || base.starts_with("http://")) {
            return Err(ConfigError::Invalid(id.into(), "base_url must start with https:// or http://".into()));
        }
        let api = match (cfg.api.as_deref(), flavor) {
            (None | Some("chat"), _) => Api::Chat,
            (Some("responses"), Flavor::Kimi) => {
                return Err(ConfigError::Invalid(id.into(), "kimi has no Responses API; use api = \"chat\"".into()))
            }
            (Some("responses"), _) => Api::Responses,
            (Some(other), _) => {
                return Err(ConfigError::Invalid(id.into(), format!("api \"{other}\" is not \"chat\" or \"responses\"")))
            }
        };
        let (builtin, prefixes, open_aliases) = flavor.table();
        Ok(Self {
            flavor,
            api,
            base: base.to_owned(),
            table: Mutex::new(ModelTable::load(id, builtin, cfg, prefixes, open_aliases)?),
            cache_dir: cfg.cache_dir.clone(),
            http: Http::new(),
            reports_cost: flavor == Flavor::Compatible && base.contains("openrouter.ai"),
        })
    }

    fn table(&self) -> std::sync::MutexGuard<'_, ModelTable> {
        self.table.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `Authorization: Bearer`, unless the account needs no key.
    fn headers(creds: &Credential) -> Result<HeaderMap, ProviderError> {
        let mut h = HeaderMap::new();
        if creds.kind != AuthMethodKind::None {
            let value = Zeroizing::new(format!("Bearer {}", creds.secret.expose()));
            h.insert(AUTHORIZATION, sensitive(&value)?);
        }
        Ok(h)
    }

    async fn list_models(&self, creds: &Credential) -> Result<(Vec<ModelId>, Option<String>), ProviderError> {
        let url = format!("{}/models", self.base);
        let headers = Self::headers(creds)?;
        let resp = self
            .http
            .send(&CancellationToken::new(), |c| c.get(&url).headers(headers))
            .await?;
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
        ProviderId(self.flavor.id())
    }

    fn capabilities(&self) -> Capabilities {
        self.flavor.capabilities()
    }

    fn models(&self) -> Vec<ModelInfo> {
        self.table().models()
    }

    async fn refresh_models(&self, creds: &Credential) -> Result<Vec<ModelInfo>, ProviderError> {
        let (ids, _) = self.list_models(creds).await?;
        // A local server lists what it has and says nothing of sizes; these are only a start,
        // and `providers/<id>/models.json` is where to put the real numbers.
        let (context_window, max_output) = if self.flavor == Flavor::Compatible { (32_768, 4_096) } else { (128_000, 8_192) };
        Ok(self.table().refreshed(
            self.flavor.id(),
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
            completion_tokens: self.flavor == Flavor::OpenAi,
            reasoning,
            user: self.flavor != Flavor::Compatible,
            keep_reasoning: self.flavor == Flavor::Kimi,
            logprobs: req.logprobs,
            include_cost: self.reports_cost,
        };
        let (path, body) = match self.api {
            Api::Chat => ("chat/completions", wire::chat_body(&req, opts)),
            Api::Responses => ("responses", wire::responses_body(&req, opts)),
        };
        tracing::debug!(account = %creds.account, model = %req.model, provider = self.flavor.id(), "openai: stream");

        let url = format!("{}/{path}", self.base);
        let headers = Self::headers(creds)?;
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
        let window = rate_limit(resp.headers(), SystemTime::now());

        let mut events = Events::new(resp, cancel.clone());
        let mut turn = stream::Turn::new(price, self.reports_cost);
        while let Some(ev) = events.next().await? {
            let chunks = match self.api {
                Api::Chat => turn.chat_event(&ev.data)?,
                Api::Responses => turn.responses_event(&ev.data)?,
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
        let mut done = turn.into_response(request_id)?;
        done.rate_limit = window;
        Ok(done)
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
