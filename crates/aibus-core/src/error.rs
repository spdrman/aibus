//! One error type for the binary, with the exit code mapping from 06 section 3.

use std::path::PathBuf;

use aibus_provider::{ConfigError, ProviderError};

use crate::ids::AccountName;
use crate::run::RunError;

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("no credential for account {0}")]
    Missing(AccountName),
    #[error("account {0} was rejected")]
    Rejected(AccountName),
    /// Also the TUI banner.
    #[error("no account serves model {0}; add one with: aibus account add <name> --provider <id>")]
    NoAccountForModel(String),
    /// A `key_ref` that is not `keyring:`, `env:` or `file:`. The value is never echoed: it may
    /// be the secret itself.
    #[error("account {0}: key_ref is not a reference")]
    NotAReference(AccountName),
    #[error("no account named {0}")]
    UnknownAccount(AccountName),
    #[error("{}: mode {:04o} lets other users read the key; run: chmod 600 {}", .0.display(), .1, .0.display())]
    FileMode(PathBuf, u32),
    #[error("{}: the key file is not owned by you", .0.display())]
    FileOwner(PathBuf),
    #[error("{}: {}", .0.display(), .1)]
    Unreadable(PathBuf, String),
    /// No keychain on this machine. `account add` offers `--env` and `--file` instead.
    #[error("no keychain available: {0}; use --env <VAR> or --file <PATH> instead, a key is never stored in plaintext")]
    NoKeychain(String),
    #[error("keychain: {0}")]
    Keychain(String),
}

#[derive(Debug, thiserror::Error)]
pub enum AibusError {
    #[error("usage: {0}")]
    Usage(String),
    #[error("config: {0}")]
    Config(#[from] ConfigError),
    /// The config or settings file is invalid; the message carries `path:line`.
    #[error("config: {0}")]
    ConfigFile(#[from] crate::config::ConfigFileError),
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
            AibusError::Config(_) | AibusError::ConfigFile(_) => 3,
            AibusError::Auth(_) => 4,
            AibusError::Provider(_) => 5,
            AibusError::Denied(_) => 6,
            AibusError::Interrupted => 7,
            AibusError::Budget(_) => 8,
            AibusError::Run(_) => 10,
        }
    }
}
