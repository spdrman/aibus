//! The tap run directory and the hook receiver's side of it.
//!
//! A tapped session's trace lives in `<root>/<session id>/`, a normal trace store opened with
//! `PayloadPolicy::MetadataOnly`: names, statuses, ids and sizes, never message text or tool
//! input. Hook payloads go through the supervisor's `Normalizer` (`hook_line`).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use aibus_core::Redactor;
use aibus_supervisor::Normalizer;
use aibus_trace::{AttemptRef, PayloadPolicy, StoreLimits, TraceStore};
use serde_json::{json, Value};
use uuid::Uuid;

/// The most one hook payload may be: a tool's whole input can be big, and the store keeps none.
const MAX_BODY: usize = 1 << 20;
const META: &str = "tap-meta.json";

/// A plain session id: letters, digits, `-` and `_`, 1 to 64 of them. Never a path.
pub fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// `<root>/<session>/`, after checking the session id is a plain id.
pub fn run_dir(root: &Path, session_id: &str) -> Result<PathBuf, String> {
    if !valid_id(session_id) {
        return Err("a session id is 1 to 64 letters, digits, - or _".into());
    }
    Ok(root.join(session_id))
}

/// One tapped session's writer.
pub struct Tap {
    dir: PathBuf,
    session_id: String,
    store: TraceStore,
    norm: Normalizer,
}

fn uuid_of(label: &str, session_id: &str) -> Uuid {
    let hex = aibus_trace::sha256_hex(format!("{label}:{session_id}").as_bytes());
    Uuid::from_u128(u128::from_str_radix(&hex[..32], 16).unwrap_or(1))
}

impl Tap {
    pub fn open(root: &Path, session_id: &str, redactor: &Redactor) -> Result<Self, String> {
        let dir = run_dir(root, session_id)?;
        let (store, _) = TraceStore::open(
            &dir,
            redactor.clone(),
            PayloadPolicy::MetadataOnly,
            StoreLimits::default(),
        )
        .map_err(|e| format!("tap run {}: {e}", dir.display()))?;
        let norm = Normalizer::with_ids(
            AttemptRef {
                workflow_run_id: uuid_of("tap-run", session_id),
                attempt_id: uuid_of("tap-attempt", session_id),
                parent_attempt_id: None,
                provider_extension: "aibus.tap.claude".into(),
            },
            store.ids(),
        );
        Ok(Self {
            dir,
            session_id: session_id.to_owned(),
            store,
            norm,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// One hook payload (the POST body). Returns how many events it added. A payload for another
    /// session, over a megabyte or not a JSON object is refused and changes nothing.
    pub fn ingest(&mut self, body: &[u8], at: SystemTime) -> Result<usize, String> {
        if body.len() > MAX_BODY {
            return Err("hook payload over 1 MiB".into());
        }
        let v: Value = serde_json::from_slice(body).map_err(|_| "hook payload is not JSON")?;
        if v.get("session_id").and_then(Value::as_str) != Some(self.session_id.as_str()) {
            return Err("hook payload is for another session".into());
        }
        // One line, whatever the sender's formatting was.
        let line = serde_json::to_vec(&v).map_err(|e| e.to_string())?;
        let batch = self.norm.hook_line(&line, at);
        let n = self
            .store
            .append(batch)
            .map_err(|e| format!("tap store: {e}"))?
            .len();
        if let Some(id) = self.store.last_id() {
            self.norm.observe(id);
        }
        // The path is trusted only when it names this session's own file: a forged payload cannot
        // point the transcript tail at another session's transcript.
        let own = format!("{}.jsonl", self.session_id);
        let path = v
            .get("transcript_path")
            .and_then(Value::as_str)
            .filter(|p| {
                Path::new(p)
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy() == own)
            });
        let meta = json!({"transcript_path": path, "cwd": v.get("cwd").and_then(Value::as_str)});
        let _ = crate::hooks::write_private(
            &self.dir,
            &self.dir.join(META),
            meta.to_string().as_bytes(),
            0o600,
        );
        Ok(n)
    }
}

/// The transcript path a hook reported for the session, if one was recorded.
pub fn transcript_path(dir: &Path) -> Option<PathBuf> {
    let b = std::fs::read(dir.join(META)).ok()?;
    let v: Value = serde_json::from_slice(&b).ok()?;
    v["transcript_path"].as_str().map(PathBuf::from)
}
