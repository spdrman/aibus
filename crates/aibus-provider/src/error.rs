//! Provider and config errors (02 section 6). Messages are already redacted.

use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ProviderErrorKind {
    Auth,
    RateLimit,
    Overloaded,
    ContextTooLong,
    InvalidRequest,
    ModelNotFound,
    Network,
    Timeout,
    Server,
    Cancelled,
    Unknown,
}

/// What the UI acts on. `source` is skipped by serde, so a scripted error loaded from RON
/// never carries one.
#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
#[error("{kind:?}: {message}")]
pub struct ProviderError {
    pub kind: ProviderErrorKind,
    pub retryable: bool,
    #[serde(default)]
    pub retry_after: Option<Duration>,
    #[serde(default)]
    pub request_id: Option<String>,
    /// Already redacted.
    pub message: String,
    #[serde(skip)]
    #[source]
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl ProviderError {
    /// `retryable` follows the kind: transient failures retry, the rest do not.
    pub fn new(kind: ProviderErrorKind, message: impl Into<String>) -> Self {
        use ProviderErrorKind::*;
        let retryable = matches!(kind, RateLimit | Overloaded | Network | Timeout | Server);
        Self {
            kind,
            retryable,
            retry_after: None,
            request_id: None,
            message: message.into(),
            source: None,
        }
    }
}

/// Seconds from a `Retry-After` header. The HTTP-date form is not used by any provider here.
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let secs: f64 = value.trim().parse().ok()?;
    (secs.is_finite() && secs >= 0.0).then(|| Duration::from_secs_f64(secs.min(3600.0)))
}

impl ProviderError {
    /// The error for a non-2xx answer, by status (04 section 2; the table is asserted against
    /// each provider's recorded error bodies). `message` is what the provider said, shortened.
    pub fn from_http(
        status: u16,
        message: &str,
        retry_after: Option<Duration>,
        request_id: Option<String>,
    ) -> Self {
        use ProviderErrorKind::*;
        let lower = message.to_ascii_lowercase();
        let context = [
            "context_length_exceeded",
            "maximum context length",
            "context window",
            "token limit",
            "prompt is too long",
            "input is too long",
        ]
        .iter()
        .any(|p| lower.contains(p));
        let kind = match status {
            400 | 422 if context => ContextTooLong,
            400 | 422 => InvalidRequest,
            401 | 403 => Auth,
            404 => ModelNotFound,
            408 | 504 => Timeout,
            413 => ContextTooLong,
            429 => RateLimit,
            503 | 529 => Overloaded,
            500..=599 => Server,
            _ => Unknown,
        };
        let mut message: String = message.chars().take(500).collect();
        if message.is_empty() {
            message = format!("HTTP {status}");
        }
        let mut e = Self::new(kind, message);
        e.retry_after = retry_after;
        e.request_id = request_id;
        e
    }
}

/// A factory could not build a provider from its config, or the registry rejected it.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("provider {0}: {1}")]
    Invalid(String, String),
    /// The provider needs a setting no account has given it, which the message names.
    #[error("provider {0}: {1}")]
    NotConfigured(String, String),
    #[error("no provider registered as {0}")]
    UnknownProvider(String),
    #[error("provider {0} registered twice")]
    DuplicateProvider(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(status: u16, message: &str) -> (ProviderErrorKind, bool) {
        let e = ProviderError::from_http(status, message, None, None);
        (e.kind, e.retryable)
    }

    #[test]
    fn statuses_map_to_kinds_and_only_transient_ones_retry() {
        use ProviderErrorKind::*;
        assert_eq!(kind(401, "x"), (Auth, false));
        assert_eq!(kind(403, "x"), (Auth, false));
        assert_eq!(kind(404, "x"), (ModelNotFound, false));
        assert_eq!(kind(429, "x"), (RateLimit, true));
        assert_eq!(kind(529, "x"), (Overloaded, true));
        assert_eq!(kind(503, "x"), (Overloaded, true));
        assert_eq!(kind(500, "x"), (Server, true));
        assert_eq!(kind(502, "x"), (Server, true));
        assert_eq!(kind(504, "x"), (Timeout, true));
        assert_eq!(kind(400, "bad field"), (InvalidRequest, false));
        assert_eq!(kind(413, "x"), (ContextTooLong, false));
        assert_eq!(kind(418, "x"), (Unknown, false));
    }

    #[test]
    fn a_400_about_the_context_window_is_context_too_long() {
        for m in [
            "prompt is too long: 213482 tokens > 200000 maximum",
            "This model's maximum context length is 128000 tokens",
            "Your request exceeded model token limit: 8192",
            "context_length_exceeded",
        ] {
            assert_eq!(kind(400, m), (ProviderErrorKind::ContextTooLong, false), "{m}");
        }
    }

    #[test]
    fn the_message_is_cut_and_never_empty_and_retry_after_parses_seconds() {
        let long = "x".repeat(2_000);
        assert_eq!(ProviderError::from_http(400, &long, None, None).message.len(), 500);
        assert_eq!(ProviderError::from_http(500, "", None, None).message, "HTTP 500");
        assert_eq!(parse_retry_after(" 12 "), Some(Duration::from_secs(12)));
        assert_eq!(parse_retry_after("1.5"), Some(Duration::from_millis(1500)));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2026 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after("-3"), None);
        assert_eq!(parse_retry_after("999999"), Some(Duration::from_secs(3600)));
    }
}
