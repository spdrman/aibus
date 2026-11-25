//! Tap in to Claude Code sessions that are already open (#348 phases 1 and 2, #355, #373).
//!
//! * `agents`: the list, from `claude agents --json` (the one process this crate starts).
//! * `hooks`: the settings entries that make a session post its events to aibus, shown as a diff.
//! * `tap`: those events through the supervisor's `Normalizer` into a tap run directory.
//! * `intent`: plan and task files, shown as intent.
//! * `transcript`: the opt-in, version-gated transcript tail.
//! * `show`: one text for `aibus sessions tap` and `/tap`, built on `aibus_trace::view`.
//!
//! * `control`: guidance to a session (#373), over the documented inbox socket only, with consent
//!   at both ends. Nothing here types a key, answers a permission prompt or changes a setting.
#![forbid(unsafe_code)]

pub mod agents;
pub mod control;
pub mod hooks;
pub mod intent;
pub mod show;
pub mod tap;
pub mod transcript;

use std::path::PathBuf;

/// Where aibus keeps its state and where the CLI keeps its own.
#[derive(Clone, Debug)]
pub struct Env {
    /// aibus's state directory; taps live under `<state>/taps/claude/<session>/`.
    pub state: PathBuf,
    /// The CLI's config directory (`CLAUDE_CONFIG_DIR`, else `~/.claude`).
    pub config: PathBuf,
}

impl Env {
    /// The CLI's config directory from the process environment.
    pub fn claude_config_from_env() -> Option<PathBuf> {
        std::env::var_os("CLAUDE_CONFIG_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|v| !v.is_empty())
                    .map(|h| PathBuf::from(h).join(".claude"))
            })
    }

    /// `<state>/taps/claude`, where one directory per tapped session lives.
    pub fn taps(&self) -> PathBuf {
        self.state.join("taps").join("claude")
    }
}

/// Opens `path` for reading only when it is a regular file of this user, no bigger than `max`:
/// no link is followed (`O_NOFOLLOW`), a FIFO or device returns at once and is refused, and the
/// checks are made on the open file, so nothing can be swapped in between.
pub(crate) fn open_own(path: &std::path::Path, max: u64) -> Option<std::fs::File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)
        .ok()?;
    let m = f.metadata().ok()?;
    (m.is_file() && m.len() <= max && m.uid() == rustix::process::getuid().as_raw()).then_some(f)
}

/// The whole of `open_own(path, max)`.
pub(crate) fn read_own(path: &std::path::Path, max: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::new();
    open_own(path, max)?.read_to_end(&mut buf).ok()?;
    Some(buf)
}
