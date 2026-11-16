//! The hook entries in the user's settings.

use std::path::Path;

use serde_json::Value;

/// The loopback endpoint and the per-install token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub port: u16,
    pub token: String,
}

/// The events that carry a session's activity.
pub const EVENTS: &[&str] = &[];

/// What a change did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Changed,
    Declined,
    Unchanged,
}

pub fn with_hooks(settings: &Value, _ep: &Endpoint) -> Value {
    settings.clone()
}

pub fn without_hooks(settings: &Value) -> Value {
    settings.clone()
}

/// A line diff of the two documents, pretty printed.
pub fn diff(_before: &Value, _after: &Value) -> String {
    String::new()
}

/// Installs (`Some`) or removes (`None`) the entries in `settings_path`, after `confirm` has seen
/// the exact diff. Declined or unchanged means nothing was written.
pub fn apply(
    _settings_path: &Path,
    _ep: Option<&Endpoint>,
    _confirm: &mut dyn FnMut(&str) -> bool,
) -> Result<Outcome, String> {
    Err("not implemented".into())
}

/// Whether `header` (the `Authorization` value) carries the token.
pub fn accepts(_ep: &Endpoint, _header: Option<&str>) -> bool {
    true
}
