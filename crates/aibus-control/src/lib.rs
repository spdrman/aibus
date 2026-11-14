//! The control endpoint of a live terminal session (#274), both ends of it.
//!
//! A terminal session opens an `Endpoint`: a Unix socket in the runtime directory, mode 0600,
//! named by the session id, with a token file and an info file next to it (both 0600), all three
//! removed when the session ends and swept by the next session when one died without cleaning up.
//! `aibus mcp --session` reaches it through a `Link`. One JSON object per line each way, the same
//! framing as the headless control channel. Nothing here listens on a network.
//!
//! What a connection has to get past, in order: the peer is this user (the kernel's peer
//! credentials), its first line is a `hello` with the session's token (compared in constant
//! time), and no other client holds the session. After that every line is capped in size, lines
//! are rate limited, and the ones that change the session have a tighter limit of their own.
#![forbid(unsafe_code)]

#[cfg(unix)]
mod endpoint;
#[cfg(unix)]
mod link;

#[cfg(unix)]
pub use endpoint::Endpoint;
#[cfg(unix)]
pub use link::Link;

#[cfg(unix)]
use std::ffi::OsStr;
#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::path::{Path, PathBuf};

/// The longest line either end accepts: a 64 KiB text, even with every character escaped, and its
/// envelope.
pub const LINE_MAX: usize = 400 * 1024;
/// Lines a client may send a minute.
pub const LINES_PER_MINUTE: u32 = 60;
/// Of those, the ones that change the session (`send`, `command`, `cancel`, `permission`).
pub const CHANGES_PER_MINUTE: u32 = 10;

/// This process's user.
#[cfg(unix)]
pub(crate) fn uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// `$XDG_RUNTIME_DIR/aibus/sessions`, or `<tmp>/aibus-<uid>/sessions` where there is none (macOS).
#[cfg(unix)]
pub fn runtime_dir(xdg_runtime: Option<&OsStr>, tmp: &Path) -> PathBuf {
    match xdg_runtime.map(Path::new).filter(|p| p.is_absolute()) {
        Some(run) => run.join("aibus").join("sessions"),
        None => tmp.join(format!("aibus-{}", uid())).join("sessions"),
    }
}

/// Makes `dir` (0700, and its missing parents) or checks the one that is there: a directory, not
/// a link, owned by this user and closed to everyone else. A directory that fails is refused, never
/// repaired: somebody else may have made it.
#[cfg(unix)]
pub fn private_dir(dir: &Path) -> io::Result<()> {
    if !dir.exists() {
        aibus_core::persist::fs::create_private_dir(dir)?;
    }
    check_private(dir)
}

#[cfg(unix)]
pub(crate) fn check_private(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let meta = std::fs::symlink_metadata(dir)?;
    let open = meta.permissions().mode() & 0o077 != 0;
    if !meta.is_dir() || meta.uid() != uid() || open {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} is not a private directory of this user (want mode 0700, owned by uid {})",
                dir.display(),
                uid()
            ),
        ));
    }
    Ok(())
}

/// A session id names files, so it is checked like a name: a ulid's characters, nothing else.
#[cfg(unix)]
pub(crate) fn valid_id(id: &str) -> bool {
    (1..=40).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// `<dir>/<id>.<ext>`.
#[cfg(unix)]
pub(crate) fn file(dir: &Path, id: &str, ext: &str) -> PathBuf {
    dir.join(format!("{id}.{ext}"))
}

/// The name a client gave, safe to put on a screen and in a log: no control characters, at most
/// 32 characters, `client` when nothing is left.
#[cfg(unix)]
pub(crate) fn client_name(raw: &str) -> String {
    let name: String = raw.chars().filter(|c| !c.is_control()).take(32).collect();
    match name.trim() {
        "" => "client".into(),
        n => n.into(),
    }
}

#[cfg(all(test, unix))]
mod tests;
