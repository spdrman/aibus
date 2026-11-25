//! The causal inspector view model and the reproducibility report (#294, spec 8.1 "Inspector",
//! 7.3). The inspector is one selected node as typed fields: its prompt and output when the trace
//! kept them, its native ids, the source events behind it, its gaps, elapsed time, reported usage
//! and linked artifacts. Whatever is not in the trace is a `Known::Gap` with the reason. The
//! reproducibility report says what a recorded replay needs (the trace, offline) apart from what
//! a fresh re-execution would need, and which of those inputs the trace does not carry.

use serde::Serialize;
use uuid::Uuid;

use crate::compare::{ArtifactRef, Known};
use crate::view::Recorded;
use crate::{EventKind, Metric, Origin, Visibility};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Agent,
    Tool,
    Attempt,
}

/// Text the run kept (under payload retention), and which event it came from.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Retained {
    pub from_event: u64,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EvidenceRow {
    pub position: u64,
    pub kind: EventKind,
    pub visibility: Visibility,
    pub origin: Origin,
    pub native_type: Option<String>,
    pub parser: String,
    pub raw_sha256: Option<String>,
    pub payload_ref: Option<String>,
    /// The position of the event this one confirms.
    pub confirms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NativeId {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TraceGap {
    pub position: u64,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InspectorView {
    pub node: String,
    pub kind: NodeKind,
    pub attempt_id: Uuid,
    pub prompt: Known<Retained>,
    pub output: Known<Retained>,
    pub native_ids: Vec<NativeId>,
    /// How the node is linked to its parent, with the evidence level.
    pub link: Known<String>,
    pub source_evidence: Vec<EvidenceRow>,
    pub gaps: Vec<TraceGap>,
    pub elapsed: Metric,
    pub usage: Known<Vec<Metric>>,
    pub artifacts: Known<Vec<ArtifactRef>>,
    pub worktree_diff: Known<String>,
}

/// The inspector of one agent id, tool use id or attempt id; `None` when nothing has that id.
pub fn inspector(_rec: &Recorded, _node: &str) -> Option<InspectorView> {
    unimplemented!("#294")
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Needed {
    pub name: String,
    pub value: Known<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Replay {
    /// Replay reads the recorded events and calls nothing: it is always offline.
    pub offline: bool,
    pub available: bool,
    pub events: usize,
    /// What makes the replay incomplete: skipped lines, a torn tail, payloads not kept.
    pub caveats: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Rerun {
    /// Always `rerun`: a new model execution is never called a replay.
    pub label: String,
    pub available: bool,
    pub blocked_by: Option<String>,
    pub outcome_may_differ: bool,
    pub needs: Vec<Needed>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Reproducibility {
    pub run: String,
    pub replay: Replay,
    pub rerun: Rerun,
}

pub fn reproducibility(_id: &str, _rec: &Recorded) -> Reproducibility {
    unimplemented!("#294")
}

pub fn render_reproducibility(_r: &Reproducibility) -> String {
    unimplemented!("#294")
}
