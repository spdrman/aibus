//! Tap in to Claude Code sessions that are already open (#348 phase 1, #355). Read-only.
//!
//! * `agents`: the list, from `claude agents --json` (the one process this crate starts).
//! * `hooks`: the settings entries that make a session post its events to aibus, shown as a diff.
//! * `tap`: those events through the supervisor's `Normalizer` into a tap run directory.
//! * `intent`: plan and task files, shown as intent.
//! * `transcript`: the opt-in, version-gated transcript tail.
//! * `show`: one text for `aibus sessions tap` and `/tap`, built on `aibus_trace::view`.
//!
//! Nothing here sends to a session: no inbox socket, no keystroke, no permission answer.
#![forbid(unsafe_code)]

pub mod agents;
pub mod hooks;
pub mod intent;
pub mod show;
pub mod tap;
pub mod transcript;

use std::path::PathBuf;

/// Where aibus keeps its state and where the CLI keeps its own.
#[derive(Clone, Debug)]
pub struct Env {
    /// aibus's state directory; taps live under `<state>/taps/claude/<session>/`.
    pub state: PathBuf,
    /// The CLI's config directory (`CLAUDE_CONFIG_DIR`, else `~/.claude`).
    pub config: PathBuf,
}

impl Env {
    /// The CLI's config directory from the process environment.
    pub fn claude_config_from_env() -> Option<PathBuf> {
        None
    }
}
