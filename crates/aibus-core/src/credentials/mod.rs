//! Where keys come from (04 section 5): the OS keychain, an environment variable or a `0600`
//! file. The store resolves one per request, keeps it for 60 seconds, and registers it with the
//! redactor. Credentials never reach the graph: only `KeyRef`, a name, does.

mod file;
mod memory;
mod oauth;

pub use memory::MemoryKeychain;
pub use oauth::{FakeOAuth, OAuthFlow, OAuthTokens};

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aibus_provider::{AuthMethodKind, Credential, Secret};
use smol_str::SmolStr;
use tokio_util::sync::CancellationToken;

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
    /// `AWS_ACCESS_KEY_ID` and friends, read at request time and never stored.
    AwsEnv,
    /// Static keys under `[profile]` in `~/.aws/credentials`.
    AwsProfile { profile: String },
}

impl fmt::Display for KeyRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyRef::Keyring { service, name } => write!(f, "keyring:{service}/{name}"),
            KeyRef::Env { var } => write!(f, "env:{var}"),
            KeyRef::File { path } => write!(f, "file:{}", path.display()),
            KeyRef::AwsEnv => write!(f, "aws-env"),
            KeyRef::AwsProfile { profile } => write!(f, "aws-profile:{profile}"),
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
    /// Seconds since the epoch, for token expiry.
    wall: Box<dyn Fn() -> u64 + Send + Sync>,
    oauth: Option<Arc<dyn OAuthFlow>>,
    cache: Mutex<HashMap<AccountName, Cached>>,
    /// Access tokens a token source fetched (`gcloud`), by account, each with its own lifetime.
    tokens: Mutex<HashMap<AccountName, Cached>>,
    /// One lock per OAuth account, so two requests that both find an expired token do one
    /// refresh: the second waits, then reads what the first stored.
    refreshing: Mutex<HashMap<AccountName, Arc<tokio::sync::Mutex<()>>>>,
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
            wall: Box::new(oauth::unix_now),
            oauth: None,
            cache: Mutex::new(HashMap::new()),
            tokens: Mutex::new(HashMap::new()),
            refreshing: Mutex::new(HashMap::new()),
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

    /// The flow that refreshes OAuth tokens. Without one an expired token is an auth error.
    pub fn with_oauth(mut self, flow: Arc<dyn OAuthFlow>) -> Self {
        self.oauth = Some(flow);
        self
    }

    /// Takes the wall clock (seconds since the epoch) from `wall`, for token expiry in tests.
    pub fn with_wall_clock(mut self, wall: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        self.wall = Box::new(wall);
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
        if account.auth.oauth {
            // Never cached: the token has its own expiry.
            return self.resolve_oauth(&name);
        }
        if matches!(account.key_ref(), Some(KeyRef::AwsEnv)) {
            // Never cached: the variables are read as they are at the time of the request.
            return self.resolve_aws_env(&name);
        }
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
            Some(KeyRef::AwsProfile { profile }) => {
                let path = match (self.env)("AWS_SHARED_CREDENTIALS_FILE").filter(|p| !p.is_empty()) {
                    Some(p) => PathBuf::from(p),
                    None => PathBuf::from((self.env)("HOME").ok_or_else(missing)?).join(".aws/credentials"),
                };
                let (id, secret, token) = file::aws_profile(&path, &profile)?.ok_or_else(missing)?;
                self.register_aws(&name, &secret, token.as_deref());
                (aws_pair(&id, &secret, token.as_deref()), AuthMethodKind::Env)
            }
            Some(KeyRef::AwsEnv) => unreachable!("handled above"),
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

    /// The stored tokens and whether they expire within a minute.
    fn load_oauth(&self, name: &AccountName) -> Result<(OAuthTokens, bool), CredentialError> {
        const SKEW: u64 = 60;
        let raw = self
            .keychain
            .get(KEYCHAIN_SERVICE, &user(name))?
            .ok_or_else(|| CredentialError::Missing(name.clone()))?;
        let tokens = OAuthTokens::decode(raw.expose())
            .ok_or_else(|| CredentialError::OAuth("the stored tokens are unreadable".into()))?;
        let stale = tokens.expires_at.is_some_and(|e| e <= (self.wall)() + SKEW);
        Ok((tokens, stale))
    }

    /// The AWS environment as it is now: `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` and, when
    /// set, `AWS_SESSION_TOKEN`, joined the way the Bedrock transport reads them.
    fn resolve_aws_env(&self, name: &AccountName) -> Result<Credential, CredentialError> {
        let get = |v: &str| (self.env)(v).filter(|v| !v.is_empty());
        let (Some(id), Some(secret)) = (get("AWS_ACCESS_KEY_ID"), get("AWS_SECRET_ACCESS_KEY")) else {
            return Err(CredentialError::Missing(name.clone()));
        };
        let token = get("AWS_SESSION_TOKEN");
        self.register_aws(name, &secret, token.as_deref());
        Ok(Credential {
            account: name.clone(),
            secret: Secret::new(aws_pair(&id, &secret, token.as_deref())),
            kind: AuthMethodKind::Env,
            resolved_at: (self.now)(),
        })
    }

    /// The secret half and the session token are what must never show: each is registered under
    /// a name of its own, next to the whole `ID:SECRET` string the generic path registers.
    fn register_aws(&self, name: &AccountName, secret: &str, token: Option<&str>) {
        self.redactor.register(&AccountName(format!("{name}:secret").into()), secret);
        let token_name = AccountName(format!("{name}:token").into());
        match token {
            Some(t) => self.redactor.register(&token_name, t),
            None => self.redactor.unregister(&token_name),
        }
    }

    /// An access token a token source fetched, kept `ttl` under the account's name and never on
    /// disk. `fetch` runs only when there is no token younger than `ttl`; what it returns is
    /// registered with the redactor. A failure is not cached, and its text is `fetch`'s to keep
    /// free of the token.
    pub fn cached_token(
        &self,
        name: &AccountName,
        ttl: Duration,
        fetch: impl FnOnce() -> Result<String, String>,
    ) -> Result<Secret<String>, CredentialError> {
        let now = (self.now)();
        let tokens = || self.tokens.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = tokens().get(name).filter(|c| now.duration_since(c.at) < ttl) {
            return Ok(Secret::new(c.secret.expose().clone()));
        }
        let token = fetch().map_err(CredentialError::Token)?;
        self.redactor.register(&AccountName(format!("{name}:token").into()), &token);
        tokens().insert(name.clone(), Cached { secret: Secret::new(token.clone()), kind: AuthMethodKind::Bearer, at: now });
        Ok(Secret::new(token))
    }

    /// The stored tokens, refreshed first when they expire within a minute. A refresh that
    /// fails is an auth error and the old tokens stay where they were. This is the sync entry
    /// (`CredentialSource` is sync): a fresh token costs one keychain read, and only a refresh
    /// runs the async flow, on a scoped thread with its own runtime so it never parks the
    /// caller's reactor. `resolve_async` is the entry for async callers.
    fn resolve_oauth(&self, name: &AccountName) -> Result<Credential, CredentialError> {
        let (tokens, stale) = self.load_oauth(name)?;
        if !stale {
            return Ok(self.oauth_credential(name, &tokens));
        }
        std::thread::scope(|s| {
            s.spawn(|| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                    .map_err(|e| CredentialError::OAuth(format!("no runtime for the refresh: {e}")))?;
                rt.block_on(self.refresh_once(name, &CancellationToken::new()))
            })
            .join()
            .unwrap_or_else(|_| Err(CredentialError::OAuth("the refresh panicked".into())))
        })
    }

    /// `resolve` for async callers: an expired OAuth token is refreshed without blocking the
    /// runtime, and `cancel` stops the exchange.
    pub async fn resolve_async(
        &self,
        account: &config::Account,
        cancel: &CancellationToken,
    ) -> Result<Credential, CredentialError> {
        if !account.auth.oauth {
            return self.resolve(account);
        }
        let name = account.account_name();
        let (tokens, stale) = self.load_oauth(&name)?;
        if !stale {
            return Ok(self.oauth_credential(&name, &tokens));
        }
        self.refresh_once(&name, cancel).await
    }

    /// The single-flight refresh: take the account's lock, read the tokens again (the holder
    /// before us may have refreshed them), and only exchange when they are still stale.
    async fn refresh_once(
        &self,
        name: &AccountName,
        cancel: &CancellationToken,
    ) -> Result<Credential, CredentialError> {
        let lock = self
            .refreshing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(name.clone())
            .or_default()
            .clone();
        let _held = lock.lock().await;
        let (mut tokens, stale) = self.load_oauth(name)?;
        if !stale {
            return Ok(self.oauth_credential(name, &tokens));
        }
        let fail = |m: &str| {
            CredentialError::OAuth(format!("{m}; run: aibus account add {name} --oauth"))
        };
        let refresh = tokens
            .refresh
            .as_ref()
            .ok_or_else(|| fail("the token expired and there is no refresh token"))?;
        let flow = self
            .oauth
            .as_ref()
            .ok_or_else(|| fail("the token expired and no OAuth flow is available"))?;
        let mut fresh = flow
            .refresh(refresh.expose(), cancel)
            .await
            .map_err(|e| CredentialError::OAuth(format!("refresh failed: {e}")))?;
        // A refresh may leave the refresh token out: the old one still stands.
        if fresh.refresh.is_none() {
            fresh.refresh = tokens.refresh.take();
        }
        self.store_oauth(name, &fresh)?;
        Ok(self.oauth_credential(name, &fresh))
    }

    fn oauth_credential(&self, name: &AccountName, tokens: &OAuthTokens) -> Credential {
        self.register_oauth(name, tokens);
        Credential {
            account: name.clone(),
            secret: Secret::new(tokens.access.expose().clone()),
            kind: AuthMethodKind::OAuth,
            resolved_at: (self.now)(),
        }
    }

    fn register_oauth(&self, name: &AccountName, tokens: &OAuthTokens) {
        self.redactor.register(name, tokens.access.expose());
        if let Some(r) = &tokens.refresh {
            self.redactor
                .register(&AccountName(format!("{name}:refresh").into()), r.expose());
        }
    }

    /// Puts OAuth tokens in the keychain, where a key would go.
    pub fn store_oauth(
        &self,
        account: &AccountName,
        tokens: &OAuthTokens,
    ) -> Result<(), CredentialError> {
        self.keychain.set(KEYCHAIN_SERVICE, &user(account), &tokens.encode())?;
        self.cache().remove(account);
        self.register_oauth(account, tokens);
        Ok(())
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
        self.redactor.unregister(&AccountName(format!("{account}:refresh").into()));
        Ok(())
    }

    pub fn probe(&self) -> StoreHealth {
        self.keychain.probe()
    }
}

