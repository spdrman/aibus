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

pub mod eval;
pub mod experiment;
pub mod otel;
mod project;
mod store;
pub mod suggest;
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

/// The envelope version written by this build. A reader refuses a line with any other. Version 2
/// dropped the integer `sequence` of version 1: events order by their uuid v7 `event_id` (#319).
pub const SCHEMA_VERSION: u32 = 2;
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

/// The envelope (spec 7.1), schema version 2. `event_id` is a uuid v7 and is the ordering key: a
/// trace is its events sorted by it, and replay, torn-tail repair and idempotent append all key on
/// it. `observed_at` is aibus's own clock (Claude Code puts no timestamp on most events); `occurred_at` is the
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
    /// The 1-based rank of this event in the run's `event_id` order, set when a trace is read, for
    /// display ("event 12 of 400"). Never written and never used to order anything.
    #[serde(skip)]
    pub position: u64,
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
    /// An event of `attempt` with no agent, native ids or data yet. `event_id` is the caller's, a uuid v7
    /// from an `EventIds` (the store refuses any other).
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
            position: 0,
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

/// Makes uuid v7 event ids that never go backwards: the same millisecond (or a clock that steps
/// back) bumps a 12-bit counter in `rand_a`, so two events made in one millisecond still sort in
/// the order they were made, and a full counter moves on to the next millisecond. The rest of the
/// id is a hash of `parts`. A fresh generator given the same `parts` and the same time makes the
/// same id, so normalising the same lines again from the start gives the same ids and the store
/// keeps one of each (idempotent append is keyed on the id). That does not hold for one generator
/// fed the same line twice, nor for one seeded from a store (`TraceStore::ids`): each moves on and
/// makes a new id. The counter only breaks ties inside one writer; the order of a trace is the id.
/// The time saturates at 2^48 - 1 (a planted maximum id) with a full counter, it never wraps to 0;
/// the store then refuses an id that does not sort after the last one (`StoreError::OutOfOrder`).
#[derive(Clone, Copy, Debug, Default)]
pub struct EventIds {
    ms: u64,
    counter: u16,
}

impl EventIds {
    /// A generator whose next id sorts after `last`.
    pub fn after(last: Uuid) -> Self {
        let mut g = Self::default();
        g.observe(last);
        g
    }

    /// Makes the next id sort after `id` when it would not already.
    pub fn observe(&mut self, id: Uuid) {
        let b = id.as_bytes();
        let ms = b[..6].iter().fold(0u64, |a, x| a << 8 | u64::from(*x));
        let counter = u16::from(b[6] & 0x0f) << 8 | u16::from(b[7]);
        if (ms, counter) > (self.ms, self.counter) {
            (self.ms, self.counter) = (ms, counter);
        }
    }

    /// The next id, stamped with `at` unless an earlier id already sits at or past it.
    pub fn next(&mut self, at: SystemTime, parts: &[&[u8]]) -> Uuid {
        let at_ms = at
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64)
            & 0xffff_ffff_ffff;
        if at_ms > self.ms {
            (self.ms, self.counter) = (at_ms, 0);
        } else if self.counter == 0x0fff {
            // At the very top of the 48-bit range there is no next millisecond: stay put (the ids
            // then tie on time and counter and order by their hash) rather than wrap to time 0.
            if self.ms < 0xffff_ffff_ffff {
                (self.ms, self.counter) = (self.ms + 1, 0);
            }
        } else {
            self.counter += 1;
        }
        let mut h = Sha256::new();
        for p in parts {
            h.update((p.len() as u64).to_le_bytes());
            h.update(p);
        }
        let tail = h.finalize();
        let mut b = [0u8; 16];
        b[..6].copy_from_slice(&self.ms.to_be_bytes()[2..]);
        b[6] = 0x70 | (self.counter >> 8) as u8;
        b[7] = self.counter as u8;
        b[8] = 0x80 | (tail[0] & 0x3f);
        b[9..].copy_from_slice(&tail[1..8]);
        Uuid::from_bytes(b)
    }
}
