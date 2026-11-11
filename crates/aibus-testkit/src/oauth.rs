//! A scripted authorization server for the OAuth flows: a token endpoint (one answer, a script
//! of answers, or the same answer for ever) and a device authorization endpoint. It listens on
//! 127.0.0.1 only; the browser is played by the test.

use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub struct FakeAuthServer {
    server: MockServer,
}

impl FakeAuthServer {
    pub async fn start() -> Self {
        Self { server: MockServer::start().await }
    }

    /// `/auth`, `/token`, `/device` and so on, on this server.
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.server.uri())
    }

    /// The token endpoint answers `status` with `body`, to every request. Paths: the key-exchange one OpenRouter
    /// publishes, and `/token`.
    pub async fn token_status(&self, status: u16, body: Value) {
        for p in ["/api/v1/auth/keys", "/token"] {
            Mock::given(method("POST"))
                .and(path(p))
                .respond_with(ResponseTemplate::new(status).set_body_json(&body))
                .mount(&self.server)
                .await;
        }
    }

    /// One answer per request to `/token`, in order.
    pub async fn token_script(&self, answers: Vec<(u16, Value)>) {
        for (status, body) in answers {
            Mock::given(method("POST"))
                .and(path("/token"))
                .respond_with(ResponseTemplate::new(status).set_body_json(body))
                .up_to_n_times(1)
                .mount(&self.server)
                .await;
        }
    }

    /// The device authorization endpoint (`/device`) answers 200 with `body`.
    pub async fn device_start(&self, body: Value) {
        Mock::given(method("POST"))
            .and(path("/device"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&self.server)
            .await;
    }

    /// The bodies of the requests the server got, JSON or form encoded, oldest first.
    pub async fn bodies(&self) -> Vec<Value> {
        let seen = self.server.received_requests().await.unwrap_or_default();
        seen.iter()
            .filter(|r| r.url.path() != "/device")
            .map(|r| {
                serde_json::from_slice(&r.body).unwrap_or_else(|_| {
                    let pairs = r.url.query_pairs().chain(url_encoded(&r.body));
                    Value::Object(pairs.map(|(k, v)| (k.into_owned(), Value::String(v.into_owned()))).collect())
                })
            })
            .collect()
    }
}

fn url_encoded(body: &[u8]) -> impl Iterator<Item = (std::borrow::Cow<'_, str>, std::borrow::Cow<'_, str>)> {
    // `form_urlencoded` is not a dependency here: a form body is split by hand, and the test
    // values are plain ASCII except the grant type's colons.
    std::str::from_utf8(body).unwrap_or("").split('&').filter_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        Some((k.into(), v.replace("%3A", ":").replace('+', " ").into()))
    })
}
