//! The real `OAuthFlow` (#115): two generic logins driven by data, so a provider is a `Def`
//! and not code. `pkce_loopback` is RFC 7636 with a one-request listener on 127.0.0.1;
//! `device_code` is RFC 8628. Neither ever sends another product's client id, user agent or
//! headers (docs/SECURITY.md, "Identity"): a `Def` exists only for a vendor that documents a
//! login for third-party applications.

mod device;
mod pkce;
pub(crate) use pkce::open;
#[cfg(test)]
mod tests;

use std::time::Duration;

use aibus_core::{OAuthFlow, OAuthTokens};
use aibus_provider::Secret;
use aibus_provider_http::reqwest::header::{CONTENT_TYPE, USER_AGENT};
use aibus_provider_http::reqwest::{Client, Url};
use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    PkceLoopback,
    DeviceCode,
}

/// Whether the authorization server echoes `state` back. When it does not, the PKCE verifier
/// alone proves the callback belongs to this login.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StateMode {
    Echoed,
    None,
}

/// What the login hands back: OAuth tokens to refresh, or one durable API key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Tokens,
    ApiKey,
}

/// What a provider publishes about its login for third-party applications.
#[derive(Clone, Debug)]
pub struct Def {
    pub kind: Kind,
    pub authorize_url: String,
    pub token_url: String,
    /// The device authorization endpoint, for `DeviceCode`.
    pub device_url: Option<String>,
    /// Only when the vendor publishes one for third parties.
    pub client_id: Option<String>,
    pub scopes: Vec<String>,
    /// The loopback port to try first.
    pub callback_port: Option<u16>,
    /// The vendor registered exactly this port: a busy one is an error, not a fallback.
    pub port_pinned: bool,
    /// The authorize URL parameter that carries our callback (`redirect_uri` unless the vendor
    /// names it otherwise).
    pub redirect_param: &'static str,
    pub state: StateMode,
    pub result: Outcome,
}

/// The login a provider offers, if any. OpenRouter's PKCE flow needs no client registration
/// and returns a durable key. It matches the `openrouter` id, and `openai-compatible` pointed
/// at openrouter.ai until the provider list carries its own entry.
pub fn definition(provider: &str, base_url: Option<&str>) -> Option<Def> {
    let host = base_url.and_then(|u| Url::parse(u).ok()).and_then(|u| u.host_str().map(str::to_owned));
    let openrouter = provider == "openrouter"
        || (provider == "openai-compatible" && host.as_deref() == Some("openrouter.ai"));
    openrouter.then(|| Def {
        kind: Kind::PkceLoopback,
        authorize_url: "https://openrouter.ai/auth".into(),
        token_url: "https://openrouter.ai/api/v1/auth/keys".into(),
        device_url: None,
        client_id: None,
        scopes: vec![],
        callback_port: Some(3000),
        port_pinned: false,
        redirect_param: "callback_url",
        state: StateMode::None,
        result: Outcome::ApiKey,
    })
}

pub struct Login {
    def: Def,
    http: Client,
    /// Start the platform opener on the authorize URL (a terminal is attached).
    pub open_browser: bool,
    /// Also accept the redirect address pasted on stdin (a terminal is attached).
    pub paste: bool,
}

impl Login {
    pub fn new(def: Def) -> Self {
        let http = Client::builder()
            .use_rustls_tls()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        Self { def, http, open_browser: false, paste: false }
    }

    /// A POST to the token or device endpoint. The answer is cut off at 64 KiB, and only the
    /// parsed JSON leaves this function, never the raw text.
    async fn post(&self, url: &str, body: String, json: bool, cancel: &CancellationToken) -> Result<(u16, Value), String> {
        let ctype = if json { "application/json" } else { "application/x-www-form-urlencoded" };
        let send = self
            .http
            .post(url)
            .header(CONTENT_TYPE, ctype)
            .header(USER_AGENT, concat!("aibus/", env!("CARGO_PKG_VERSION")))
            .header("accept", "application/json")
            .body(body)
            .send();
        let resp = tokio::select! {
            () = cancel.cancelled() => return Err("cancelled".into()),
            r = send => r.map_err(|e| format!("could not reach the authorization server: {}", e.without_url()))?,
        };
        let status = resp.status().as_u16();
        let bytes = resp.bytes().await.map_err(|e| e.without_url().to_string())?;
        let value = serde_json::from_slice(&bytes[..bytes.len().min(64 * 1024)]).unwrap_or(Value::Null);
        Ok((status, value))
    }

    /// Turns a token endpoint answer into tokens, or an error naming the status and the
    /// server's `error` code, nothing else from the body.
    fn tokens(&self, status: u16, body: &Value) -> Result<OAuthTokens, String> {
        if !(200..300).contains(&status) {
            let why = body["error"].as_str().unwrap_or("no error code");
            return Err(format!("the token endpoint answered {status}: {why}"));
        }
        let secret = |k: &str| body[k].as_str().filter(|s| !s.is_empty()).map(|s| Secret::new(s.to_owned()));
        if self.def.result == Outcome::ApiKey {
            let key = secret("key").ok_or("the token endpoint sent no key")?;
            return Ok(OAuthTokens { access: key, refresh: None, expires_at: None });
        }
        let access = secret("access_token").ok_or("the token endpoint sent no access token")?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        Ok(OAuthTokens {
            access,
            refresh: secret("refresh_token"),
            expires_at: body["expires_in"].as_u64().map(|s| now + s),
        })
    }
}

/// `k=v&k=v`, percent-encoded.
fn form(pairs: &[(&str, &str)]) -> String {
    let mut u = Url::parse("http://form.invalid/").expect("a constant URL");
    u.query_pairs_mut().extend_pairs(pairs);
    u.query().unwrap_or_default().to_owned()
}

#[async_trait]
impl OAuthFlow for Login {
    async fn authorize(
        &self,
        notify: &(dyn for<'n> Fn(&'n str) + Send + Sync),
        cancel: &CancellationToken,
    ) -> Result<OAuthTokens, String> {
        match self.def.kind {
            Kind::PkceLoopback => self.pkce(notify, cancel).await,
            Kind::DeviceCode => self.device(notify, cancel).await,
        }
    }

    async fn refresh(&self, refresh_token: &str, cancel: &CancellationToken) -> Result<OAuthTokens, String> {
        if self.def.result == Outcome::ApiKey {
            return Err("this login returns a key that does not expire, so there is nothing to refresh".into());
        }
        let mut pairs = vec![("grant_type", "refresh_token"), ("refresh_token", refresh_token)];
        if let Some(id) = &self.def.client_id {
            pairs.push(("client_id", id));
        }
        let (status, body) = self.post(&self.def.token_url, form(&pairs), false, cancel).await?;
        self.tokens(status, &body)
    }

    fn yields_key(&self) -> bool {
        self.def.result == Outcome::ApiKey
    }
}
