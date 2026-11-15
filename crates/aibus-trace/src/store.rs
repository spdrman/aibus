//! The append-only local trace store.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use aibus_core::persist::fs::{create_private_dir, write_atomic};
use aibus_core::Redactor;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    sha256_hex, AttemptRef, EventIds, EventKind, Origin, TraceEvent, Visibility, SCHEMA_VERSION,
};

/// Whether payloads (message text, tool input and output, the raw line of an unknown event) are
/// kept. Spec 10.3: metadata by default, payloads only when the user asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayloadPolicy {
    MetadataOnly,
    /// Keep redacted payloads, up to this many bytes for the whole run.
    Retain {
        max_bytes: u64,
    },
}

/// Size limits of one run's trace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoreLimits {
    /// Past this, `MessageChunk`, `UsageReported` and `TraceWarning` events are dropped (counted,
    /// with one `GapDetected`); lifecycle events are always written.
    pub soft_max_bytes: u64,
    /// The largest single event line.
    pub max_event_bytes: usize,
}

impl Default for StoreLimits {
    fn default() -> Self {
        Self {
            soft_max_bytes: 256 * 1024 * 1024,
            max_event_bytes: 64 * 1024,
        }
    }
}

/// The events one raw line produced, and that line's parsed payload (written only under
/// `PayloadPolicy::Retain`, redacted, and referenced from every event of the batch).
#[derive(Debug, Default)]
pub struct Batch {
    pub events: Vec<TraceEvent>,
    pub payload: Option<Value>,
}

/// What `open` found and repaired.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recovery {
    /// Bytes after the last complete line, moved to a `.torn-<offset>` file and cut off.
    pub torn_bytes: u64,
    /// Complete lines that are not an event of this schema. They stay where they are.
    pub corrupt_lines: u64,
}

/// What a read found besides the events.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadReport {
    pub corrupt_lines: u64,
    pub torn_bytes: u64,
    pub unsupported_schema: u64,
}

#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    /// Another writer holds the run.
    Locked(PathBuf),
    /// The run was written by schema 1, which orders by an integer `sequence`. Nothing is read,
    /// repaired or appended: the format was never released, so there is no migration.
    OldSchema {
        dir: PathBuf,
        lines: u64,
    },
    /// An event whose id is not a uuid v7, the key the trace orders by.
    NotUuidV7(Uuid),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "trace store: {e}"),
            Self::Locked(p) => write!(f, "trace store {} is held by another writer", p.display()),
            Self::OldSchema { dir, lines } => write!(
                f,
                "trace store {} holds {lines} schema 1 lines (integer sequence numbers); this build \
                 reads schema 2, where events order by their uuid v7 id. Move the run aside and \
                 record it again",
                dir.display()
            ),
            Self::NotUuidV7(id) => write!(
                f,
                "event id {id} is not a uuid v7, and a trace orders by that id"
            ),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

const EVENTS: &str = "events.jsonl";

/// One workflow run's trace directory, opened for appending by one writer.
///
/// ```text
/// <dir>/events.jsonl            one TraceEvent per line, append-only, 0600
/// <dir>/objects/<sha256>.json   redacted payloads, content-addressed, 0600 (Retain only)
/// <dir>/events.jsonl.torn-<n>   bytes a crash left after the last complete line
/// ```
///
/// Each `append` is one `write` of whole lines followed by `sync_data`, so a crash leaves at most
/// one torn line at the end, which the next `open` moves aside. The file is locked (`fs4`) while a
/// writer has it; the lock goes with the process, so a restart after a crash just opens it again.
pub struct TraceStore {
    dir: PathBuf,
    file: File,
    redactor: Redactor,
    policy: PayloadPolicy,
    limits: StoreLimits,
    /// Makes the ids of the store's own events, after every id seen so far.
    ids: EventIds,
    seen: HashSet<Uuid>,
    bytes: u64,
    payload_bytes: u64,
    over_soft_limit: bool,
    payload_budget_gap: bool,
    /// The run id, from the first event seen, for the store's own events.
    run: Option<Uuid>,
}

