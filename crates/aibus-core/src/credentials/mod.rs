//! Where keys come from (04 section 5): the OS keychain, an environment variable or a `0600`
//! file. The store resolves one per request, keeps it for 60 seconds, and registers it with the
//! redactor. Credentials never reach the graph: only `KeyRef`, a name, does.

mod file;
mod memory;

pub use memory::MemoryKeychain;

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aibus_provider::{AuthMethodKind, Credential, Secret};
use smol_str::SmolStr;

use crate::config;
use crate::error::CredentialError;
use crate::ids::AccountName;
use crate::redact::Redactor;

/// The keychain service name; the user is `account:<name>`.
pub const KEYCHAIN_SERVICE: &str = "aibus";

/// Where an account's key lives. `Display` is what the graph holds as `key_ref`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum KeyRef {
    Keyring { service: SmolStr, name: AccountName },
    Env { var: String },
    File { path: PathBuf },
}

impl fmt::Display for KeyRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyRef::Keyring { service, name } => write!(f, "keyring:{service}/{name}"),
            KeyRef::Env { var } => write!(f, "env:{var}"),
            KeyRef::File { path } => write!(f, "file:{}", path.display()),
        }
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum KeychainError {
    /// No keychain on this machine (a headless box without Secret Service).
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Failed(String),
}

impl From<KeychainError> for CredentialError {
    fn from(e: KeychainError) -> Self {
        match e {
            KeychainError::Unavailable(m) => CredentialError::NoKeychain(m),
            KeychainError::Failed(m) => CredentialError::Keychain(m),
        }
    }
}

/// What `aibus doctor` reports for the credential store.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum StoreHealth {
    /// The keychain's name, as in `keyring (Secret Service)`.
    Available(String),
    Unavailable(String),
}

/// The OS keychain, or a fake of it. The real one is in the binary, so this crate does not
/// need `keyring`.
pub trait Keychain: Send + Sync {
    /// `None` when there is no such entry.
    fn get(&self, service: &str, user: &str) -> Result<Option<Secret<String>>, KeychainError>;
    fn set(&self, service: &str, user: &str, secret: &Secret<String>) -> Result<(), KeychainError>;
    /// Removing what is not there is not an error.
    fn delete(&self, service: &str, user: &str) -> Result<(), KeychainError>;
    fn probe(&self) -> StoreHealth;
}

struct Cached {
    secret: Secret<String>,
    kind: AuthMethodKind,
    at: Instant,
}

pub struct CredentialStore {
    keychain: Arc<dyn Keychain>,
    redactor: Redactor,
    #[allow(clippy::type_complexity)]
    env: Box<dyn Fn(&str) -> Option<String> + Send + Sync>,
    now: Box<dyn Fn() -> Instant + Send + Sync>,
    cache: Mutex<HashMap<AccountName, Cached>>,
}

fn user(name: &AccountName) -> String {
    format!("account:{name}")
}

