//! `.aibus` bundles (#271): a set of workflows in one file, for packaging and sharing.
//!
//! A bundle is a plain, uncompressed tar (POSIX ustar) with a `.aibus` extension, the way a
//! `.docx` is a zip. The layout is fixed and nothing else is read:
//!
//! ```text
//! manifest.toml            format = 1, name, description, workflows = [...], agents = [...]
//! workflows/<name>.toml    one per workflow the manifest lists
//! agents/<name>.md         optional, one per agent the manifest lists
//! README or README.md      optional
//! ```
//!
//! A bundle is input from someone else, so it is read in memory, never unpacked to disk, with
//! hard limits, and refused whole at the first thing that is not exactly that layout.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ids::AgentName;
use crate::Redactor;

/// The file extension, without the dot. A bundle is `<name>.aibus`, never a bare `.aibus` (that
/// is the project directory's name).
pub const EXTENSION: &str = "aibus";

/// The `format` a manifest has to say.
pub const FORMAT: u32 = 1;

/// The most a bundle file may be, headers and padding included.
pub const MAX_BUNDLE_BYTES: usize = 1024 * 1024;

/// The most entries (files and the two directories) a bundle may hold.
pub const MAX_ENTRIES: usize = 64;

/// The most any one file inside a bundle may be: the `workflow_create` cap.
pub const MAX_FILE_BYTES: usize = super::MAX_BYTES;

/// `manifest.toml`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: u32,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub workflows: Vec<String>,
    #[serde(default)]
    pub agents: Vec<String>,
}

/// What a bundle holds once every check has passed. Texts are the exact bytes of the files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bundle {
    pub manifest: Manifest,
    /// `(name, text)` in the manifest's order.
    pub workflows: Vec<(String, String)>,
    /// `(name, text)` in the manifest's order.
    pub agents: Vec<(String, String)>,
    /// `(file name, text)`.
    pub readme: Option<(String, String)>,
}

/// Why a bundle was refused. Nothing of a refused bundle is listed, run or written.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum BundleError {
    #[error("the bundle is {0} bytes, the most is {MAX_BUNDLE_BYTES}")]
    TooBig(u64),
    #[error("the bundle has more than {MAX_ENTRIES} entries")]
    TooManyEntries,
    #[error("the bundle is cut short or not a tar file: {0}")]
    Truncated(String),
    #[error("a tar header is not valid: {0}")]
    BadHeader(String),
    #[error("{0} is a link; a bundle holds plain files only")]
    Link(String),
    #[error("{0} is a device, fifo or other special file; a bundle holds plain files only")]
    Special(String),
    #[error("{0} is an absolute path")]
    Absolute(String),
    #[error("{0} climbs out with `..`")]
    Traversal(String),
    #[error("{0} is not part of the bundle layout (manifest.toml, workflows/<name>.toml, agents/<name>.md, README)")]
    Outside(String),
    #[error("{0} is in the bundle twice")]
    Duplicate(String),
    #[error("{0} is {1} bytes, the most for one file is {MAX_FILE_BYTES}")]
    EntryTooBig(String, u64),
    #[error("`{0}` is not a name: lowercase letters, digits, - and _, starting with a letter or digit, 64 at most")]
    BadName(String),
    #[error("{0} is not UTF-8 text")]
    NotUtf8(String),
    #[error("{0} contains something that looks like a credential")]
    Credential(String),
    #[error("manifest.toml: {0}")]
    Manifest(String),
    #[error("workflows/{0}.toml: {1}")]
    Workflow(String, String),
    #[error("agents/{0}.md: {1}")]
    Agent(String, String),
    #[error("{0}: {1}")]
    Io(String, String),
}

/// Reads a bundle from its bytes. Every limit and refusal of the module comment applies, and
/// nothing is written anywhere.
pub fn read(bytes: &[u8], redactor: &Redactor) -> Result<Bundle, BundleError> {
    let _ = (bytes, redactor);
    todo!("#271")
}

/// Reads the bundle at `path`: a regular file of at most `MAX_BUNDLE_BYTES`, then `read`.
pub fn read_file(path: &Path, redactor: &Redactor) -> Result<Bundle, BundleError> {
    let _ = (path, redactor);
    todo!("#271")
}

/// Builds a bundle: `manifest.toml`, then each workflow and agent, then the README, as ustar
/// entries with fixed metadata (mode 0644, owner 0, time 0), so the same input gives the same
/// bytes. The result is read back with `read` before it is returned, so `pack` never makes a
/// bundle that `install` would refuse for its shape.
pub fn pack(
    name: &str,
    description: &str,
    workflows: &[(String, String)],
    agents: &[(String, String)],
    readme: Option<(&str, &str)>,
    redactor: &Redactor,
) -> Result<Vec<u8>, BundleError> {
    let _ = (name, description, workflows, agents, readme, redactor);
    todo!("#271")
}

/// Where `install` writes and what it may write.
pub struct Install<'a> {
    /// The scope's `workflows` directory (`<project>/.aibus/workflows` or `<config>/workflows`).
    pub workflows_dir: PathBuf,
    /// The scope's `agents` directory.
    pub agents_dir: PathBuf,
    /// The agents a step may name, besides the bundle's own.
    pub roster: &'a [AgentName],
    /// Replace files that are already there.
    pub force: bool,
    /// Trust the bundle's gates (and agent hooks): the `--allow gates` of a write.
    pub allow_gates: bool,
}

/// Why `install` wrote nothing.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum InstallError {
    #[error(transparent)]
    Bundle(#[from] BundleError),
    #[error("workflows/{0}.toml does not validate: {1}")]
    Invalid(String, String),
    #[error("workflow {0}, step {1} has a gate, a shell command aibus runs as you ({2}); install with --allow gates to trust it")]
    Gate(String, usize, String),
    #[error("agent {0} has hooks or a permission mode, which run or allow commands as you; install with --allow gates to trust it")]
    AgentPowers(String),
    #[error("{} exist(s), install with --force to replace", .0.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "))]
    Exists(Vec<PathBuf>),
    #[error("{0} is not a plain file or directory, refusing to write through it")]
    NotPlain(PathBuf),
    #[error("{0}")]
    Io(String),
}

/// Unpacks a bundle into loose files, through the checks `workflow_create` makes (name, 64 KiB,
/// 32 steps, `validate` against the roster, credentials, the gate rule) plus the bundle's own.
/// Every check runs before anything touches the disk; then the files are written atomically with
/// mode 0600, byte for byte as they are in the bundle. Returns the paths written.
pub fn install(
    bytes: &[u8],
    to: &Install<'_>,
    redactor: &Redactor,
) -> Result<Vec<PathBuf>, InstallError> {
    let _ = (bytes, to, redactor);
    todo!("#271")
}
