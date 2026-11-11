//! The one place providers are registered. A list, not logic: a new provider adds one line.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aibus_core::config::Account;
use aibus_core::persist::paths::Paths;
use aibus_core::CredentialStore;
use aibus_provider::models::cache_is_stale;
use aibus_provider::presets::resolve;
use aibus_provider::{
    Api, ConfigError, ProviderConfig, ProviderDef, ProviderFactory, ProviderRegistry,
};

/// Fake, plus one adapter per wire: the Messages API, the two OpenAI ones and Gemini's. Which vendors use
/// them is data (`aibus_provider::presets`, `[[provider]]`), not another line here.
pub fn builtin_factories() -> Vec<Box<dyn ProviderFactory>> {
    vec![
        Box::new(aibus_provider::fake::Factory),
        Box::new(aibus_provider_anthropic::Factory),
        Box::new(aibus_provider_openai::Factory::openai()),
        Box::new(aibus_provider_gemini::Factory),
    ]
}

/// What each provider id is built from: its definition (the preset of that id, the user's
/// `[[provider]]` of that id, or both), the settings of the enabled accounts that use it, and
/// where the model tables live. A provider is one instance per id, so an id carries its URL:
/// two local servers are two ids. The old form, `base_url` and `api` on the account, still
/// works and is laid over the definition; two accounts of one id that disagree there are an
/// error, since taking one of them would send the other account's key to the wrong server.
pub fn provider_configs(
    accounts: &[Account],
    defs: &[ProviderDef],
    paths: &Paths,
) -> Result<BTreeMap<String, ProviderConfig>, ConfigError> {
    let mut out: BTreeMap<String, (ProviderConfig, &str)> = BTreeMap::new();
    for a in accounts.iter().filter(|a| a.enabled) {
        let invalid = |why: String| ConfigError::Invalid(a.provider.clone(), why);
        let mut cfg = ProviderConfig {
            def: resolve(&a.provider, defs.iter().find(|d| d.id == a.provider)).map_err(invalid)?,
            cache_dir: Some(paths.cache.join("models")),
            overrides_dir: Some(paths.config.join("providers")),
            ..ProviderConfig::default()
        };
        match &mut cfg.def {
            Some(def) => {
                if let Some(url) = &a.base_url {
                    def.base_url = Some(url.clone());
                }
                // `api = "responses"` on an account is `api = "openai-responses"` on its provider.
                if matches!(def.api, Some(Api::OpenaiChat | Api::OpenaiResponses)) {
                    match a.api.as_deref() {
                        None | Some("chat") => {}
                        Some("responses") => def.api = Some(Api::OpenaiResponses),
                        Some(other) => {
                            return Err(invalid(format!("api \"{other}\" is not \"chat\" or \"responses\"")));
                        }
                    }
                }
            }
            None => {
                cfg.base_url.clone_from(&a.base_url);
                cfg.api.clone_from(&a.api);
            }
        }
        match out.get(&a.provider) {
            Some((first, by)) if *first != cfg => {
                return Err(ConfigError::Invalid(
                    a.provider.clone(),
                    format!(
                        "accounts {by} and {} set a different base_url or api; define one [[provider]] id per server",
                        a.name
                    ),
                ));
            }
            Some(_) => {}
            None => {
                out.insert(a.provider.clone(), (cfg, &a.name));
            }
        }
    }
    Ok(out.into_iter().map(|(k, (cfg, _))| (k, cfg)).collect())
}

/// One enabled account per provider whose model list is missing or older than 7 days. The fake
/// has no list to fetch.
fn stale_accounts<'a>(accounts: &'a [Account], cache_dir: &Path, now: SystemTime) -> Vec<&'a Account> {
    let mut seen = BTreeSet::new();
    accounts
        .iter()
        .filter(|a| a.enabled && a.provider != "fake" && seen.insert(a.provider.as_str()))
        .filter(|a| cache_is_stale(cache_dir, &a.provider, now))
        .collect()
}

