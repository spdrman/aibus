//! Send guidance to a tapped Claude Code session (#348 phase 2, #373).
//!
//! The one path is the inbox socket the vendor documents for "a script or hook to post into a
//! session". It needs both ends to agree: I confirm each send at the keyboard, and the session's
//! own `crossSessionInbound` setting (read, never changed) must not be `refuse`. Nothing here
//! answers a permission prompt, edits a setting, types a key or resumes a session id.
//!
//! Every send is audited as a row with the client, the session id, the action, `via: "tap"` and a
//! hash of the redacted text, never the text itself.

use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use aibus_core::Redactor;
use serde_json::{json, Value};

use crate::agents::Session;
use crate::Env;

/// The most text one send may carry.
pub const MAX_TEXT: usize = 8 << 10;
const MAX_PER_MINUTE: usize = 10;
const MAX_SETTINGS: u64 = 1 << 20;

/// What a session's `crossSessionInbound` says about messages from another process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inbound {
    Unset,
    Accept,
    Hold,
    Refuse,
}

impl Inbound {
    /// How strict it is; a project or local value may only raise this.
    fn rank(self) -> u8 {
        match self {
            Self::Unset | Self::Accept => 0,
            Self::Hold => 1,
            Self::Refuse => 2,
        }
    }
}

/// The value one settings file holds, if it holds the key. An unrecognised value is not guessed
/// at: `unknown` is what to use for it.
fn read_value(path: &Path, own_only: bool, unknown: Inbound) -> Option<Inbound> {
    let buf = if own_only {
        crate::read_own(path, MAX_SETTINGS)?
    } else {
        // Managed settings are usually owned by root, so no ownership check; no link either.
        let f = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
            )
            .open(path)
            .ok()?;
        if !f.metadata().ok()?.is_file() {
            return None;
        }
        let mut b = Vec::new();
        f.take(MAX_SETTINGS).read_to_end(&mut b).ok()?;
        b
    };
    let v: Value = serde_json::from_slice(&buf).ok()?;
    let raw = v.get("crossSessionInbound")?;
    Some(match raw.as_str() {
        Some("accept") => Inbound::Accept,
        Some("hold") => Inbound::Hold,
        Some("refuse") => Inbound::Refuse,
        _ => unknown,
    })
}

/// Where the vendor puts managed settings on this OS.
pub fn managed_path() -> PathBuf {
    if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/ClaudeCode/managed-settings.json")
    } else {
        PathBuf::from("/etc/claude-code/managed-settings.json")
    }
}

/// `crossSessionInbound` with the vendor's precedence: managed settings win over the user's, and
/// a project or local value counts only when it is stricter than what is already in force (so a
/// repository can never loosen what the user or the administrator set). An unrecognised value
/// holds in user, project and local settings and refuses in managed settings.
pub fn inbound(config: &Path, project: &Path, managed: Option<&Path>) -> Inbound {
    let managed = managed.and_then(|p| read_value(p, false, Inbound::Refuse));
    let user = read_value(&config.join("settings.json"), true, Inbound::Hold);
    let mut now = managed.or(user).unwrap_or(Inbound::Unset);
    for f in ["settings.json", "settings.local.json"] {
        let p = project.join(".claude").join(f);
        if let Some(v) = read_value(&p, true, Inbound::Hold) {
            if v.rank() > now.rank() {
                now = v;
            }
        }
    }
    now
}

fn pid_alive(pid: u32) -> bool {
    i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .is_some_and(|p| rustix::process::test_kill_process(p).is_ok())
}

/// The inbox socket, only when it is a socket (not a link to one) of this user that nobody else
/// can write to.
fn trusted_socket(p: &Path) -> Result<(), String> {
    let m = std::fs::symlink_metadata(p).map_err(|e| format!("inbox socket: {e}"))?;
    if !m.file_type().is_socket() {
        return Err("inbox socket path is not a socket (links are not followed)".into());
    }
    if m.uid() != rustix::process::getuid().as_raw() {
        return Err("inbox socket belongs to another user".into());
    }
    if m.mode() & 0o022 != 0 {
        return Err(
            "inbox socket is group or world writable, so other users could write to it".into(),
        );
    }
    Ok(())
}

/// Takes the one-controller lock for the session: a file holding this process's pid. A lock
/// whose pid is gone is taken over.
fn lock(path: &Path) -> Result<(), String> {
    for _ in 0..2 {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
        {
            Ok(mut f) => {
                return writeln!(f, "{}", std::process::id()).map_err(|e| e.to_string());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let held = std::fs::read_to_string(path)
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok());
                if held.is_some_and(|p| p != std::process::id() && pid_alive(p)) {
                    return Err("another controller already holds this session".into());
                }
                let _ = std::fs::remove_file(path);
            }
            Err(e) => return Err(format!("control lock: {e}")),
        }
    }
    Err("another controller already holds this session".into())
}

