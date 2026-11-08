//! Engine: definitions, runs, tools, hooks, continuity, orchestrator.
#![forbid(unsafe_code)]

pub mod accounts;
pub mod agent;
pub mod app;
pub mod config;
pub mod context;
pub mod credentials;
pub mod effects;
pub mod error;
pub mod hooks;
pub mod ids;
pub mod msg;
pub mod permission;
pub mod persist;
pub mod redact;
pub mod replay;
pub mod run;
pub mod state;
pub mod tool;
pub mod tools;

pub use accounts::{AccountRegistry, SelectError, SelectHints};
pub use agent::*;
pub use app::{App, Ticker, MSG_CAPACITY};
pub use credentials::{
    AccountCredential, CredentialSource, CredentialStore, KeyRef, Keychain, KeychainError,
    MemoryKeychain, StoreHealth,
};
pub use redact::Redactor;
pub use effects::{Effect, Effects, PendingTool, SpawnSpec};
pub use error::{AibusError, CredentialError};
pub use hooks::{Blocked, Fired, HookInput, HookRunner, Ran};
pub use ids::*;
pub use msg::*;
pub use run::*;
pub use tool::*;
pub use tools::{
    effective_tools, Bash, EffectiveTools, Gate, JobView, ShellDeny, ToolExecutor, ToolRegistry,
};
