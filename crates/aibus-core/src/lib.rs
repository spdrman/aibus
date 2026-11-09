//! Engine: definitions, runs, tools, hooks, continuity, orchestrator.
#![forbid(unsafe_code)]

pub mod accounts;
pub mod advisor;
pub mod agent;
pub mod app;
pub mod assist;
pub mod config;
pub mod context;
pub mod credentials;
pub mod effects;
pub mod error;
pub mod hooks;
pub mod ids;
pub mod msg;
pub mod orchestrator;
pub mod permission;
pub mod persist;
pub mod redact;
pub mod replay;
pub mod router;
pub mod run;
pub mod state;
pub mod tool;
pub mod tools;
pub mod worktree;

pub use accounts::{AccountRegistry, SelectError, SelectHints};
pub use agent::*;
pub use app::{App, Ticker, MSG_CAPACITY};
pub use assist::{AdvisorSeat, Assist, RouterSeat, Seat};
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
pub use orchestrator::{parse_mention, Launcher, Limits, Orchestrator, Roster, Waited};
pub use run::*;
pub use tool::*;
pub use worktree::WorktreeInfo;
pub use tools::{
    effective_tools, Bash, Delegate, EffectiveTools, Fork, Gate, JobView, ResumeRun, SendMessage,
    ShellDeny, ToolExecutor, ToolRegistry,
};
