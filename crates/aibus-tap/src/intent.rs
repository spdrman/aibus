//! Plan and task files, as intent.

use std::path::Path;

use aibus_core::Redactor;

/// Lines for the intent section, redacted and cleaned.
pub fn read(
    _config: &Path,
    _session_id: &str,
    _started_ms: Option<i64>,
    _redactor: &Redactor,
) -> Vec<String> {
    Vec::new()
}
