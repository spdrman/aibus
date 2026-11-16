//! The file side of `/sessions` and `/tap` (#355), the way `runs` is for `/runs`.

use std::path::Path;

use aibus_core::{Msg, SessionsQuery};

/// What `query` shows. `state` is aibus's state directory, `config` the CLI's; `lister` is the
/// `claude agents --json` call, passed in so a test never needs the CLI.
pub fn apply(
    _query: &SessionsQuery,
    _state: &Path,
    _config: &Path,
    _lister: &dyn Fn() -> Result<Vec<u8>, String>,
) -> Vec<Msg> {
    Vec::new()
}
