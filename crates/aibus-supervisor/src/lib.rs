//! The CLI supervisor (#282, spec 5.1).
//!
//! `cli` finds the official executable, probes its version and flags and builds the argv and the
//! bounded environment; `host` launches it (absolute path, argv vector, no shell, null or prompt
//! stdin), reads stdout and stderr under size, rate and time limits, cancels gracefully and then
//! kills the process group; `claude` turns Claude Code's `stream-json` lines and hook payloads into
//! trace events. The attempt ends when the process ends, never at the first `result` line.
#![forbid(unsafe_code)]

pub mod claude;
pub mod cli;
pub mod host;

pub use claude::Normalizer;
pub use host::{supervise, Launch, Outcome, SupervisorError};

use std::time::Duration;

/// How the process ended, as the host saw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    /// It exited by itself with this code.
    Exited(i32),
    /// A signal aibus did not send ended it.
    Signalled(i32),
    /// The user's cancel ended it (gracefully, or by the kill after the grace period).
    Cancelled,
    /// The wall-time limit ended it.
    WallTime,
    /// It wrote more than `Limits::max_stdout_bytes`.
    OutputLimit,
}

/// The host's limits for one attempt (spec 5.1: wall time, event size, storage).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub wall_time: Duration,
    /// Between the graceful stop (SIGTERM to the process group) and the kill.
    pub grace: Duration,
    /// A stdout line longer than this is skipped to its newline and recorded as a gap.
    pub max_line_bytes: usize,
    /// All stdout together; past this the attempt is stopped.
    pub max_stdout_bytes: u64,
    /// stderr kept for the trace; past this it is still drained, and only counted.
    pub max_stderr_bytes: u64,
    /// Lines read a second, stdout and stderr together. Past this the host stops reading for a
    /// moment and the pipe pushes back on the CLI: nothing is dropped.
    pub lines_per_second: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            wall_time: Duration::from_secs(30 * 60),
            grace: Duration::from_secs(5),
            max_line_bytes: 8 * 1024 * 1024,
            max_stdout_bytes: 1024 * 1024 * 1024,
            max_stderr_bytes: 1024 * 1024,
            lines_per_second: 2_000,
        }
    }
}