fn audit_append(path: &Path, row: &Value) -> Result<(), String> {
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("audit log: {e}"))?;
    writeln!(f, "{row}").map_err(|e| format!("audit log: {e}"))
}

fn sent_in_last_minute(audit: &Path, now_ms: i64) -> usize {
    let Ok(s) = std::fs::read_to_string(audit) else {
        return 0;
    };
    s.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["via"] == "tap" && v["outcome"] == "sent")
        .filter_map(|v| v["ts_ms"].as_i64())
        .filter(|t| *t > now_ms - 60_000 && *t <= now_ms)
        .count()
}

/// Sends `text` to the session's inbox socket after `confirm` says yes to the exact text that
/// will leave (redacted). `managed` overrides where managed settings are read from.
///
/// Returns what happened in words: `sent`, and always `delivery unknown`, because the socket
/// gives no receipt.
pub fn send(
    env: &Env,
    session: &Session,
    text: &str,
    redactor: &Redactor,
    managed: Option<&Path>,
    now_ms: i64,
    confirm: &mut dyn FnMut(&str) -> bool,
) -> Result<String, String> {
    if session.state == "gone" {
        return Err("that session is gone; there is nothing to send to".into());
    }
    let Some(sock) = session.socket.as_deref() else {
        return Err(
            "no inbox socket is known for this session (the session list did not carry one)".into(),
        );
    };
    if !crate::tap::valid_id(&session.session_id) {
        return Err("not a plain session id".into());
    }
    let text = text.trim();
    if text.is_empty() {
        return Err("nothing to send".into());
    }
    if text.len() > MAX_TEXT {
        return Err("guidance is capped at 8 KiB".into());
    }
    let project = Path::new(&session.cwd);
    let default_managed = managed_path();
    let setting = inbound(
        &env.config,
        if project.is_absolute() {
            project
        } else {
            Path::new("/nonexistent")
        },
        managed.or_else(|| {
            default_managed
                .exists()
                .then_some(default_managed.as_path())
        }),
    );
    if setting == Inbound::Refuse {
        return Err("the session's crossSessionInbound is `refuse`, so nothing is sent; aibus never changes that setting".into());
    }
    trusted_socket(sock)?;

    let dir = env.state.join("taps").join("control");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(|e| format!("control dir: {e}"))?;
    let audit = dir.join("audit.jsonl");
    if sent_in_last_minute(&audit, now_ms) >= MAX_PER_MINUTE {
        return Err("more than 10 a minute: wait a little before sending again".into());
    }
    let lock_path = dir.join(format!("{}.lock", session.session_id));
    lock(&lock_path)?;
    // The lock is released on every exit from this block.
    let result = (|| -> Result<String, String> {
        let clean = redactor.redact(text);
        let message = format!(
            "[aibus] guidance sent from aibus by the person at the keyboard, not a permission answer: {clean}"
        );
        let prompt = format!(
            "send to {} ({}):\n{message}",
            aibus_trace::view::clean(&session.name, 40),
            aibus_trace::view::clean(&session.session_id, 8)
        );
        if !confirm(&prompt) {
            return Err("not confirmed, nothing sent".into());
        }
        let row = |outcome: &str| {
            json!({
                "ts_ms": now_ms, "via": "tap", "client": "claude", "session": session.session_id,
                "action": "guidance", "outcome": outcome,
                "sha256": aibus_trace::sha256_hex(clean.as_bytes()),
            })
        };
        let wrote = (|| -> std::io::Result<()> {
            let mut c = UnixStream::connect(sock)?;
            c.set_write_timeout(Some(Duration::from_secs(3)))?;
            let line = format!("{}\n", json!({"text": message}));
            c.write_all(line.as_bytes())?;
            c.flush()
        })();
        if let Err(e) = wrote {
            let _ = audit_append(audit, &row("failed"));
            return Err(format!("could not write to the inbox socket: {e}"));
        }
        audit_append(audit, &row("sent"))?;
        let mut out = String::from(
            "sent; delivery unknown (the socket gives no receipt, and aibus shows it as delivered only if a later event proves it)",
        );
        if setting == Inbound::Hold {
            out.push_str("\nheld by the session, approve it there");
        }
        Ok(out)
    })();
    let _ = std::fs::remove_file(&lock_path);
    result
}

/// The control section of the tap view: what can be done to this session and what never can.
pub fn status_lines(session: &Session, setting: Inbound) -> String {
    let never = "aibus never answers a permission prompt, changes a setting or types a key";
    if session.socket.is_none() || setting == Inbound::Refuse {
        let why = if setting == Inbound::Refuse {
            "the session refuses inbound messages (crossSessionInbound)"
        } else {
            "no inbox socket is known for this session"
        };
        return format!("control: watch only ({why}); {never}\n");
    }
    let held = if setting == Inbound::Hold {
        " (the session holds them, approve there)"
    } else {
        ""
    };
    format!(
        "control: send guidance{held} with `aibus sessions send <session> <text>`, confirmed each time; change the plan: guidance that asks for it; cancel: not available for this client; {never}\n"
    )
}
