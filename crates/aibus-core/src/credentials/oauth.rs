//! OAuth tokens (04 section 4, 09 section 8). The login and the refresh sit behind `OAuthFlow`,
//! so core opens no socket and a test uses `FakeOAuth`. Tokens live in the keychain as one JSON
//! value under the same entry a key would use, and never anywhere else.

use std::fmt;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use aibus_provider::Secret;
use serde::{Deserialize, Serialize};

pub struct OAuthTokens {
    pub access: Secret<String>,
    pub refresh: Option<Secret<String>>,
    /// Seconds since the epoch; `None` for a token that does not expire.
    pub expires_at: Option<u64>,
}

// Not derived, so a later derive cannot print a token.
impl fmt::Debug for OAuthTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthTokens")
            .field("access", &self.access)
            .field("refresh", &self.refresh)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Serialize, Deserialize)]
struct Stored {
    access: String,
    refresh: Option<String>,
    expires_at: Option<u64>,
}

impl OAuthTokens {
    /// The keychain value. `Secret` has no `Serialize`, on purpose, so this goes through plain
    /// strings that live only until the keychain call returns.
    pub(super) fn encode(&self) -> Secret<String> {
        let s = Stored {
            access: self.access.expose().clone(),
            refresh: self.refresh.as_ref().map(|r| r.expose().clone()),
            expires_at: self.expires_at,
        };
        Secret::new(serde_json::to_string(&s).expect("plain strings serialise"))
    }

    pub(super) fn decode(raw: &str) -> Option<Self> {
        let s: Stored = serde_json::from_str(raw).ok()?;
        Some(Self {
            access: Secret::new(s.access),
            refresh: s.refresh.map(Secret::new),
            expires_at: s.expires_at,
        })
    }
}

/// The device or loopback exchange with the provider. The real one lives in the binary. Async,
/// so a login waiting on a browser never holds a runtime thread, and every call takes a cancel
/// token that ends the wait.
#[async_trait]
pub trait OAuthFlow: Send + Sync {
    /// The interactive login. `notify` shows the user a line to act on (the device code and
    /// URL, or the address the browser was sent to).
    async fn authorize(
        &self,
        notify: &(dyn for<'n> Fn(&'n str) + Send + Sync),
        cancel: &CancellationToken,
    ) -> Result<OAuthTokens, String>;
    /// Trades a refresh token for new tokens.
    async fn refresh(
        &self,
        refresh_token: &str,
        cancel: &CancellationToken,
    ) -> Result<OAuthTokens, String>;
    /// `true` when the login hands back one durable API key (in `access`) instead of tokens to
    /// refresh. The account is then stored like any key account and the OAuth is only the
    /// login step.
    fn yields_key(&self) -> bool {
        false
    }
}

pub(super) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A scripted flow for tests: no browser, no network. Each login or refresh hands out
/// `oauth-access-<n>-abcdef` / `oauth-refresh-<n>-abcdef` valid for `lifetime` seconds from `now`.
pub struct FakeOAuth {
    pub now: u64,
    pub lifetime: u64,
    /// Fail every refresh with this message.
    pub refresh_error: Option<String>,
    /// Hand out no new refresh token on a refresh.
    pub keep_refresh: bool,
    /// How long a refresh takes, so a test can have two callers overlap.
    pub refresh_delay: Duration,
    /// Act like a login that returns a key.
    pub yields_key: bool,
    pub refreshes: Mutex<Vec<String>>,
    issued: Mutex<u32>,
}

impl FakeOAuth {
    pub fn new(now: u64, lifetime: u64) -> Self {
        Self {
            now,
            lifetime,
            refresh_error: None,
            keep_refresh: false,
            refresh_delay: Duration::ZERO,
            yields_key: false,
            refreshes: Mutex::new(Vec::new()),
            issued: Mutex::new(0),
        }
    }

