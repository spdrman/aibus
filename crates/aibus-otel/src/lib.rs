//! Opt-in OTLP/HTTP export (#292, spec 7.2 and 10.3).
//!
//! There is no default destination and no `OTEL_*` variable is read: with `None`, `export` returns
//! before it opens a socket or starts a thread. With a destination, the request runs on a worker
//! thread with a hard timeout, and whatever goes wrong (refused, hung, a 5xx) comes back as
//! `Outcome::Failed` for the caller to log. Nothing here returns an error into the run, and the
//! local trace is never touched.
#![forbid(unsafe_code)]

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

/// Sends `body` (from `aibus_trace::otel::to_otlp`) to `dest` and waits at most `dest.timeout`. A caller that must not wait
/// runs it on its own thread (`std::thread::spawn(move || export(dest.as_ref(), &body))`).
pub fn export(dest: Option<&Destination>, body: &Value) -> Outcome {
    let Some(d) = dest else {
        return Outcome::Disabled;
    };
    // The deadline is enforced here and not left to reqwest's blocking timeout (a 300 ms timeout
    // was seen taking about 30 s on a loaded host): the request runs on its own thread and a missed
    // deadline abandons it. The abandoned thread ends when reqwest gives up.
    let (tx, rx) = std::sync::mpsc::channel();
    let (d, body) = (d.clone(), body.to_string());
    let timeout = d.timeout;
    std::thread::spawn(move || {
        // One client for the process: building it loads and parses the system root certificates,
        // which is slow in an unoptimised build (seconds under load), so it happens once, on the
        // worker thread and inside the deadline, never on the caller's thread.
        static CLIENT: std::sync::OnceLock<Result<reqwest::blocking::Client, String>> =
            std::sync::OnceLock::new();
        let client = match CLIENT.get_or_init(|| {
            reqwest::blocking::Client::builder()
                // A collector endpoint has no business redirecting; following one would resend custom
                // auth headers and, on 307/308, the body.
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| e.without_url().to_string())
        }) {
            Ok(c) => c,
            Err(e) => return drop(tx.send(Outcome::Failed(e.clone()))),
        };
        let mut req = client
            .post(&d.endpoint)
            .timeout(d.timeout)
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
