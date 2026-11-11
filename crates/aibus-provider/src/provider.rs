//! The `Provider` trait, its capability and request types, and the registry (04 sections 2 and 3.1).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::error::{ConfigError, ProviderError};
use crate::secret::{AuthMethodKind, Credential};
use crate::presets::ProviderDef;
use crate::types::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum SystemRole {
    /// The API has a separate system field.
    Separate,
    /// No system role: the prompt is folded into the first message.
    FirstMessage,
}

/// What a provider can do, so core can degrade instead of guessing.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Capabilities {
    pub tools: bool,
    pub parallel_tool_calls: bool,
    pub streaming: bool,
    pub thinking: bool,
    pub prompt_cache: bool,
    /// The router prefers providers with this.
    pub logprobs: bool,
    pub effort: bool,
    pub images: bool,
    pub system_role: SystemRole,
    pub max_tools: Option<u32>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: ModelId,
    pub context_window: u32,
    pub max_output: u32,
    pub family: SmolStr,
    #[serde(default)]
    pub deprecated: bool,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Price {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    #[serde(default)]
    pub cache_read_per_mtok: Option<f64>,
    #[serde(default)]
    pub cache_write_per_mtok: Option<f64>,
}

impl Price {
    /// What `u` cost, or `None` when it used a cache bucket this price has no rate for: an
    /// unknown price is "n/a", never a guess (Q25). `u.input_tokens` excludes the cache buckets.
    pub fn cost_usd(&self, u: &Usage) -> Option<f64> {
        let per = |tokens: u64, rate: Option<f64>| match (tokens, rate) {
            (0, _) => Some(0.0),
            (n, Some(r)) => Some(n as f64 * r),
            (_, None) => None,
        };
        let total = per(u.input_tokens, Some(self.input_per_mtok))?
            + per(u.output_tokens, Some(self.output_per_mtok))?
            + per(u.cache_read, self.cache_read_per_mtok)?
            + per(u.cache_write, self.cache_write_per_mtok)?;
        Some(total / 1_000_000.0)
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AccountInfo {
    pub label: String,
    pub org: Option<String>,
    pub models: Vec<ModelId>,
}

/// Provider-neutral request. Credentials are never part of it.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: ModelId,
    pub system: String,
    /// 02 section 5.
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub tool_choice: ToolChoice,
    pub max_output_tokens: u32,
    pub temperature: Option<f32>,
    pub effort: Option<Effort>,
    pub thinking: Option<ThinkingBudget>,
    pub cache_hint: CacheHint,
    pub logprobs: bool,
    pub metadata: RequestMetadata,
}

#[derive(Clone, PartialEq, Debug)]
pub enum StreamChunk {
    TextDelta(String),
    ThinkingDelta(String),
    ToolUseStart {
        id: ToolCallId,
        name: SmolStr,
    },
    ToolUseInputDelta {
        id: ToolCallId,
        json_fragment: String,
    },
    ToolUseEnd {
        id: ToolCallId,
    },
    Usage(Usage),
    Logprob {
        token: String,
        logprob: f32,
    },
    Done(StopReason),
    /// The account's rate limit holds the request back: "waiting for work slot". Empty once it
    /// is let through. Sent by the gate (`limits::Gated`), never by a provider.
    Waiting(String),
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Refusal,
    Other(SmolStr),
}

#[derive(Clone, PartialEq, Debug)]
pub struct ChatResponse {
    pub message: Message,
    pub usage: Usage,
    pub stop: StopReason,
    pub request_id: Option<String>,
}

#[async_trait]
pub trait Provider: Send + Sync + 'static {
    /// Stable id used in config: "anthropic", "openai", "fake". Lower-case, no spaces.
    fn id(&self) -> ProviderId;

    /// What this provider can do, so core can degrade instead of guessing.
    fn capabilities(&self) -> Capabilities;

    /// Known models for alias resolution and context sizes. May be a static table
    /// refreshed by `refresh_models` when the API supports listing.
    fn models(&self) -> Vec<ModelInfo>;
    async fn refresh_models(&self, creds: &Credential) -> Result<Vec<ModelInfo>, ProviderError>;

    /// Map a model reference to this provider's native id, or say it cannot.
    fn resolve_model(&self, r: &ModelRef, aliases: &AliasTable) -> Option<ModelId>;

    /// One streaming completion. Chunks go to `out`; the final response is returned.
    ///
    /// Must honour `cancel` within 100 ms, returning `ProviderErrorKind::Cancelled`, whether
    /// it is waiting on the network, on a full `out` channel or on a delay. Must never log
    /// `creds`, request headers or anything derived from the secret.
    async fn stream(
        &self,
        creds: &Credential,
        req: ChatRequest,
        out: mpsc::Sender<StreamChunk>,
        cancel: CancellationToken,
    ) -> Result<ChatResponse, ProviderError>;

