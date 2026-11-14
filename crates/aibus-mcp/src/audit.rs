//! The audit log: `<state>/logs/mcp/<ulid>.jsonl`, one process per file, one redacted line per
//! tool call (and one for the start, which records the flags).
//!
//! `{t, client:{name, version}, addr?, method, tool, args, ok, ms, run?}`. `addr` is the peer
//! address of an HTTP request and absent over stdio. The arguments are the
//! caller's own text, so they are redacted before they are measured and capped at 4 KiB; the
//! whole line goes through the redactor once more before it is written, since a client name is
//! text from outside too.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use aibus_core::persist::fs::create_private_dir;
use aibus_core::persist::time::rfc3339;
use aibus_core::Redactor;
use rmcp::model::Implementation;
use serde_json::{json, Value};

/// The most bytes of (redacted) arguments one line keeps.
const ARGS_CAP: usize = 4096;

pub struct AuditLog {
    file: Mutex<File>,
    redactor: Redactor,
}

impl AuditLog {
    /// A new file in `dir`, mode 0600 (the directory is made with 0700).
    pub fn create(dir: &Path, redactor: Redactor) -> io::Result<Self> {
        create_private_dir(dir)?;
        let mut opts = OpenOptions::new();
        opts.create_new(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let file = opts.open(dir.join(format!("{}.jsonl", ulid::Ulid::new())))?;
        Ok(Self {
            file: Mutex::new(file),
            redactor,
        })
    }

    /// Writes one line. A failure to write is logged and otherwise ignored: a full disk must not
    /// turn a read-only call into an error.
    // Nine arguments, and every one is a different column of the line. Six callers each pass their
    // own values, so a struct to carry them would only be a second spelling of the same call.
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &self,
        client: Option<&Implementation>,
        addr: Option<SocketAddr>,
        method: &str,
        tool: Option<&str>,
        run: Option<&str>,
        args: &Value,
        ok: bool,
        took: Duration,
    ) {
        let text = self.redactor.redact(&args.to_string()).into_owned();
        let args = if text.len() <= ARGS_CAP {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        } else {
            let mut end = ARGS_CAP;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            Value::String(format!("{}...", &text[..end]))
        };
        let line = json!({
            "t": rfc3339(SystemTime::now()),
            "client": client.map(|c| json!({ "name": c.name, "version": c.version })),
            "addr": addr.map(|a| a.to_string()),
            "method": method,
            "tool": tool,
            "run": run,
            "args": args,
            "ok": ok,
            "ms": took.as_millis() as u64,
        });
        let line = self.redactor.redact(&line.to_string()).into_owned();
        let written = {
            let mut f = self.file.lock().unwrap_or_else(|e| e.into_inner());
            writeln!(f, "{line}")
        };
        if let Err(e) = written {
            tracing::warn!("mcp audit log: {e}");
        }
    }
}
