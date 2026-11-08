//! Provider trait, account registry, credentials and the fake provider.
#![forbid(unsafe_code)]

pub mod degrade;
pub mod error;
pub mod fake;
pub mod provider;
pub mod secret;
pub mod types;

pub use error::{ConfigError, ProviderError, ProviderErrorKind};
pub use provider::{
    AccountInfo, Capabilities, ChatRequest, ChatResponse, ModelInfo, Price, Provider,
    ProviderConfig, ProviderFactory, ProviderRegistry, StopReason, StreamChunk, SystemRole,
};
pub use secret::{AuthMethodKind, Credential, Secret};
pub use types::*;
