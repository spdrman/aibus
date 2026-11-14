//! The append-only local trace store.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use aibus_core::Redactor;
use serde_json::Value;

use crate::TraceEvent;

/// Whether payloads (message text, tool input and output, the raw line of an unknown event) are
/// kept. Spec 10.3: metadata by default, payloads only when the user asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayloadPolicy {
    MetadataOnly,
    /// Keep redacted payloads, up to this many bytes for the whole run.
    Retain {
        max_bytes: u64,
    },
}

/// Size limits of one run's trace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoreLimits {
    /// Past this, `MessageChunk`, `UsageReported` and `TraceWarning` events are dropped (counted,
    /// with one `GapDetected`); lifecycle events are always written.
    pub soft_max_bytes: u64,
    /// The largest single event line.
    pub max_event_bytes: usize,
}

impl Default for StoreLimits {
    fn default() -> Self {
        Self {
            soft_max_bytes: 256 * 1024 * 1024,
            max_event_bytes: 64 * 1024,
        }
    }
}

/// The events one raw line produced, and that line's parsed payload (written only under
/// `PayloadPolicy::Retain`, redacted, and referenced from every event of the batch).
#[derive(Debug, Default)]
pub struct Batch {
    pub events: Vec<TraceEvent>,
    pub payload: Option<Value>,
}

/// What `open` found and repaired.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recovery {
    /// Bytes after the last complete line, moved to a `.torn-<offset>` file and cut off.
    pub torn_bytes: u64,
    /// Complete lines that are not an event of this schema. They stay where they are.
    pub corrupt_lines: u64,
}

/// What a read found besides the events.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadReport {
    pub corrupt_lines: u64,
    pub torn_bytes: u64,
    pub unsupported_schema: u64,
}

#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    /// Another writer holds the run.
    Locked(PathBuf),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "trace store: {e}"),
            Self::Locked(p) => write!(f, "trace store {} is held by another writer", p.display()),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// One workflow run's trace directory, opened for appending by one writer.
///
/// ```text
/// <dir>/events.jsonl            one TraceEvent per line, append-only, 0600
/// <dir>/objects/<sha256>.json   redacted payloads, content-addressed, 0600 (Retain only)
/// <dir>/events.jsonl.torn-<n>   bytes a crash left after the last complete line
/// ```
pub struct TraceStore {
    #[allow(dead_code)]
    file: File,
}

impl TraceStore {
    /// Opens (or creates) the run directory, takes the writer lock, and repairs a torn tail.
    pub fn open(
        _dir: &Path,
        _redactor: Redactor,
        _policy: PayloadPolicy,
        _limits: StoreLimits,
    ) -> Result<(Self, Recovery), StoreError> {
        todo!()
    }

    /// Redacts, numbers and appends a batch, then syncs. An event whose id is already in the trace
    /// is skipped. Returns the events as written.
    pub fn append(&mut self, _batch: Batch) -> Result<Vec<TraceEvent>, StoreError> {
        todo!()
    }

    /// The next sequence number.
    pub fn next_sequence(&self) -> u64 {
        todo!()
    }

    /// Every event in a run directory, in file order, without taking the lock.
    pub fn read(_dir: &Path) -> io::Result<(Vec<TraceEvent>, ReadReport)> {
        todo!()
    }
}
