//! A login or a logout in the terminal changes the config file; the accounts a session picks
//! from have to follow without a restart.

use std::path::Path;
use std::sync::Arc;

use aibus_core::persist::paths::Paths;
use aibus_core::{AccountName, Keychain, SelectHints};
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
    let (registry, views, _) = reload_accounts(paths, &dir.join("settings.toml"), &[Box::new(Fake)]).unwrap();
    (registry, views)
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

// G4b (#161): a session that started on the built-in fake swaps to the account added later.

const FAKE: &str = "[[account]]\nname = \"fake\"\nprovider = \"fake\"\nauth = { none = true }\n";
const REAL: &str = "sk-real-0123456789abcdef";
const KEPT: &str = "[[account]]\nname = \"work\"\nprovider = \"fake\"\ndefault_model = \"fake-large\"\nauth = { keyring = true }\n";

fn launcher(registry: Arc<aibus_core::AccountRegistry>, builtin: bool, keys: Arc<aibus_core::MemoryKeychain>) -> crate::launch::ChildLauncher {
    use crate::launch::{ChildLauncher, Live};
    let first = ModelRef::Full(ModelId("fake-small".into()));
    let redactor = aibus_core::Redactor::new();
    ChildLauncher {
        agents: Default::default(),
        live: std::sync::RwLock::new(Live {
            registry,
            builtin,
            defaults: aibus_core::ModelDefaults { default_model: Some(first.clone()), ..Default::default() },
            session_default: first,
        }),
        store: Arc::new(aibus_core::CredentialStore::new(keys, redactor.clone())),
        redactor,
        tools: Arc::new(aibus_core::ToolExecutor::new(aibus_core::ToolRegistry::default())),
        hooks: Arc::new(aibus_core::HookRunner::default()),
        deny: Vec::new(),
        cwd: std::env::temp_dir(),
        runs_dir: std::env::temp_dir(),
        limits: Default::default(),
        budgets: Default::default(),
        data_home: None,
        max_spawn_depth: 2,
        chosen: Default::default(),
        defs: Default::default(),
        worktrees: std::env::temp_dir(),
        assist: Default::default(),
        workflow_dirs: Vec::new(),
        author_server: false,
        author_write: Default::default(),
    }
}

fn started_on_the_fake() -> (TempDir, Paths, crate::launch::ChildLauncher, Arc<aibus_core::MemoryKeychain>) {
    let (dir, paths) = home();
    std::fs::write(paths.config_file(), FAKE).unwrap();
    let (registry, _, _) = reload_accounts(&paths, &dir.path().join("settings.toml"), &[Box::new(Fake)]).unwrap();
    let keys = Arc::new(aibus_core::MemoryKeychain::default());
    keys.set("aibus", "work", &aibus_provider::Secret::new(REAL.to_owned())).unwrap();
    let l = launcher(registry, true, keys.clone());
    (dir, paths, l, keys)
}

#[test]
fn a_session_without_an_account_runs_the_next_prompt_on_the_added_one() {
    use aibus_provider::fake::FAKE_KEY;
    let (dir, paths, l, _keys) = started_on_the_fake();
    let start = l.live().session_default;
    let p = l.pick(&start, &SelectHints::default()).unwrap();
    assert_eq!(p.creds.credential().unwrap().secret.expose(), FAKE_KEY, "before the login it is the fake");

    std::fs::write(paths.config_file(), KEPT).unwrap();
    let (registry, views, defaults) = reload_accounts(&paths, &dir.path().join("settings.toml"), &[Box::new(Fake)]).unwrap();
    l.set_accounts(registry, &views, &defaults).unwrap();

    let live = l.live();
    assert!(!live.builtin);
    // The fake's default model is gone: the account's own default stands in everywhere.
    let want = ModelRef::Full(ModelId("fake-large".into()));
    assert_eq!(live.session_default, want);
    assert_eq!(live.defaults.default_model, Some(want.clone()));
    let p = l.pick(&live.session_default, &SelectHints::default()).unwrap();
    assert_eq!(p.account, AccountName("work".into()));
    // Never the fake key on a real account, whichever model is asked for.
    for model in ["fake-small", "fake-large"] {
        let p = l.pick(&ModelRef::Full(ModelId(model.into())), &SelectHints::default()).unwrap();
        let key = p.creds.credential().unwrap();
        assert_eq!(key.secret.expose(), REAL);
        assert_ne!(key.secret.expose(), FAKE_KEY);
    }
    // The fake account is not in the new registry, so a run that was pinned to it fails to pick.
    let pinned = SelectHints { explicit: Some(AccountName("fake".into())), ..Default::default() };
    assert!(l.pick(&start, &pinned).is_err());
}

#[test]
fn defaults_model_wins_over_the_account_default_when_leaving_the_fake() {
    let (dir, paths, l, _keys) = started_on_the_fake();
    std::fs::write(paths.config_file(), format!("[defaults]\nmodel = \"fake-small\"\n{KEPT}")).unwrap();
    let (registry, views, defaults) = reload_accounts(&paths, &dir.path().join("settings.toml"), &[Box::new(Fake)]).unwrap();
    l.set_accounts(registry, &views, &defaults).unwrap();
    assert_eq!(l.live().session_default, ModelRef::Full(ModelId("fake-small".into())));
}

#[test]
fn a_swap_with_no_model_to_default_to_keeps_the_fake_whole() {
    use aibus_provider::fake::FAKE_KEY;
    let (dir, paths, l, _keys) = started_on_the_fake();
    // An empty config has no account at all: nothing to swap to, and the registry stays.
    std::fs::write(paths.config_file(), "").unwrap();
    let (registry, views, defaults) = reload_accounts(&paths, &dir.path().join("settings.toml"), &[Box::new(Fake)]).unwrap();
    assert!(l.set_accounts(registry, &views, &defaults).is_err());
    let live = l.live();
    assert!(live.builtin);
    let p = l.pick(&live.session_default, &SelectHints::default()).unwrap();
    assert_eq!(p.creds.credential().unwrap().secret.expose(), FAKE_KEY);
}

#[test]
fn between_real_accounts_only_the_registry_is_swapped() {
    let (dir, paths) = home();
    std::fs::write(paths.config_file(), KEPT).unwrap();
    let keys = Arc::new(aibus_core::MemoryKeychain::default());
    keys.set("aibus", "work", &aibus_provider::Secret::new(REAL.to_owned())).unwrap();
    let (registry, views, defaults) = reload_accounts(&paths, &dir.path().join("settings.toml"), &[Box::new(Fake)]).unwrap();
    let l = launcher(registry.clone(), false, keys);
    let before = l.live().session_default;
    l.set_accounts(registry, &views, &defaults).unwrap();
    assert_eq!(l.live().session_default, before);
}
