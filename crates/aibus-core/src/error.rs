//! One error type for the binary, with the exit code mapping from 06 section 3.

use aibus_provider::{ConfigError, ProviderError};

use crate::ids::AccountName;
use crate::run::RunError;

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("no credential for account {0}")]
    Missing(AccountName),
    #[error("account {0} was rejected")]
    Rejected(AccountName),
    #[error("no account serves model {0}")]
    NoAccountForModel(String),
    /// A `key_ref` that is not `keyring:`, `env:` or `file:`. The value is never echoed: it may
    /// be the secret itself.
    #[error("account {0}: key_ref is not a reference")]
    NotAReference(AccountName),
}

#[derive(Debug, thiserror::Error)]
pub enum AibusError {
    #[error("usage: {0}")]
    Usage(String),
    #[error("config: {0}")]
    Config(#[from] ConfigError),
    #[error("auth: {0}")]
    Auth(#[from] CredentialError),
    #[error("provider: {0}")]
    Provider(#[from] ProviderError),
    #[error("denied: {0}")]
    Denied(String),
    #[error("interrupted")]
    Interrupted,
    #[error("budget exceeded: {0}")]
    Budget(String),
    #[error("run failed: {0}")]
    Run(#[from] RunError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl AibusError {
    /// Process exit code (06 section 3). 9 is unassigned, 11 and 12 belong to `doctor` and
    /// `agent validate`, which do not produce an `AibusError`.
    pub fn exit_code(&self) -> u8 {
        match self {
            AibusError::Io(_) => 1,
            AibusError::Usage(_) => 2,
            AibusError::Config(_) => 3,
            AibusError::Auth(_) => 4,
            AibusError::Provider(_) => 5,
            AibusError::Denied(_) => 6,
            AibusError::Interrupted => 7,
            AibusError::Budget(_) => 8,
            AibusError::Run(_) => 10,
        }
    }
}
