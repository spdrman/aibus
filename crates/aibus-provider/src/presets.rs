//! Provider definitions: the vendor as data, the wire as code (issue #110).
//!
//! A [`ProviderDef`] says which wire dialect (`api`) a provider speaks, where it lives, what
//! its key is called in the environment, which model table it starts from and which
//! compatibility quirks it has. The built-in ones are one reviewed file, `presets.json`; a
//! `[[provider]]` table in the user's config overrides a preset of the same id field by field,
//! or defines a server the presets do not know. The registry builds one provider instance per
//! id, so two Ollama servers are two ids and never conflict.
//!
//! Nothing in a definition is a secret: `headers` are rejected when they look like
//! credentials, the same way `auth.key` is rejected in an account.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

/// The wire dialects the adapters speak. Gemini, Bedrock and Vertex come later in the epic.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Api {
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
    #[serde(rename = "openai-chat")]
    OpenaiChat,
    #[serde(rename = "openai-responses")]
    OpenaiResponses,
}

impl Api {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AnthropicMessages => "anthropic-messages",
            Self::OpenaiChat => "openai-chat",
            Self::OpenaiResponses => "openai-responses",
        }
    }
}

/// Where a provider's model table starts: the name of a table an adapter ships (`openai`,
/// `kimi`, `anthropic`, `empty`) or the table itself, in the shape of `models.json`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ModelTableSpec {
    Named(String),
    Inline(serde_json::Value),
}

/// The ways an OpenAI-shaped server differs from OpenAI. These vary together per vendor.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Compat {
    /// `max_completion_tokens` instead of `max_tokens`.
    pub completion_tokens: bool,
    /// Send the run and session ids as `user`.
    pub user: bool,
    /// Send earlier reasoning back as `reasoning_content`.
    pub keep_reasoning: bool,
    pub logprobs: bool,
    pub effort: bool,
    pub images: bool,
    pub prompt_cache: bool,
    pub max_tools: Option<u32>,
    /// An alias target resolves whatever it is: the server names its own models.
    pub open_aliases: bool,
    /// The server has a Responses API; `api = "openai-responses"` is refused when it does not.
    pub responses: bool,
    /// The key goes in this header as it is, instead of `Authorization: Bearer` (Azure's `api-key`).
    pub auth_header: Option<String>,
}

impl Default for Compat {
    fn default() -> Self {
        Self {
            completion_tokens: false,
            user: false,
            keep_reasoning: false,
            logprobs: false,
            effort: false,
            images: false,
            prompt_cache: false,
            max_tools: None,
            open_aliases: false,
            responses: true,
            auth_header: None,
        }
    }
}

/// One provider. In a preset or a `[[provider]]` table every field but `id` may be left out;
/// [`resolve`] fills what is missing from the preset of the same id, and checks the result.
#[derive(Clone, PartialEq, Eq, Default, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderDef {
    pub id: String,
    pub api: Option<Api>,
    /// `None` when only the user can say (Azure, a bare compatible server).
    pub base_url: Option<String>,
    /// Environment variables that usually hold the key, first one preferred.
    pub env_vars: Vec<String>,
    /// `api_key`, `bearer`, `env`, `none`.
    pub auth_methods: Vec<String>,
    pub model_table: Option<ModelTableSpec>,
    /// A full model id with one of these prefixes resolves even when no table lists it.
    pub prefixes: Vec<String>,
    pub compat: Option<Compat>,
    /// Extra request headers. Never a secret.
    pub headers: BTreeMap<String, String>,
    /// Extra query parameters on every request (`api-version` on Azure).
    pub query: BTreeMap<String, String>,
}

impl ProviderDef {
    pub fn compat(&self) -> Compat {
        self.compat.clone().unwrap_or_default()
    }

    /// The id as a `&'static str`, which `ProviderId` needs. A config names a handful of
    /// ids once at startup, so each distinct id is leaked once.
    pub fn static_id(&self) -> &'static str {
        static IDS: Mutex<BTreeSet<&'static str>> = Mutex::new(BTreeSet::new());
        let mut ids = IDS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(known) = ids.get(self.id.as_str()) {
            return *known;
        }
        let leaked: &'static str = Box::leak(self.id.clone().into_boxed_str());
        ids.insert(leaked);
        leaked
    }

    /// `user` on top of `self`: what the user set wins, headers and query merge.
    fn overlaid(mut self, user: &ProviderDef) -> Self {
        self.id.clone_from(&user.id);
        self.api = user.api.or(self.api);
        if user.base_url.is_some() {
            self.base_url.clone_from(&user.base_url);
        }
        if !user.env_vars.is_empty() {
            self.env_vars.clone_from(&user.env_vars);
        }
        if !user.auth_methods.is_empty() {
            self.auth_methods.clone_from(&user.auth_methods);
        }
        if user.model_table.is_some() {
            self.model_table.clone_from(&user.model_table);
        }
        if !user.prefixes.is_empty() {
            self.prefixes.clone_from(&user.prefixes);
        }
        if user.compat.is_some() {
            self.compat.clone_from(&user.compat);
        }
        self.headers.extend(user.headers.clone());
        self.query.extend(user.query.clone());
        self
    }

    /// What is wrong with this definition on its own, as `(field, message)`.
    pub fn problems(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        if self.id.is_empty() || !self.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            out.push(("id", "an id is letters, digits, `-` and `_`".to_owned()));
        }
        if let Some(url) = &self.base_url {
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                out.push(("base_url", "must start with https:// or http://".to_owned()));
            }
        }
        for name in self.headers.keys().chain(self.query.keys()) {
            let lower = name.to_ascii_lowercase();
            if ["auth", "key", "token", "secret", "password", "cookie", "credential"].iter().any(|w| lower.contains(w)) {
                out.push((
                    "headers",
                    format!("{name} looks like a credential; credentials do not go in config, run: aibus account add <name> --key-stdin"),
                ));
            }
        }
        for m in &self.auth_methods {
            if !["api_key", "bearer", "env", "none"].contains(&m.as_str()) {
                out.push(("auth_methods", format!("{m}: expected api_key, bearer, env or none")));
            }
        }
        out
    }
}

