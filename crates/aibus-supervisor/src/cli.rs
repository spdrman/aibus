//! Finding, probing and invoking the official CLI: discovery, version and flag probes, the
//! allowlisted `auth status`, the argv and the bounded environment. Nothing here reads a credential.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use regex::Regex;
use serde_json::Value;
use tokio::io::AsyncReadExt;

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

impl std::error::Error for ProbeError {}

/// `2.1.311 (Claude Code)` to `Version(2, 1, 311)`.
pub fn parse_version(out: &str) -> Option<Version> {
    let re = Regex::new(r"^\s*(\d{1,6})\.(\d{1,6})\.(\d{1,6})\b").expect("valid regex");
    let c = re.captures(out)?;
    Some(Version(
        c[1].parse().ok()?,
        c[2].parse().ok()?,
        c[3].parse().ok()?,
    ))
}

/// Checks `--version` and `--help` output.
pub fn probe(version_out: &str, help_out: &str) -> Result<Probe, ProbeError> {
    /// The flags aibus cannot run without.
    const REQUIRED: [&str; 5] = [
        "--print",
        "--output-format",
        "--verbose",
        "--permission-prompts",
        "--setting-sources",
    ];
    /// The help paragraph of `flag`: its line and the indented lines after it, up to the next option.
    fn paragraph<'a>(help: &'a str, flag: &str) -> Vec<&'a str> {
        let mut lines = help.lines().skip_while(|l| {
            let t = l.trim_start();
            !(l.starts_with("  -")
                && (t.starts_with(&format!("{flag} "))
                    || t.contains(&format!(", {flag} "))
                    || t == flag))
        });
        let Some(first) = lines.next() else {
            return Vec::new();
        };
        let mut out = vec![first];
        out.extend(lines.take_while(|l| !l.starts_with("  -") && !l.trim().is_empty()));
        out
    }

    let found = parse_help(version_out, help_out)?;
    if found.version < MIN_VERSION {
        return Err(ProbeError::TooOld(found.version));
    }
    let flags = &found.flags;
    if let Some(missing) = REQUIRED.iter().find(|f| !flags.contains(**f)) {
        return Err(ProbeError::MissingFlag((*missing).into()));
    }
    let says_default = |flag: &str, also: &str| {
        let text = paragraph(help_out, flag).join(" ").to_lowercase();
        text.contains("default") && text.contains(also)
    };
    if says_default("--bare", "") || says_default("--print", "bare") {
        return Err(ProbeError::BareDefault);
    }
    Ok(found)
}

/// What `--version` and `--help` say, for any CLI: the version and every long flag, with no
/// opinion about which ones matter (an extension's manifest says that). `probe` adds the Claude
/// Code rules on top.
pub fn parse_help(version_out: &str, help_out: &str) -> Result<Probe, ProbeError> {
    let version = parse_version(version_out)
        .ok_or_else(|| ProbeError::Version(version_out.chars().take(80).collect()))?;
    let re = Regex::new(r"(?m)^ {2}(?:-\w, )?(--[A-Za-z][\w-]*)(?:, (--[A-Za-z][\w-]*))?")
        .expect("valid regex");
    let flags: BTreeSet<String> = re
        .captures_iter(help_out)
        .flat_map(|c| [c.get(1), c.get(2)])
        .flatten()
        .map(|m| m.as_str().to_owned())
        .collect();
    Ok(Probe {
        version,
        tested: version == TESTED_VERSION,
        flags,
    })
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
pub async fn discover(dirs: &[PathBuf], name: &str, env: &[(OsString, OsString)]) -> Discovery {
    /// Why a resolved executable must not be run, if it must not.
    fn unsafe_reason(real: &Path) -> Option<String> {
        let meta = match std::fs::metadata(real) {
            Ok(m) => m,
            Err(e) => return Some(format!("cannot stat: {e}")),
        };
        if !meta.is_file() {
            return Some("not a regular file".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let me = rustix::process::geteuid().as_raw();
            let mode = meta.permissions().mode();
            if mode & 0o111 == 0 {
                return Some("not executable".into());
            }
            if mode & 0o022 != 0 {
                return Some("writable by other users".into());
            }
            if meta.uid() != me && meta.uid() != 0 {
                return Some(format!(
                    "owned by uid {}, not this user or root",
                    meta.uid()
                ));
            }
            if let Some(parent) = real.parent().and_then(|p| std::fs::metadata(p).ok()) {
                if parent.permissions().mode() & 0o022 != 0 {
                    return Some("in a directory writable by other users".into());
                }
            }
        }
        None
    }

    let mut candidates = Vec::new();
    for dir in dirs {
        let path = dir.join(name);
        if !dir.is_absolute() {
            candidates.push(Candidate {
                path,
                resolved: None,
                verdict: Err("relative directory on the search path".into()),
            });
            continue;
        }
        if std::fs::symlink_metadata(&path).is_err() {
            continue;
        }
        let resolved = std::fs::canonicalize(&path).ok();
        let verdict = match &resolved {
            None => Err("does not resolve".into()),
            Some(real) => match unsafe_reason(real) {
                Some(why) => Err(why),
                None => {
                    match capture(real, &["--version"], env, Duration::from_secs(10), 4096).await {
                        Err(e) => Err(format!("--version failed: {e}")),
                        Ok(out) => {
                            let text = String::from_utf8_lossy(&out);
                            match parse_version(&text) {
                                None => Err(format!(
                                    "--version printed {:?}",
                                    text.trim().chars().take(80).collect::<String>()
                                )),
                                Some(v) if v < MIN_VERSION => {
                                    Err(format!("version {v} is older than {MIN_VERSION}"))
                                }
                                Some(v) => Ok(v),
                            }
                        }
                    }
                }
            },
        };
        candidates.push(Candidate {
            path,
            resolved,
            verdict,
        });
    }
    let chosen = candidates.iter().position(|c| c.verdict.is_ok());
    Discovery { candidates, chosen }
}

/// Runs `exe args` with exactly `env`, null stdin and no shell, and returns stdout (at most
/// `max_bytes`). For `--version`, `--help` and `auth status --json`.
pub async fn capture(
    exe: &Path,
    args: &[&str],
    env: &[(OsString, OsString)],
    timeout: Duration,
    max_bytes: usize,
) -> std::io::Result<Vec<u8>> {
    if !exe.is_absolute() {
        return Err(std::io::Error::other(
            "the executable must be an absolute path",
        ));
    }
    let mut child = tokio::process::Command::new(exe)
        .args(args)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("no stdout"))?;
    let mut out = Vec::new();
    let read = async {
        let mut buf = [0u8; 8192];
        loop {
            let n = stdout.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            let room = max_bytes.saturating_sub(out.len());
            out.extend_from_slice(&buf[..n.min(room)]);
        }
        child.wait().await
    };
    let status = tokio::time::timeout(timeout, read)
        .await
        .map_err(|_| std::io::Error::other("timed out"))??;
    if !status.success() {
        return Err(std::io::Error::other(format!("exited with {status}")));
    }
    Ok(out)
}

/// What `auth status --json` may contribute: four fields, nothing else (no email, org or token).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthStatus {
    pub logged_in: Option<bool>,
    pub auth_method: Option<String>,
    pub api_provider: Option<String>,
    pub subscription_type: Option<String>,
}

