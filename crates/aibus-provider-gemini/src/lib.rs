//! Google's native Gemini API (`gemini-generate-content`): `streamGenerateContent`, the model
//! list and `countTokens`. The OpenAI shim preset (`gemini-openai`) covers plain chat; this is
//! for what the shim drops: thought signatures, thought summaries and a model table with prices.
//!
//! The key reaches the wire only as the sensitive `x-goog-api-key` header of the `&Credential`
//! a call is handed, never in a URL, and nothing here logs a header, a body or the credential.
//! Vertex for Gemini is a different URL and token source and is not served here.
#![forbid(unsafe_code)]

mod stream;
mod wire;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use aibus_provider::models::ModelTable;
use aibus_provider::presets::resolve;
use aibus_provider::{
    AccountInfo, AliasTable, Api, AuthMethodKind, Capabilities, ChatRequest, ChatResponse,
    ConfigError, Credential, ModelId, ModelInfo, ModelRef, ModelTableSpec, Price, Provider,
    ProviderConfig, ProviderDef, ProviderError, ProviderErrorKind, ProviderFactory, ProviderId,
    StreamChunk, SystemRole,
};
use aibus_provider_http::reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE,
};
use aibus_provider_http::{cancelled, emit, json, rate_limit, request_id, sensitive, Events, Http};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub struct Factory;

impl ProviderFactory for Factory {
    fn id(&self) -> ProviderId {
        ProviderId("gemini")
    }

    fn build(&self, cfg: &ProviderConfig) -> Result<Arc<dyn Provider>, ConfigError> {
        let def = match &cfg.def {
            Some(def) => def.clone(),
            None => {
                let user = ProviderDef {
                    id: "gemini".into(),
                    base_url: cfg.base_url.clone(),
                    ..ProviderDef::default()
                };
                resolve("gemini", Some(&user))
                    .map_err(|e| ConfigError::Invalid("gemini".into(), e))?
                    .ok_or_else(|| ConfigError::UnknownProvider("gemini".into()))?
            }
        };
        Ok(Arc::new(Gemini::new(def, cfg)?))
    }

    fn auth_methods(&self) -> &'static [AuthMethodKind] {
        &[
            AuthMethodKind::ApiKey,
            AuthMethodKind::Bearer,
            AuthMethodKind::Env,
        ]
    }

    fn apis(&self) -> &'static [&'static str] {
        &["gemini-generate-content"]
    }
}

pub struct Gemini {
    id: &'static str,
    /// No trailing slash; `/v1beta/...` goes after it.
    base: String,
    /// From the definition: sent on every request.
    extra_headers: HeaderMap,
    table: Mutex<ModelTable>,
    cache_dir: Option<PathBuf>,
    http: Http,
}

impl Gemini {
    fn new(def: ProviderDef, cfg: &ProviderConfig) -> Result<Self, ConfigError> {
        let id = def.static_id();
        let bad = |what: &str, e: String| ConfigError::Invalid(id.into(), format!("{what}: {e}"));
        if def.api != Some(Api::GeminiGenerateContent) {
            return Err(ConfigError::Invalid(
                id.into(),
                format!("api {:?} is not gemini-generate-content", def.api),
            ));
        }
        let base = def
            .base_url
            .as_deref()
            .ok_or_else(|| {
                ConfigError::NotConfigured(
                    id.into(),
                    "base_url is required, e.g. https://generativelanguage.googleapis.com".into(),
                )
            })?
            .trim_end_matches('/');
        if !(base.starts_with("https://") || base.starts_with("http://")) {
            return Err(ConfigError::Invalid(
                id.into(),
                "base_url must start with https:// or http://".into(),
            ));
        }
        let mut extra_headers = HeaderMap::new();
        for (k, v) in &def.headers {
            let name = HeaderName::from_bytes(k.as_bytes())
                .map_err(|e| bad("headers", format!("{k}: {e}")))?;
            extra_headers.insert(
                name,
                HeaderValue::from_str(v).map_err(|e| bad("headers", format!("{k}: {e}")))?,
            );
        }
        let builtin = match &def.model_table {
            Some(ModelTableSpec::Inline(v)) => v.to_string(),
            Some(ModelTableSpec::Named(n)) => aibus_provider::catalog::table(n)
                .ok_or_else(|| bad("model_table", format!("no built-in table called {n}")))?
                .to_owned(),
            None => aibus_provider::catalog::table("empty")
                .unwrap_or_default()
                .to_owned(),
        };
        let prefixes: Vec<&str> = def.prefixes.iter().map(String::as_str).collect();
        Ok(Self {
            id,
            base: base.to_owned(),
            extra_headers,
            table: Mutex::new(ModelTable::load(
                id,
                &builtin,
                cfg,
                &prefixes,
                def.open_aliases.unwrap_or(false),
            )?),
            cache_dir: cfg.cache_dir.clone(),
            http: Http::new(),
        })
    }

    fn table(&self) -> std::sync::MutexGuard<'_, ModelTable> {
        self.table.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The definition's headers, then the key in `x-goog-api-key`.
    fn headers(&self, creds: &Credential) -> Result<HeaderMap, ProviderError> {
        let mut h = self.extra_headers.clone();
        h.insert(
            HeaderName::from_static("x-goog-api-key"),
            sensitive(creds.secret.expose())?,
        );
        Ok(h)
    }