    fn issue(&self, with_refresh: bool) -> OAuthTokens {
        let mut n = self.issued.lock().unwrap_or_else(|e| e.into_inner());
        *n += 1;
        OAuthTokens {
            access: Secret::new(format!("oauth-access-{n}-abcdef")),
            refresh: with_refresh.then(|| Secret::new(format!("oauth-refresh-{n}-abcdef"))),
            expires_at: Some(self.now + self.lifetime),
        }
    }
}

#[async_trait]
impl OAuthFlow for FakeOAuth {
    async fn authorize(
        &self,
        notify: &(dyn for<'n> Fn(&'n str) + Send + Sync),
        _cancel: &CancellationToken,
    ) -> Result<OAuthTokens, String> {
        notify("open https://auth.invalid/device and enter code ABCD-1234");
        Ok(self.issue(true))
    }

    async fn refresh(
        &self,
        refresh_token: &str,
        cancel: &CancellationToken,
    ) -> Result<OAuthTokens, String> {
        self.refreshes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(refresh_token.to_owned());
        tokio::select! {
            () = cancel.cancelled() => return Err("cancelled".into()),
            () = tokio::time::sleep(self.refresh_delay) => {}
        }
        match &self.refresh_error {
            Some(e) => Err(e.clone()),
            None => Ok(self.issue(!self.keep_refresh)),
        }
    }

    fn yields_key(&self) -> bool {
        self.yields_key
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    use aibus_provider::AuthMethodKind;

    use super::*;
    use crate::config;
    use crate::credentials::{CredentialStore, MemoryKeychain, KEYCHAIN_SERVICE};
    use crate::error::CredentialError;
    use crate::ids::AccountName;
    use crate::redact::Redactor;
    use crate::Keychain;

    struct Rig {
        kc: Arc<MemoryKeychain>,
        store: CredentialStore,
        flow: Arc<FakeOAuth>,
        clock: Arc<AtomicU64>,
        redactor: Redactor,
        account: config::Account,
    }

    async fn rig(flow: FakeOAuth) -> Rig {
        let kc = Arc::new(MemoryKeychain::default());
        let flow = Arc::new(flow);
        let clock = Arc::new(AtomicU64::new(1_000));
        let c = clock.clone();
        let redactor = Redactor::new();
        let store = CredentialStore::new(kc.clone(), redactor.clone())
            .with_oauth(flow.clone())
            .with_wall_clock(move || c.load(Ordering::SeqCst));
        let account = config::Account {
            name: "work".into(),
            provider: "fake".into(),
            auth: config::Auth {
                oauth: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let r = Rig {
            kc,
            store,
            flow,
            clock,
            redactor,
            account,
        };
        r.store
            .store_oauth(
                &r.account.account_name(),
                &r.flow
                    .authorize(&|_| {}, &CancellationToken::new())
                    .await
                    .unwrap(),
            )
            .unwrap();
        r
    }

    #[tokio::test]
    async fn a_fresh_token_is_used_as_it_is_and_marked_oauth() {
        let r = rig(FakeOAuth::new(1_000, 3_600)).await;
        let c = r.store.resolve(&r.account).unwrap();
        assert_eq!(
            (c.secret.expose().as_str(), c.kind),
            ("oauth-access-1-abcdef", AuthMethodKind::OAuth)
        );
        assert!(r.flow.refreshes.lock().unwrap().is_empty());
        assert_eq!(
            r.account.key_ref().unwrap().to_string(),
            "keyring:aibus/work"
        );
    }

    #[tokio::test]
    async fn a_token_about_to_expire_is_refreshed_before_use_and_the_new_one_is_stored() {
        let r = rig(FakeOAuth::new(1_000, 3_600)).await;
        // 61 seconds left: still fine. 60 left: refreshed.
        r.clock.store(1_000 + 3_600 - 61, Ordering::SeqCst);
        assert_eq!(
            r.store.resolve(&r.account).unwrap().secret.expose(),
            "oauth-access-1-abcdef"
        );
        r.clock.store(1_000 + 3_600 - 60, Ordering::SeqCst);
        let c = r.store.resolve(&r.account).unwrap();
        assert_eq!(c.secret.expose(), "oauth-access-2-abcdef");
        assert_eq!(
            *r.flow.refreshes.lock().unwrap(),
            ["oauth-refresh-1-abcdef"]
        );
        let kept = r.kc.get(KEYCHAIN_SERVICE, "account:work").unwrap().unwrap();
        assert!(kept.expose().contains("oauth-access-2-abcdef"));
    }

    #[tokio::test]
    async fn a_refresh_without_a_new_refresh_token_keeps_the_old_one() {
        let mut flow = FakeOAuth::new(1_000, 3_600);
        flow.keep_refresh = true;
        let r = rig(flow).await;
        r.clock.store(10_000, Ordering::SeqCst);
        r.store.resolve(&r.account).unwrap();
        let kept = r.kc.get(KEYCHAIN_SERVICE, "account:work").unwrap().unwrap();
        assert!(kept.expose().contains("oauth-refresh-1-abcdef"));
    }

    #[tokio::test]
    async fn a_failed_refresh_is_an_auth_error_and_the_stored_tokens_stay() {
        let mut flow = FakeOAuth::new(1_000, 3_600);
        flow.refresh_error = Some("invalid_grant".into());
        let r = rig(flow).await;
        r.clock.store(10_000, Ordering::SeqCst);
        let e = r.store.resolve(&r.account).unwrap_err();
        assert!(
            matches!(&e, CredentialError::OAuth(m) if m.contains("invalid_grant")),
            "{e}"
        );
        assert_eq!(crate::AibusError::from(e).exit_code(), 4);
        let kept = r.kc.get(KEYCHAIN_SERVICE, "account:work").unwrap().unwrap();
        assert!(kept.expose().contains("oauth-access-1-abcdef"));
    }

    #[tokio::test]
    async fn an_expired_token_with_no_flow_or_no_refresh_token_asks_for_a_login() {
        let mut account = config::Account {
            name: "w".into(),
            provider: "fake".into(),
            ..Default::default()
        };
        account.auth.oauth = true;
        let store = CredentialStore::new(Arc::new(MemoryKeychain::default()), Redactor::new())
            .with_wall_clock(|| 10_000);
        let name = account.account_name();
        let expired = |refresh: Option<&str>| OAuthTokens {
            access: Secret::new("old-access-token".into()),
            refresh: refresh.map(|r| Secret::new(r.into())),
            expires_at: Some(5),
        };
        store
            .store_oauth(&name, &expired(Some("some-refresh")))
            .unwrap();
        let e = store.resolve(&account).unwrap_err().to_string();
        assert!(
            e.contains("no OAuth flow") && e.contains("aibus account add w --oauth"),
            "{e}"
        );
        store.store_oauth(&name, &expired(None)).unwrap();
        assert!(store
            .resolve(&account)
            .unwrap_err()
            .to_string()
            .contains("no refresh token"));
    }

    #[tokio::test]
    async fn tokens_never_show_in_debug_or_in_redacted_output() {
        let r = rig(FakeOAuth::new(1_000, 3_600)).await;
        let text = format!("{:?}", r.store.resolve(&r.account).unwrap());
        assert!(!text.contains("oauth-access"), "{text}");
        let t = OAuthTokens::decode(r#"{"access":"oauth-access-9-abcdef","refresh":"oauth-refresh-9-abcdef","expires_at":1}"#).unwrap();
        assert!(!format!("{t:?}").contains("oauth-"));
        let out = r
            .redactor
            .redact("a oauth-access-1-abcdef r oauth-refresh-1-abcdef")
            .into_owned();
        assert!(!out.contains("oauth-"), "{out}");
        r.store.remove(&AccountName("work".into())).unwrap();
        assert_eq!(
            r.redactor.redact("oauth-refresh-1-abcdef"),
            "oauth-refresh-1-abcdef"
        );
    }
}
