//! Claude Code `stream-json` and hook payloads to trace events.
//!
//! What each native shape becomes (the spike's `evidence.md` is the source for every row):
//!
//! | Native | Event |
//! |---|---|
//! | first `system/init` (or `SessionStart` hook) | `agent_started` for the lead, keyed on `session_id` |
//! | later `system/init` (a background wake-up) | nothing new: counted |
//! | `tool_use` named `Agent` or `Task` (or `PreToolUse` for one) | `task_delegated` |
//! | any other `tool_use` (or `PreToolUse`) | `tool_started` |
//! | `tool_result` (or `PostToolUse`, `PostToolUseFailure`) | `tool_completed` / `tool_failed`; for a delegation, `agent_stopped` of the child |
//! | `system/task_started` (or `SubagentStart`, or a child message seen first) | `agent_started` of the child, keyed on `agent_id` / `task_id` |
//! | `system/task_updated` with a final status, `task_notification`, `SubagentStop` | `agent_stopped` |
//! | `task_notification.usage`, each `result` | `usage_reported` (agent / attempt, vendor-reported; cost `estimated`) |
//! | `system/permission_denied` | `tool_failed` with reason `permission_denied` |
//! | `text` blocks | `message_chunk` (sizes only; the text is a payload) |
//! | `thinking` blocks | `gap_detected`: reasoning is not exposed |
//! | an error `result` or stderr line that asks for a login | `auth_required`, its text never stored |
//! | `hook_started`, `task_progress`, `rate_limit_event`, `stream_event`, ... | nothing: counted |
//! | anything else | `trace_warning` naming the native type |
//!
//! A fact a second line or channel reports again is written as its own event that `corroborates`
//! the first (`correlated` when it came from another channel), so nothing is counted twice and no
//! evidence is lost. `result` never ends the attempt: one process prints several (background
//! children wake the lead up), so the attempt ends in `finish`, when the process has exited.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aibus_trace::{
    sha256_hex, AttemptRef, AttemptStatus, Batch, DataQuality, EventIds, EventKind, Metric, Origin,
    TraceEvent, Visibility,
};
use regex::Regex;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::Ending;

/// The parser id and version stamped on every event this module writes.
pub const PARSER: &str = "claude-code-stream-json/1";

/// The provider extension id on every event. Provisional: Anthropic's legal page says the Claude
/// Code name cannot be part of another product's or feature's name (spike #281, condition 2), so
/// the final id is the owner's to pick in #286. A trace stores it as a plain string.
pub const EXTENSION_ID: &str = "org.devshell.aibus.reference-cli";

/// Names and ids copied into an event are cut to this many bytes.
const MAX_NAME: usize = 256;

/// Where a raw line came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Channel {
    Stdout,
    Stderr,
    Hooks,
    /// aibus's own observations (spawn, exit, limits).
    Host,
}

impl Channel {
    fn name(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::Hooks => "hooks",
            Self::Host => "host",
        }
    }
}

/// What the normaliser has seen, for the attempt's summary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub lines: u64,
    pub duplicates: u64,
    pub malformed: u64,
    pub unknown: u64,
    pub results: u64,
    /// Known native types that carry nothing the trace projects (`hook_started`, `task_progress`,
    /// `rate_limit_event`, ...), by type.
    pub ignored: BTreeMap<String, u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Fact {
    Agent,
    AgentStop,
    AgentUsage,
    Delegation,
    ToolStart,
    ToolEnd,
}

/// The raw line an event comes from: its channel, number, hash and arrival time, and a counter
/// for the events it has produced so far (part of each event id).
struct Line {
    channel: Channel,
    no: u64,
    sha: String,
    at: SystemTime,
    index: u32,
    native_type: Option<String>,
    session_id: Option<String>,
}

/// One attempt's normaliser. Feed it lines in arrival order from any channel; it keeps the ids it
/// needs to link children to parents and to recognise a fact a second time, all bounded.
pub struct Normalizer {
    attempt: AttemptRef,
    lead: Option<String>,
    facts: HashMap<(Fact, String), (Uuid, Channel)>,
    /// tool_use_id -> the agent that made the call.
    owner: HashMap<String, String>,
    /// Delegation tool_use_id -> the child, once known.
    delegations: HashMap<String, Option<String>>,
    open_tools: BTreeSet<String>,
    open_agents: BTreeSet<String>,
    recent: HashMap<Channel, (VecDeque<String>, HashSet<String>)>,
    line_no: HashMap<Channel, u64>,
    host_seq: u64,
    /// Makes the event ids: uuid v7, never going backwards, so the ids order the trace.
    ids: EventIds,
    last_result_error: Option<bool>,
    auth_required: bool,
    capped: bool,
    ended: bool,
    counters: Counters,
}

fn short(s: &str) -> String {
    let mut end = s.len().min(MAX_NAME);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(short)
}

fn is_delegation(tool: &str) -> bool {
    matches!(tool, "Agent" | "Task")
}

/// Words a login or credential prompt uses. Matching lines are never stored.
fn credential_prompt(s: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)(not logged in|log ?in\b|/login|api[ _-]?key|password|passphrase|credential|oauth|authenticat|unauthori[sz]ed|sign[ -]?in\b|access token|refresh token)")
            .expect("valid regex")
    })
    .is_match(s)
}

fn reported(v: &Value, name: &str, unit: &str, source: &str) -> Metric {
    let value = v.as_f64().filter(|f| f.is_finite());
    Metric {
        name: name.into(),
        value,
        unit: unit.into(),
        quality: if value.is_some() {
            DataQuality::Reported
        } else {
            DataQuality::Unknown
        },
        source: source.into(),
    }
}