    /// `base/v1beta/models/<model>:<method>`. The id goes into a path, so anything but the
    /// characters Google's ids use is refused rather than escaped.
    fn model_url(&self, model: &ModelId, method: &str) -> Result<String, ProviderError> {
        let id = model.0.strip_prefix("models/").unwrap_or(&model.0);
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidRequest,
                format!("{id:?} is not a Gemini model id"),
            ));
        }
        Ok(format!("{}/v1beta/models/{id}:{method}", self.base))
    }

    /// Model ids with the sizes the API reports for each (`inputTokenLimit`, `outputTokenLimit`).
    /// Only models that can `generateContent` are kept: the list also has embedding models.
    async fn list_models(
        &self,
        creds: &Credential,
    ) -> Result<Vec<(ModelId, Option<(u32, u32)>)>, ProviderError> {
        let url = format!("{}/v1beta/models?pageSize=1000", self.base);
        let headers = self.headers(creds)?;
        let resp = self
            .http
            .send(&CancellationToken::new(), |c| c.get(&url).headers(headers))
            .await?;
        let body = json(resp).await?;
        Ok(body["models"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| {
                m["supportedGenerationMethods"]
                    .as_array()
                    .is_none_or(|a| a.iter().any(|x| x == "generateContent"))
            })
            .filter_map(|m| {
                let id = m["name"].as_str()?;
                let sizes = (
                    m["inputTokenLimit"].as_u64(),
                    m["outputTokenLimit"].as_u64(),
                );
                let sizes = match sizes {
                    (Some(i), Some(o)) => Some((
                        u32::try_from(i).unwrap_or(u32::MAX),
                        u32::try_from(o).unwrap_or(u32::MAX),
                    )),
                    _ => None,
                };
                Some((
                    ModelId(id.strip_prefix("models/").unwrap_or(id).into()),
                    sizes,
                ))
            })
            .collect())
    }

    /// Whether the model takes `thinkingConfig`: everything but the 2.0 and 1.x families, which
    /// refuse it. The name says so for models released after the table was written too.
    fn reasoning(&self, model: &ModelId) -> bool {
        !["gemini-1.", "gemini-2.0"]
            .iter()
            .any(|p| model.0.starts_with(p))
    }
}

#[async_trait]
impl Provider for Gemini {
    fn id(&self) -> ProviderId {
        ProviderId(self.id)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            tools: true,
            parallel_tool_calls: true,
            streaming: true,
            thinking: true,
            // Caching is implicit; there is no breakpoint to place.
            prompt_cache: false,
            logprobs: false,
            effort: false,
            images: true,
            system_role: SystemRole::Separate,
            max_tools: None,
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        self.table().models()
    }

    async fn refresh_models(&self, creds: &Credential) -> Result<Vec<ModelInfo>, ProviderError> {
        let listed = self.list_models(creds).await?;
        let sizes = |id: &ModelId| {
            listed
                .iter()
                .find(|(x, _)| x == id)
                .and_then(|(_, s)| *s)
                .unwrap_or((1_048_576, 8_192))
        };
        let ids = listed.iter().map(|(id, _)| id.clone()).collect();
        Ok(self.table().refreshed(
            self.id,
            ids,
            |id| {
                let (context_window, max_output) = sizes(id);
                ModelInfo {
                    id: id.clone(),
                    context_window,
                    max_output,
                    family: "gemini".into(),
                    deprecated: false,
                }
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
        let price = self.table().price(&req.model);
        let body = wire::body(&req, self.reasoning(&req.model));
        tracing::debug!(account = %creds.account, model = %req.model, provider = self.id, "gemini: stream");

        let url = format!(
            "{}?alt=sse",
            self.model_url(&req.model, "streamGenerateContent")?
        );
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
            .await?;
        let request_id = request_id(&resp);
        let window = rate_limit(resp.headers(), SystemTime::now());

        let mut events = Events::new(resp, cancel.clone());
        let mut turn = stream::Turn::new(price);
        while let Some(ev) = events.next().await? {
            for chunk in turn.event(&ev.data)? {
                emit(&out, &cancel, chunk).await?;
            }
        }
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        for chunk in turn.close()? {
            emit(&out, &cancel, chunk).await?;
        }
        let mut done = turn.into_response(request_id);
        done.rate_limit = window;
        Ok(done)
    }

    async fn count_tokens(
        &self,
        creds: &Credential,
        req: &ChatRequest,
    ) -> Option<Result<u64, ProviderError>> {
        let counted = async {
            let url = self.model_url(&req.model, "countTokens")?;
            let headers = self.headers(creds)?;
            let mut inner = wire::body(
                &self.capabilities().degrade(req.clone()),
                self.reasoning(&req.model),
            );
            inner["model"] = json!(format!(
                "models/{}",
                req.model.0.strip_prefix("models/").unwrap_or(&req.model.0)
            ));
            let body = json!({"generateContentRequest": inner});
            let resp = self
                .http
                .send(&CancellationToken::new(), |c| {
                    c.post(&url)
                        .headers(headers)
                        .header(CONTENT_TYPE, "application/json")
                        .body(body.to_string())
                })
                .await?;
            let answer: Value = json(resp).await?;
            answer["totalTokens"].as_u64().ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::Unknown,
                    "countTokens answered without totalTokens",
                )
            })
        };
        Some(counted.await)
    }

    async fn verify(&self, creds: &Credential) -> Result<AccountInfo, ProviderError> {
        let models = self
            .list_models(creds)
            .await?
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        Ok(AccountInfo {
            label: creds.account.to_string(),
            org: None,
            models,
            cost_usd: None,
        })
    }

    fn price(&self, model: &ModelId) -> Option<Price> {
        self.table().price(model)
    }

    fn estimate_tokens(&self, req: &ChatRequest) -> u64 {
        aibus_provider::tokens::estimate(req, self.table().chars_per_token(&req.model))
    }
}
