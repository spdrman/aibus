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

/// A factory could not build a provider from its config, or the registry rejected it.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("provider {0}: {1}")]
    Invalid(String, String),
    #[error("no provider registered as {0}")]
    UnknownProvider(String),
    #[error("provider {0} registered twice")]
    DuplicateProvider(String),
}
