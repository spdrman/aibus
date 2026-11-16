//! The Linux subreaper helper (#352). Not implemented yet.

use std::ffi::OsString;

/// The first argument that selects helper mode. Internal: not a documented command.
pub const MARKER: &str = "--aibus-internal-subreaper";

/// Runs the helper; `args` is what follows `MARKER`. Returns the exit code.
pub fn run(_args: Vec<OsString>) -> u8 {
    eprintln!("aibus: the subreaper helper is not implemented");
    70
}