impl CredentialStore {
    pub fn new(keychain: Arc<dyn Keychain>, redactor: Redactor) -> Self {
        Self {
            keychain,
            redactor,
            env: Box::new(|var| std::env::var(var).ok()),
            now: Box::new(Instant::now),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Reads variables through `env` instead of the process environment.
    pub fn with_env(
        mut self,
        env: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.env = Box::new(env);
        self
    }

    /// Takes the time from `now`, so a test can move past the 60 seconds.
    pub fn with_clock(mut self, now: impl Fn() -> Instant + Send + Sync + 'static) -> Self {
        self.now = Box::new(now);
        self
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, HashMap<AccountName, Cached>> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The account's key, from the cache when it is under a minute old. A key read fresh is
    /// registered with the redactor, replacing the one it rotates.
    pub fn resolve(&self, account: &config::Account) -> Result<Credential, CredentialError> {
        // A rotated key takes effect within a minute.
        const TTL: Duration = Duration::from_secs(60);
        let name = account.account_name();
        let now = (self.now)();
        if let Some(c) = self
            .cache()
            .get(&name)
            .filter(|c| now.duration_since(c.at) < TTL)
        {
            return Ok(Credential {
                account: name,
                secret: Secret::new(c.secret.expose().clone()),
                kind: c.kind,
                resolved_at: c.at,
            });
        }
        let missing = || CredentialError::Missing(name.clone());
        let (key, kind) = match account.key_ref() {
            None => (String::new(), AuthMethodKind::None),
            Some(KeyRef::Env { var }) => {
                let v = (self.env)(&var).filter(|v| !v.is_empty());
                (v.ok_or_else(missing)?, AuthMethodKind::Env)
            }
            Some(KeyRef::File { path }) => {
                let v = Some(file::read_key(&path)?).filter(|v| !v.is_empty());
                (v.ok_or_else(missing)?, AuthMethodKind::ApiKey)
            }
            Some(KeyRef::Keyring { service, name: n }) => {
                let v = self.keychain.get(&service, &user(&n))?;
                (v.ok_or_else(missing)?.expose().clone(), AuthMethodKind::ApiKey)
            }
        };
        self.redactor.register(&name, &key);
        self.cache().insert(
            name.clone(),
            Cached {
                secret: Secret::new(key.clone()),
                kind,
                at: now,
            },
        );
        Ok(Credential {
            account: name,
            secret: Secret::new(key),
            kind,
            resolved_at: now,
        })
    }

    /// Puts a key in the keychain. Never anywhere else: there is no plaintext fallback.
    pub fn store(
        &self,
        account: &AccountName,
        secret: &Secret<String>,
    ) -> Result<(), CredentialError> {
        self.keychain.set(KEYCHAIN_SERVICE, &user(account), secret)?;
        self.cache().remove(account);
        self.redactor.register(account, secret.expose());
        Ok(())
    }

    /// Forgets the key everywhere this store put it.
    pub fn remove(&self, account: &AccountName) -> Result<(), CredentialError> {
        self.keychain.delete(KEYCHAIN_SERVICE, &user(account))?;
        self.cache().remove(account);
        self.redactor.unregister(account);
        Ok(())
    }

    pub fn probe(&self) -> StoreHealth {
        self.keychain.probe()
    }
}

impl Drop for CredentialStore {
    fn drop(&mut self) {
        for name in self.cache().keys() {
            self.redactor.unregister(name);
        }
    }
}

/// What a run asks for before each request.
pub trait CredentialSource: Send + Sync {
    fn credential(&self) -> Result<Credential, CredentialError>;
}

/// A fixed credential, for the built-in fake and for tests.
impl CredentialSource for Credential {
    fn credential(&self) -> Result<Credential, CredentialError> {
        Ok(Credential {
            account: self.account.clone(),
            secret: Secret::new(self.secret.expose().clone()),
            kind: self.kind,
            resolved_at: self.resolved_at,
        })
    }
}

/// One account's key, resolved through the store on every call.
pub struct AccountCredential {
    pub store: Arc<CredentialStore>,
    pub account: config::Account,
}

impl CredentialSource for AccountCredential {
    fn credential(&self) -> Result<Credential, CredentialError> {
        self.store.resolve(&self.account)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn account(name: &str, auth: config::Auth) -> config::Account {
        config::Account {
            name: name.into(),
            provider: "fake".into(),
            auth,
            ..Default::default()
        }
    }

    fn keyring() -> config::Auth {
        config::Auth {
            keyring: true,
            ..Default::default()
        }
    }

    #[test]
    fn key_refs_print_as_the_graph_holds_them() {
        let a = account("work", keyring());
        assert_eq!(a.key_ref().unwrap().to_string(), "keyring:aibus/work");
        let e = account("w", config::Auth { env: Some("MY_KEY".into()), ..Default::default() });
        assert_eq!(e.key_ref().unwrap().to_string(), "env:MY_KEY");
        let f = account("w", config::Auth { file: Some("/run/k".into()), ..Default::default() });
        assert_eq!(f.key_ref().unwrap().to_string(), "file:/run/k");
        let n = account("w", config::Auth { none: true, ..Default::default() });
        assert_eq!(n.key_ref(), None);
        let bare = account("anthropic-work", config::Auth::default());
        assert_eq!(
            bare.key_ref().unwrap().to_string(),
            "env:AIBUS_ACCOUNT_ANTHROPIC_WORK_KEY"
        );
    }

    #[test]
    fn a_keychain_key_is_resolved_registered_and_forgotten_on_remove() {
        let redactor = Redactor::new();
        let store = CredentialStore::new(Arc::new(MemoryKeychain::default()), redactor.clone());
        let a = account("work", keyring());
        assert!(matches!(store.resolve(&a), Err(CredentialError::Missing(_))));

        store
            .store(&a.account_name(), &Secret::new("sk-work-123456".into()))
            .unwrap();
        let c = store.resolve(&a).unwrap();
        assert_eq!(c.secret.expose(), "sk-work-123456");
        assert_eq!(c.kind, AuthMethodKind::ApiKey);
        assert_eq!(redactor.redact("key sk-work-123456"), "key [redacted:work]");

        store.remove(&a.account_name()).unwrap();
        assert!(matches!(store.resolve(&a), Err(CredentialError::Missing(_))));
        assert_eq!(redactor.redact("key sk-work-123456"), "key sk-work-123456");
    }

    #[test]
    fn a_key_is_reused_for_a_minute_and_read_again_after_that() {
        let kc = Arc::new(MemoryKeychain::default());
        let t0 = Instant::now();
        let offset = Arc::new(Mutex::new(Duration::ZERO));
        let clock = offset.clone();
        let store = CredentialStore::new(kc.clone(), Redactor::new())
            .with_clock(move || t0 + *clock.lock().unwrap());
        let a = account("work", keyring());
        let put = |v: &str| kc.set(KEYCHAIN_SERVICE, "account:work", &Secret::new(v.into())).unwrap();
        put("first-key-1");
        assert_eq!(store.resolve(&a).unwrap().secret.expose(), "first-key-1");

        put("second-key-2");
        *offset.lock().unwrap() = Duration::from_secs(59);
        assert_eq!(store.resolve(&a).unwrap().secret.expose(), "first-key-1");
        *offset.lock().unwrap() = Duration::from_secs(60);
        assert_eq!(store.resolve(&a).unwrap().secret.expose(), "second-key-2");
    }

    #[test]
    fn an_env_key_is_read_through_the_given_environment() {
        let vars: BTreeMap<&str, &str> = [("MY_KEY", "env-key-123456"), ("EMPTY", "")].into();
        let store = CredentialStore::new(Arc::new(MemoryKeychain::default()), Redactor::new())
            .with_env(move |v| vars.get(v).map(|s| (*s).to_owned()));
        let set = account("a", config::Auth { env: Some("MY_KEY".into()), ..Default::default() });
        let c = store.resolve(&set).unwrap();
        assert_eq!((c.secret.expose().as_str(), c.kind), ("env-key-123456", AuthMethodKind::Env));
        for var in ["EMPTY", "ABSENT"] {
            let a = account("b", config::Auth { env: Some(var.into()), ..Default::default() });
            assert!(matches!(store.resolve(&a), Err(CredentialError::Missing(_))), "{var}");
        }
    }

    #[test]
    fn no_auth_resolves_to_an_empty_credential() {
        let store = CredentialStore::new(Arc::new(MemoryKeychain::default()), Redactor::new());
        let a = account("local", config::Auth { none: true, ..Default::default() });
        let c = store.resolve(&a).unwrap();
        assert_eq!((c.secret.expose().as_str(), c.kind), ("", AuthMethodKind::None));
    }

    #[test]
    fn a_missing_keychain_is_an_error_not_a_fallback() {
        let kc = MemoryKeychain::unavailable("no Secret Service");
        let store = CredentialStore::new(Arc::new(kc), Redactor::new());
        let a = account("work", keyring());
        assert!(matches!(store.resolve(&a), Err(CredentialError::NoKeychain(_))));
        let e = store
            .store(&a.account_name(), &Secret::new("k".into()))
            .unwrap_err();
        assert!(matches!(e, CredentialError::NoKeychain(_)));
        assert_eq!(store.probe(), StoreHealth::Unavailable("no Secret Service".into()));
    }

    #[test]
    fn debug_never_prints_the_key() {
        let c = Credential {
            account: AccountName("w".into()),
            secret: Secret::new("sk-secret-value".into()),
            kind: AuthMethodKind::ApiKey,
            resolved_at: Instant::now(),
        };
        assert!(!format!("{c:?}").contains("sk-secret-value"));
    }
}
