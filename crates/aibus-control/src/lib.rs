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
/// Connections that have not said hello yet; past this a new one is closed at once.
pub const PENDING_MAX: usize = 16;

/// This process's user.
#[cfg(unix)]
pub(crate) fn uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// The longest name a session puts in its directory: an id of `valid_id`'s longest, and the
/// `.<16 hex>.tmp` a socket has while it is made. `sun_path` takes 104 bytes on macOS and 108 on
/// Linux, and 100 leaves room under both.
#[cfg(unix)]
const SOCKET_PATH_MAX: usize = 100;
#[cfg(unix)]
const LONGEST_NAME: usize = 40 + ".0123456789abcdef.tmp".len();

/// `$XDG_RUNTIME_DIR/aibus/sessions`, or `<tmp>/aibus-<uid>/sessions` where there is none
/// (macOS). When the second would put a socket past what `sun_path` takes (macOS's `$TMPDIR` is
/// about 49 characters), it is `<short>/aibus-<uid>/sessions` instead, with `short` a base the
/// binary takes as `/tmp`. `$XDG_RUNTIME_DIR` is the user's own choice and is never replaced.
#[cfg(unix)]
pub fn runtime_dir(xdg_runtime: Option<&OsStr>, tmp: &Path, short: &Path) -> PathBuf {
    if let Some(run) = xdg_runtime.map(Path::new).filter(|p| p.is_absolute()) {
        return run.join("aibus").join("sessions");
    }
    let under = |base: &Path| base.join(format!("aibus-{}", uid())).join("sessions");
    let dir = under(tmp);
    if dir.as_os_str().len() + 1 + LONGEST_NAME <= SOCKET_PATH_MAX {
        dir
    } else {
        under(short)
    }
}

/// The directory this process uses: `runtime_dir` of `$XDG_RUNTIME_DIR`, the temporary directory
/// and `/tmp`. The session and `aibus mcp` both call it, so they always agree on where to look.
#[cfg(unix)]
pub fn default_runtime_dir() -> PathBuf {
    runtime_dir(
        std::env::var_os("XDG_RUNTIME_DIR").as_deref(),
        &std::env::temp_dir(),
        Path::new("/tmp"),
    )
}

/// Makes the session directory, or checks the one that is there. `dir` is `<base>/<aibus>/sessions`
/// (`runtime_dir`): `sessions` and the directory above it have to be directories of this user,
/// not links, mode 0700; `<base>` (`$XDG_RUNTIME_DIR` or the temporary directory, resolved) has
/// to be this user's or root's, and when others can write to it, sticky, so nobody else can
/// rename what is in it. What is missing is made 0700; what fails is refused, never repaired:
/// somebody else may have made it.
#[cfg(unix)]
pub fn private_dir(dir: &Path) -> io::Result<()> {
    let parent = dir.parent().ok_or_else(|| refused(dir, "has no parent"))?;
    let base = parent
        .parent()
        .ok_or_else(|| refused(parent, "has no parent"))?;
    if !base.exists() {
        aibus_core::persist::fs::create_private_dir(base)?;
    }
    check_base(base)?;
    for d in [parent, dir] {
        match std::fs::symlink_metadata(d) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new().mode(0o700).create(d)?;
            }
            Err(e) => return Err(e),
            Ok(_) => {}
        }
        check_private(d)?;
    }
    Ok(())
}

/// The checks of `private_dir` without making anything, for the client's side.
#[cfg(unix)]
pub(crate) fn check_chain(dir: &Path) -> io::Result<()> {
    let parent = dir.parent().ok_or_else(|| refused(dir, "has no parent"))?;
    let base = parent
        .parent()
        .ok_or_else(|| refused(parent, "has no parent"))?;
    check_private(dir)?;
    check_private(parent)?;
    check_base(base)
}

#[cfg(unix)]
fn refused(path: &Path, why: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("{} {why}", path.display()),
    )
}

/// `$XDG_RUNTIME_DIR` or the temporary directory, links resolved: this user's or root's, and
/// sticky when others can write to it.
#[cfg(unix)]
fn check_base(base: &Path) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let real = std::fs::canonicalize(base)?;
    let meta = std::fs::metadata(&real)?;
    let mode = meta.permissions().mode();
    if !meta.is_dir() || (meta.uid() != uid() && meta.uid() != 0) {
        return Err(refused(&real, "is not a directory of this user or of root"));
    }
    if mode & 0o022 != 0 && mode & 0o1000 == 0 {
        return Err(refused(
            &real,
            "can be written by others and is not sticky, so they could rename what is in it",
        ));
    }
    Ok(())
}

/// One directory of ours: a directory (not a link), owned by this user, closed to everyone else.
#[cfg(unix)]
pub(crate) fn check_private(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let meta = std::fs::symlink_metadata(dir)?;
    let open = meta.permissions().mode() & 0o077 != 0;
    if !meta.is_dir() || meta.uid() != uid() || open {
        return Err(refused(
            dir,
            &format!(
                "is not a private directory of this user (want mode 0700, owned by uid {})",
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

#[cfg(unix)]
pub(crate) use aibus_core::remote::client_name;

#[cfg(all(test, unix))]
mod tests;
