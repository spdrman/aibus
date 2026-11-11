//! A scripted authorization server for the OAuth flows: a token endpoint (one answer, a script
//! of answers, or the same answer for ever) and a device authorization endpoint. It listens on
//! 127.0.0.1 only; the browser is played by the test.
//!
//! It is served by a task on the caller's own runtime, not on a thread of its own. A test with a
//! paused clock needs that: tokio advances a paused clock whenever the runtime has nothing ready,
//! and a reply that has to come from another thread is not ready yet at that moment, so the
//! client's timeout (or the flow's five minute wait) fires before the answer arrives.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Default)]
struct State {
    /// The answer for every request to a path, set by `token_status`.
    always: Vec<(&'static str, u16, Value)>,
    /// One answer per request to `/token`, set by `token_script`; it wins while it lasts.
    script: VecDeque<(u16, Value)>,
    device: Option<Value>,
    seen: Vec<(String, Vec<u8>)>,
}

pub struct FakeAuthServer {
    base: String,
    state: Arc<Mutex<State>>,
}

impl FakeAuthServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind a loopback port");
        let base = format!("http://{}", listener.local_addr().expect("a local address"));
        let state = Arc::new(Mutex::new(State::default()));
        let served = state.clone();
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                tokio::spawn(answer(sock, served.clone()));
            }
        });
        Self { base, state }
    }

    /// `/auth`, `/token`, `/device` and so on, on this server.
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// The token endpoint answers `status` with `body`, to every request. Paths: the key-exchange one OpenRouter
    /// publishes, and `/token`.
    pub async fn token_status(&self, status: u16, body: Value) {
        let mut s = self.state.lock().unwrap();
        for p in ["/api/v1/auth/keys", "/token"] {
            s.always.push((p, status, body.clone()));
        }
    }

    /// One answer per request to `/token`, in order.
    pub async fn token_script(&self, answers: Vec<(u16, Value)>) {
        self.state.lock().unwrap().script.extend(answers);
    }

    /// The device authorization endpoint (`/device`) answers 200 with `body`.
    pub async fn device_start(&self, body: Value) {
        self.state.lock().unwrap().device = Some(body);
    }

    /// The bodies of the requests the server got, JSON or form encoded, oldest first.
    pub async fn bodies(&self) -> Vec<Value> {
        let seen = self.state.lock().unwrap().seen.clone();
        seen.iter()
            .filter(|(path, _)| !path.starts_with("/device"))
            .map(|(path, body)| {
                serde_json::from_slice(body).unwrap_or_else(|_| {
                    let query = path.split_once('?').map_or("", |(_, q)| q);
                    let pairs = form(query.as_bytes()).chain(form(body));
                    Value::Object(pairs.map(|(k, v)| (k, Value::String(v))).collect())
                })
            })
            .collect()
    }
}

/// `k=v&k=v`, split by hand: `form_urlencoded` is not a dependency here, and the test values are
/// plain ASCII except the grant type's colons.
fn form(body: &[u8]) -> impl Iterator<Item = (String, String)> + '_ {
    std::str::from_utf8(body).unwrap_or("").split('&').filter_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        Some((k.to_owned(), v.replace("%3A", ":").replace('+', " ")))
    })
}

/// One request, one answer, then the connection closes.
async fn answer(mut sock: TcpStream, state: Arc<Mutex<State>>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let (head_len, want) = loop {
        let Ok(n) = sock.read(&mut chunk).await else { return };
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            break (end + 4, len);
        }
    };
    while buf.len() < head_len + want {
        let Ok(n) = sock.read(&mut chunk).await else { return };
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let line = String::from_utf8_lossy(&buf[..head_len]).lines().next().unwrap_or_default().to_owned();
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default().to_owned());
    let path = target.split('?').next().unwrap_or_default();
    let (status, body) = {
        let mut s = state.lock().unwrap();
        s.seen.push((target.clone(), buf[head_len..].to_vec()));
        match (method, path) {
            ("POST", "/device") => s.device.clone().map_or((404, Value::Null), |b| (200, b)),
            ("POST", "/token") if !s.script.is_empty() => s.script.pop_front().expect("checked"),
            ("POST", p) => s
                .always
                .iter()
                .find(|(q, ..)| *q == p)
                .map_or((404, Value::Null), |(_, st, b)| (*st, b.clone())),
            _ => (404, Value::Null),
        }
    };
    let body = serde_json::to_vec(&body).unwrap_or_default();
    let head = format!(
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let _ = sock.write_all(head.as_bytes()).await;
    let _ = sock.write_all(&body).await;
    let _ = sock.shutdown().await;
}