#[derive(Deserialize)]
struct PresetFile {
    providers: Vec<ProviderDef>,
}

/// The built-in definitions, in file order.
pub fn presets() -> &'static [ProviderDef] {
    static PRESETS: OnceLock<Vec<ProviderDef>> = OnceLock::new();
    PRESETS.get_or_init(|| {
        serde_json::from_str::<PresetFile>(include_str!("presets.json"))
            .expect("presets.json is valid (checked by a test)")
            .providers
    })
}

pub fn preset(id: &str) -> Option<&'static ProviderDef> {
    presets().iter().find(|p| p.id == id)
}

/// The definition an account's `provider = id` means: the preset of that id, overridden by the
/// user's `[[provider]]` of that id. A user-defined id that is no preset starts from the
/// generic compatible server when it speaks an OpenAI api, so `ollama-home` needs only a
/// `base_url`. `Ok(None)` when `id` is neither a preset nor defined (the fake, a test provider).
pub fn resolve(id: &str, user: Option<&ProviderDef>) -> Result<Option<ProviderDef>, String> {
    let def = match (preset(id), user) {
        (None, None) => return Ok(None),
        (Some(p), None) => p.clone(),
        (Some(p), Some(u)) => p.clone().overlaid(u),
        (None, Some(u)) => {
            let generic = matches!(u.api, Some(Api::OpenaiChat | Api::OpenaiResponses))
                .then(|| preset("openai-compatible"))
                .flatten();
            generic.cloned().unwrap_or_default().overlaid(u)
        }
    };
    if def.api.is_none() {
        return Err(format!("provider {id} has no api: set api to one of anthropic-messages, openai-chat, openai-responses"));
    }
    match def.problems().into_iter().next() {
        Some((field, why)) => Err(format!("provider {id}: {field}: {why}")),
        None => Ok(Some(def)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_preset_file_parses_and_every_preset_is_whole() {
        let all = presets();
        assert!(all.len() >= 21);
        let mut ids: Vec<_> = all.iter().map(|p| p.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), all.len(), "an id twice");
        for p in all {
            assert!(p.api.is_some() && p.model_table.is_some(), "{}", p.id);
            assert!(p.problems().is_empty(), "{}: {:?}", p.id, p.problems());
            let local = p.base_url.as_deref().is_some_and(|u| u.starts_with("http://127.0.0.1"));
            // A cloud vendor ships a table with something in it; only a local server may be empty.
            if let Some(ModelTableSpec::Inline(v)) = &p.model_table {
                assert!(!v["models"].as_array().unwrap().is_empty() || local, "{}", p.id);
            }
        }
    }

    #[test]
    fn a_user_entry_overrides_the_preset_field_by_field() {
        let user = ProviderDef { id: "groq".into(), base_url: Some("http://gw.example/v1".into()), ..ProviderDef::default() };
        let def = resolve("groq", Some(&user)).unwrap().unwrap();
        assert_eq!(def.base_url.as_deref(), Some("http://gw.example/v1"));
        assert_eq!(def.env_vars, ["GROQ_API_KEY"], "what the user left out stays the preset's");
        assert_eq!(def.api, Some(Api::OpenaiChat));
    }

    #[test]
    fn a_new_openai_id_starts_from_the_generic_compatible_server() {
        let user = ProviderDef {
            id: "ollama-home".into(),
            api: Some(Api::OpenaiChat),
            base_url: Some("http://10.0.0.5:11434/v1".into()),
            ..ProviderDef::default()
        };
        let def = resolve("ollama-home", Some(&user)).unwrap().unwrap();
        assert!(def.compat().open_aliases && def.compat().logprobs);
        assert_eq!(def.id, "ollama-home");
        assert!(resolve("fake", None).unwrap().is_none());
        let bare = ProviderDef { id: "x".into(), ..ProviderDef::default() };
        assert!(resolve("x", Some(&bare)).unwrap_err().contains("no api"));
    }

    #[test]
    fn a_credential_looking_header_is_refused() {
        for name in ["Authorization", "x-api-key", "api-key", "X-Auth-Token", "cookie"] {
            let user = ProviderDef {
                id: "p".into(),
                api: Some(Api::OpenaiChat),
                headers: [(name.to_owned(), "v".to_owned())].into(),
                ..ProviderDef::default()
            };
            assert!(resolve("p", Some(&user)).unwrap_err().contains("credential"), "{name}");
        }
        let ok = ProviderDef { id: "p".into(), api: Some(Api::OpenaiChat), headers: [("x-team".to_owned(), "home".to_owned())].into(), ..ProviderDef::default() };
        assert!(resolve("p", Some(&ok)).is_ok());
    }

    #[test]
    fn static_id_hands_out_one_str_per_id() {
        let a = ProviderDef { id: "same".into(), ..ProviderDef::default() };
        assert!(std::ptr::eq(a.static_id(), a.clone().static_id()));
    }
}
