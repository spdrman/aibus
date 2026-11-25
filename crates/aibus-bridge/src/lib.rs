//! The typed bridge between the Rust core and the React UI (B1.3, #306).
//!
//! The UI never reads the event store and never runs anything: it sends a `Request` (one of a closed
//! set of read-only `Command`s) and gets back a `Reply` built from the `aibus-trace` projections, the
//! same ones the TUI renders (#284), not a second set. Events carry the trace's own `Visibility`
//! (observed, correlated, inferred, unavailable), so an inferred edge is never shown as a fact.
//!
//! Order is the uuid v7 event id (#319). A subscriber holds a cursor (the last event id it applied);
//! every batch says which cursor it continues from, so a dropped batch shows up as a gap and the
//! subscriber asks again from its cursor and gets exactly the missing events, in order. A cursor the
//! trace no longer has means the subscriber must reload the projection. `position` is display only.
//!
//! The wire types derive `ts_rs::TS`; the generated TypeScript is checked in under `bindings/` and
//! `tests/contract.rs` fails when it drifts from the Rust.
#![forbid(unsafe_code)]

use std::fmt;
use std::path::Path;
use std::sync::mpsc::{SyncSender, TrySendError};

use aibus_trace::view::{self, Recorded};
use aibus_trace::{project, Evidence, TraceEvent, SCHEMA_VERSION as TRACE_SCHEMA};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

/// The version of the DTOs in this crate. A request or batch with another version is refused.
pub const BRIDGE_SCHEMA: u32 = 1;
/// The most events in one batch, whatever the caller asks for.
pub const MAX_BATCH: u32 = 500;
const DEFAULT_BATCH: u32 = 200;

