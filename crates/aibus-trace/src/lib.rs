//! The normalized evidence trace (#282, spec section 7.1).
//!
//! One `TraceEvent` per fact aibus can source: an agent started, a tool finished, a child was
//! delegated, a gap in what the vendor shows. Every event says where it came from (`origin`), how
//! sure aibus is of it (`visibility`: observed, correlated, inferred or unavailable) and which raw
//! lines back it (`evidence_refs`, `source.raw_sha256`). A second channel reporting a fact aibus
//! already holds is kept as its own event that `corroborates` the first, so the evidence is never
//! thrown away and a projection never counts the fact twice.
//!
//! `TraceStore` is the append-only local file the events live in, `project` turns a trace back into
//! the execution graph and timeline without any provider, and `view` (#284) renders a recorded run
//! for a person: the fleet, the planned and execution graph, the timeline, the inspector and the
//! offline replay. Nothing here launches a process.
#![forbid(unsafe_code)]

mod project;
mod store;
pub mod view;

pub use project::{
    project, AgentNode, AttemptView, Edge, Evidence, GapRow, Projection, TimelineRow, ToolNode,
    ToolOutcome,
};
pub use store::{Batch, PayloadPolicy, ReadReport, Recovery, StoreError, StoreLimits, TraceStore};

use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// The envelope version written by this build. A reader refuses a line with any other.
pub const SCHEMA_VERSION: u32 = 1;
/// Bumped whenever what reaches the store before redaction changes (fields dropped, rules added),
/// so an old trace says which rules it was written under.
pub const REDACTION_VERSION: u32 = 1;

/// Spec 7.1, exactly. A kind outside this list does not exist; a vendor event aibus cannot map is a
/// `TraceWarning` that names its native type.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    WorkflowStarted,
    StepScheduled,
    RouteDecided,
    ApprovalRequested,
    ApprovalResolved,
    AttemptStarted,
    AttemptCompleted,
    AttemptFailed,
    AgentStarted,
    AgentStopped,
    TaskDelegated,
    MessageChunk,
    ToolStarted,
    ToolCompleted,
    ToolFailed,
    CheckpointWritten,
    ArtifactCreated,
    UsageReported,
    AuthRequired,
    GapDetected,
    TraceWarning,
}

/// Where an event came from (spec 7.1).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    AibusScheduler,
    Jev,
    CliStream,
    Hook,
    VendorTranscript,
    OpenrouterAdapter,
    Derived,
}

/// The evidence level (spec 7.1). The order is strength: `Observed` is a native explicit event,
/// `Correlated` links independent ids or channels, `Inferred` is a labelled heuristic and
/// `Unavailable` is a gap.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Observed,
    Correlated,
    Inferred,
    Unavailable,
}

/// Invariant 9: every number says how good it is.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum DataQuality {
    /// The vendor said so.
    Reported,
    /// aibus measured or computed it from things it observed (its own clock, a count).
    Derived,
    /// A rate-card or client-side estimate, not a bill.
    Estimated,
    /// Nobody knows; `value` is `None`.
    Unknown,
}

/// A number with its unit, quality and source, never a bare float.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Metric {
    pub name: String,
    pub value: Option<f64>,
    pub unit: String,
    pub quality: DataQuality,
    pub source: String,
}

/// How an attempt ended (spec 5.3's terminal states). `Interrupted` is a process that ended
/// without finishing (killed, a limit, a signal aibus did not send); `Cancelled` is the user's stop.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AttemptStatus {
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

/// The vendor's own identifiers on an event, kept as opaque strings.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Native {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// `type` or `type/subtype` (stream) or `hook_event_name` (hooks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_type: Option<String>,
}

/// What produced the event: the parser and its version, the redaction rules it was written under
/// and the hash of the raw line it came from.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub parser: String,
    pub redaction_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_sha256: Option<String>,
}

/// The ids every event of one attempt carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttemptRef {
    pub workflow_run_id: Uuid,
    pub attempt_id: Uuid,
    pub parent_attempt_id: Option<Uuid>,
    pub provider_extension: String,
}

/// The envelope (spec 7.1), schema version 1. `sequence` is assigned by the store; `observed_at`
/// is aibus's own clock (Claude Code puts no timestamp on most events); `occurred_at` is the
/// vendor's timestamp when it gave one, and `None` when it did not, never a copy of `observed_at`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TraceEvent {
    pub schema_version: u32,
    pub event_id: Uuid,
    pub workflow_run_id: Uuid,
    pub attempt_id: Uuid,
    pub agent_id: Option<String>,
    pub parent_agent_id: Option<String>,
    pub parent_attempt_id: Option<Uuid>,
    pub parent_tool_use_id: Option<String>,
    pub origin: Origin,
    pub visibility: Visibility,
    pub kind: EventKind,
    pub occurred_at: Option<String>,
    pub observed_at: String,
    pub sequence: u64,
    pub provider_extension: String,
    pub payload_ref: Option<String>,
    pub evidence_refs: Vec<String>,
    /// The event this one confirms from another line or channel. Projections count only events
    /// where this is `None`.
    #[serde(default)]
    pub corroborates: Option<Uuid>,
    #[serde(default)]
    pub native: Native,
    pub source: Source,
    /// Small, per-kind metadata (names, statuses, sizes, `Metric`s). Never message text, tool
    /// input or output: those are payloads and only reach the store under `PayloadPolicy::Retain`.
    #[serde(default)]
    pub data: Value,
}

impl TraceEvent {
    /// An event of `attempt` with no agent, native ids or data yet. `event_id` is the caller's: the
    /// normaliser derives it from the raw line so that feeding the same line twice is idempotent.
    pub fn new(
        attempt: &AttemptRef,
        event_id: Uuid,
        kind: EventKind,
        origin: Origin,
        visibility: Visibility,
        observed_at: SystemTime,
        parser: &str,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            event_id,
            workflow_run_id: attempt.workflow_run_id,
            attempt_id: attempt.attempt_id,
            agent_id: None,
            parent_agent_id: None,
            parent_attempt_id: attempt.parent_attempt_id,
            parent_tool_use_id: None,
            origin,
            visibility,
            kind,
            occurred_at: None,
            observed_at: aibus_core::persist::time::rfc3339(observed_at),
            sequence: 0,
            provider_extension: attempt.provider_extension.clone(),
            payload_ref: None,
            evidence_refs: Vec::new(),
            corroborates: None,
            native: Native::default(),
            source: Source {
                parser: parser.to_owned(),
                redaction_version: REDACTION_VERSION,
                raw_sha256: None,
            },
            data: Value::Null,
        }
    }
}

/// Lowercase hex sha256.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A version 8 UUID that is a pure function of `parts`, so the same raw line in the same attempt
/// always gets the same event id and appending it twice is a no-op.
pub fn derived_id(parts: &[&[u8]]) -> Uuid {
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u64).to_le_bytes());
        h.update(p);
    }
    let digest = h.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    Uuid::new_v8(bytes)
}
