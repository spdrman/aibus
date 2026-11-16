//! The hook entries in the user's settings.
//!
//! One `http` hook per event, posting to a loopback endpoint with a per-install token in an
//! `Authorization` header. Claude Code treats a connection failure or a non-2xx answer as a
//! non-blocking error unless `onFailure` says `block`, which these entries never set, so a
//! session whose aibus is not listening is not slowed or stopped: the hook fails open.
//! Nothing is written without `confirm` having seen the exact diff.

use std::path::Path;

use serde_json::{json, Value};

/// The loopback endpoint and the per-install token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub port: u16,
    pub token: String,
}

/// The events the normaliser maps. No `PermissionRequest` (aibus never decides a prompt) and no
/// `Stop` (a Stop hook can hold a turn open, and a tap has no reason to be near that).
pub const EVENTS: &[&str] = &[
    "SessionStart",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "SubagentStart",
    "SubagentStop",
];

/// The path every entry posts to, and how aibus recognises its own entries again.
pub const HOOK_PATH: &str = "/aibus-tap/hook";

/// What a change did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Changed,
    Declined,
    Unchanged,
}

impl Endpoint {
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}{HOOK_PATH}", self.port)
    }

    /// The endpoint saved by an earlier install under `<state>/tap/endpoint.json`, or a new one
    /// (a free loopback port and a random token), saved with mode 0600.
    pub fn load_or_create(state: &Path) -> Result<Self, String> {
        let dir = state.join("tap");
        let file = dir.join("endpoint.json");
        if let Some(v) = std::fs::read(&file)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        {
            if let (Some(port), Some(token)) = (
                v["port"].as_u64().and_then(|p| u16::try_from(p).ok()),
                v["token"].as_str().filter(|t| t.len() >= 32),
            ) {
                return Ok(Self {
                    port,
                    token: token.to_owned(),
                });
            }
        }
        let port = std::net::TcpListener::bind(("127.0.0.1", 0))
            .and_then(|l| l.local_addr())
            .map_err(|e| format!("no free loopback port: {e}"))?
            .port();
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let ep = Self { port, token };
        write_private(
            &dir,
            &file,
            json!({"port": ep.port, "token": ep.token})
                .to_string()
                .as_bytes(),
        )?;
        Ok(ep)
    }
}

/// Creates `dir` (0700) and writes `file` (0600) with `bytes`.
pub(crate) fn write_private(dir: &Path, file: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(file)
        .map_err(|e| format!("{}: {e}", file.display()))?;
    f.write_all(bytes).map_err(|e| e.to_string())
}

/// The script a command hook runs, written by `install_files`.
pub const SCRIPT_NAME: &str = "aibus-tap-hook.sh";

/// Writes the hook script (0700) and the files it reads (0600) under `<state>/tap/`, and returns
/// the script's path.
pub fn install_files(_state: &Path, _ep: &Endpoint) -> Result<std::path::PathBuf, String> {
    Err("not implemented".into())
}

/// The settings `command` string for a script path.
pub fn command_for(script: &Path) -> String {
    script.display().to_string()
}

impl Endpoint {
    /// A new token for the same port, saved; the old one stops working.
    pub fn rotate(&self, _state: &Path) -> Result<Self, String> {
        Ok(self.clone())
    }
}

/// The new text of a settings file with aibus's hooks put in (`Some(command)`) or taken out
/// (`None`), or `None` when nothing changes. Refuses what it cannot edit.
pub fn edit(_text: &str, _command: Option<&str>) -> Result<Option<String>, String> {
    Ok(None)
}

/// A line diff of two texts.
pub fn diff(_before: &str, _after: &str) -> String {
    String::new()
}

/// Installs or removes the entries in `settings_path`, after `confirm` has seen the exact diff.
pub fn apply(
    _settings_path: &Path,
    _command: Option<&str>,
    _confirm: &mut dyn FnMut(&str) -> bool,
) -> Result<Outcome, String> {
    Err("not implemented".into())
}

/// Whether `header` (the `Authorization` value) carries the token. Compared in constant time, as
/// digests, so the length does not leak either.
pub fn accepts(ep: &Endpoint, header: Option<&str>) -> bool {
    use subtle::ConstantTimeEq;
    let want = aibus_trace::sha256_hex(format!("Bearer {}", ep.token).as_bytes());
    let got = aibus_trace::sha256_hex(header.unwrap_or("\0").as_bytes());
    want.as_bytes().ct_eq(got.as_bytes()).into()
}
