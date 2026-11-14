//! A session on disk (07 section 1): `session.json`, the snapshot, the commit log, `msgs.jsonl.zst`
//! and the runs. `SessionWriter` is what a live session writes through; `load` is what
//! `session resume` reads back.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use aibus_graph::{Graph, Snapshot};
use serde::{Deserialize, Serialize};

use super::commits::{self, CommitLog, CommitRecord};
use super::frames::FrameWriter;
use super::fs::{create_private_dir, write_atomic};
use super::transcript::{run_of, RunMeta, Transcript};
use crate::app::App;
use crate::config::Persist;
use crate::error::AibusError;
use crate::ids::{RunId, SessionId};
use crate::msg::Msg;
use crate::redact::Redactor;
use crate::replay::msg_to_json;
use crate::state::migrate;
use crate::state::{PERSIST_DUE, SCHEMA};
use aibus_graph::Value;

pub const SNAPSHOT: &str = "graph.snapshot.json.zst";
pub const MSGS: &str = "msgs.jsonl.zst";
pub const MEMORY: &str = "memory.json";

/// `session.json`.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct SessionMeta {
    pub schema_version: i64,
    pub id: String,
    pub started: String,
    pub topic: Option<String>,
    pub cwd: PathBuf,
    pub lead: String,
    pub outcome: Option<String>,
    pub ended: Option<String>,
    /// The outcome given for the handoff (`--outcome`, `AIBUS_OUTCOME`), canonical.
    #[serde(default)]
    pub handoff_outcome: Option<String>,
    /// A pinned session is never pruned.
    #[serde(default)]
    pub pinned: bool,
}

impl SessionMeta {
    pub fn new(id: SessionId, started: String, topic: Option<String>, cwd: PathBuf, lead: &str) -> Self {
        Self {
            schema_version: SCHEMA,
            id: id.to_string(),
            started,
            topic,
            cwd,
            lead: lead.into(),
            outcome: None,
            ended: None,
            handoff_outcome: None,
            pinned: false,
        }
    }

    pub fn write(&self, dir: &Path) -> io::Result<()> {
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        write_atomic(&dir.join("session.json"), text)
    }

    pub fn read(dir: &Path) -> io::Result<Self> {
        let text = fs::read_to_string(dir.join("session.json"))?;
        serde_json::from_str(&text).map_err(io::Error::other)
    }
}

