//! The one place providers are registered. A list, not logic: a new provider adds one line.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aibus_core::config::Account;
use aibus_core::persist::paths::Paths;
use aibus_core::CredentialStore;
use aibus_provider::models::cache_is_stale;
use aibus_provider::{ConfigError, ProviderConfig, ProviderFactory, ProviderRegistry};

pub fn builtin_factories() -> Vec<Box<dyn ProviderFactory>> {
    vec![
        Box::new(aibus_provider::fake::Factory),
        Box::new(aibus_provider_anthropic::Factory),
        Box::new(aibus_provider_openai::Factory::openai()),
        Box::new(aibus_provider_openai::Factory::kimi()),
        Box::new(aibus_provider_openai::Factory::compatible()),
    ]
}

/// What each provider id is built from: the settings of the enabled accounts that use it, and
/// where the model tables live. A provider is one instance for all its accounts, so accounts of
/// one provider that disagree on `base_url` or `api` are an error: taking one of them would send
/// the other account's key to the wrong server.
pub fn provider_configs(
    accounts: &[Account],
    paths: &Paths,
) -> Result<BTreeMap<String, ProviderConfig>, ConfigError> {
    let mut out: BTreeMap<String, (ProviderConfig, &str)> = BTreeMap::new();
    for a in accounts.iter().filter(|a| a.enabled) {
        let cfg = ProviderConfig {
            base_url: a.base_url.clone(),
            api: a.api.clone(),
            cache_dir: Some(paths.cache.join("models")),
            overrides_dir: Some(paths.config.join("providers")),
        };
        match out.get(&a.provider) {
            Some((first, by)) if *first != cfg => {
                return Err(ConfigError::Invalid(
                    a.provider.clone(),
                    format!(
                        "accounts {by} and {} set a different base_url or api; use one provider id per server",
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
        let configs = provider_configs(&accounts, &paths()).unwrap();
        assert_eq!(
            configs["openai-compatible"].base_url.as_deref(),
            Some("http://127.0.0.1:11434/v1")
        );
        assert_eq!(configs["anthropic"].base_url, None);
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
        assert!(provider_configs(&same, &paths()).is_ok());
        let differ = [
            account("a", "anthropic", None),
            account("b", "anthropic", Some("https://gw.example")),
        ];
        let err = provider_configs(&differ, &paths()).unwrap_err().to_string();
        assert!(err.contains("accounts a and b"), "{err}");
        let mut off = account("b", "anthropic", Some("https://gw.example"));
        off.enabled = false;
        assert!(provider_configs(&[account("a", "anthropic", None), off], &paths()).is_ok());
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

    #[test]
    fn lists_the_fake_and_each_id_once() {
        let mut ids: Vec<_> = builtin_factories().iter().map(|f| f.id().0).collect();
        for id in ["fake", "anthropic", "openai", "kimi", "openai-compatible"] {
            assert!(ids.contains(&id), "{id}");
        }
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), builtin_factories().len());
    }
}