impl Normalizer {
    pub fn new(attempt: AttemptRef) -> Self {
        Self {
            attempt,
            lead: None,
            facts: HashMap::new(),
            owner: HashMap::new(),
            delegations: HashMap::new(),
            open_tools: BTreeSet::new(),
            open_agents: BTreeSet::new(),
            recent: HashMap::new(),
            line_no: HashMap::new(),
            host_seq: 0,
            ids: EventIds::default(),
            last_result_error: None,
            auth_required: false,
            capped: false,
            ended: false,
            counters: Counters::default(),
        }
    }

    pub fn counters(&self) -> &Counters {
        &self.counters
    }

    /// Entries in the normaliser's maps and sets, which are capped.
    pub fn tracked(&self) -> usize {
        self.facts.len()
            + self.owner.len()
            + self.delegations.len()
            + self.open_tools.len()
            + self.open_agents.len()
    }

    /// Whether another id may be remembered; the first refusal adds one `gap_detected`.
    fn room(&mut self, l: &mut Line, out: &mut Vec<TraceEvent>) -> bool {
        /// Entries the normaliser keeps across its maps and sets. Past this it stops learning new ids,
        /// says so once with a `gap_detected`, and carries on.
        const MAX_TRACKED: usize = 65_536;

        if self.tracked() < MAX_TRACKED {
            return true;
        }
        if !self.capped {
            self.capped = true;
            let mut e = self.event(
                l,
                EventKind::GapDetected,
                Origin::Derived,
                Visibility::Unavailable,
            );
            e.data = json!({ "reason": "tracking_limit", "max_tracked": MAX_TRACKED });
            out.push(e);
        }
        false
    }

    fn next_line(&mut self, channel: Channel, raw: &[u8], at: SystemTime) -> Line {
        let no = self.line_no.entry(channel).or_insert(0);
        *no += 1;
        Line {
            channel,
            no: *no,
            sha: sha256_hex(raw),
            at,
            index: 0,
            native_type: None,
            session_id: None,
        }
    }

    /// Whether this exact line came through this channel recently.
    fn duplicate(&mut self, l: &Line) -> bool {
        /// Exact duplicate lines are recognised within this many recent lines of the same channel.
        const DEDUP_WINDOW: usize = 4_096;

        let (order, set) = self.recent.entry(l.channel).or_default();
        if set.contains(&l.sha) {
            return true;
        }
        if order.len() == DEDUP_WINDOW {
            if let Some(old) = order.pop_front() {
                set.remove(&old);
            }
        }
        order.push_back(l.sha.clone());
        set.insert(l.sha.clone());
        false
    }

    fn event(
        &mut self,
        l: &mut Line,
        kind: EventKind,
        origin: Origin,
        visibility: Visibility,
    ) -> TraceEvent {
        let id = self.ids.next(
            l.at,
            &[
                self.attempt.attempt_id.as_bytes(),
                l.channel.name().as_bytes(),
                l.sha.as_bytes(),
                &l.index.to_le_bytes(),
            ],
        );
        l.index += 1;
        let mut e = TraceEvent::new(&self.attempt, id, kind, origin, visibility, l.at, PARSER);
        if l.channel != Channel::Host {
            e.evidence_refs
                .push(format!("raw:{}:L{}", l.channel.name(), l.no));
            e.source.raw_sha256 = Some(l.sha.clone());
        }
        e.native.native_type.clone_from(&l.native_type);
        e.native.session_id.clone_from(&l.session_id);
        e
    }

    /// Registers `ev` as the first report of `key`, or marks it as confirming the first one.
    /// Returns whether it was the first.
    fn fact(
        &mut self,
        key: (Fact, String),
        channel: Channel,
        ev: &mut TraceEvent,
        l: &mut Line,
        out: &mut Vec<TraceEvent>,
    ) -> bool {
        if let Some((first, ch)) = self.facts.get(&key) {
            ev.corroborates = Some(*first);
            if *ch != channel {
                ev.visibility = Visibility::Correlated;
            }
            return false;
        }
        if self.room(l, out) {
            self.facts.insert(key, (ev.event_id, channel));
        }
        true
    }

    fn remember_owner(
        &mut self,
        tool_use_id: &str,
        agent: Option<&String>,
        l: &mut Line,
        out: &mut Vec<TraceEvent>,
    ) {
        if let Some(a) = agent {
            if !self.owner.contains_key(tool_use_id) && self.room(l, out) {
                self.owner.insert(tool_use_id.to_owned(), a.clone());
            }
        }
    }

    fn open_agent(&mut self, id: &str, l: &mut Line, out: &mut Vec<TraceEvent>) {
        if !self.facts.contains_key(&(Fact::AgentStop, id.to_owned())) && self.room(l, out) {
            self.open_agents.insert(id.to_owned());
        }
    }

    fn gap_line(&mut self, l: &mut Line, reason: &str, bytes: usize) -> Batch {
        self.counters.malformed += 1;
        let origin = if l.channel == Channel::Hooks {
            Origin::Hook
        } else {
            Origin::CliStream
        };
        let mut e = self.event(l, EventKind::GapDetected, origin, Visibility::Unavailable);
        e.data = json!({ "reason": reason, "bytes": bytes, "channel": l.channel.name() });
        Batch {
            events: vec![e],
            payload: None,
        }
    }

    fn ignore(&mut self, what: &str) {
        *self.counters.ignored.entry(short(what)).or_insert(0) += 1;
    }

