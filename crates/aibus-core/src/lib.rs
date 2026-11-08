//! Engine: definitions, runs, tools, hooks, continuity, orchestrator.
#![forbid(unsafe_code)]

pub mod agent;
pub mod app;
pub mod effects;
pub mod error;
pub mod ids;
pub mod msg;
pub mod run;
pub mod state;
pub mod tool;
pub mod tools;

pub use agent::*;
pub use app::{App, Ticker, MSG_CAPACITY};
pub use effects::{Effect, Effects, PendingTool, SpawnSpec};
pub use error::{AibusError, CredentialError};
pub use ids::*;
pub use msg::*;
pub use run::*;
pub use tool::*;
pub use tools::{effective_tools, EffectiveTools, ToolExecutor, ToolRegistry};