/// `ACCESS_KEY_ID:SECRET_ACCESS_KEY[:SESSION_TOKEN]`, the one string the Bedrock transport signs with.
fn aws_pair(id: &str, secret: &str, token: Option<&str>) -> String {
    match token {
        Some(t) => format!("{id}:{secret}:{t}"),
        None => format!("{id}:{secret}"),
    }
}

impl Drop for CredentialStore {
    fn drop(&mut self) {
        for name in self.cache().keys() {
            self.redactor.unregister(name);
            for part in ["secret", "token"] {
                self.redactor.unregister(&AccountName(format!("{name}:{part}").into()));
            }
        }
        let tokens = self.tokens.lock().unwrap_or_else(|e| e.into_inner());
        for name in tokens.keys() {
            self.redactor.unregister(&AccountName(format!("{name}:token").into()));
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

    fn env_of(pairs: &[(&str, &str)]) -> Arc<Mutex<BTreeMap<String, String>>> {
        Arc::new(Mutex::new(pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()))
    }

    fn store_with(env: &Arc<Mutex<BTreeMap<String, String>>>, redactor: &Redactor) -> CredentialStore {
        let env = env.clone();
        CredentialStore::new(Arc::new(MemoryKeychain::default()), redactor.clone())
            .with_env(move |v| env.lock().unwrap().get(v).cloned())
    }

    fn aws_env_account() -> config::Account {
        account("bedrock", config::Auth { aws_env: true, ..Default::default() })
    }

    const AKID: &str = "AKIDEXAMPLE";
    const SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";

    #[test]
    fn aws_env_reads_the_usual_variables_at_request_time_and_never_caches_them() {
        let env = env_of(&[("AWS_ACCESS_KEY_ID", AKID), ("AWS_SECRET_ACCESS_KEY", SECRET)]);
        let redactor = Redactor::new();
        let store = store_with(&env, &redactor);
        let a = aws_env_account();
        assert_eq!(a.key_ref().unwrap().to_string(), "aws-env");

        let c = store.resolve(&a).unwrap();
        assert_eq!(c.secret.expose(), &format!("{AKID}:{SECRET}"));
        assert_eq!(c.kind, AuthMethodKind::Env);
        // The secret half is registered with the redactor, like a key would be.
        assert!(!redactor.redact(&format!("saw {SECRET}")).contains(SECRET));

        // A session token joins as the third part, and a rotated value is seen at once.
        env.lock().unwrap().insert("AWS_SESSION_TOKEN".into(), "FwoGZXIvYXdzEXAMPLETOKEN".into());
        env.lock().unwrap().insert("AWS_ACCESS_KEY_ID".into(), "AKIDROTATED".into());
        let c = store.resolve(&a).unwrap();
        assert_eq!(c.secret.expose(), &format!("AKIDROTATED:{SECRET}:FwoGZXIvYXdzEXAMPLETOKEN"));
        assert!(!redactor.redact("t FwoGZXIvYXdzEXAMPLETOKEN").contains("FwoGZXIvYXdzEXAMPLETOKEN"));
    }

    #[test]
    fn aws_env_without_the_pair_is_a_missing_credential() {
        let env = env_of(&[("AWS_ACCESS_KEY_ID", AKID)]);
        let store = store_with(&env, &Redactor::new());
        assert!(matches!(store.resolve(&aws_env_account()), Err(CredentialError::Missing(_))));
    }

    #[cfg(unix)]
    fn credentials_file(mode: u32, text: &str) -> (tempfile::TempDir, String) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials");
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        let p = path.display().to_string();
        (dir, p)
    }

    const PROFILES: &str = "[default]\naws_access_key_id = AKIDDEFAULT\naws_secret_access_key = DEFAULTSECRET123\n\n\
        [work]\naws_access_key_id = AKIDWORK\naws_secret_access_key = WORKSECRET456\naws_session_token = WORKTOKEN789\n";

    #[cfg(unix)]
    #[test]
    fn aws_profile_reads_static_keys_from_the_credentials_file() {
        let (_dir, path) = credentials_file(0o600, PROFILES);
        let env = env_of(&[("AWS_SHARED_CREDENTIALS_FILE", &path)]);
        let redactor = Redactor::new();
        let store = store_with(&env, &redactor);
        let work = account("w", config::Auth { aws_profile: Some("work".into()), ..Default::default() });
        assert_eq!(work.key_ref().unwrap().to_string(), "aws-profile:work");
        let c = store.resolve(&work).unwrap();
        assert_eq!(c.secret.expose(), "AKIDWORK:WORKSECRET456:WORKTOKEN789");
        assert!(!redactor.redact("x WORKSECRET456").contains("WORKSECRET456"));

        let default = account("d", config::Auth { aws_profile: Some("default".into()), ..Default::default() });
        assert_eq!(store.resolve(&default).unwrap().secret.expose(), "AKIDDEFAULT:DEFAULTSECRET123");
        let ghost = account("g", config::Auth { aws_profile: Some("ghost".into()), ..Default::default() });
        assert!(matches!(store.resolve(&ghost), Err(CredentialError::Missing(_))));
    }

    #[cfg(unix)]
    #[test]
    fn a_world_readable_credentials_file_is_refused_with_the_chmod_hint() {
        for mode in [0o644, 0o640, 0o604] {
            let (_dir, path) = credentials_file(mode, PROFILES);
            let store = store_with(&env_of(&[("AWS_SHARED_CREDENTIALS_FILE", &path)]), &Redactor::new());
            let work = account("w", config::Auth { aws_profile: Some("work".into()), ..Default::default() });
            let e = store.resolve(&work).unwrap_err();
            assert!(matches!(e, CredentialError::FileMode(_, m) if m == mode), "{mode:o}");
            assert!(e.to_string().contains(&format!("chmod 600 {path}")), "{e}");
            assert!(!e.to_string().contains("WORKSECRET456"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_default_credentials_file_is_under_home() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".aws")).unwrap();
        let (_d, path) = credentials_file(0o600, PROFILES);
        std::fs::copy(&path, dir.path().join(".aws/credentials")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path().join(".aws/credentials"), std::fs::Permissions::from_mode(0o600)).unwrap();
        let home = dir.path().display().to_string();
        let store = store_with(&env_of(&[("HOME", &home)]), &Redactor::new());
        let work = account("w", config::Auth { aws_profile: Some("work".into()), ..Default::default() });
        assert_eq!(store.resolve(&work).unwrap().secret.expose(), "AKIDWORK:WORKSECRET456:WORKTOKEN789");
    }

    #[test]
    fn a_gcloud_account_has_no_stored_key_and_a_none_credential() {
        let store = store_with(&env_of(&[]), &Redactor::new());
        let a = account("vertex", config::Auth { gcloud: true, ..Default::default() });
        assert_eq!(a.key_ref(), None);
        let c = store.resolve(&a).unwrap();
        assert_eq!((c.kind, c.secret.expose().as_str()), (AuthMethodKind::None, ""));
    }

    #[test]
    fn a_fetched_token_is_cached_for_its_ttl_under_the_account_name_and_redacted() {
        let now = Arc::new(Mutex::new(Instant::now()));
        let redactor = Redactor::new();
        let clock = now.clone();
        let store = CredentialStore::new(Arc::new(MemoryKeychain::default()), redactor.clone())
            .with_clock(move || *clock.lock().unwrap());
        let name = AccountName("vertex".into());
        let ttl = Duration::from_secs(50 * 60);
        let calls = Mutex::new(0);
        let fetch = |token: &'static str| {
            let calls = &calls;
            move || {
                *calls.lock().unwrap() += 1;
                Ok::<_, String>(token.to_owned())
            }
        };
        let t = store.cached_token(&name, ttl, fetch("ya29.first-token-0001")).unwrap();
        assert_eq!(t.expose(), "ya29.first-token-0001");
        assert!(!redactor.redact("t ya29.first-token-0001").contains("ya29.first"));
        *now.lock().unwrap() += Duration::from_secs(49 * 60);
        assert_eq!(store.cached_token(&name, ttl, fetch("ya29.second-token-0002")).unwrap().expose(), "ya29.first-token-0001");
        assert_eq!(*calls.lock().unwrap(), 1);
        *now.lock().unwrap() += Duration::from_secs(2 * 60);
        assert_eq!(store.cached_token(&name, ttl, fetch("ya29.second-token-0002")).unwrap().expose(), "ya29.second-token-0002");
        assert_eq!(*calls.lock().unwrap(), 2);
    }

    #[test]
    fn a_failed_fetch_is_an_error_and_is_not_cached() {
        let store = store_with(&env_of(&[]), &Redactor::new());
        let name = AccountName("vertex".into());
        let ttl = Duration::from_secs(50 * 60);
        let e = store.cached_token(&name, ttl, || Err("gcloud is not logged in".to_owned())).unwrap_err();
        assert!(e.to_string().contains("gcloud is not logged in"));
        assert_eq!(store.cached_token(&name, ttl, || Ok("ya29.after-0003".to_owned())).unwrap().expose(), "ya29.after-0003");
    }
}
