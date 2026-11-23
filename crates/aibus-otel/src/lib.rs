//! Opt-in OTLP/HTTP export (#292, spec 7.2 and 10.3).
//!
//! There is no default destination and no `OTEL_*` variable is read: with `None` no
//! socket is opened and no thread is started. With a destination, the export runs on its own
//! thread with a hard timeout, and whatever goes wrong (refused, hung, a 5xx) comes back as
//! `Outcome::Failed` for the caller to log. Nothing here returns an error into the run, and the
//! local trace is never touched.
#![forbid(unsafe_code)]

use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;

/// Where to send, set by the user. `endpoint` is the full OTLP/HTTP traces URL, for a collector
/// usually `http://host:4318/v1/traces`. `headers` carry auth (Langfuse takes a basic-auth header).
#[derive(Clone)]
pub struct Destination {
    pub endpoint: String,
    pub headers: Vec<(String, String)>,
    pub timeout: Duration,
}

impl std::fmt::Debug for Destination {
    // Header values are credentials (Langfuse basic auth): names only.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.headers.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("Destination")
            .field("endpoint", &self.endpoint)
            .field("headers", &names)
            .field("timeout", &self.timeout)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// No destination configured: nothing was sent.
    Disabled,
    Sent,
    Failed(String),
}

/// Sends `body` (from `aibus_trace::otel::to_otlp`) to `dest` and waits. Prefer `spawn`.
pub fn export(dest: Option<&Destination>, body: &Value) -> Outcome {
    let Some(d) = dest else {
        return Outcome::Disabled;
    };
    // reqwest's blocking timeout did not cut a hung collector off in time (a 300 ms timeout took
    // about 30 s), so the deadline is enforced here: the request runs on its own thread and a
    // missed deadline abandons it. The abandoned thread ends when reqwest gives up.
    let (tx, rx) = std::sync::mpsc::channel();
    let (d, body) = (d.clone(), body.to_string());
    let timeout = d.timeout;
    std::thread::spawn(move || {
        let client = match reqwest::blocking::Client::builder()
            .timeout(d.timeout)
            .connect_timeout(d.timeout)
            .build()
        {
            Ok(c) => c,
            Err(e) => return drop(tx.send(Outcome::Failed(e.without_url().to_string()))),
        };
        let mut req = client
            .post(&d.endpoint)
            .header("content-type", "application/json")
            .body(body);
        for (k, v) in &d.headers {
            req = req.header(k, v);
        }
        let out = match req.send() {
            Ok(r) if r.status().is_success() => Outcome::Sent,
            Ok(r) => Outcome::Failed(format!("collector answered {}", r.status())),
            Err(e) => Outcome::Failed(e.without_url().to_string()),
        };
        let _ = tx.send(out);
    });
    rx.recv_timeout(timeout)
        .unwrap_or_else(|_| Outcome::Failed("export timed out".into()))
}

/// `export` on its own thread, so a slow or dead collector never holds the caller up. Join the
/// handle (or drop it) whenever convenient: a panic in the thread is the only thing `join` reports
/// as an `Err`, and the caller can ignore that too.
pub fn spawn(dest: Option<Destination>, body: Value) -> JoinHandle<Outcome> {
    std::thread::spawn(move || export(dest.as_ref(), &body))
}
