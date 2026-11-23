//! Durable, aibus-owned run state (#289, spec 5.3). A run of a compiled plan lives in
//! `<root>/<run id>/`: the pinned `plan.json`, a `lock` and one append-only journal per attempt,
//! `<attempt id>.jsonl`. A journal holds what the host told the scheduler (`Input`), one record per
//! line, each with an event id (uuid v7), a monotonic sequence number and the time it was written.
//! The scheduler is deterministic, so replaying the inputs on the pinned plan rebuilds every
//! decision it made. Each record carries the event count and a hash chain over the record itself
//! and the scheduler events it added, so an edited line (any field of it) or a build that would
//! decide differently is refused on replay instead of trusted.
//!
//! A transition is written and synced before the host sees what it returns, and an input that
//! changes nothing is not written, so asking twice (a recovery that runs again, an approval sent
//! twice) records once. A torn last line is a write nobody was told about, and is cut off.
//!
//! Only aibus-owned state is here. A vendor session is never claimed restorable: after a restart a
//! vendor CLI step starts over in a new session, and only once a person agrees. A step with
//! irreversible side effects that was running at the crash goes to `requires_reconciliation`.
//!
//! Causl is not used for this yet: spec 7.4 asks for its event persistence and snapshot
//! durability to be validated before crash recovery leans on it, and nothing has validated that.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::compile::content_hash;
use super::handoff::{self, Artifact, Context, HandoffError};
use super::sched::{Action, Outcome, RunState, Scheduler, Status};
use crate::persist::fs::create_private_dir;

/// What the host tells a run. `Begin` opens an attempt and only `Run` writes it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Input {
    Begin {
        run_id: Uuid,
        attempt_id: Uuid,
        /// The attempt this one retries. Its trace is left as it was.
        previous: Option<Uuid>,
        plan_hash: String,
        /// Consents already delivered in earlier attempts of this run.
        spent_handoffs: BTreeSet<String>,
    },
    Advance,
    Report {
        step: String,
        outcome: Outcome,
    },
    Approve {
        step: String,
        by: String,
    },
    Deny {
        step: String,
        by: String,
    },
    SetAccount {
        step: String,
        provider: String,
        account: String,
    },
    Cancel {
        by: String,
    },
    Interrupt,
    Resume,
    Elapsed {
        seconds: u64,
    },
    Reconcile {
        step: String,
        applied: bool,
        by: String,
    },
    /// A handoff consent was used (`Run::deliver` writes it before the bytes are handed over).
    HandoffSpent {
        preview_sha256: String,
    },
}

/// One journal line.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Record {
    pub id: Uuid,
    pub seq: u64,
    pub at_unix_ms: u64,
    pub input: Input,
    /// The scheduler's event count after this input.
    pub events: u64,
    /// `sha256(previous chain + this record's other fields + the events its input added)`.
    pub chain: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DurableError {
    #[error("the plan was refused: {}", .0.join("; "))]
    Plan(Vec<String>),
    #[error("another aibus process has run {0} open")]
    Live(Uuid),
    #[error("{0}")]
    Refused(String),
    #[error("a person has to say whether these happened first: {}", .0.join(", "))]
    NeedsReconciliation(Vec<String>),
    #[error(
        "a {0:?} run is not retried: only a failed or cancelled one is (resume an interrupted one)"
    )]
    NotRetryable(RunState),
    #[error(transparent)]
    Handoff(#[from] HandoffError),
    #[error("{}: {why}", .path.display())]
    Journal { path: PathBuf, why: String },
    #[error(transparent)]
    Io(#[from] io::Error),
}

fn journal_error(path: &Path, why: impl Into<String>) -> DurableError {
    DurableError::Journal {
        path: path.to_owned(),
        why: why.into(),
    }
}

/// Opens `path` without following a symlink, creating it 0600 when `opts` says so, and refuses a
/// file that is not a regular file of ours that only we can read: someone else could have written
/// the approvals in it.
fn open_private(path: &Path, opts: &mut OpenOptions) -> Result<File, DurableError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    let file = opts.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(journal_error(path, "not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if meta.uid() != rustix::process::geteuid().as_raw() {
            return Err(journal_error(
                path,
                format!("owned by uid {}, not by you", meta.uid()),
            ));
        }
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(journal_error(
                path,
                format!(
                    "mode {:o}, want owner-only",
                    meta.permissions().mode() & 0o777
                ),
            ));
        }
    }
    Ok(file)
}

