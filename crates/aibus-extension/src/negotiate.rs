//! Compatibility negotiation and capability probes. The manifest declares; the real CLI decides:
//! a capability marked `probe_required` is supported only when the probe flags are in `--help`
//! and the version is high enough, and anything the host cannot deliver says so with a reason.

use std::collections::BTreeMap;

use aibus_supervisor::cli::Probe as CliProbe;

use crate::manifest::{Capability, Manifest};
use crate::version::Version;

/// The SDK protocol this host speaks. A manifest's `compat.protocol` has to contain it.
pub const PROTOCOL: Version = Version(1, 0, 0);

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Incompatible {
    #[error("the extension speaks SDK protocol {wants}, this host speaks {have}")]
    Protocol { wants: String, have: Version },
    #[error("the extension does not support {os} (it lists {lists})")]
    Os { os: String, lists: String },
    #[error("the CLI is version {version}, the extension was written for {wants}")]
    CliVersion { version: Version, wants: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Supported,
    Unsupported(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Negotiated {
    pub capabilities: BTreeMap<Capability, Verdict>,
}

impl Negotiated {
    pub fn supports(&self, c: Capability) -> bool {
        matches!(self.capabilities.get(&c), Some(Verdict::Supported))
    }
}

/// The checks that need no CLI: the protocol range and the operating system.
pub fn check_host(m: &Manifest, os: &str) -> Result<(), Incompatible> {
    todo!("{m:?} {os}")
}

/// Everything `check_host` says, then the CLI's version range, then each capability.
pub fn negotiate(m: &Manifest, os: &str, cli: &CliProbe) -> Result<Negotiated, Incompatible> {
    todo!("{m:?} {os} {cli:?}")
}
