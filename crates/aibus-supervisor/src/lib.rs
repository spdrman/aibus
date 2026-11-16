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
#[cfg(unix)]
mod descendants;
pub mod host;
#[cfg(target_os = "linux")]
pub mod reaper;

pub use claude::{Channel, Normalizer};
pub use host::{supervise, Launch, Outcome, SupervisorError};

use std::time::{Duration, SystemTime};

use aibus_trace::Batch;

/// One attempt's stateful normaliser, the shape the Phase 1 gate ratified (A1.5 amendment E1): the
/// host feeds it every line from every channel in arrival order and writes what it returns. The
/// built-in Claude normaliser and the declarative one of an extension (#286) both implement it, and
/// `supervise` runs either, so the supervisor never knows which provider it is supervising.
pub trait Engine: Send {
    /// Makes the next event id sort after `id` (`supervise` seeds it from the run it writes to).
    fn observe(&mut self, id: uuid::Uuid);
    fn start(&mut self, at: SystemTime, info: serde_json::Value) -> Batch;
    fn stream_line(&mut self, line: &[u8], at: SystemTime) -> Batch;
    fn hook_line(&mut self, line: &[u8], at: SystemTime) -> Batch;
    fn stderr_line(&mut self, line: &[u8], at: SystemTime) -> Batch;
    fn gap(&mut self, channel: Channel, reason: &str, bytes: u64, at: SystemTime) -> Batch;
    fn finish(&mut self, ending: Ending, wall: Duration, at: SystemTime) -> Batch;
}

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
    /// stderr kept for the trace; past this it is drained and dropped, and one `stderr_limit` gap records the dropped bytes (the head up to the cap is kept).
    pub max_stderr_bytes: u64,
    /// Lines read a second, stdout and stderr together. Past this the host stops reading for a
    /// moment and the pipe pushes back on the CLI: nothing is dropped.
    pub lines_per_second: u32,
    /// After the CLI exits, the pipes may still hold output (or a descendant may hold them open).
    /// The host keeps reading and only ends the drain when nothing has arrived for this long.
    pub drain_stall: Duration,
    /// The most the drain after exit may take in all, whatever keeps arriving. If lines are still
    /// coming when it ends, a `gap_detected` (`unread_after_exit`) says so.
    pub drain_max: Duration,
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
            drain_stall: Duration::from_secs(2),
            drain_max: Duration::from_secs(30),
        }
    }
}