/// Refreshes the stale model lists on a thread of its own, so the first frame never waits for
/// the network or the keychain (08 section 8). Every failure is dropped: the old list stays.
pub fn refresh_stale_models(
    accounts: &[Account],
    providers: Arc<ProviderRegistry>,
    store: Arc<CredentialStore>,
    cache_dir: PathBuf,
) {
    let stale: Vec<Account> = stale_accounts(accounts, &cache_dir, SystemTime::now())
        .into_iter()
        .cloned()
        .collect();
    if stale.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_time().build() else {
            return;
        };
        for a in &stale {
            let (Ok(provider), Ok(cred)) = (providers.get(&a.provider), store.resolve(a)) else {
                continue;
            };
            let refreshed = rt.block_on(tokio::time::timeout(
                Duration::from_secs(30),
                provider.refresh_models(&cred),
            ));
            tracing::debug!(account = %a.name, ok = matches!(refreshed, Ok(Ok(_))), "model list refresh");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(name: &str, provider: &str, base_url: Option<&str>) -> Account {
        Account {
            name: name.into(),
            provider: provider.into(),
            base_url: base_url.map(Into::into),
            ..Account::default()
        }
    }

    fn paths() -> Paths {
        Paths::resolve(|k| (k == "HOME").then(|| "/home/u".to_owned())).unwrap()
    }

    #[test]
    fn configs_carry_the_accounts_settings_and_the_model_directories() {
        let accounts = [
            account("a", "anthropic", None),
            account("l", "openai-compatible", Some("http://127.0.0.1:11434/v1")),
        ];
        let configs = provider_configs(&accounts, &[], &paths()).unwrap();
        let url = |id: &str| configs[id].def.as_ref().unwrap().base_url.clone();
        assert_eq!(url("openai-compatible").as_deref(), Some("http://127.0.0.1:11434/v1"));
        assert_eq!(url("anthropic").as_deref(), Some("https://api.anthropic.com"));
        assert_eq!(
            configs["anthropic"].cache_dir.as_deref(),
            Some(std::path::Path::new("/home/u/.cache/aibus/models"))
        );
        assert_eq!(
            configs["anthropic"].overrides_dir.as_deref(),
            Some(std::path::Path::new("/home/u/.config/aibus/providers"))
        );
    }

    #[test]
    fn two_accounts_of_one_provider_may_agree_but_not_disagree() {
        let same = [account("a", "anthropic", None), account("b", "anthropic", None)];
        assert!(provider_configs(&same, &[], &paths()).is_ok());
        let differ = [
            account("a", "anthropic", None),
            account("b", "anthropic", Some("https://gw.example")),
        ];
        let err = provider_configs(&differ, &[], &paths()).unwrap_err().to_string();
        assert!(err.contains("accounts a and b"), "{err}");
        let mut off = account("b", "anthropic", Some("https://gw.example"));
        off.enabled = false;
        assert!(provider_configs(&[account("a", "anthropic", None), off], &[], &paths()).is_ok());
    }

    #[test]
    fn only_a_missing_or_week_old_list_is_stale_and_each_provider_is_asked_once() {
        use aibus_provider::models::CACHE_TTL;
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000_000);
        let write = |provider: &str, age: Duration| {
            let at = now.duration_since(SystemTime::UNIX_EPOCH).unwrap() - age;
            std::fs::write(
                dir.path().join(format!("{provider}.json")),
                format!(r#"{{"fetched_at": {}, "models": []}}"#, at.as_secs()),
            )
            .unwrap();
        };
        write("anthropic", Duration::from_secs(60));
        write("openai", CACHE_TTL + Duration::from_secs(1));
        let mut off = account("e", "kimi", None);
        off.enabled = false;
        let accounts = [
            account("a", "anthropic", None),
            account("o1", "openai", None),
            account("o2", "openai", None),
            account("k", "kimi", None),
            off,
            account("f", "fake", None),
        ];
        let names: Vec<_> = stale_accounts(&accounts, dir.path(), now).iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["o1", "k"]);
    }

    fn local(id: &str, url: &str) -> ProviderDef {
        ProviderDef {
            id: id.into(),
            api: Some(Api::OpenaiChat),
            base_url: Some(url.into()),
            ..ProviderDef::default()
        }
    }

    #[test]
    fn two_local_servers_with_two_ids_build_two_instances() {
        let defs = [local("ollama-home", "http://10.0.0.5:11434/v1"), local("ollama-work", "http://10.0.0.6:11434/v1")];
        let accounts = [account("home", "ollama-home", None), account("work", "ollama-work", None)];
        let configs = provider_configs(&accounts, &defs, &paths()).unwrap();
        let registry = ProviderRegistry::new(&builtin_factories(), &configs).unwrap();
        for id in ["ollama-home", "ollama-work"] {
            assert_eq!(registry.get(id).unwrap().id().0, id);
        }
    }

    #[test]
    fn a_provider_table_with_a_preset_id_overrides_the_preset() {
        let defs = [ProviderDef { id: "groq".into(), base_url: Some("https://gw.example/openai/v1".into()), ..ProviderDef::default() }];
        let configs = provider_configs(&[account("g", "groq", None)], &defs, &paths()).unwrap();
        let def = configs["groq"].def.as_ref().unwrap();
        assert_eq!(def.base_url.as_deref(), Some("https://gw.example/openai/v1"));
        assert_eq!(def.env_vars, ["GROQ_API_KEY"]);
        assert!(ProviderRegistry::new(&builtin_factories(), &configs).unwrap().get("groq").is_ok());
    }

    #[test]
    fn a_preset_needs_no_provider_table_and_an_unknown_id_is_left_for_the_registry() {
        let accounts = [account("g", "groq", None), account("o", "ollama", None), account("x", "nobody", None)];
        let configs = provider_configs(&accounts, &[], &paths()).unwrap();
        assert_eq!(configs["groq"].def.as_ref().unwrap().base_url.as_deref(), Some("https://api.groq.com/openai/v1"));
        assert!(configs["nobody"].def.is_none());
        let registry = ProviderRegistry::new(&builtin_factories(), &configs).unwrap();
        assert!(registry.get("ollama").is_ok() && registry.get("nobody").is_err());
    }

    #[test]
    fn api_responses_on_an_openai_account_is_the_responses_api_of_that_provider() {
        let mut a = account("o", "openai", None);
        a.api = Some("responses".into());
        let configs = provider_configs(&[a.clone()], &[], &paths()).unwrap();
        assert_eq!(configs["openai"].def.as_ref().unwrap().api, Some(Api::OpenaiResponses));
        a.api = Some("grpc".into());
        assert!(provider_configs(&[a], &[], &paths()).unwrap_err().to_string().contains("grpc"));
    }

    /// The 0.1.0 corpus: one config per id that existed then. Each loads with no edits, as the
    /// accounts it always meant,, and builds a provider.
    #[test]
    fn every_0_1_0_config_still_loads_and_builds() {
        let corpus = [
            ("fake", include_str!("../../../fixtures/config/0.1.0-fake.toml")),
            ("anthropic", include_str!("../../../fixtures/config/0.1.0-anthropic.toml")),
            ("openai", include_str!("../../../fixtures/config/0.1.0-openai.toml")),
            ("kimi", include_str!("../../../fixtures/config/0.1.0-kimi.toml")),
            ("openai-compatible", include_str!("../../../fixtures/config/0.1.0-openai-compatible.toml")),
        ];
        for (id, text) in corpus {
            let (table, warnings) = aibus_core::config::parse_file(Path::new("0.1.0.toml"), text).unwrap();
            assert!(warnings.is_empty(), "{id}: {warnings:?}");
            let cfg: aibus_core::config::Config = toml::Value::Table(table).try_into().unwrap();
            assert!(cfg.problems().is_empty(), "{id}: {:?}", cfg.problems());
            assert!(cfg.providers.is_empty(), "{id}");
            let [a] = &cfg.accounts[..] else { panic!("{id}: one account") };
            assert_eq!(a.provider, id);
            assert!(a.enabled, "{id}");

            let configs = provider_configs(&cfg.accounts, &cfg.providers, &paths()).unwrap();
            let registry = ProviderRegistry::new(&builtin_factories(), &configs).unwrap();
            assert_eq!(registry.get(id).unwrap().id().0, id, "{id}");
        }
    }

    #[test]
    fn the_old_account_settings_still_reach_the_provider() {
        let base = |id: &str, text: &str| {
            let (table, _) = aibus_core::config::parse_file(Path::new("c.toml"), text).unwrap();
            let cfg: aibus_core::config::Config = toml::Value::Table(table).try_into().unwrap();
            let configs = provider_configs(&cfg.accounts, &cfg.providers, &paths()).unwrap();
            configs[id].def.clone().unwrap()
        };
        let gw = base("anthropic", include_str!("../../../fixtures/config/0.1.0-anthropic.toml"));
        assert_eq!(gw.base_url.as_deref(), Some("https://gw.example.com"));
        let oa = base("openai", include_str!("../../../fixtures/config/0.1.0-openai.toml"));
        assert_eq!(oa.api, Some(Api::OpenaiResponses));
        let local = base("openai-compatible", include_str!("../../../fixtures/config/0.1.0-openai-compatible.toml"));
        assert_eq!(local.base_url.as_deref(), Some("http://127.0.0.1:11434/v1"));
    }

    #[test]
    fn lists_the_fake_and_one_adapter_per_wire_and_every_old_id_is_a_preset() {
        let mut ids: Vec<_> = builtin_factories().iter().map(|f| f.id().0).collect();
        for id in ["fake", "anthropic", "openai"] {
            assert!(ids.contains(&id), "{id}");
        }
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), builtin_factories().len());
        for id in ["anthropic", "openai", "kimi", "openai-compatible"] {
            assert!(aibus_provider::presets::preset(id).is_some(), "{id}");
        }
    }
}
