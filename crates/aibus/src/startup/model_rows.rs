//! The rows `/model` draws come from the accounts' provider tables, joined with prices, aliases
//! and capabilities, and reading them makes no request.

use std::sync::Arc;

use aibus_core::config::Config;
use aibus_core::persist::paths::Paths;
use aibus_core::{AccountName, AccountRegistry};
use aibus_provider::fake::{FakeProvider, Script};
use aibus_provider::{ModelId, ProviderConfig, ProviderFactory, ProviderId, ProviderRegistry};

use super::account_views;
use crate::providers::{builtin_factories, provider_configs};

const CONFIG: &str = r#"
[models]
fast = "gpt/gpt-5-mini"

[[account]]
name = "work"
provider = "fake"
auth = { none = true }

[[account]]
name = "gpt"
provider = "openai"
auth = { none = true }

[[account]]
name = "lpu"
provider = "groq"
auth = { none = true }
"#;

/// Hands out the one fake the test holds, so its request log can be read after.
struct Held(Arc<FakeProvider>);

impl ProviderFactory for Held {
    fn id(&self) -> ProviderId {
        ProviderId("fake")
    }

    fn build(&self, _: &ProviderConfig) -> Result<Arc<dyn aibus_provider::Provider>, aibus_provider::ConfigError> {
        Ok(self.0.clone())
    }

    fn auth_methods(&self) -> &'static [aibus_provider::AuthMethodKind] {
        &[aibus_provider::AuthMethodKind::None]
    }
}

fn registry() -> (Config, AccountRegistry, Arc<FakeProvider>) {
    let cfg: Config = toml::from_str(CONFIG).unwrap();
    let paths = Paths::resolve(|k| (k == "HOME").then(|| "/home/u".to_owned())).unwrap();
    let configs = provider_configs(&cfg.accounts, &cfg.providers, &paths).unwrap();
    let fake = Arc::new(FakeProvider::new(Script::new()));
    let mut factories: Vec<Box<dyn ProviderFactory>> = vec![Box::new(Held(fake.clone()))];
    factories.extend(builtin_factories().into_iter().filter(|f| f.id().0 != "fake"));
    let providers = Arc::new(ProviderRegistry::new(&factories, &configs).unwrap());
    (cfg.clone(), AccountRegistry::new(&cfg, providers), fake)
}

#[test]
fn models_rows_join_accounts_models_prices_and_aliases() {
    let (cfg, registry, _) = registry();
    let views = account_views(&cfg, &registry);
    let of = |name: &str| views.iter().find(|v| v.name == AccountName(name.into())).unwrap();
    let row = |account: &str, id: &str| of(account).models.iter().find(|m| m.id == ModelId(id.into())).unwrap().clone();

    // The fake's table has a price for both its models.
    let small = row("work", "fake-small");
    assert_eq!((small.input_per_mtok, small.output_per_mtok), (Some(1.0), Some(5.0)));
    assert_eq!(small.context_window, 200_000);

    // An OpenAI-shaped account lists its catalog with the catalog's prices, and `[models]` names
    // the alias that points at its account.
    let mini = row("gpt", "gpt-5-mini");
    assert_eq!((mini.input_per_mtok, mini.output_per_mtok), (Some(0.25), Some(2.0)));
    assert_eq!(mini.alias.as_deref(), Some("fast"));
    assert_eq!(row("gpt", "gpt-5").alias, None);
    // The alias is pinned to `gpt`, so no other account shows it.
    assert!(of("work").models.iter().all(|m| m.alias.is_none()));

    // A table with no price says so: `None` here, `n/a` on the screen, never a guess.
    let unpriced = &of("lpu").models;
    assert!(!unpriced.is_empty());
    assert!(unpriced.iter().all(|m| m.input_per_mtok.is_none() && m.output_per_mtok.is_none()));
}

#[test]
fn deprecated_models_are_not_rows() {
    let (cfg, registry, _) = registry();
    let all = account_views(&cfg, &registry);
    for view in &all {
        let provider_models = registry.provider(&view.name).unwrap().models();
        let kept = provider_models.iter().filter(|m| !m.deprecated).count();
        assert_eq!(view.models.len(), kept, "{}", view.name);
    }
}

#[test]
fn the_rows_cost_no_request() {
    let (cfg, registry, fake) = registry();
    let _ = account_views(&cfg, &registry);
    assert!(fake.requests().is_empty(), "reading the tables never reaches the provider");
}
