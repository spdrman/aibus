//! Provider trait, account registry, credentials and the fake provider.
#![forbid(unsafe_code)]

pub mod degrade;
pub mod error;
pub mod fake;
pub mod limits;
pub mod models;
pub mod provider;
pub mod secret;
pub mod sse;
pub mod tokens;
pub mod types;

pub use error::{ConfigError, ProviderError, ProviderErrorKind, RateLimitInfo};
pub use limits::{Gated, Limiter, Permit, RateLimits};
pub use provider::{
    AccountInfo, Capabilities, ChatRequest, ChatResponse, ModelInfo, Price, Provider,
    ProviderConfig, ProviderFactory, ProviderRegistry, StopReason, StreamChunk, SystemRole,
};
pub use secret::{AuthMethodKind, Credential, Secret};
pub use types::*;
