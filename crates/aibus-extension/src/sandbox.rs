//! The sandbox host: the only place a manifest becomes a process. The extension chooses nothing
//! the host does not allow: the executable is one of its declared file names found on the host's
//! own search path (by `cli::discover`, which refuses a relative directory and a file others can
//! write), the argv is its static tokens plus the optional flags the probe found, the environment
//! is `cli::bounded_env` over its declared names, the directory is the one the host granted, and
//! the limits are the smaller of what it asked and the host's ceiling. There is no ambient
//! credential: the keychain is never opened here and no `AIBUS_*`, credential-shaped or loader
//! variable is ever passed.

use std::ffi::OsString;
use std::path::PathBuf;

use aibus_supervisor::cli::{Probe as CliProbe, ProbeError};
use aibus_supervisor::{Engine, Launch, Limits};
use aibus_trace::AttemptRef;

use crate::manifest::{Capability, Manifest};
use crate::negotiate::{Incompatible, Negotiated};

/// What the host grants one launch. The extension cannot widen it.
#[derive(Clone, Debug)]
pub struct Grant {
    /// The only directory the CLI runs in. Absolute, existing; resolved before use.
    pub workdir: PathBuf,
    /// Absolute directories searched for the executable, in order.
    pub search_dirs: Vec<PathBuf>,
    /// The host's environment, from which only allowed names are taken.
    pub parent_env: Vec<(OsString, OsString)>,
    /// The most any extension may use; the manifest's request is clamped to it.
    pub ceiling: Limits,
    /// `macos`, `linux` or `windows`.
    pub os: String,
}

#[derive(Debug)]
pub struct Prepared {
    /// Ready for `supervise`, but for the prompt, which the caller puts in `stdin`.
    pub launch: Launch,
    pub negotiated: Negotiated,
    pub probe: CliProbe,
}

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("none of {names:?} was found as a safe executable: {why:?}")]
    NoExecutable {
        names: Vec<String>,
        why: Vec<String>,
    },
    #[error("probe: {0}")]
    Probe(#[from] ProbeError),
    #[error("{0}")]
    Incompatible(#[from] Incompatible),
    #[error("the extension asked for {0} but the CLI does not support it")]
    CapabilityMissing(String),
    #[error("environment: {0}")]
    Env(String),
    #[error("workdir: {0}")]
    Workdir(String),
    #[error("refused: {0}")]
    Refused(String),
}

/// The checks every launch has to pass whoever built it: an absolute executable whose file name is
/// one the manifest declared, argv that is the manifest's tokens plus its optional flags only, an
/// environment of allowed names only, and an existing absolute working directory.
pub fn check_launch(m: &Manifest, launch: &Launch) -> Result<(), SandboxError> {
    todo!("{m:?} {launch:?}")
}

/// Finds and probes the CLI, negotiates, and builds the launch. `want` are the optional
/// capabilities the caller would like the optional flags for; one the CLI lacks is an error, not a
/// silent drop.
pub async fn prepare(
    m: &Manifest,
    grant: &Grant,
    want: &[Capability],
) -> Result<Prepared, SandboxError> {
    todo!("{m:?} {grant:?} {want:?}")
}

/// The engine that reads this extension's events.
pub fn engine(m: &Manifest, attempt: AttemptRef) -> Box<dyn Engine> {
    todo!("{m:?} {attempt:?}")
}
