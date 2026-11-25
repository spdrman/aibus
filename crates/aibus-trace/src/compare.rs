//! Comparing two recorded runs (#294, spec 8.1 "Comparison"): quality, latency, tokens and cost,
//! tool counts, routing, outputs, git state and environment mismatches, side by side. Everything
//! comes from the two traces (and an evaluation result when one is handed in). Whatever the trace
//! does not carry is a `Known::Gap` with the reason, never a default, a zero or a guess, and two
//! runs are only called comparable when every field of their environment is known and equal
//! (invariant 9, spec 2.2). Nothing here reads anything but the two run directories.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::eval::EvaluationResult;
use crate::view::{Recorded, RunState};
use crate::{DataQuality, Metric, Origin, Visibility};

/// A value the trace carries, or the reason it does not.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Known<T> {
    Value(T),
    Gap(String),
}

/// One run and the evaluation result that goes with it, when there is one.
pub struct RunInput<'a> {
    pub id: &'a str,
    pub rec: &'a Recorded,
    pub result: Option<&'a EvaluationResult>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolCounts {
    pub total: usize,
    pub completed: usize,
    pub failed: usize,
    /// Started and never reported back.
    pub unknown: usize,
    pub by_name: BTreeMap<String, usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RouteRow {
    pub position: u64,
    pub visibility: Visibility,
    pub origin: Origin,
    /// The text and number fields of the event's metadata, as recorded.
    pub fields: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ArtifactRef {
    pub position: u64,
    pub name: Option<String>,
    pub payload_ref: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Outputs {
    pub message_chunks: usize,
    pub message_bytes: Known<u64>,
    /// Events that point at a payload the run kept, of the events that could.
    pub payloads_retained: usize,
    pub artifacts: Vec<ArtifactRef>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Quality {
    pub suite: String,
    pub case_id: String,
    pub variant: String,
    pub verdict: String,
    pub checks_passed: usize,
    pub checks_failed: usize,
    /// Checks nobody reported an outcome for. They never count as passes.
    pub checks_not_run: usize,
}

/// One environment field of one run.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EnvEntry {
    pub name: String,
    pub value: Known<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RunSide {
    pub id: String,
    pub state: RunState,
    pub provider: Option<String>,
    pub events: usize,
    pub attempts: usize,
    pub agents: usize,
    /// Delegation edges by level: observed, correlated, inferred, unavailable.
    pub edges: [usize; 4],
    pub gaps: usize,
    pub tools: ToolCounts,
    /// First to last event on aibus's clock, derived; `unknown` when the trace has no times.
    pub latency: Metric,
    /// Attempt-wide usage with each number's quality; always has a `cost_usd` row.
    pub usage: Vec<Metric>,
    pub routing: Known<Vec<RouteRow>>,
    pub outputs: Outputs,
    pub quality: Known<Quality>,
    pub git_revision: Known<String>,
    pub worktree_diff: Known<String>,
    pub environment: Vec<EnvEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Delta {
    pub metric: String,
    pub a: Metric,
    pub b: Metric,
    /// `b - a`, only when both are known and in the same unit.
    pub change: Known<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvVerdict {
    Same,
    Differs,
    /// One side or both do not know.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EnvRow {
    pub name: String,
    pub a: Known<String>,
    pub b: Known<String>,
    pub verdict: EnvVerdict,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Comparable {
    /// Every environment field is known and equal.
    Yes,
    /// At least one field differs.
    No(Vec<String>),
    /// Nothing differs that is known, but these fields are unknown on a side.
    Unknown(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Comparison {
    pub a: RunSide,
    pub b: RunSide,
    pub deltas: Vec<Delta>,
    pub environment: Vec<EnvRow>,
    pub comparable: Comparable,
}

/// One run read as a side of a comparison.
pub fn side(_run: &RunInput) -> RunSide {
    unimplemented!("#294")
}

/// Two runs side by side.
pub fn compare_runs(_a: &RunInput, _b: &RunInput) -> Comparison {
    unimplemented!("#294")
}

/// The comparison as text: the five reviewer questions, one section each.
pub fn render(_c: &Comparison) -> String {
    unimplemented!("#294")
}

#[allow(dead_code)]
fn _unused(_: DataQuality) {}
