//! What a person reads of a recorded run (#284): the fleet, a run's planned and execution graph,
//! its timeline, the inspector and the offline replay. The interface only; the red tests of #284
//! run against it before it does anything.
#![allow(unused_variables)]

use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::{EventKind, Metric, ReadReport, TraceEvent, Visibility};

pub const MAX_TRACE_BYTES: u64 = 512 << 20;

pub struct Recorded {
    pub dir: PathBuf,
    pub events: Vec<TraceEvent>,
    pub raw: Vec<Vec<u8>>,
    pub report: ReadReport,
    pub plan: Option<Value>,
    pub plan_error: Option<String>,
}

impl Recorded {
    pub fn load(dir: &Path) -> io::Result<Self> {
        todo!("#284")
    }

    pub fn from_bytes(dir: &Path, bytes: &[u8]) -> Self {
        todo!("#284")
    }
}

pub fn clean(s: &str, max: usize) -> String {
    todo!("#284")
}

pub fn terminal_safe_line(raw: &[u8]) -> Vec<u8> {
    todo!("#284")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    Blocked,
    Open,
    Empty,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RunRow {
    pub id: String,
    pub workflow_run_id: Option<Uuid>,
    pub provider: Option<String>,
    pub state: RunState,
    pub requires_reconciliation: bool,
    pub first_seen: Option<String>,
    pub last_seen: Option<String>,
    pub events: usize,
    pub attempts: usize,
    pub agents: usize,
    pub tools: usize,
    pub edges: [usize; 4],
    pub gaps: usize,
    pub cost: Metric,
    pub corrupt_lines: u64,
    pub torn_bytes: u64,
    pub unsupported_schema: u64,
    pub error: Option<String>,
}

impl RunRow {
    pub fn of(id: &str, rec: &Recorded) -> Self {
        todo!("#284")
    }
}

pub fn resolve(root: &Path, id: &str) -> Result<PathBuf, String> {
    todo!("#284")
}

pub fn fleet(root: &Path) -> Vec<RunRow> {
    todo!("#284")
}

pub fn render_fleet(rows: &[RunRow]) -> String {
    todo!("#284")
}

pub fn graph(rec: &Recorded, name: &str) -> String {
    todo!("#284")
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    pub agent: Option<String>,
    pub tool: Option<String>,
    pub kind: Option<EventKind>,
    pub level: Option<Visibility>,
}

pub fn timeline(rec: &Recorded, filter: &Filter) -> String {
    todo!("#284")
}

pub fn inspect(rec: &Recorded, node: &str) -> Option<String> {
    todo!("#284")
}

pub fn event(rec: &Recorded, sequence: u64) -> Option<String> {
    todo!("#284")
}

pub fn replay(rec: &Recorded, name: &str) -> String {
    todo!("#284")
}