/// The wire name of an enum value (`tool_started`, `cli_stream`, ...).
fn wire(v: impl Serialize) -> String {
    match serde_json::to_value(v) {
        Ok(Value::String(s)) => s,
        _ => "?".into(),
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Observed,
    Correlated,
    Inferred,
    Unavailable,
}

impl From<aibus_trace::Visibility> for Visibility {
    fn from(v: aibus_trace::Visibility) -> Self {
        match v {
            aibus_trace::Visibility::Observed => Self::Observed,
            aibus_trace::Visibility::Correlated => Self::Correlated,
            aibus_trace::Visibility::Inferred => Self::Inferred,
            aibus_trace::Visibility::Unavailable => Self::Unavailable,
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcome {
    Completed,
    Failed,
    /// Started and never finished in the trace.
    Unknown,
}

impl From<aibus_trace::ToolOutcome> for ToolOutcome {
    fn from(v: aibus_trace::ToolOutcome) -> Self {
        match v {
            aibus_trace::ToolOutcome::Completed => Self::Completed,
            aibus_trace::ToolOutcome::Failed => Self::Failed,
            aibus_trace::ToolOutcome::Unknown => Self::Unknown,
        }
    }
}

/// One source event behind a node or an edge.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
pub struct EvidenceDto {
    pub event_id: String,
    pub visibility: Visibility,
    pub origin: String,
}

impl From<&Evidence> for EvidenceDto {
    fn from(e: &Evidence) -> Self {
        Self {
            event_id: e.event_id.to_string(),
            visibility: e.visibility.into(),
            origin: wire(e.origin),
        }
    }
}

/// One event of a run. Metadata only: payloads and tool input never cross the bridge.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
pub struct EventDto {
    /// A uuid v7 and the ordering key.
    pub event_id: String,
    /// 1-based rank in the run, for display ("event 12 of 400"). Never an ordering key.
    #[ts(type = "number")]
    pub position: u64,
    pub observed_at: String,
    pub occurred_at: Option<String>,
    /// The trace's `EventKind` wire name, for example `tool_started`.
    pub kind: String,
    /// The trace's `Origin` wire name, for example `cli_stream`.
    pub origin: String,
    pub visibility: Visibility,
    pub attempt_id: String,
    pub agent_id: Option<String>,
    pub parent_agent_id: Option<String>,
    pub parent_tool_use_id: Option<String>,
    pub tool_use_id: Option<String>,
    /// The event this one confirms, when it is a second channel reporting the same fact.
    pub corroborates: Option<String>,
}

impl From<&TraceEvent> for EventDto {
    fn from(e: &TraceEvent) -> Self {
        Self {
            event_id: e.event_id.to_string(),
            position: e.position,
            observed_at: e.observed_at.clone(),
            occurred_at: e.occurred_at.clone(),
            kind: wire(e.kind),
            origin: wire(e.origin),
            visibility: e.visibility.into(),
            attempt_id: e.attempt_id.to_string(),
            agent_id: e.agent_id.clone(),
            parent_agent_id: e.parent_agent_id.clone(),
            parent_tool_use_id: e.parent_tool_use_id.clone(),
            tool_use_id: e.native.tool_use_id.clone(),
            corroborates: e.corroborates.map(|c| c.to_string()),
        }
    }
}

/// One row of the run list.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
pub struct RunDto {
    pub id: String,
    /// The fleet's `RunState` wire name: `completed`, `failed`, `open`, ...
    pub state: String,
    pub provider: Option<String>,
    pub last_seen: Option<String>,
    pub events: u32,
    pub attempts: u32,
    pub agents: u32,
    pub tools: u32,
    /// Delegation edges by level, in the order observed, correlated, inferred, unavailable.
    pub edges: [u32; 4],
    pub gaps: u32,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
pub struct AgentDto {
    pub attempt_id: String,
    pub agent_id: String,
    pub role: Option<String>,
    pub status: Option<String>,
    pub evidence: Vec<EvidenceDto>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
pub struct ToolCallDto {
    pub attempt_id: String,
    pub tool_use_id: String,
    pub agent_id: Option<String>,
    pub name: Option<String>,
    pub outcome: ToolOutcome,
    pub evidence: Vec<EvidenceDto>,
}

/// A delegation edge. `level` is the strongest evidence behind it; an `inferred` edge is a labelled
/// heuristic and the UI must say so.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
pub struct EdgeDto {
    pub attempt_id: String,
    pub parent: Option<String>,
    pub child: String,
    pub via_tool_use_id: Option<String>,
    pub level: Visibility,
    pub cross_checked: bool,
    pub sources: Vec<EvidenceDto>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
pub struct GapDto {
    #[ts(type = "number")]
    pub position: u64,
    pub attempt_id: String,
    pub reason: String,
}

/// The materialized read model of one run: runs, agents, tool calls and their relationships. `head`
/// is the cursor to subscribe from after loading it.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
pub struct ProjectionDto {
    pub schema_version: u32,
    pub run: String,
    pub head: Option<String>,
    pub agents: Vec<AgentDto>,
    pub tools: Vec<ToolCallDto>,
    pub edges: Vec<EdgeDto>,
    pub gaps: Vec<GapDto>,
}

/// A bounded slice of a run's events. `after` is the cursor it continues from (`None`: the start of
/// the run) and `next` the cursor after its last event, so a subscriber can tell a gap from a quiet
/// run without counting.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
pub struct EventBatch {
    pub schema_version: u32,
    pub run: String,
    pub after: Option<String>,
    pub events: Vec<EventDto>,
    pub next: Option<String>,
    /// More events were already in the run when this batch was cut.
    pub more: bool,
}

/// The closed set of things the UI can ask for. Every one is a read. There is no command that runs
/// a process, writes the store or takes a path, and an unknown command does not deserialize.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    ListRuns,
    Projection {
        run: String,
    },
    Events {
        run: String,
        after: Option<String>,
        limit: Option<u32>,
    },
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
pub struct Request {
    pub schema_version: u32,
    pub command: Command,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reply {
    Runs { runs: Vec<RunDto> },
    Projection { projection: ProjectionDto },
    Events { batch: EventBatch },
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The UI and aibus (or the trace on disk) speak different schema versions.
    SchemaMismatch,
    BadRequest,
    UnknownRun,
    /// The cursor is not a uuid.
    BadCursor,
    /// The cursor is not in the run any more: reload the projection.
    CursorUnknown,
    /// A batch did not continue from the subscriber's cursor: resubscribe from the cursor.
    Gap,
    Io,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
pub struct BridgeError {
    pub code: ErrorCode,
    /// Says what happened and what to do about it.
    pub message: String,
}

impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", wire(self.code), self.message)
    }
}

impl std::error::Error for BridgeError {}

fn err(code: ErrorCode, message: impl Into<String>) -> BridgeError {
    BridgeError {
        code,
        message: message.into(),
    }
}

/// What `handle_json` returns, so the UI gets one shape for success and failure.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Ok { reply: Reply },
    Error { error: BridgeError },
}

/// Answers one request from the runs under `root`.
pub fn handle(root: &Path, req: &Request) -> Result<Reply, BridgeError> {
    if req.schema_version != BRIDGE_SCHEMA {
        let fix = if req.schema_version < BRIDGE_SCHEMA {
            "update the desktop app to match this aibus"
        } else {
            "update aibus to match the desktop app"
        };
        return Err(err(
            ErrorCode::SchemaMismatch,
            format!(
                "the UI speaks bridge schema {} and aibus speaks {}: {fix}",
                req.schema_version, BRIDGE_SCHEMA
            ),
        ));
    }
    match &req.command {
        Command::ListRuns => Ok(Reply::Runs {
            runs: view::fleet(root)
                .iter()
                .map(|r| RunDto {
                    id: r.id.clone(),
                    state: wire(r.state),
                    provider: r.provider.clone(),
                    last_seen: r.last_seen.clone(),
                    events: r.events as u32,
                    attempts: r.attempts as u32,
                    agents: r.agents as u32,
                    tools: r.tools as u32,
                    edges: r.edges.map(|n| n as u32),
                    gaps: r.gaps as u32,
                    error: r.error.clone(),
                })
                .collect(),
        }),
        Command::Projection { run } => {
            let rec = load(root, run)?;
            let p = project(&rec.events);
            Ok(Reply::Projection {
                projection: ProjectionDto {
                    schema_version: BRIDGE_SCHEMA,
                    run: run.clone(),
                    head: rec.events.last().map(|e| e.event_id.to_string()),
                    agents: p
                        .agents
                        .iter()
                        .map(|a| AgentDto {
                            attempt_id: a.attempt_id.to_string(),
                            agent_id: a.agent_id.clone(),
                            role: a.role.clone(),
                            status: a.status.clone(),
                            evidence: a.evidence.iter().map(Into::into).collect(),
                        })
                        .collect(),
                    tools: p
                        .tools
                        .iter()
                        .map(|t| ToolCallDto {
                            attempt_id: t.attempt_id.to_string(),
                            tool_use_id: t.tool_use_id.clone(),
                            agent_id: t.agent_id.clone(),
                            name: t.name.clone(),
                            outcome: t.outcome.into(),
                            evidence: t.evidence.iter().map(Into::into).collect(),
                        })
                        .collect(),
                    edges: p
                        .edges
                        .iter()
                        .map(|e| EdgeDto {
                            attempt_id: e.attempt_id.to_string(),
                            parent: e.parent.clone(),
                            child: e.child.clone(),
                            via_tool_use_id: e.via_tool_use_id.clone(),
                            level: e.level.into(),
                            cross_checked: e.cross_checked,
                            sources: e.sources.iter().map(Into::into).collect(),
                        })
                        .collect(),
                    gaps: p
                        .gaps
                        .iter()
                        .map(|g| GapDto {
                            position: g.position,
                            attempt_id: g.attempt_id.to_string(),
                            reason: g.reason.clone(),
                        })
                        .collect(),
                },
            })
        }
        Command::Events { run, after, limit } => {
            let rec = load(root, run)?;
            Ok(Reply::Events {
                batch: events_after(run, &rec.events, after.as_deref(), *limit)?,
            })
        }
    }
}

/// `handle` for the wire: parses a request, answers it, and turns any failure (including a request
/// that does not parse, such as an unknown command) into a `Response::Error`.
pub fn handle_json(root: &Path, request: &str) -> String {
    let res = serde_json::from_str::<Request>(request)
        .map_err(|e| err(ErrorCode::BadRequest, format!("not a bridge request: {e}")))
        .and_then(|r| handle(root, &r));
    let out = match res {
        Ok(reply) => Response::Ok { reply },
        Err(error) => Response::Error { error },
    };
    serde_json::to_string(&out).unwrap_or_default()
}

fn load(root: &Path, run: &str) -> Result<Recorded, BridgeError> {
    let dir = view::resolve(root, run).map_err(|m| err(ErrorCode::UnknownRun, m))?;
    let rec = Recorded::load(&dir).map_err(|e| err(ErrorCode::Io, format!("{run}: {e}")))?;
    if rec.events.is_empty() && rec.report.unsupported_schema > 0 {
        return Err(err(
            ErrorCode::SchemaMismatch,
            format!(
                "{run} was written under another trace schema ({} lines skipped) and this build reads \
                 schema {TRACE_SCHEMA}: open it with the aibus that recorded it, or update aibus",
                rec.report.unsupported_schema
            ),
        ));
    }
    Ok(rec)
}

/// The events of a run after `after`, at most `limit` (default 200, never over `MAX_BATCH`). `events`
/// is in event id order, as `Recorded` keeps it.
fn events_after(
    run: &str,
    events: &[TraceEvent],
    after: Option<&str>,
    limit: Option<u32>,
) -> Result<EventBatch, BridgeError> {
    let start = match after {
        None => 0,
        Some(c) => {
            let id = c.parse::<uuid::Uuid>().map_err(|_| {
                err(
                    ErrorCode::BadCursor,
                    format!("{c:?} is not an event id: resubscribe from the start of the run"),
                )
            })?;
            match events.binary_search_by_key(&id, |e| e.event_id) {
                Ok(i) => i + 1,
                Err(_) => {
                    return Err(err(
                        ErrorCode::CursorUnknown,
                        format!("{run} has no event {id}: reload the projection and subscribe from its head"),
                    ))
                }
            }
        }
    };
    let n = limit.unwrap_or(DEFAULT_BATCH).clamp(1, MAX_BATCH) as usize;
    let end = (start + n).min(events.len());
    let slice = &events[start..end];
    Ok(EventBatch {
        schema_version: BRIDGE_SCHEMA,
        run: run.to_owned(),
        after: after.map(str::to_owned),
        next: slice
            .last()
            .map(|e| e.event_id.to_string())
            .or_else(|| after.map(str::to_owned)),
        events: slice.iter().map(Into::into).collect(),
        more: end < events.len(),
    })
}

/// Pushes the batches after `after` into a bounded channel until the run is caught up or the channel
/// is full, and returns how many it sent. A full channel is back-pressure, not an error: nothing is
/// dropped from the run, and the consumer's cursor says where to continue. A closed channel (the
/// UI went away) is an `Io` error.
pub fn stream(
    root: &Path,
    run: &str,
    after: Option<&str>,
    limit: Option<u32>,
    tx: &SyncSender<EventBatch>,
) -> Result<u32, BridgeError> {
    let rec = load(root, run)?;
    let mut cursor = after.map(str::to_owned);
    let mut sent = 0;
    loop {
        let batch = events_after(run, &rec.events, cursor.as_deref(), limit)?;
        if batch.events.is_empty() {
            return Ok(sent);
        }
        let (next, more) = (batch.next.clone(), batch.more);
        match tx.try_send(batch) {
            Ok(()) => sent += 1,
            Err(TrySendError::Full(_)) => return Ok(sent),
            Err(TrySendError::Disconnected(_)) => {
                return Err(err(ErrorCode::Io, "the subscriber went away"))
            }
        }
        cursor = next;
        if !more {
            return Ok(sent);
        }
    }
}

/// The UI side of a subscription: the run, the cursor (the last event id applied) and the checks
/// that make a missed or reordered batch a `Gap` instead of a silent hole.
#[derive(Clone, Debug, PartialEq)]
pub struct Subscriber {
    pub run: String,
    pub cursor: Option<String>,
}

impl Subscriber {
    /// The request that asks for the events after the cursor. After a gap, an error or a dropped
    /// connection this is the resubscribe.
    pub fn request(&self, limit: Option<u32>) -> Request {
        Request {
            schema_version: BRIDGE_SCHEMA,
            command: Command::Events {
                run: self.run.clone(),
                after: self.cursor.clone(),
                limit,
            },
        }
    }

    /// Applies `batch` and returns how many events it held, or a `Gap` (or `SchemaMismatch`) error
    /// that leaves the cursor where it was. A batch applies only when it continues from the cursor,
    /// its events ascend strictly and `next` is the last of them.
    pub fn apply(&mut self, batch: &EventBatch) -> Result<usize, BridgeError> {
        if batch.schema_version != BRIDGE_SCHEMA {
            return Err(err(
                ErrorCode::SchemaMismatch,
                format!(
                    "a batch in bridge schema {} reached a UI that speaks {}: update the desktop app and aibus together",
                    batch.schema_version, BRIDGE_SCHEMA
                ),
            ));
        }
        let ascending = batch
            .events
            .windows(2)
            .all(|w| w[0].event_id < w[1].event_id);
        let continues = batch.run == self.run && batch.after == self.cursor;
        let ends_at_next = match batch.events.last() {
            Some(last) => batch.next.as_deref() == Some(last.event_id.as_str()),
            None => batch.next == batch.after,
        };
        let starts_after = match (batch.events.first(), &self.cursor) {
            (Some(first), Some(c)) => first.event_id > *c,
            _ => true,
        };
        if !(continues && ascending && ends_at_next && starts_after) {
            return Err(err(
                ErrorCode::Gap,
                format!(
                    "the batch continues from {:?} but the cursor is {:?}: resubscribe from the cursor",
                    batch.after, self.cursor
                ),
            ));
        }
        self.cursor = batch.next.clone();
        Ok(batch.events.len())
    }

    /// Takes the head of a freshly loaded projection as the cursor, for after `CursorUnknown`.
    pub fn resync(&mut self, projection: &ProjectionDto) {
        self.cursor = projection.head.clone();
    }
}
