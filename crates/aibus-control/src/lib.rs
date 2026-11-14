//! The control endpoint of a live terminal session (#274), both ends of it.
//!
//! A terminal session opens `Endpoint`: a Unix socket in the runtime directory, mode 0600, named
//! by the session id, with a token file and an info file next to it, all three removed when the
//! session ends. `aibus mcp --session` reaches it through `Link`. One JSON object per line each
//! way, the same framing as the headless control channel. Nothing here listens on a network.
#![forbid(unsafe_code)]

#[cfg(unix)]
use std::ffi::OsStr;
#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::path::{Path, PathBuf};

#[cfg(unix)]
use aibus_core::remote::Remote;
#[cfg(unix)]
use aibus_core::Redactor;
#[cfg(unix)]
use serde_json::Value;
#[cfg(unix)]
use tokio::sync::mpsc;

/// The longest line either end accepts: a 64 KiB text and its envelope.
pub const LINE_MAX: usize = 66 * 1024;
/// Lines a client may send a minute.
pub const LINES_PER_MINUTE: u32 = 60;
/// Of those, the ones that change the session (`send`, `command`, `cancel`, `permission`).
pub const CHANGES_PER_MINUTE: u32 = 10;

/// `$XDG_RUNTIME_DIR/aibus/sessions`, or `<tmp>/aibus-<uid>/sessions` where there is none.
#[cfg(unix)]
pub fn runtime_dir(_xdg_runtime: Option<&OsStr>, _tmp: &Path) -> PathBuf {
    todo!()
}

/// Makes `dir` (0700) or checks the one that is there: a directory, not a link, owned by this
/// user and closed to everyone else.
#[cfg(unix)]
pub fn private_dir(_dir: &Path) -> io::Result<()> {
    todo!()
}

/// The session's end of the socket.
#[cfg(unix)]
pub struct Endpoint {
    /// The user a peer has to be.
    pub(crate) uid: u32,
}

#[cfg(unix)]
impl Endpoint {
    pub fn open(_dir: &Path, _id: &str, _info: &Value) -> io::Result<Self> {
        todo!()
    }

    pub fn socket(&self) -> PathBuf {
        todo!()
    }

    pub fn token_file(&self) -> PathBuf {
        todo!()
    }

    pub fn info_file(&self) -> PathBuf {
        todo!()
    }

    pub fn serve(&mut self, _tx: mpsc::Sender<Remote>, _redactor: Redactor) -> io::Result<()> {
        todo!()
    }
}

/// The client's end, for `aibus mcp`.
#[cfg(unix)]
pub struct Link {}

#[cfg(unix)]
impl Link {
    pub fn new(_dir: PathBuf, _wanted: Option<String>) -> Self {
        todo!()
    }
}

#[cfg(unix)]
impl aibus_mcp::SessionLink for Link {
    fn list(&self) -> Result<Value, String> {
        todo!()
    }

    fn target(&self) -> Result<String, String> {
        todo!()
    }

    fn call(&self, _client: &str, _request: Value) -> Result<Value, String> {
        todo!()
    }
}

#[cfg(all(test, unix))]
mod tests;