    fn unknown(&mut self, l: &mut Line, origin: Origin, what: &str, out: &mut Vec<TraceEvent>) {
        self.counters.unknown += 1;
        let mut e = self.event(l, EventKind::TraceWarning, origin, Visibility::Observed);
        e.data = json!({ "reason": "unknown_variant", "what": short(what) });
        out.push(e);
    }

    /// Parses a line into a JSON object, or says why not.
    fn parse(line: &[u8]) -> Result<Value, &'static str> {
        let s = std::str::from_utf8(line).map_err(|_| "invalid_utf8")?;
        match serde_json::from_str::<Value>(s) {
            Ok(v) if v.is_object() => Ok(v),
            Ok(_) => Err("not_an_object"),
            Err(_) => Err("malformed_json"),
        }
    }

    /// `attempt_started`, from the host. `info` is small metadata (CLI version, flags).
    pub fn start(&mut self, at: SystemTime, info: Value) -> Batch {
        let mut l = self.host_line(at);
        let mut e = self.event(
            &mut l,
            EventKind::AttemptStarted,
            Origin::AibusScheduler,
            Visibility::Observed,
        );
        e.data = info;
        Batch {
            events: vec![e],
            payload: None,
        }
    }

    fn host_line(&mut self, at: SystemTime) -> Line {
        self.host_seq += 1;
        Line {
            channel: Channel::Host,
            no: self.host_seq,
            sha: format!("host-{}", self.host_seq),
            at,
            index: 0,
            native_type: None,
            session_id: None,
        }
    }

    /// One stdout line (without its newline).
    pub fn stream_line(&mut self, line: &[u8], at: SystemTime) -> Batch {
        self.counters.lines += 1;
        let mut l = self.next_line(Channel::Stdout, line, at);
        if line.iter().all(u8::is_ascii_whitespace) {
            self.ignore("blank");
            return Batch::default();
        }
        if self.duplicate(&l) {
            self.counters.duplicates += 1;
            return Batch::default();
        }
        let v = match Self::parse(line) {
            Ok(v) => v,
            Err(reason) => return self.gap_line(&mut l, reason, line.len()),
        };
        let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
        let sub = v.get("subtype").and_then(Value::as_str);
        l.native_type = Some(short(&match (ty, sub) {
            ("system", Some(s)) => format!("system/{s}"),
            _ => ty.to_owned(),
        }));
        l.session_id = text(&v, "session_id");
        let mut out = Vec::new();
        let mut keep_payload = true;
        match ty {
            "system" => self.system(&mut l, &v, sub.unwrap_or(""), &mut out),
            "assistant" | "user" => self.message(&mut l, &v, ty, &mut out),
            "result" => keep_payload = self.result(&mut l, &v, &mut out),
            "rate_limit_event" | "stream_event" => self.ignore(ty),
            other => self.unknown(
                &mut l,
                Origin::CliStream,
                if other.is_empty() { "<no type>" } else { other },
                &mut out,
            ),
        }
        Batch {
            payload: (keep_payload && !out.is_empty()).then_some(v),
            events: out,
        }
    }

    fn system(&mut self, l: &mut Line, v: &Value, sub: &str, out: &mut Vec<TraceEvent>) {
        match sub {
            "init" => {
                let Some(sid) = text(v, "session_id") else {
                    return self.unknown(
                        l,
                        Origin::CliStream,
                        "system/init without session_id",
                        out,
                    );
                };
                match &self.lead {
                    Some(lead) if *lead == sid => return self.ignore("system/init (wake-up)"),
                    Some(_) => {
                        let mut e = self.event(
                            l,
                            EventKind::TraceWarning,
                            Origin::CliStream,
                            Visibility::Observed,
                        );
                        e.data = json!({ "reason": "second_session_in_one_process" });
                        out.push(e);
                        return;
                    }
                    None => self.lead = Some(sid.clone()),
                }
                let mut tools: Vec<String> = Vec::new();
                for t in v
                    .get("tools")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .take(MAX_NAME)
                {
                    if let Some(name) = t.as_str() {
                        // `init` lists the delegation tool as `Task`, calls name it `Agent`.
                        let name = if is_delegation(name) {
                            "Agent".to_owned()
                        } else {
                            short(name)
                        };
                        if !tools.contains(&name) {
                            tools.push(name);
                        }
                    }
                }
                let mut e = self.event(
                    l,
                    EventKind::AgentStarted,
                    Origin::CliStream,
                    Visibility::Observed,
                );
                e.agent_id = Some(sid.clone());
                e.data = json!({
                    "role": "lead",
                    "model": text(v, "model"),
                    "cli_version": text(v, "claude_code_version"),
                    "permission_mode": text(v, "permissionMode"),
                    "tools": tools,
                });
                if self.fact((Fact::Agent, sid.clone()), l.channel, &mut e, l, out) {
                    self.open_agent(&sid, l, out);
                }
                out.push(e);
            }
            "task_started" => {
                let Some(child) = text(v, "task_id") else {
                    return self.unknown(
                        l,
                        Origin::CliStream,
                        "system/task_started without task_id",
                        out,
                    );
                };
                let via = text(v, "tool_use_id");
                let mut e = self.event(
                    l,
                    EventKind::AgentStarted,
                    Origin::CliStream,
                    Visibility::Observed,
                );
                e.agent_id = Some(child.clone());
                e.parent_agent_id = via
                    .as_ref()
                    .and_then(|t| self.owner.get(t).cloned())
                    .or_else(|| self.lead.clone());
                e.parent_tool_use_id.clone_from(&via);
                e.native.tool_use_id.clone_from(&via);
                e.data = json!({
                    "subagent_type": text(v, "subagent_type"),
                    "backgrounded": v.get("is_backgrounded").and_then(Value::as_bool),
                    "spawn_depth": v.get("spawn_depth").and_then(Value::as_u64),
                    "task_type": text(v, "task_type"),
                });
                if self.fact((Fact::Agent, child.clone()), l.channel, &mut e, l, out) {
                    self.open_agent(&child, l, out);
                }
                if let Some(t) = via {
                    if self.delegations.contains_key(&t) || self.room(l, out) {
                        self.delegations.insert(t, Some(child));
                    }
                }
                out.push(e);
            }
            "task_updated" | "task_notification" => {
                let Some(child) = text(v, "task_id") else {
                    return self.unknown(l, Origin::CliStream, sub, out);
                };
                let status = if sub == "task_updated" {
                    v.pointer("/patch/status")
                } else {
                    v.get("status")
                }
                .and_then(Value::as_str);
                let Some(status) = status.filter(|s| {
                    matches!(
                        *s,
                        "completed" | "failed" | "killed" | "stopped" | "cancelled" | "error"
                    )
                }) else {
                    return self.ignore(&format!("system/{sub} (not final)"));
                };
                let mut e = self.event(
                    l,
                    EventKind::AgentStopped,
                    Origin::CliStream,
                    Visibility::Observed,
                );
                e.agent_id = Some(child.clone());
                e.data = json!({ "status": status });
                if let Some(ms) = v.pointer("/patch/end_time").and_then(Value::as_u64) {
                    e.occurred_at = Some(aibus_core::persist::time::rfc3339(
                        UNIX_EPOCH + Duration::from_millis(ms),
                    ));
                }
                self.fact((Fact::AgentStop, child.clone()), l.channel, &mut e, l, out);
                self.open_agents.remove(&child);
                out.push(e);
                if let Some(usage) = v.get("usage").filter(|_| sub == "task_notification") {
                    let mut u = self.event(
                        l,
                        EventKind::UsageReported,
                        Origin::CliStream,
                        Visibility::Observed,
                    );
                    u.agent_id = Some(child.clone());
                    u.data = json!({
                        "scope": "agent",
                        "cumulative": false,
                        "metrics": [
                            reported(&usage["total_tokens"], "total_tokens", "tokens", "task_notification.usage"),
                            reported(&usage["tool_uses"], "tool_uses", "calls", "task_notification.usage"),
                            reported(&usage["duration_ms"], "duration_ms", "ms", "task_notification.usage"),
                        ],
                    });
                    self.fact((Fact::AgentUsage, child), l.channel, &mut u, l, out);
                    out.push(u);
                }
            }
            "permission_denied" => {
                let Some(id) = text(v, "tool_use_id") else {
                    return self.unknown(
                        l,
                        Origin::CliStream,
                        "system/permission_denied without tool_use_id",
                        out,
                    );
                };
                let mut e = self.event(
                    l,
                    EventKind::ToolFailed,
                    Origin::CliStream,
                    Visibility::Observed,
                );
                e.agent_id = self.owner.get(&id).cloned().or_else(|| self.lead.clone());
                e.native.tool_use_id = Some(id.clone());
                e.data = json!({
                    "tool": text(v, "tool_name"),
                    "reason": "permission_denied",
                    "decision_reason_type": text(v, "decision_reason_type"),
                });
                self.fact((Fact::ToolEnd, id.clone()), l.channel, &mut e, l, out);
                self.open_tools.remove(&id);
                out.push(e);
            }
            "hook_response"
                if v.get("exit_code")
                    .and_then(Value::as_i64)
                    .is_some_and(|c| c != 0)
                    || v.get("outcome")
                        .and_then(Value::as_str)
                        .is_some_and(|o| o != "success") =>
            {
                let mut e = self.event(
                    l,
                    EventKind::TraceWarning,
                    Origin::CliStream,
                    Visibility::Observed,
                );
                e.data = json!({ "reason": "hook_failed", "hook_event": text(v, "hook_event"), "exit_code": v.get("exit_code").and_then(Value::as_i64) });
                out.push(e);
            }
            "hook_started"
            | "hook_response"
            | "task_progress"
            | "background_tasks_changed"
            | "thinking_tokens"
            | "status" => {
                self.ignore(&format!("system/{sub}"));
            }
            other => self.unknown(l, Origin::CliStream, &format!("system/{other}"), out),
        }
    }

    fn message(&mut self, l: &mut Line, v: &Value, role: &str, out: &mut Vec<TraceEvent>) {
        /// Content blocks read from one message.
        const MAX_BLOCKS: usize = 1_024;

        let via = text(v, "parent_tool_use_id");
        let named = text(v, "agent_id");
        let agent = named
            .clone()
            .or_else(|| {
                via.as_ref()
                    .and_then(|t| self.delegations.get(t).cloned().flatten())
            })
            .or_else(|| self.lead.clone());
        let occurred = v
            .get("timestamp")
            .and_then(Value::as_str)
            .filter(|t| t.len() <= 40)
            .map(str::to_owned);
        // A child whose own message arrives before its `task_started`: its existence is explicit.
        if let (Some(child), Some(t)) = (&named, &via) {
            if !self.facts.contains_key(&(Fact::Agent, child.clone())) {
                let mut e = self.event(
                    l,
                    EventKind::AgentStarted,
                    Origin::CliStream,
                    Visibility::Observed,
                );
                e.agent_id = Some(child.clone());
                e.parent_tool_use_id = Some(t.clone());
                e.parent_agent_id = self.owner.get(t).cloned().or_else(|| self.lead.clone());
                e.occurred_at.clone_from(&occurred);
                e.data = json!({ "subagent_type": text(v, "subagent_type"), "first_seen_in": "message" });
                if self.fact((Fact::Agent, child.clone()), l.channel, &mut e, l, out) {
                    self.open_agent(child, l, out);
                }
                if !self.delegations.contains_key(t) && self.room(l, out) {
                    self.delegations.insert(t.clone(), Some(child.clone()));
                }
                out.push(e);
            }
        }
        let content = v.pointer("/message/content");
        let blocks: Vec<&Value> = match content {
            Some(Value::Array(a)) => a.iter().take(MAX_BLOCKS).collect(),
            Some(s @ Value::String(_)) => vec![s],
            _ => Vec::new(),
        };
        let base = |e: &mut TraceEvent| {
            e.agent_id.clone_from(&agent);
            e.parent_tool_use_id.clone_from(&via);
            e.occurred_at.clone_from(&occurred);
        };
        for b in blocks {
            let kind = if b.is_string() {
                "text"
            } else {
                b.get("type").and_then(Value::as_str).unwrap_or("")
            };
            match kind {
                "text" => {
                    let bytes = b
                        .as_str()
                        .or_else(|| b.get("text").and_then(Value::as_str))
                        .map_or(0, str::len);
                    let mut e = self.event(
                        l,
                        EventKind::MessageChunk,
                        Origin::CliStream,
                        Visibility::Observed,
                    );
                    base(&mut e);
                    e.data = json!({ "role": role, "bytes": bytes });
                    out.push(e);
                }
                "thinking" | "redacted_thinking" => {
                    let mut e = self.event(
                        l,
                        EventKind::GapDetected,
                        Origin::CliStream,
                        Visibility::Unavailable,
                    );
                    base(&mut e);
                    e.data = json!({ "reason": "reasoning_not_exposed" });
                    out.push(e);
                }
                "tool_use" => {
                    let (Some(id), Some(name)) = (text(b, "id"), text(b, "name")) else {
                        self.unknown(l, Origin::CliStream, "tool_use without id or name", out);
                        continue;
                    };
                    self.remember_owner(&id, agent.as_ref(), l, out);
                    let mut e;
                    if is_delegation(&name) {
                        e = self.event(
                            l,
                            EventKind::TaskDelegated,
                            Origin::CliStream,
                            Visibility::Observed,
                        );
                        e.data = json!({
                            "tool": "Agent",
                            "native_name": name,
                            "subagent_type": b.pointer("/input/subagent_type").and_then(Value::as_str).map(short),
                            "run_in_background": b.pointer("/input/run_in_background").and_then(Value::as_bool),
                        });
                        self.fact((Fact::Delegation, id.clone()), l.channel, &mut e, l, out);
                        if !self.delegations.contains_key(&id) && self.room(l, out) {
                            self.delegations.insert(id.clone(), None);
                        }
                    } else {
                        e = self.event(
                            l,
                            EventKind::ToolStarted,
                            Origin::CliStream,
                            Visibility::Observed,
                        );
                        e.data = json!({ "tool": name });
                        let first =
                            self.fact((Fact::ToolStart, id.clone()), l.channel, &mut e, l, out);
                        if first
                            && !self.facts.contains_key(&(Fact::ToolEnd, id.clone()))
                            && self.room(l, out)
                        {
                            self.open_tools.insert(id.clone());
                        }
                    }
                    base(&mut e);
                    e.native.tool_use_id = Some(id);
                    out.push(e);
                }
                "tool_result" => {
                    let Some(id) = text(b, "tool_use_id") else {
                        self.unknown(l, Origin::CliStream, "tool_result without tool_use_id", out);
                        continue;
                    };
                    if let Some(child) = self.delegations.get(&id).cloned() {
                        let child = v
                            .pointer("/tool_use_result/agentId")
                            .and_then(Value::as_str)
                            .map(short)
                            .or(child);
                        let Some(child) = child else { continue };
                        let mut e = self.event(
                            l,
                            EventKind::AgentStopped,
                            Origin::CliStream,
                            Visibility::Observed,
                        );
                        e.agent_id = Some(child.clone());
                        e.native.tool_use_id = Some(id);
                        e.occurred_at.clone_from(&occurred);
                        let status = v
                            .pointer("/tool_use_result/status")
                            .and_then(Value::as_str)
                            .unwrap_or("returned");
                        e.data = json!({ "status": short(status) });
                        self.fact((Fact::AgentStop, child.clone()), l.channel, &mut e, l, out);
                        self.open_agents.remove(&child);
                        out.push(e);
                        continue;
                    }
                    let failed = b.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                    let kind = if failed {
                        EventKind::ToolFailed
                    } else {
                        EventKind::ToolCompleted
                    };
                    let mut e = self.event(l, kind, Origin::CliStream, Visibility::Observed);
                    base(&mut e);
                    e.agent_id = self.owner.get(&id).cloned().or_else(|| agent.clone());
                    e.native.tool_use_id = Some(id.clone());
                    let bytes = match b.get("content") {
                        Some(Value::String(s)) => s.len(),
                        Some(other) => other.to_string().len(),
                        None => 0,
                    };
                    e.data = json!({ "bytes": bytes });
                    self.fact((Fact::ToolEnd, id.clone()), l.channel, &mut e, l, out);
                    self.open_tools.remove(&id);
                    out.push(e);
                }
                other => self.unknown(l, Origin::CliStream, &format!("block/{other}"), out),
            }
        }
        if matches!(content, Some(Value::Array(a)) if a.len() > MAX_BLOCKS) {
            let mut e = self.event(
                l,
                EventKind::GapDetected,
                Origin::CliStream,
                Visibility::Unavailable,
            );
            e.data = json!({ "reason": "blocks_truncated", "max_blocks": MAX_BLOCKS });
            out.push(e);
        }
    }

    /// A `result` line. Returns whether its payload may be kept (not when it asks for a login).
    fn result(&mut self, l: &mut Line, v: &Value, out: &mut Vec<TraceEvent>) -> bool {
        self.counters.results += 1;
        let is_error = v.get("is_error").and_then(Value::as_bool).unwrap_or(false);
        self.last_result_error = Some(is_error);
        let mut keep = true;
        let says_login = v
            .get("result")
            .and_then(Value::as_str)
            .is_some_and(credential_prompt);
        let status_401 = v.get("api_error_status").and_then(Value::as_i64) == Some(401);
        if is_error && (says_login || status_401) {
            self.auth_required = true;
            keep = false;
            let mut e = self.event(
                l,
                EventKind::AuthRequired,
                Origin::CliStream,
                Visibility::Observed,
            );
            e.agent_id.clone_from(&self.lead);
            e.data = json!({ "reason_code": "cli_reports_auth_required" });
            out.push(e);
        }
        let usage = v.get("usage").cloned().unwrap_or(Value::Null);
        let cost = v
            .get("total_cost_usd")
            .and_then(Value::as_f64)
            .filter(|f| f.is_finite());
        let mut e = self.event(
            l,
            EventKind::UsageReported,
            Origin::CliStream,
            Visibility::Observed,
        );
        e.agent_id.clone_from(&self.lead);
        e.data = json!({
            "scope": "attempt",
            "cumulative": true,
            "result_index": self.counters.results,
            "is_error": is_error,
            "metrics": [
                reported(&usage["input_tokens"], "input_tokens", "tokens", "result.usage"),
                reported(&usage["output_tokens"], "output_tokens", "tokens", "result.usage"),
                reported(&usage["cache_creation_input_tokens"], "cache_creation_input_tokens", "tokens", "result.usage"),
                reported(&usage["cache_read_input_tokens"], "cache_read_input_tokens", "tokens", "result.usage"),
                reported(&v["num_turns"], "num_turns", "turns", "result.num_turns"),
                reported(&v["duration_ms"], "duration_ms", "ms", "result.duration_ms"),
                Metric {
                    name: "cost_usd".into(),
                    value: cost,
                    unit: "usd".into(),
                    quality: if cost.is_some() { DataQuality::Estimated } else { DataQuality::Unknown },
                    source: "result.total_cost_usd (client-side list-price estimate)".into(),
                },
                Metric {
                    name: "billed_cost_usd".into(),
                    value: None,
                    unit: "usd".into(),
                    quality: DataQuality::Unknown,
                    source: "not reported by the CLI".into(),
                },
            ],
        });
        out.push(e);
        keep
    }

    /// One hook payload (one JSON object per line; blank lines are skipped).
    pub fn hook_line(&mut self, line: &[u8], at: SystemTime) -> Batch {
        if line.iter().all(u8::is_ascii_whitespace) {
            return Batch::default();
        }
        self.counters.lines += 1;
        let mut l = self.next_line(Channel::Hooks, line, at);
        if self.duplicate(&l) {
            self.counters.duplicates += 1;
            return Batch::default();
        }
        let v = match Self::parse(line) {
            Ok(v) => v,
            Err(reason) => return self.gap_line(&mut l, reason, line.len()),
        };
        let event = v
            .get("hook_event_name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        l.native_type = Some(short(&format!("hook/{event}")));
        l.session_id = text(&v, "session_id");
        let named = text(&v, "agent_id");
        let agent = named
            .clone()
            .or_else(|| self.lead.clone())
            .or_else(|| l.session_id.clone());
        let tool_use_id = text(&v, "tool_use_id");
        let tool = text(&v, "tool_name").unwrap_or_default();
        let mut out = Vec::new();
        let ch = Channel::Hooks;
        match event.as_str() {
            "SessionStart" => {
                let Some(sid) = l.session_id.clone() else {
                    return Batch::default();
                };
                let lead = self.lead.get_or_insert(sid).clone();
                let mut e = self.event(
                    &mut l,
                    EventKind::AgentStarted,
                    Origin::Hook,
                    Visibility::Observed,
                );
                e.agent_id = Some(lead.clone());
                e.data = json!({ "role": "lead" });
                if self.fact((Fact::Agent, lead.clone()), ch, &mut e, &mut l, &mut out) {
                    self.open_agent(&lead, &mut l, &mut out);
                }
                out.push(e);
            }
            "PreToolUse" if tool_use_id.is_some() => {
                let id = tool_use_id.unwrap_or_default();
                self.remember_owner(&id, agent.as_ref(), &mut l, &mut out);
                let mut e;
                if is_delegation(&tool) {
                    e = self.event(
                        &mut l,
                        EventKind::TaskDelegated,
                        Origin::Hook,
                        Visibility::Observed,
                    );
                    e.data = json!({ "tool": "Agent", "native_name": tool, "subagent_type": v.pointer("/tool_input/subagent_type").and_then(Value::as_str).map(short) });
                    self.fact((Fact::Delegation, id.clone()), ch, &mut e, &mut l, &mut out);
                    if !self.delegations.contains_key(&id) && self.room(&mut l, &mut out) {
                        self.delegations.insert(id.clone(), None);
                    }
                } else {
                    e = self.event(
                        &mut l,
                        EventKind::ToolStarted,
                        Origin::Hook,
                        Visibility::Observed,
                    );
                    e.data = json!({ "tool": tool });
                    let first =
                        self.fact((Fact::ToolStart, id.clone()), ch, &mut e, &mut l, &mut out);
                    if first
                        && !self.facts.contains_key(&(Fact::ToolEnd, id.clone()))
                        && self.room(&mut l, &mut out)
                    {
                        self.open_tools.insert(id.clone());
                    }
                }
                e.agent_id = agent;
                e.native.tool_use_id = Some(id);
                out.push(e);
            }
            "PostToolUse" | "PostToolUseFailure" if tool_use_id.is_some() => {
                let id = tool_use_id.unwrap_or_default();
                if is_delegation(&tool) {
                    let child = v
                        .pointer("/tool_response/agentId")
                        .and_then(Value::as_str)
                        .map(short)
                        .or_else(|| self.delegations.get(&id).cloned().flatten());
                    if let Some(child) = child {
                        let mut e = self.event(
                            &mut l,
                            EventKind::AgentStopped,
                            Origin::Hook,
                            Visibility::Observed,
                        );
                        e.agent_id = Some(child.clone());
                        e.native.tool_use_id = Some(id);
                        e.data = json!({ "status": v.pointer("/tool_response/status").and_then(Value::as_str).map_or_else(|| "returned".into(), short) });
                        self.fact(
                            (Fact::AgentStop, child.clone()),
                            ch,
                            &mut e,
                            &mut l,
                            &mut out,
                        );
                        self.open_agents.remove(&child);
                        out.push(e);
                    }
                } else {
                    let kind = if event == "PostToolUse" {
                        EventKind::ToolCompleted
                    } else {
                        EventKind::ToolFailed
                    };
                    let mut e = self.event(&mut l, kind, Origin::Hook, Visibility::Observed);
                    e.agent_id = self.owner.get(&id).cloned().or(agent);
                    e.native.tool_use_id = Some(id.clone());
                    e.data = json!({ "tool": tool });
                    self.fact((Fact::ToolEnd, id.clone()), ch, &mut e, &mut l, &mut out);
                    self.open_tools.remove(&id);
                    out.push(e);
                }
            }
            "SubagentStart" | "SubagentStop" if named.is_some() => {
                let child = named.unwrap_or_default();
                if event == "SubagentStart" {
                    // The payload names no parent: the edge has to come from another line.
                    let mut e = self.event(
                        &mut l,
                        EventKind::AgentStarted,
                        Origin::Hook,
                        Visibility::Observed,
                    );
                    e.agent_id = Some(child.clone());
                    e.data = json!({ "subagent_type": text(&v, "agent_type") });
                    if self.fact((Fact::Agent, child.clone()), ch, &mut e, &mut l, &mut out) {
                        self.open_agent(&child, &mut l, &mut out);
                    }
                    out.push(e);
                } else {
                    let mut e = self.event(
                        &mut l,
                        EventKind::AgentStopped,
                        Origin::Hook,
                        Visibility::Observed,
                    );
                    e.agent_id = Some(child.clone());
                    e.data = json!({ "status": "stopped" });
                    self.fact(
                        (Fact::AgentStop, child.clone()),
                        ch,
                        &mut e,
                        &mut l,
                        &mut out,
                    );
                    self.open_agents.remove(&child);
                    out.push(e);
                }
            }
            "UserPromptSubmit" | "Stop" | "SessionEnd" | "Notification" | "PreCompact"
            | "PermissionRequest" | "TaskCreated" | "TaskCompleted" | "TeammateIdle" => {
                self.ignore(&format!("hook/{event}"));
            }
            other => self.unknown(&mut l, Origin::Hook, &format!("hook/{other}"), &mut out),
        }
        Batch {
            payload: (!out.is_empty()).then_some(v),
            events: out,
        }
    }

    /// One stderr line. Its text reaches the store only as a payload, and never when it looks like
    /// a login or credential prompt.
    pub fn stderr_line(&mut self, line: &[u8], at: SystemTime) -> Batch {
        if line.iter().all(u8::is_ascii_whitespace) {
            return Batch::default();
        }
        self.counters.lines += 1;
        let mut l = self.next_line(Channel::Stderr, line, at);
        if self.duplicate(&l) {
            self.counters.duplicates += 1;
            return Batch::default();
        }
        let text = String::from_utf8_lossy(line);
        if credential_prompt(&text) {
            self.auth_required = true;
            let mut e = self.event(
                &mut l,
                EventKind::AuthRequired,
                Origin::CliStream,
                Visibility::Observed,
            );
            e.agent_id.clone_from(&self.lead);
            e.data = json!({ "reason_code": "credential_prompt_on_stderr", "channel": "stderr" });
            return Batch {
                events: vec![e],
                payload: None,
            };
        }
        let mut e = self.event(
            &mut l,
            EventKind::TraceWarning,
            Origin::CliStream,
            Visibility::Observed,
        );
        e.data = json!({ "reason": "stderr", "channel": "stderr", "bytes": line.len() });
        Batch {
            events: vec![e],
            payload: Some(json!({ "stderr": text })),
        }
    }

    /// A gap the host found (a line over the size limit, a limit that stopped the attempt).
    pub fn gap(&mut self, channel: Channel, reason: &str, bytes: u64, at: SystemTime) -> Batch {
        let mut l = if channel == Channel::Host {
            self.host_line(at)
        } else {
            self.counters.lines += 1;
            let mut l = self.next_line(channel, reason.as_bytes(), at);
            l.sha = sha256_hex(format!("{}:{}:{reason}", channel.name(), l.no).as_bytes());
            l
        };
        let origin = match channel {
            Channel::Stdout | Channel::Stderr => Origin::CliStream,
            Channel::Hooks => Origin::Hook,
            Channel::Host => Origin::AibusScheduler,
        };
        let mut e = self.event(
            &mut l,
            EventKind::GapDetected,
            origin,
            Visibility::Unavailable,
        );
        e.data = json!({ "reason": short(reason), "bytes": bytes, "channel": channel.name() });
        Batch {
            events: vec![e],
            payload: None,
        }
    }

    /// The process ended: closes what is still open and writes `attempt_completed` or
    /// `attempt_failed` with the status, so the attempt ends here and only here.
    pub fn finish(&mut self, ending: Ending, wall: Duration, at: SystemTime) -> Batch {
        if self.ended {
            return Batch::default();
        }
        self.ended = true;
        let (status, reason) = match ending {
            Ending::Exited(0) if self.auth_required => (AttemptStatus::Failed, "auth_required"),
            Ending::Exited(0) => match self.last_result_error {
                Some(false) => (AttemptStatus::Completed, "completed"),
                Some(true) => (AttemptStatus::Failed, "result_error"),
                None => (AttemptStatus::Failed, "no_result"),
            },
            Ending::Exited(_) => (
                AttemptStatus::Failed,
                if self.auth_required {
                    "auth_required"
                } else {
                    "exit_code"
                },
            ),
            Ending::Signalled(_) => (AttemptStatus::Interrupted, "signal"),
            Ending::WallTime => (AttemptStatus::Interrupted, "wall_time"),
            Ending::OutputLimit => (AttemptStatus::Interrupted, "output_limit"),
            Ending::Cancelled => (AttemptStatus::Cancelled, "cancelled"),
        };
        let mut out = Vec::new();
        let mut l = self.host_line(at);
        if status != AttemptStatus::Completed
            && (self.last_result_error.is_none()
                || matches!(
                    status,
                    AttemptStatus::Interrupted | AttemptStatus::Cancelled
                ))
        {
            let mut e = self.event(
                &mut l,
                EventKind::GapDetected,
                Origin::CliStream,
                Visibility::Unavailable,
            );
            e.data = json!({ "reason": "stream_truncated" });
            out.push(e);
        }
        let reconcile = !self.open_tools.is_empty();
        for id in std::mem::take(&mut self.open_tools) {
            let mut e = self.event(
                &mut l,
                EventKind::GapDetected,
                Origin::Derived,
                Visibility::Unavailable,
            );
            e.agent_id = self.owner.get(&id).cloned();
            e.native.tool_use_id = Some(id);
            e.data = json!({ "reason": "tool_outcome_unknown", "requires_reconciliation": true });
            out.push(e);
        }
        for id in std::mem::take(&mut self.open_agents) {
            let lead = self.lead.as_deref() == Some(id.as_str());
            // The lead is the process, so its end is observed; the children live inside it, so
            // that they ended with it is aibus's inference.
            let (origin, vis, why) = if lead {
                (
                    Origin::AibusScheduler,
                    Visibility::Observed,
                    "process_exited",
                )
            } else {
                (Origin::Derived, Visibility::Inferred, "ended_with_process")
            };
            let mut e = self.event(&mut l, EventKind::AgentStopped, origin, vis);
            e.agent_id = Some(id);
            e.data = json!({ "status": why });
            out.push(e);
        }
        let kind = if status == AttemptStatus::Completed {
            EventKind::AttemptCompleted
        } else {
            EventKind::AttemptFailed
        };
        let mut e = self.event(&mut l, kind, Origin::AibusScheduler, Visibility::Observed);
        e.agent_id.clone_from(&self.lead);
        let c = &self.counters;
        e.data = json!({
            "status": status,
            "reason": reason,
            "exit_code": match ending { Ending::Exited(code) => Some(code), _ => None },
            "signal": match ending { Ending::Signalled(s) => Some(s), _ => None },
            "results_seen": c.results,
            "requires_reconciliation": reconcile,
            "lines": c.lines,
            "duplicates": c.duplicates,
            "malformed": c.malformed,
            "unknown": c.unknown,
            "ignored": c.ignored,
            "metrics": [Metric {
                name: "wall_ms".into(),
                value: Some(wall.as_millis() as f64),
                unit: "ms".into(),
                quality: DataQuality::Derived,
                source: "aibus monotonic clock, spawn to exit".into(),
            }],
        });
        out.push(e);
        Batch {
            events: out,
            payload: None,
        }
    }
}

impl crate::Engine for Normalizer {
    fn start(&mut self, at: SystemTime, info: Value) -> Batch {
        Self::start(self, at, info)
    }
    fn stream_line(&mut self, line: &[u8], at: SystemTime) -> Batch {
        Self::stream_line(self, line, at)
    }
    fn hook_line(&mut self, line: &[u8], at: SystemTime) -> Batch {
        Self::hook_line(self, line, at)
    }
    fn stderr_line(&mut self, line: &[u8], at: SystemTime) -> Batch {
        Self::stderr_line(self, line, at)
    }
    fn gap(&mut self, channel: Channel, reason: &str, bytes: u64, at: SystemTime) -> Batch {
        Self::gap(self, channel, reason, bytes, at)
    }
    fn finish(&mut self, ending: Ending, wall: Duration, at: SystemTime) -> Batch {
        Self::finish(self, ending, wall, at)
    }
}
