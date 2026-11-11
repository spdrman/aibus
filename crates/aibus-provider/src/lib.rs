//! Provider trait, account registry, credentials and the fake provider.
#![forbid(unsafe_code)]

pub mod catalog;
pub mod degrade;
pub mod error;
pub mod fake;
pub mod limits;
pub mod models;
pub mod presets;
pub mod provider;
pub mod secret;
pub mod sse;
pub mod tokens;
pub mod types;

pub use error::{ConfigError, ProviderError, ProviderErrorKind, RateLimitInfo};
pub use limits::{Gated, Limiter, Permit, RateLimits};
pub use provider::{
    AccountInfo, Capabilities, CapabilityOverrides, ChatRequest, ChatResponse, ModelInfo, Price, Provider,
    ProviderConfig, ProviderFactory, ProviderRegistry, StopReason, StreamChunk, SystemRole,
};
pub use presets::{Api, CompatFlag, MaxTokensField, ModelTableSpec, ProviderDef, ReasoningField};
pub use secret::{AuthMethodKind, Credential, Secret};
pub use types::*;
