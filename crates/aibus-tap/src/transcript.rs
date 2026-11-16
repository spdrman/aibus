//! The transcript tail, behind an explicit per-client opt-in.

use std::path::Path;

use aibus_core::Redactor;

/// Claude Code versions the parser was checked against (synthetic fixtures; see the spec).
pub const TESTED: &[&str] = &[];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// The user has not run `aibus sessions allow-transcripts claude`.
    Off,
    /// On, and the version is one the parser was tested on.
    Tested,
    /// On, but the version is not tested: only what parses is shown.
    UntestedVersion,
    /// On, but the file was refused or unreadable.
    Unreadable(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tail {
    pub status: Status,
    /// `role: text`, redacted, cleaned and cut.
    pub messages: Vec<String>,
    pub tool_calls: u64,
    /// Entries or blocks of a type the parser does not know: `trace_warning unknown_variant`.
    pub unknown_variants: u64,
    pub bytes_read: u64,
}

pub fn allowed(_state: &Path) -> bool {
    true
}

pub fn set_allowed(_state: &Path, _on: bool) -> Result<(), String> {
    Ok(())
}

/// Reads the end of the transcript at `path` when the opt-in is on.
pub fn tail(
    _state: &Path,
    _config: &Path,
    _path: &Path,
    _version: Option<&str>,
    _redactor: &Redactor,
) -> Tail {
    Tail {
        status: Status::Tested,
        messages: Vec::new(),
        tool_calls: 0,
        unknown_variants: 0,
        bytes_read: 0,
    }
}