    /// Token count for budgeting; `None` when the provider has no endpoint and
    /// core should use the heuristic.
    async fn count_tokens(
        &self,
        creds: &Credential,
        req: &ChatRequest,
    ) -> Option<Result<u64, ProviderError>>;

    /// A cheap authenticated call. Used by `aibus account test`.
    async fn verify(&self, creds: &Credential) -> Result<AccountInfo, ProviderError>;

    /// Pricing per model, for cost estimates. Static table; `None` when unknown.
    fn price(&self, model: &ModelId) -> Option<Price>;

    /// The request's size in tokens when `count_tokens` has no endpoint to ask: the
    /// 4-characters-per-token heuristic with a 10 % margin, using this provider's per-family
    /// overrides where it has them (Q20).
    fn estimate_tokens(&self, req: &ChatRequest) -> u64 {
        crate::tokens::estimate(req, crate::tokens::CHARS_PER_TOKEN)
    }
}

/// What a factory gets to build a provider from: the settings of the accounts that use it, and
/// where the model tables live.
#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub struct ProviderConfig {
    pub base_url: Option<String>,
    /// The wire dialect for providers that have more than one: "chat" or "responses".
    pub api: Option<String>,
    /// `~/.cache/aibus/models`: `<provider>.json` is the last refreshed model list.
    pub cache_dir: Option<PathBuf>,
    /// `~/.config/aibus/providers`: `<provider>/models.json` overrides the built-in table.
    pub overrides_dir: Option<PathBuf>,
    /// The resolved definition this instance is built from (a preset, a `[[provider]]` table or
    /// both). When it is set it wins over `base_url` and `api` above, which are the old
    /// account-level settings a factory falls back to when it is built without one.
    pub def: Option<ProviderDef>,
}

pub trait ProviderFactory: Send + Sync {
    fn id(&self) -> ProviderId;
    fn build(&self, cfg: &ProviderConfig) -> Result<Arc<dyn Provider>, ConfigError>;
    /// Which `auth` values this provider accepts.
    fn auth_methods(&self) -> &'static [AuthMethodKind];
    /// The wire dialects (`Api::as_str`) this factory can build an instance of from a
    /// definition. A factory that serves none is only built as itself, from its own id.
    fn apis(&self) -> &'static [&'static str] {
        &[]
    }
}

/// Built providers by id. Constructed from the binary's explicit factory list.
pub struct ProviderRegistry {
    providers: BTreeMap<&'static str, Arc<dyn Provider>>,
}

impl ProviderRegistry {
    /// Builds one instance per definition in `configs` (an entry with `def` set, keyed by
    /// provider id) with the factory that serves its `api`, then every factory as itself with
    /// its entry in `configs`, or the default config when it has none. An instance from a
    /// definition wins over a factory's own of the same id; a definition no factory serves is
    /// left to the factory of the same id. Two factories with one id is a config error.
    pub fn new(
        factories: &[Box<dyn ProviderFactory>],
        configs: &BTreeMap<String, ProviderConfig>,
    ) -> Result<Self, ConfigError> {
        let mut providers = BTreeMap::new();
        let mut unserved = std::collections::BTreeSet::new();
        for (id, cfg) in configs {
            let Some(def) = &cfg.def else { continue };
            let api = def
                .api
                .ok_or_else(|| ConfigError::Invalid(id.clone(), "the definition has no api".into()))?
                .as_str();
            // No adapter for this api (a test registry of stand-ins): the factory of the same
            // id, if there is one, builds from the config as it did before definitions.
            match factories.iter().find(|f| f.apis().contains(&api)) {
                Some(factory) => {
                    providers.insert(def.static_id(), factory.build(cfg)?);
                }
                None => {
                    unserved.insert(id.as_str());
                }
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for f in factories {
            let id = f.id().0;
            if !seen.insert(id) {
                return Err(ConfigError::DuplicateProvider(id.to_owned()));
            }
            if providers.contains_key(id) {
                continue;
            }
            let cfg = configs.get(id).filter(|c| c.def.is_none() || unserved.contains(id)).cloned();
            // A provider that needs settings (`openai-compatible` needs a base URL) is left out
            // while no account uses it, instead of failing everyone else's startup.
            let built = match f.build(&cfg.clone().unwrap_or_default()) {
                Err(ConfigError::NotConfigured(..)) if cfg.is_none() => continue,
                built => built?,
            };
            providers.insert(id, built);
        }
        Ok(Self { providers })
    }

    pub fn get(&self, id: &str) -> Result<Arc<dyn Provider>, ConfigError> {
        self.providers
            .get(id)
            .cloned()
            .ok_or_else(|| ConfigError::UnknownProvider(id.to_owned()))
    }

    pub fn ids(&self) -> Vec<&'static str> {
        self.providers.keys().copied().collect()
    }
}
