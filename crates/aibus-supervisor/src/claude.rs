//! Claude Code `stream-json` and hook payloads to trace events.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use aibus_trace::{AttemptRef, Batch};
use serde_json::Value;

use crate::Ending;

/// The parser id and version stamped on every event this module writes.
pub const PARSER: &str = "claude-code-stream-json/1";

/// The provider extension id on every event. Provisional: Anthropic's legal page says the Claude
/// Code name cannot be part of another product's or feature's name (spike #281, condition 2), so
/// the final id is the owner's to pick in #286. A trace stores it as a plain string.
pub const EXTENSION_ID: &str = "org.aibus.reference-cli";

/// Where a raw line came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Channel {
    Stdout,
    Stderr,
    Hooks,
    /// aibus's own observations (spawn, exit, limits).
    Host,
}

/// What the normaliser has seen, for the attempt's summary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub lines: u64,
    pub duplicates: u64,
    pub malformed: u64,
    pub unknown: u64,
    pub results: u64,
    /// Known native types that carry nothing the trace projects (`hook_started`, `task_progress`,
    /// `rate_limit_event`, ...), by type.
    pub ignored: BTreeMap<String, u64>,
}

/// One attempt's normaliser. Feed it lines in arrival order from any channel; it keeps the ids it
/// needs to link children to parents and to recognise a fact a second time, all bounded.
pub struct Normalizer {
    #[allow(dead_code)]
    attempt: AttemptRef,
}

impl Normalizer {
    pub fn new(attempt: AttemptRef) -> Self {
        Self { attempt }
    }

    /// `attempt_started`, from the host. `info` is small metadata (CLI version, flags).
    pub fn start(&mut self, _at: SystemTime, _info: Value) -> Batch {
        todo!()
    }

    /// One stdout line (without its newline).
    pub fn stream_line(&mut self, _line: &[u8], _at: SystemTime) -> Batch {
        todo!()
    }

    /// One hook payload (one JSON object per line; blank lines are skipped).
    pub fn hook_line(&mut self, _line: &[u8], _at: SystemTime) -> Batch {
        todo!()
    }

    /// One stderr line. Its text reaches the store only as a payload, and never when it looks like
    /// a login or credential prompt.
    pub fn stderr_line(&mut self, _line: &[u8], _at: SystemTime) -> Batch {
        todo!()
    }

    /// A gap the host found (a line over the size limit, a limit that stopped the attempt).
    pub fn gap(&mut self, _channel: Channel, _reason: &str, _bytes: u64, _at: SystemTime) -> Batch {
        todo!()
    }

    /// The process ended: closes what is still open and writes `attempt_completed` or
    /// `attempt_failed` with the status, so the attempt ends here and only here.
    pub fn finish(&mut self, _ending: Ending, _wall: Duration, _at: SystemTime) -> Batch {
        todo!()
    }

    pub fn counters(&self) -> &Counters {
        todo!()
    }

    /// Entries in the normaliser's maps and sets, which are capped.
    pub fn tracked(&self) -> usize {
        todo!()
    }
}
