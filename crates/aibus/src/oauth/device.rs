//! RFC 8628: the device authorization grant.

use std::time::Duration;

use aibus_core::OAuthTokens;
use tokio_util::sync::CancellationToken;

use super::{form, Login};

/// The documented default polling interval, and what `slow_down` adds to it.
const INTERVAL: u64 = 5;

impl Login {
    pub(super) async fn device(
        &self,
        notify: &(dyn for<'n> Fn(&'n str) + Send + Sync + '_),
        cancel: &CancellationToken,
    ) -> Result<OAuthTokens, String> {
        let url = self.def.device_url.as_deref().ok_or("this login has no device endpoint")?;
        let client = self.def.client_id.as_deref().unwrap_or_default();
        let scope = self.def.scopes.join(" ");
        let (status, d) = self.post(url, form(&[("client_id", client), ("scope", &scope)]), false, cancel).await?;
        if !(200..300).contains(&status) {
            return Err(format!("the device endpoint answered {status}: {}", d["error"].as_str().unwrap_or("no error code")));
        }
        let (Some(device_code), Some(user_code)) = (d["device_code"].as_str(), d["user_code"].as_str()) else {
            return Err("the device endpoint sent no code".into());
        };
        let uri = d["verification_uri"].as_str().or(d["verification_url"].as_str()).unwrap_or_default();
        notify(&format!("open {uri} and enter the code {user_code}"));

        let mut interval = d["interval"].as_u64().unwrap_or(INTERVAL);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(d["expires_in"].as_u64().unwrap_or(900));
        loop {
            tokio::select! {
                () = cancel.cancelled() => return Err("cancelled".into()),
                () = tokio::time::sleep(Duration::from_secs(interval)) => {}
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("the device code expired; run the command again".into());
            }
            let body = form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", device_code),
                ("client_id", client),
            ]);
            let (status, t) = self.post(&self.def.token_url, body, false, cancel).await?;
            if (200..300).contains(&status) {
                return self.tokens(status, &t);
            }
            match t["error"].as_str() {
                Some("authorization_pending") => {}
                Some("slow_down") => interval += INTERVAL,
                Some("expired_token") => return Err("the device code expired; run the command again".into()),
                Some("access_denied") => return Err("the login was denied in the browser".into()),
                _ => return self.tokens(status, &t),
            }
        }
    }
}