pub fn auth_status(json: &[u8]) -> Option<AuthStatus> {
    let v: Value = serde_json::from_slice(json).ok()?;
    let text = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .map(|s| s.chars().take(64).collect())
    };
    Some(AuthStatus {
        logged_in: v.get("loggedIn").and_then(Value::as_bool),
        auth_method: text("authMethod"),
        api_provider: text("apiProvider"),
        subscription_type: text("subscriptionType"),
    })
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
pub fn argv(probe: &Probe, req: &Request) -> Result<Vec<OsString>, ProbeError> {
    let mut a: Vec<OsString> = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--permission-prompts",
        "none",
        "--setting-sources",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    a.push(req.setting_sources.clone().into());
    let mut optional = |flag: &str, value: Option<&String>, on: bool| -> Result<(), ProbeError> {
        if !on {
            return Ok(());
        }
        if !probe.flags.contains(flag) {
            return Err(ProbeError::MissingFlag(flag.into()));
        }
        a.push(flag.into());
        if let Some(v) = value {
            a.push(v.into());
        }
        Ok(())
    };
    optional("--model", req.model.as_ref(), req.model.is_some())?;
    optional(
        "--max-budget-usd",
        req.max_budget_usd.as_ref(),
        req.max_budget_usd.is_some(),
    )?;
    optional("--include-hook-events", None, req.include_hook_events)?;
    optional("--forward-subagent-text", None, req.forward_subagent_text)?;
    optional(
        "--agents",
        req.agents_json.as_ref(),
        req.agents_json.is_some(),
    )?;
    Ok(a)
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
    parent: impl IntoIterator<Item = (OsString, OsString)>,
    pass: &[&str],
    set: &[(&str, &str)],
) -> Result<Vec<(OsString, OsString)>, EnvError> {
    const ALLOW: [&str; 8] = [
        "HOME",
        "USER",
        "LOGNAME",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TMPDIR",
        "CLAUDE_CONFIG_DIR",
    ];
    let credential =
        Regex::new(r"(?i)(KEY|TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIAL|AUTH|COOKIE|SESSION_ID)")
            .expect("valid regex");
    let inherited_session = |n: &str| n == "CLAUDECODE" || n.starts_with("CLAUDE_CODE_");
    for name in pass {
        if credential.is_match(name) || inherited_session(name) || *name == "PATH" {
            return Err(EnvError::Refused((*name).into()));
        }
    }
    for (name, _) in set {
        if credential.is_match(name) || *name == "PATH" {
            return Err(EnvError::Refused((*name).into()));
        }
    }
    let mut env: Vec<(OsString, OsString)> = vec![("PATH".into(), "/usr/bin:/bin".into())];
    for (k, v) in parent {
        let Some(name) = k.to_str() else { continue };
        if (ALLOW.contains(&name) || pass.contains(&name)) && !env.iter().any(|(e, _)| e == &k) {
            env.push((k, v));
        }
    }
    for (k, v) in set {
        env.retain(|(e, _)| e != k);
        env.push(((*k).into(), (*v).into()));
    }
    Ok(env)
}