/// The complete lines of a journal, and how many bytes they take. A last line with no newline is
/// a write that never finished, and is not a record.
fn records(path: &Path, file: &mut File) -> Result<(Vec<Record>, u64), DurableError> {
    let mut raw = Vec::new();
    file.read_to_end(&mut raw)?;
    let mut out = Vec::new();
    let mut good = 0u64;
    for line in raw.split_inclusive(|b| *b == b'\n') {
        if !line.ends_with(b"\n") {
            break;
        }
        let r: Record = serde_json::from_slice(line)
            .map_err(|e| journal_error(path, format!("line {}: {e}", out.len() + 1)))?;
        out.push(r);
        good += line.len() as u64;
    }
    Ok((out, good))
}

/// Applies one input to the scheduler, the same way live and on replay.
fn step(s: &mut Scheduler, input: &Input) -> Result<Vec<Action>, String> {
    Ok(match input {
        Input::Begin { spent_handoffs, .. } => {
            s.spent_handoffs.extend(spent_handoffs.iter().cloned());
            Vec::new()
        }
        Input::Advance => s.advance(),
        Input::Report { step, outcome } => s.report(step, outcome.clone()).map(|()| Vec::new())?,
        Input::Approve { step, by } => s.approve(step, by).map(|()| Vec::new())?,
        Input::Deny { step, by } => s.deny(step, by).map(|()| Vec::new())?,
        Input::SetAccount {
            step,
            provider,
            account,
        } => s.set_account(step, provider, account),
        Input::Cancel { by } => s.cancel(by),
        Input::Interrupt => {
            s.interrupt();
            Vec::new()
        }
        Input::Resume => {
            s.resume();
            Vec::new()
        }
        Input::Elapsed { seconds } => s.elapsed(*seconds),
        Input::Reconcile { step, applied, by } => {
            s.reconcile(step, *applied, by).map(|()| Vec::new())?
        }
        Input::HandoffSpent { preview_sha256 } => {
            s.spent_handoffs.insert(preview_sha256.clone());
            Vec::new()
        }
    })
}

fn chain(previous: &str, r: &Record, s: &Scheduler, from: usize) -> Result<String, DurableError> {
    let head = serde_json::to_string(&(r.id, r.seq, r.at_unix_ms, &r.input, r.events))
        .map_err(io::Error::other)?;
    let added = serde_json::to_string(&s.events()[from..]).map_err(io::Error::other)?;
    Ok(content_hash(&format!("{previous}\n{head}\n{added}")))
}

/// Every attempt of a run, oldest first. Each one's trace stays readable (`trace`).
pub fn attempts(root: &Path, run_id: Uuid) -> Result<Vec<Uuid>, DurableError> {
    let mut out: Vec<Uuid> = fs::read_dir(root.join(run_id.to_string()))?
        .filter_map(|e| {
            let name = e.ok()?.file_name();
            Uuid::parse_str(name.to_str()?.strip_suffix(".jsonl")?).ok()
        })
        .collect();
    out.sort();
    Ok(out)
}

/// An attempt's records, read-only and without the lock, so a past attempt (or the one a live
/// process is writing) can be inspected at any time.
pub fn trace(root: &Path, run_id: Uuid, attempt: Uuid) -> Result<Vec<Record>, DurableError> {
    let path = root
        .join(run_id.to_string())
        .join(format!("{attempt}.jsonl"));
    let mut file = open_private(&path, OpenOptions::new().read(true))?;
    Ok(records(&path, &mut file)?.0)
}

/// A run open for writing. It holds the run's lock for as long as it lives, so a second process
/// (or a second `open` in this one) gets `Live` and a restart can tell an orphan from a live run.
pub struct Run {
    dir: PathBuf,
    run_id: Uuid,
    attempt_id: Uuid,
    plan: Value,
    sched: Scheduler,
    path: PathBuf,
    journal: File,
    seq: u64,
    chain: String,
    /// Bytes of complete records, where a failed write is cut back to.
    len: u64,
    /// A failed write that could not be cut back: nothing more is written through this handle.
    broken: bool,
    _lock: File,
}