/// The `meta.json` of every run under `dir/runs`, by run id.
pub fn read_runs(dir: &Path) -> Vec<RunMeta> {
    let mut runs: Vec<RunMeta> = fs::read_dir(dir.join("runs"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| fs::read_to_string(e.path().join("meta.json")).ok())
        .filter_map(|t| serde_json::from_str(&t).ok())
        .collect();
    runs.sort_by(|a, b| a.id.cmp(&b.id));
    runs
}

pub fn write_snapshot(dir: &Path, snap: &Snapshot) -> io::Result<()> {
    let json = serde_json::to_vec(snap).map_err(io::Error::other)?;
    write_atomic(
        &dir.join(SNAPSHOT),
        zstd::bulk::compress(&json, super::frames::LEVEL)?,
    )
}

pub fn read_snapshot(dir: &Path) -> io::Result<Snapshot> {
    let raw = fs::read(dir.join(SNAPSHOT))?;
    let json = zstd::stream::decode_all(raw.as_slice())?;
    serde_json::from_slice(&json).map_err(io::Error::other)
}

/// What `session resume` needs: the snapshot, brought up to the current schema, and the logged
/// commits after it.
pub fn load(dir: &Path) -> Result<(Snapshot, Vec<CommitRecord>), AibusError> {
    let snap = read_snapshot(dir)?;
    let snap = migrate::chain(migrate::version_of(&snap), snap)
        .map_err(|e| AibusError::Usage(e.to_string()))?;
    let tail = commits::read_all(dir)?
        .into_iter()
        .filter(|r| r.time > snap.now)
        .collect();
    Ok((snap, tail))
}

pub struct SessionWriter {
    dir: PathBuf,
    redactor: Redactor,
    msgs: FrameWriter,
    commits: CommitLog,
    runs: HashMap<RunId, Transcript>,
    store_thinking: bool,
    result_max: usize,
}

impl SessionWriter {
    /// Opens (creating) the session directory. `result_max` is
    /// `limits.transcript_result_max_bytes`.
    pub fn open(
        dir: &Path,
        redactor: Redactor,
        persist: &Persist,
        result_max: usize,
    ) -> io::Result<Self> {
        create_private_dir(&dir.join("runs"))?;
        let now = Instant::now();
        Ok(Self {
            dir: dir.to_owned(),
            redactor,
            msgs: FrameWriter::open(&dir.join(MSGS), now)?,
            commits: CommitLog::open(dir, commits::ROTATE_AT, now)?,
            runs: HashMap::new(),
            store_thinking: persist.store_thinking,
            result_max,
        })
    }

    /// The message a run was started with (the lead's prompt, or a worker's task).
    pub fn prompt(&mut self, run: RunId, at: SystemTime, text: &str) -> io::Result<()> {
        match self.runs.get_mut(&run) {
            Some(t) => t.user(at, text),
            None => Ok(()),
        }
    }

    /// `value` with every string run through the redactor, as the messages and transcripts are.
    /// The graph holds what the user typed (a run's prompt, a workflow's input), so the snapshot
    /// and the commit log need it too: a key pasted into a prompt is not a registered credential,
    /// only the patterns catch it.
    fn redacted<T: Serialize + serde::de::DeserializeOwned>(&self, value: &T) -> io::Result<T> {
        let mut json = serde_json::to_value(value).map_err(io::Error::other)?;
        self.redactor.redact_json(&mut json);
        serde_json::from_value(json).map_err(io::Error::other)
    }

    /// Records `msg` in `msgs.jsonl.zst` (redacted) and in its run's transcript.
    pub fn msg(&mut self, msg: &Msg, at: SystemTime) -> io::Result<()> {
        if let Some(mut line) = msg_to_json(msg) {
            self.redactor.redact_json(&mut line);
            self.msgs.append(&line.to_string(), Instant::now())?;
        }
        if let Msg::RunStarted {
            run,
            agent,
            role,
            parent,
            model,
            account,
        } = msg
        {
            let meta = RunMeta {
                id: run.to_string(),
                parent_id: parent.map(|p| p.to_string()),
                agent: agent.to_string(),
                role: role.as_str().into(),
                model: model.0.to_string(),
                account: account.0.to_string(),
                status: "running".into(),
                started_at: super::time::rfc3339(at),
                ..RunMeta::default()
            };
            let t = Transcript::start(
                &self.dir.join("runs"),
                meta,
                self.redactor.clone(),
                self.store_thinking,
                self.result_max,
            )?;
            self.runs.insert(*run, t);
        }
        match run_of(msg).and_then(|r| self.runs.get_mut(&r)) {
            Some(t) => t.record(msg, at),
            None => Ok(()),
        }
    }

    /// Call after each `App::step`: writes the commits it made to the log and, when
    /// `model:persist:due` says so, a snapshot (then tells the graph with `Msg::Persisted`).
    pub fn after_step<G: Graph>(&mut self, app: &mut App<G>) -> Result<(), AibusError> {
        let now = Instant::now();
        for rec in app.take_journal() {
            self.commits.append(&self.redacted(&rec)?, now)?;
        }
        self.commits.flush_if_stale(now)?;
        self.msgs.flush_if_stale(now)?;
        if app.graph.read_key(PERSIST_DUE) == Some(&Value::Bool(true)) {
            self.snapshot(app)?;
        }
        Ok(())
    }

    /// Writes the snapshot now and records it in the graph.
    fn snapshot<G: Graph>(&mut self, app: &mut App<G>) -> Result<(), AibusError> {
        let now = Instant::now();
        write_snapshot(&self.dir, &self.redacted(&app.graph.snapshot())?)?;
        app.step(Msg::Persisted)?;
        for rec in app.take_journal() {
            self.commits.append(&self.redacted(&rec)?, now)?;
        }
        self.commits.flush(now)?;
        Ok(())
    }

    /// Writes out everything still buffered.
    pub fn flush(&mut self) -> io::Result<()> {
        let now = Instant::now();
        self.commits.flush(now)?;
        self.msgs.flush(now)
    }

    /// Writes `memory.json`, the live numbers `aibus doctor --session` prints (08 section 5).
    pub fn memory<G: Graph>(&self, app: &App<G>) -> io::Result<()> {
        write_atomic(&self.dir.join(MEMORY), crate::mem::numbers(app).to_string())
    }

    /// Shutdown: a last snapshot, every buffer flushed, the last memory numbers.
    pub fn finish<G: Graph>(&mut self, app: &mut App<G>) -> Result<(), AibusError> {
        self.snapshot(app)?;
        self.memory(app)?;
        Ok(self.flush()?)
    }
}
