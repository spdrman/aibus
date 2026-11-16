//! The tap run directory and the hook receiver's side of it.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// `<root>/<session>/`, after checking the session id is a plain id (never a path).
pub fn run_dir(_root: &Path, _session_id: &str) -> Result<PathBuf, String> {
    Err("not implemented".into())
}

/// One tapped session's writer.
pub struct Tap {
    dir: PathBuf,
}

impl Tap {
    pub fn open(_root: &Path, _session_id: &str) -> Result<Self, String> {
        Err("not implemented".into())
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// One hook payload (the POST body). Returns how many events it added.
    pub fn ingest(&mut self, _body: &[u8], _at: SystemTime) -> Result<usize, String> {
        Err("not implemented".into())
    }
}

/// The transcript path a hook reported for the session, if one was recorded.
pub fn transcript_path(_dir: &Path) -> Option<PathBuf> {
    None
}
