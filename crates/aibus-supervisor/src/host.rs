//! The process host: one attempt from spawn to exit.

use std::ffi::OsString;
use std::path::PathBuf;

use aibus_trace::{AttemptStatus, StoreError, TraceStore};
use tokio_util::sync::CancellationToken;

use crate::{Ending, Limits, Normalizer};

/// Everything needed to start the CLI. `exe` must be absolute (from `cli::discover`), `cwd` an
/// existing directory, and `env` is the whole environment (from `cli::bounded_env`).
#[derive(Clone, Debug)]
pub struct Launch {
    pub exe: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
    /// Written to stdin, which is then closed (the prompt).
    pub stdin: Vec<u8>,
    pub limits: Limits,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub ending: Ending,
    pub status: AttemptStatus,
}

#[derive(Debug)]
pub enum SupervisorError {
    /// The launch was refused before anything ran.
    Refused(String),
    Spawn(std::io::Error),
    Store(StoreError),
}

impl std::fmt::Display for SupervisorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(why) => write!(f, "launch refused: {why}"),
            Self::Spawn(e) => write!(f, "could not start the CLI: {e}"),
            Self::Store(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for SupervisorError {}

/// Runs one attempt: spawns the CLI in its own process group, writes the prompt, feeds every
/// stdout and stderr line through `norm` into `store` as it arrives, and stops it on `cancel`, the
/// wall-time limit or the output limit (SIGTERM to the group, then SIGKILL after `grace`). Returns
/// once the process has exited and `attempt_completed` or `attempt_failed` is in the store.
pub async fn supervise(
    _launch: Launch,
    _norm: &mut Normalizer,
    _store: &mut TraceStore,
    _cancel: CancellationToken,
) -> Result<Outcome, SupervisorError> {
    todo!()
}
