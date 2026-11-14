//! Finding, probing and invoking the official CLI: discovery, version and flag probes, the
//! allowlisted `auth status`, the argv and the bounded environment. Nothing here reads a credential.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// `major.minor.patch` of the CLI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// Older than this is refused (the stale 1.0.34 npm build the spike found on PATH, for one).
pub const MIN_VERSION: Version = Version(2, 1, 0);
/// The version the spike's fixtures come from. Any other runs, with a `trace_warning`.
pub const TESTED_VERSION: Version = Version(2, 1, 311);

/// What `--version` and `--help` said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    pub version: Version,
    pub tested: bool,
    /// Every long flag `--help` lists.
    pub flags: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeError {
    /// `--version` printed something that is not a version.
    Version(String),
    TooOld(Version),
    /// A flag aibus cannot run without, or one the request asked for, is not in `--help`.
    MissingFlag(String),
    /// `--help` says bare mode is the default for `-p`. Bare mode never reads OAuth or the
    /// keychain, so a subscription sign-in would silently stop working: refused, never worked around.
    BareDefault,
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Version(s) => write!(f, "cannot read a version from {s:?}"),
            Self::TooOld(v) => write!(f, "version {v} is older than {MIN_VERSION}"),
            Self::MissingFlag(flag) => write!(f, "this CLI has no {flag}"),
            Self::BareDefault => f.write_str(
                "this CLI says bare mode is the default for -p, which ignores the CLI's own sign-in",
            ),
        }
    }
}

/// `2.1.311 (Claude Code)` to `Version(2, 1, 311)`.
pub fn parse_version(_out: &str) -> Option<Version> {
    todo!()
}

/// Checks `--version` and `--help` output.
pub fn probe(_version_out: &str, _help_out: &str) -> Result<Probe, ProbeError> {
    todo!()
}

/// One executable found during discovery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// Where it was found.
    pub path: PathBuf,
    /// Its canonical path, when it resolves.
    pub resolved: Option<PathBuf>,
    /// Its version, or why it was passed over.
    pub verdict: Result<Version, String>,
}

/// Every candidate in directory order, and the one chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Discovery {
    pub candidates: Vec<Candidate>,
    pub chosen: Option<usize>,
}

/// Looks for `name` in each of `dirs`, refuses unsafe ones (relative directory, not a regular
/// file, not executable, writable by others, in a directory others can write, owned by someone
/// other than this user or root), runs `--version` on the rest and chooses the first at or above
/// `MIN_VERSION`. Every candidate stays in the result with its reason, so the user sees why.
pub async fn discover(_dirs: &[PathBuf], _name: &str, _env: &[(OsString, OsString)]) -> Discovery {
    todo!()
}

/// Runs `exe args` with exactly `env`, null stdin and no shell, and returns stdout (at most
/// `max_bytes`). For `--version`, `--help` and `auth status --json`.
pub async fn capture(
    _exe: &Path,
    _args: &[&str],
    _env: &[(OsString, OsString)],
    _timeout: Duration,
    _max_bytes: usize,
) -> std::io::Result<Vec<u8>> {
    todo!()
}

/// What `auth status --json` may contribute: four fields, nothing else (no email, org or token).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthStatus {
    pub logged_in: Option<bool>,
    pub auth_method: Option<String>,
    pub api_provider: Option<String>,
    pub subscription_type: Option<String>,
}

pub fn auth_status(_json: &[u8]) -> Option<AuthStatus> {
    todo!()
}

/// What an attempt asks of the CLI. The prompt is not here: it goes on stdin, never in argv.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub model: Option<String>,
    /// `--setting-sources`, always passed. `user` keeps a repository's own hooks and MCP servers
    /// from running (under `-p` the CLI runs them without a trust prompt).
    pub setting_sources: String,
    pub max_budget_usd: Option<String>,
    pub include_hook_events: bool,
    pub forward_subagent_text: bool,
    /// Inline `--agents` JSON (the native subagent definitions #283 compiles).
    pub agents_json: Option<String>,
}

/// The argv after the executable. Fixed: `-p --output-format stream-json --verbose
/// --permission-prompts none --setting-sources <..>`; optional flags only when the probe found them,
/// and an error when the request needs one it did not.
pub fn argv(_probe: &Probe, _req: &Request) -> Result<Vec<OsString>, ProbeError> {
    todo!()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnvError {
    /// A name the caller asked to pass or set that is never allowed through.
    Refused(String),
}

impl std::fmt::Display for EnvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(n) => write!(f, "{n} is never passed to the CLI"),
        }
    }
}

/// The child's whole environment: `PATH=/usr/bin:/bin`, the allowlist (`HOME`, `USER`, `LOGNAME`,
/// `LANG`, `LC_ALL`, `LC_CTYPE`, `TMPDIR`, `CLAUDE_CONFIG_DIR`) from `parent` when set, then `pass`
/// (more names from `parent`) and `set` (explicit values). The parent session's `CLAUDECODE` and
/// `CLAUDE_CODE_*` never come through `pass`; a credential-shaped name (key, token, secret,
/// password, credential, auth, cookie) is refused in `pass` and `set` alike.
pub fn bounded_env(
    _parent: impl IntoIterator<Item = (OsString, OsString)>,
    _pass: &[&str],
    _set: &[(&str, &str)],
) -> Result<Vec<(OsString, OsString)>, EnvError> {
    todo!()
}