/// A private (0700) directory that is really a directory, owned by this user. One that already
/// exists with a wider mode is narrowed, and a link or a foreign owner is refused.
fn private_dir(path: &Path) -> io::Result<()> {
    create_private_dir(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = fs::symlink_metadata(path)?;
        if !meta.is_dir() {
            return Err(io::Error::other(format!(
                "{} is not a plain directory",
                path.display()
            )));
        }
        if meta.uid() != rustix::process::geteuid().as_raw() {
            return Err(io::Error::other(format!(
                "{} belongs to another user",
                path.display()
            )));
        }
        if meta.permissions().mode() & 0o077 != 0 {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

impl TraceStore {
    /// Opens (or creates) the run directory, takes the writer lock, and repairs a torn tail.
    pub fn open(
        dir: &Path,
        redactor: Redactor,
        policy: PayloadPolicy,
        limits: StoreLimits,
    ) -> Result<(Self, Recovery), StoreError> {
        use fs4::fs_std::FileExt;
        private_dir(dir)?;
        let path = dir.join(EVENTS);
        let mut opts = fs::OpenOptions::new();
        opts.read(true).append(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // A planted link at events.jsonl must not make the store rewrite what it points at.
            opts.mode(0o600)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
        }
        let mut file = opts.open(&path)?;
        if let Err(e) = file.try_lock_exclusive() {
            return Err(if e.kind() == io::ErrorKind::WouldBlock {
                StoreError::Locked(dir.to_owned())
            } else {
                StoreError::Io(e)
            });
        }

        let mut recovery = Recovery::default();
        let mut seen = HashSet::new();
        let mut last_id: Option<Uuid> = None;
        let mut old_lines = 0u64;
        let mut warned_corrupt = 0;
        let mut run = None;
        let mut good_len = 0u64;
        let mut raw = Vec::new();
        file.seek(SeekFrom::Start(0))?;
        file.read_to_end(&mut raw)?;
        for line in raw.split_inclusive(|b| *b == b'\n') {
            if !line.ends_with(b"\n") {
                break;
            }
            good_len += line.len() as u64;
            match serde_json::from_slice::<TraceEvent>(line) {
                Ok(e)
                    if e.schema_version == SCHEMA_VERSION && e.event_id.get_version_num() == 7 =>
                {
                    last_id = last_id.max(Some(e.event_id));
                    if e.kind == EventKind::TraceWarning && e.data["reason"] == "corrupt_lines" {
                        warned_corrupt = warned_corrupt.max(e.data["lines"].as_u64().unwrap_or(0));
                    }
                    run.get_or_insert(e.workflow_run_id);
                    seen.insert(e.event_id);
                }
                _ => {
                    let old = serde_json::from_slice::<Value>(line)
                        .is_ok_and(|v| v["schema_version"] == 1);
                    if old {
                        old_lines += 1;
                    }
                    recovery.corrupt_lines += 1;
                }
            }
        }
        // Before anything is repaired or written: a schema 1 run is left exactly as it is.
        if old_lines > 0 {
            return Err(StoreError::OldSchema {
                dir: dir.to_owned(),
                lines: old_lines,
            });
        }
        let total = raw.len() as u64;
        if good_len < total {
            recovery.torn_bytes = total - good_len;
            // Never through a link: drop whatever sits at the name, then create it fresh.
            let torn = dir.join(format!("{EVENTS}.torn-{good_len}"));
            let _ = fs::remove_file(&torn);
            let mut aside = fs::OpenOptions::new();
            aside.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                aside.mode(0o600);
            }
            aside.open(&torn)?.write_all(&raw[good_len as usize..])?;
            file.set_len(good_len)?;
            file.sync_all()?;
        }
        let mut store = Self {
            dir: dir.to_owned(),
            file,
            redactor,
            policy,
            limits,
            ids: last_id.map_or_else(EventIds::default, EventIds::after),
            seen,
            bytes: good_len,
            payload_bytes: 0,
            over_soft_limit: false,
            payload_budget_gap: false,
            run,
        };
        if recovery.torn_bytes > 0 {
            let e = store.own_event(
                EventKind::TraceWarning,
                Visibility::Observed,
                json!({ "reason": "torn_tail_recovered", "bytes": recovery.torn_bytes }),
            );
            store.append(Batch {
                events: vec![e],
                payload: None,
            })?;
        }
        // The lines stay in the file, so a reopen counts them again: warn only about new ones.
        if recovery.corrupt_lines > warned_corrupt {
            let e = store.own_event(
                EventKind::TraceWarning,
                Visibility::Observed,
                json!({ "reason": "corrupt_lines", "lines": recovery.corrupt_lines }),
            );
            store.append(Batch {
                events: vec![e],
                payload: None,
            })?;
        }
        Ok((store, recovery))
    }

    /// A generator seeded from the run: its next id sorts after every id the store holds.
    pub fn ids(&self) -> EventIds {
        self.ids
    }

    /// An event about the trace itself (a repair, a budget), run-wide: the nil attempt.
    fn own_event(&mut self, kind: EventKind, visibility: Visibility, data: Value) -> TraceEvent {
        const PARSER: &str = "aibus-trace-store/1";

        let attempt = AttemptRef {
            workflow_run_id: self.run.unwrap_or_else(Uuid::nil),
            attempt_id: Uuid::nil(),
            parent_attempt_id: None,
            provider_extension: String::new(),
        };
        let id = self
            .ids
            .next(SystemTime::now(), &[b"store", data.to_string().as_bytes()]);
        let mut e = TraceEvent::new(
            &attempt,
            id,
            kind,
            Origin::Derived,
            visibility,
            SystemTime::now(),
            PARSER,
        );
        e.data = data;
        e
    }

    /// Redacts and appends a batch, then syncs. The event id is the order and the identity: an id
    /// that is not a uuid v7 is refused (nothing from the batch is written), and an event whose id
    /// is already in the trace is skipped. Returns the events as written.
    pub fn append(&mut self, batch: Batch) -> Result<Vec<TraceEvent>, StoreError> {
        let Batch { events, payload } = batch;
        let mut keep: Vec<TraceEvent> = Vec::with_capacity(events.len());
        let mut storage_gap = false;
        if let Some(bad) = events.iter().find(|e| e.event_id.get_version_num() != 7) {
            return Err(StoreError::NotUuidV7(bad.event_id));
        }
        for e in events {
            if self.seen.contains(&e.event_id) || keep.iter().any(|k| k.event_id == e.event_id) {
                continue;
            }
            self.run.get_or_insert(e.workflow_run_id);
            let droppable = matches!(
                e.kind,
                EventKind::MessageChunk | EventKind::UsageReported | EventKind::TraceWarning
            );
            if droppable && self.bytes >= self.limits.soft_max_bytes {
                if !self.over_soft_limit {
                    self.over_soft_limit = true;
                    storage_gap = true;
                }
                continue;
            }
            keep.push(e);
        }
        // The store's own events go after everything in this batch, in the id order as well.
        for e in &keep {
            self.ids.observe(e.event_id);
        }
        if storage_gap {
            let soft = self.limits.soft_max_bytes;
            keep.push(self.own_event(
                EventKind::GapDetected,
                Visibility::Unavailable,
                json!({ "reason": "storage_limit", "soft_max_bytes": soft }),
            ));
        }
        if keep.is_empty() {
            return Ok(keep);
        }

        let payload_ref = match (payload, self.policy) {
            (Some(mut p), PayloadPolicy::Retain { max_bytes }) => {
                self.redactor.redact_json(&mut p);
                let body = serde_json::to_vec(&p).map_err(io::Error::other)?;
                if self.payload_bytes + body.len() as u64 > max_bytes {
                    if !self.payload_budget_gap {
                        self.payload_budget_gap = true;
                        keep.push(self.own_event(
                            EventKind::GapDetected,
                            Visibility::Unavailable,
                            json!({ "reason": "payload_budget_exhausted", "max_bytes": max_bytes }),
                        ));
                    }
                    None
                } else {
                    let hash = sha256_hex(&body);
                    let objects = self.dir.join("objects");
                    private_dir(&objects)?;
                    let path = objects.join(format!("{hash}.json"));
                    if !path.exists() {
                        write_atomic(&path, &body)?;
                        self.payload_bytes += body.len() as u64;
                    }
                    Some(format!("local://objects/{hash}.json"))
                }
            }
            _ => None,
        };

        let mut out = Vec::with_capacity(keep.len());
        let mut buf = Vec::new();
        for mut e in keep {
            if e.payload_ref.is_none() && e.origin != Origin::Derived {
                e.payload_ref.clone_from(&payload_ref);
            }
            self.redactor.redact_json(&mut e.data);
            // The ids come from the CLI's stream too: same redaction as the data.
            for id in [
                &mut e.agent_id,
                &mut e.parent_agent_id,
                &mut e.parent_tool_use_id,
                &mut e.native.session_id,
                &mut e.native.tool_use_id,
                &mut e.native.native_type,
            ]
            .into_iter()
            .flatten()
            {
                if let Cow::Owned(clean) = self.redactor.redact(id) {
                    *id = clean;
                }
            }
            let mut line = serde_json::to_vec(&e).map_err(io::Error::other)?;
            if line.len() >= self.limits.max_event_bytes {
                e.data = json!({ "truncated": true, "bytes": line.len() });
                e.evidence_refs.truncate(8);
                line = serde_json::to_vec(&e).map_err(io::Error::other)?;
            }
            line.push(b'\n');
            buf.extend_from_slice(&line);
            self.seen.insert(e.event_id);
            out.push(e);
        }
        self.file.write_all(&buf)?;
        self.file.sync_data()?;
        self.bytes += buf.len() as u64;
        Ok(out)
    }

    /// Every event in a run directory, in event id order, without taking the lock. `position` is
    /// each event's rank in that order.
    pub fn read(dir: &Path) -> io::Result<(Vec<TraceEvent>, ReadReport)> {
        let mut report = ReadReport::default();
        let mut events: Vec<TraceEvent> = Vec::new();
        let mut reader = BufReader::new(File::open(dir.join(EVENTS))?);
        let mut line = Vec::new();
        loop {
            line.clear();
            let n = reader.read_until(b'\n', &mut line)?;
            if n == 0 {
                break;
            }
            if !line.ends_with(b"\n") {
                report.torn_bytes += n as u64;
                break;
            }
            let v = serde_json::from_slice::<Value>(&line).ok();
            match v
                .as_ref()
                .and_then(|v| v.get("schema_version"))
                .and_then(Value::as_u64)
            {
                Some(s) if s == u64::from(SCHEMA_VERSION) => {
                    match serde_json::from_value::<TraceEvent>(v.unwrap_or_default()) {
                        Ok(e) => events.push(e),
                        Err(_) => report.corrupt_lines += 1,
                    }
                }
                Some(_) => report.unsupported_schema += 1,
                None => report.corrupt_lines += 1,
            }
        }
        events.sort_by_key(|e| e.event_id);
        for (i, e) in events.iter_mut().enumerate() {
            e.position = i as u64 + 1;
        }
        Ok((events, report))
    }
}
