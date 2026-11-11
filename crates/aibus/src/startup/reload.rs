//! A login or a logout in the terminal changes the config file; the accounts a session picks
//! from have to follow without a restart.

use std::path::Path;
use std::sync::Arc;

use aibus_core::persist::paths::Paths;
use aibus_core::{AccountName, SelectHints};
use aibus_provider::fake::{FakeProvider, Script};
use aibus_provider::{ConfigError, ModelId, ModelRef, Provider, ProviderConfig, ProviderFactory, ProviderId};
use tempfile::TempDir;

use super::reload_accounts;

struct Fake;

impl ProviderFactory for Fake {
    fn id(&self) -> ProviderId {
        ProviderId("fake")
    }

    fn build(&self, _: &ProviderConfig) -> Result<Arc<dyn Provider>, ConfigError> {
        Ok(Arc::new(FakeProvider::new(Script::new())))
    }

    fn auth_methods(&self) -> &'static [aibus_provider::AuthMethodKind] {
        &[aibus_provider::AuthMethodKind::None]
    }
}

fn home() -> (TempDir, Paths) {
    let dir = TempDir::new().unwrap();
    let home = dir.path().display().to_string();
    let config = dir.path().join("config.toml").display().to_string();
    let paths = Paths::resolve(|k| match k {
        "HOME" => Some(home.clone()),
        "AIBUS_CONFIG" => Some(config.clone()),
        _ => None,
    })
    .unwrap();
    (dir, paths)
}

const WORK: &str = "[[account]]\nname = \"work\"\nprovider = \"fake\"\nauth = { none = true }\n";

fn reload(paths: &Paths, dir: &Path) -> (Arc<aibus_core::AccountRegistry>, Vec<aibus_core::AccountView>) {
    reload_accounts(paths, &dir.join("settings.toml"), &[Box::new(Fake)]).unwrap()
}

#[test]
fn an_account_added_from_the_tui_is_selectable_without_restart() {
    let (dir, paths) = home();
    let (before, views) = reload(&paths, dir.path());
    assert!(views.is_empty());
    let want = ModelRef::Full(ModelId("fake-small".into()));
    assert!(before.select(&want, &SelectHints::default()).is_err(), "no account serves it yet");

    // `account add` wrote the account; the reload is what the session sees next.
    std::fs::write(paths.config_file(), WORK).unwrap();
    let (after, views) = reload(&paths, dir.path());
    assert_eq!(after.select(&want, &SelectHints::default()).unwrap().0, AccountName("work".into()));
    // The graph's rows (and so the /model picker) carry the account and its models.
    assert_eq!(views.len(), 1);
    assert!(views[0].models.iter().any(|m| m.id == ModelId("fake-small".into())));
}

#[test]
fn remove_reloads_the_registry() {
    let (dir, paths) = home();
    std::fs::write(paths.config_file(), WORK).unwrap();
    let (before, views) = reload(&paths, dir.path());
    assert_eq!(views.len(), 1);
    let want = ModelRef::Full(ModelId("fake-small".into()));
    assert!(before.select(&want, &SelectHints::default()).is_ok());

    std::fs::write(paths.config_file(), "").unwrap();
    let (after, views) = reload(&paths, dir.path());
    assert!(views.is_empty(), "the account is gone from the rows");
    assert!(after.select(&want, &SelectHints::default()).is_err(), "and cannot be picked");
}