impl Run {
    fn lock(dir: &Path, run_id: Uuid) -> Result<File, DurableError> {
        use fs4::fs_std::FileExt;
        let lock = open_private(
            &dir.join("lock"),
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false),
        )?;
        match lock.try_lock_exclusive() {
            Ok(()) => Ok(lock),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Err(DurableError::Live(run_id)),
            Err(e) => Err(e.into()),
        }
    }

    /// Validates `plan`, pins it under a new run id in `root` and begins the first attempt.
    pub fn create(root: &Path, plan: &Value) -> Result<Run, DurableError> {
        let sched = Scheduler::new(plan).map_err(DurableError::Plan)?;
        create_private_dir(root)?;
        let run_id = Uuid::now_v7();
        let dir = root.join(run_id.to_string());
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(&dir)?;
        let mut f = open_private(
            &dir.join("plan.json"),
            OpenOptions::new().write(true).create_new(true),
        )?;
        f.write_all(&serde_json::to_vec(plan).map_err(io::Error::other)?)?;
        f.sync_all()?;
        let lock = Self::lock(&dir, run_id)?;
        Self::begin(
            dir,
            run_id,
            None,
            plan.clone(),
            sched,
            lock,
            BTreeSet::new(),
        )
    }

    fn begin(
        dir: PathBuf,
        run_id: Uuid,
        previous: Option<Uuid>,
        plan: Value,
        sched: Scheduler,
        lock: File,
        spent_handoffs: BTreeSet<String>,
    ) -> Result<Run, DurableError> {
        // Attempt ids sort in the order they were made, which is how `open` finds the latest.
        let attempt_id = loop {
            let id = Uuid::now_v7();
            if previous.is_none_or(|p| id > p) {
                break id;
            }
        };
        let path = dir.join(format!("{attempt_id}.jsonl"));
        let journal = open_private(
            &path,
            OpenOptions::new().read(true).append(true).create_new(true),
        )?;
        #[cfg(unix)]
        File::open(&dir)?.sync_all()?;
        let input = Input::Begin {
            run_id,
            attempt_id,
            previous,
            plan_hash: sched.plan_hash().to_owned(),
            spent_handoffs,
        };
        let mut next = sched.clone();
        step(&mut next, &input).map_err(DurableError::Refused)?;
        let mut run = Run {
            dir,
            run_id,
            attempt_id,
            plan,
            sched,
            path,
            journal,
            seq: 0,
            chain: String::new(),
            len: 0,
            broken: false,
            _lock: lock,
        };
        run.write(input, next, 0)?;
        Ok(run)
    }

    /// Reopens run `run_id` after a restart: takes its lock (`Live` if a process still has it),
    /// replays the latest attempt's journal on the pinned plan and checks it decides the same.
    /// A run that had steps running is marked interrupted, since no process runs them any more;
    /// `Input::Resume` then picks it up. Opening twice records the interrupt once.
    pub fn open(root: &Path, run_id: Uuid) -> Result<Run, DurableError> {
        let dir = root.join(run_id.to_string());
        let lock = Self::lock(&dir, run_id)?;
        let plan_path = dir.join("plan.json");
        let mut text = String::new();
        open_private(&plan_path, OpenOptions::new().read(true))?.read_to_string(&mut text)?;
        let plan: Value =
            serde_json::from_str(&text).map_err(|e| journal_error(&plan_path, e.to_string()))?;
        let mut sched = Scheduler::new(&plan).map_err(DurableError::Plan)?;
        // The latest attempt that got as far as its first record: a crash in `retry` before the
        // new attempt's `Begin` was synced leaves an empty file, which is not an attempt.
        let mut found = None;
        for attempt_id in attempts(root, run_id)?.into_iter().rev() {
            let path = dir.join(format!("{attempt_id}.jsonl"));
            let mut journal = open_private(&path, OpenOptions::new().read(true).append(true))?;
            let (recs, len) = records(&path, &mut journal)?;
            if !recs.is_empty() {
                found = Some((attempt_id, path, journal, recs, len));
                break;
            }
        }
        let Some((attempt_id, path, journal, recs, len)) = found else {
            return Err(journal_error(&dir, "no attempt has begun"));
        };
        if journal.metadata()?.len() > len {
            journal.set_len(len)?;
            journal.sync_all()?;
        }
        let mut chain_now = String::new();
        for (i, r) in recs.iter().enumerate() {
            let why = |w: &str| journal_error(&path, format!("record {}: {w}", i + 1));
            if r.seq != i as u64 + 1 {
                return Err(why("sequence numbers are not contiguous"));
            }
            match (&r.input, i) {
                (
                    Input::Begin {
                        run_id: rid,
                        attempt_id: aid,
                        plan_hash,
                        ..
                    },
                    0,
                ) if *rid == run_id && *aid == attempt_id && plan_hash == sched.plan_hash() => {}
                (Input::Begin { .. }, _) => return Err(why("not the Begin of this attempt")),
                (_, 0) => return Err(why("an attempt starts with its Begin")),
                _ => {}
            }
            let before = sched.events().len();
            step(&mut sched, &r.input).map_err(|e| why(&format!("replay refused it: {e}")))?;
            chain_now = chain(&chain_now, r, &sched, before)?;
            if sched.events().len() as u64 != r.events || chain_now != r.chain {
                return Err(why(
                    "replaying it does not give the decisions recorded (edited, or another build)",
                ));
            }
        }
        let mut run = Run {
            dir,
            run_id,
            attempt_id,
            plan,
            sched,
            path,
            journal,
            seq: recs.len() as u64,
            chain: chain_now,
            len,
            broken: false,
            _lock: lock,
        };
        if run.sched.steps().any(|(_, s)| *s == Status::Running) {
            run.commit(Input::Interrupt)?;
        }
        Ok(run)
    }

    pub fn run_id(&self) -> Uuid {
        self.run_id
    }

    pub fn attempt_id(&self) -> Uuid {
        self.attempt_id
    }

    pub fn scheduler(&self) -> &Scheduler {
        &self.sched
    }

    /// Applies `input` and returns what the host should do. The record is on disk before this
    /// returns; a refusal, or an input that changes nothing, writes nothing.
    pub fn apply(&mut self, input: Input) -> Result<Vec<Action>, DurableError> {
        if matches!(input, Input::Begin { .. }) {
            return Err(DurableError::Refused(
                "an attempt begins once, through create or retry".into(),
            ));
        }
        self.commit(input)
    }

    fn commit(&mut self, input: Input) -> Result<Vec<Action>, DurableError> {
        let mut next = self.sched.clone();
        let before = next.events().len();
        let actions = step(&mut next, &input).map_err(DurableError::Refused)?;
        if next != self.sched {
            self.write(input, next, before)?;
        }
        Ok(actions)
    }

    fn write(&mut self, input: Input, next: Scheduler, before: usize) -> Result<(), DurableError> {
        if self.broken {
            return Err(journal_error(
                &self.path,
                "an earlier write failed and could not be undone; open the run again",
            ));
        }
        let mut r = Record {
            id: Uuid::now_v7(),
            seq: self.seq + 1,
            at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64),
            input,
            events: next.events().len() as u64,
            chain: String::new(),
        };
        r.chain = chain(&self.chain, &r, &next, before)?;
        let mut line = serde_json::to_vec(&r).map_err(io::Error::other)?;
        line.push(b'\n');
        if let Err(e) = self
            .journal
            .write_all(&line)
            .and_then(|()| self.journal.sync_data())
        {
            // Half a record must not sit under the next one.
            if self
                .journal
                .set_len(self.len)
                .and_then(|()| self.journal.sync_data())
                .is_err()
            {
                self.broken = true;
            }
            return Err(e.into());
        }
        self.len += line.len() as u64;
        self.seq = r.seq;
        self.chain = r.chain;
        self.sched = next;
        Ok(())
    }

    /// Hands a handoff's bytes over through `handoff::deliver`, and writes the consent spent
    /// before returning them, so a restart (or a retry of the run) never delivers it twice.
    pub fn deliver(
        &mut self,
        a: &Artifact,
        now: &Context,
        contents: Vec<(String, Vec<u8>)>,
    ) -> Result<Vec<(String, Vec<u8>)>, DurableError> {
        let mut spent = self.sched.spent_handoffs.clone();
        let out = handoff::deliver(a, now, contents, &mut spent)?;
        self.commit(Input::HandoffSpent {
            preview_sha256: a.preview_sha256.clone(),
        })?;
        Ok(out)
    }

    /// Starts a new attempt of a failed or cancelled run on the same pinned plan, under a new
    /// attempt id. The old journal is left as it was and made read-only. Refused while a step
    /// still waits for a person to say whether it happened.
    pub fn retry(&mut self) -> Result<(), DurableError> {
        let state = self.sched.state();
        if !matches!(state, RunState::Failed | RunState::Cancelled) {
            return Err(DurableError::NotRetryable(state));
        }
        let open: Vec<String> = self
            .sched
            .steps()
            .filter(|(_, s)| **s == Status::RequiresReconciliation)
            .map(|(k, _)| k.to_owned())
            .collect();
        if !open.is_empty() {
            return Err(DurableError::NeedsReconciliation(open));
        }
        let sched = Scheduler::new(&self.plan).map_err(DurableError::Plan)?;
        // A duplicate of the locked handle shares the lock: if `begin` fails it is dropped and
        // this attempt still holds the run, and once `next` replaces this one it holds it alone.
        let lock = self._lock.try_clone()?;
        let next = Self::begin(
            self.dir.clone(),
            self.run_id,
            Some(self.attempt_id),
            self.plan.clone(),
            sched,
            lock,
            self.sched.spent_handoffs.clone(),
        )?;
        let old = std::mem::replace(self, next);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&old.path, fs::Permissions::from_mode(0o400))?;
        }
        Ok(())
    }
}
