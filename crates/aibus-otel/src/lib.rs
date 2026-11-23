//! Opt-in OTLP/HTTP export (#292, spec 7.2 and 10.3).
//!
//! There is no default destination and nothing is read from the environment: with `None` no
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
#[derive(Clone, Debug)]
pub struct Destination {
    pub endpoint: String,
    pub headers: Vec<(String, String)>,
    pub timeout: Duration,
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
    let client = match reqwest::blocking::Client::builder()
        .timeout(d.timeout)
        .build()
    {
        Ok(c) => c,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    let mut req = client
        .post(&d.endpoint)
        .header("content-type", "application/json")
        .body(body.to_string());
    for (k, v) in &d.headers {
        req = req.header(k, v);
    }
    match req.send() {
        Ok(r) if r.status().is_success() => Outcome::Sent,
        Ok(r) => Outcome::Failed(format!("collector answered {}", r.status())),
        Err(e) => Outcome::Failed(e.to_string()),
    }
}

/// `export` on its own thread, so a slow or dead collector never holds the caller up. Join the
/// handle (or drop it) whenever convenient: a panic in the thread is the only thing `join` reports
/// as an `Err`, and the caller can ignore that too.
pub fn spawn(dest: Option<Destination>, body: Value) -> JoinHandle<Outcome> {
    std::thread::spawn(move || export(dest.as_ref(), &body))
}
