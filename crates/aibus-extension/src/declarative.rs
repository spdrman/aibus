//! The declarative engine: for a CLI whose stdout is JSON lines and whose every line is one event,
//! the manifest's rules say which line is which event. It keeps no state across lines beyond the
//! counters of the attempt and the tool uses still open, which is exactly why it cannot parse
//! Claude's stream (A1.5 E1). An unmatched line is a `trace_warning`, never dropped silently; a
//! line that is not a JSON object is a `gap_detected`. Message text and stderr text are never
//! copied into an event: a payload is only the parsed line, and the store keeps it only when the
//! user asked for payloads.

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime};

use aibus_supervisor::{Channel, Ending, Engine};
use aibus_trace::{
    sha256_hex, AttemptRef, AttemptStatus, Batch, DataQuality, EventIds, EventKind, Metric, Origin,
    TraceEvent, Visibility,
};
use serde_json::{json, Value};

use crate::manifest::{Rule, RuleKind, UsageQuality};

pub const PARSER: &str = "declarative-jsonl/1";

pub struct Declarative {
    attempt: AttemptRef,
    rules: Vec<Rule>,
    ids: EventIds,
    // The counters, the parser name and the ending are `pub(crate)` because the WASM host
    // (`crate::wasm`) stamps and ends an attempt through this same object, so the two engines'
    // host-owned events cannot drift apart.
    pub(crate) lines: u64,
    pub(crate) stdout_lines: u64,
    stderr_lines: u64,
    usage_quality: UsageQuality,
    host: u64,
    pub(crate) malformed: u64,
    pub(crate) unknown: u64,
    open_tools: BTreeSet<String>,
    /// A guest said a tool outcome is unknown: the terminal event asks for reconciliation.
    pub(crate) reconcile: bool,
    pub(crate) parser: &'static str,
    pub(crate) ended: bool,
}

impl Declarative {
    pub fn new(attempt: AttemptRef, rules: Vec<Rule>, usage_quality: UsageQuality) -> Self {
        Self {
            attempt,
            rules,
            ids: EventIds::default(),
            lines: 0,
            stdout_lines: 0,
            stderr_lines: 0,
            usage_quality,
            host: 0,
            malformed: 0,
            unknown: 0,
            open_tools: BTreeSet::new(),
            reconcile: false,
            parser: PARSER,
            ended: false,
        }
    }

    /// An event for the raw `key` (the channel and line number, or a host counter), made at `at`.
    pub(crate) fn event(
        &mut self,
        key: &str,
        raw: &[u8],
        kind: EventKind,
        origin: Origin,
        visibility: Visibility,
        at: SystemTime,
    ) -> TraceEvent {
        let sha = sha256_hex(raw);
        let id = self.ids.next(
            at,
            &[
                self.attempt.attempt_id.as_bytes(),
                key.as_bytes(),
                sha.as_bytes(),
                &0u32.to_le_bytes(),
            ],
        );
        let mut e = TraceEvent::new(&self.attempt, id, kind, origin, visibility, at, self.parser);
        if !key.starts_with("host") {
            e.evidence_refs.push(format!("raw:{key}"));
            e.source.raw_sha256 = Some(sha);
        }
        e
    }

    pub(crate) fn host_event(
        &mut self,
        kind: EventKind,
        origin: Origin,
        visibility: Visibility,
        at: SystemTime,
    ) -> TraceEvent {
        self.host += 1;
        let key = format!("host-{}", self.host);
        self.event(&key, key.as_bytes(), kind, origin, visibility, at)
    }

    fn gap(&mut self, key: &str, raw: &[u8], reason: &str, bytes: usize, at: SystemTime) -> Batch {
        self.malformed += 1;
        let mut e = self.event(
            key,
            raw,
            EventKind::GapDetected,
            Origin::CliStream,
            Visibility::Unavailable,
            at,
        );
        e.data = json!({ "reason": reason, "bytes": bytes, "channel": "stdout" });
        Batch {
            events: vec![e],
            payload: None,
        }
    }
}

fn pointer_text(v: &Value, p: &str) -> Option<String> {
    v.pointer(p)
        .and_then(Value::as_str)
        .map(|s| s.chars().take(256).collect())
}

/// One token count: a whole number from zero up to 2^53 (so a negative, fractional, huge or
/// non-numeric value is `unknown`, never `reported`), labelled as the manifest says its usage is.
fn metric(v: &Value, pointer: Option<&String>, name: &str, quality: UsageQuality) -> Metric {
    let value = pointer
        .and_then(|p| v.pointer(p))
        .and_then(Value::as_u64)
        .filter(|n| *n <= 1 << 53)
        .map(|n| n as f64)
        .filter(|_| quality != UsageQuality::Unknown);
    Metric {
        name: name.into(),
        value,
        unit: "tokens".into(),
        quality: match (value, quality) {
            (None, _) | (_, UsageQuality::Unknown) => DataQuality::Unknown,
            (_, UsageQuality::Reported) => DataQuality::Reported,
            (_, UsageQuality::Estimated) => DataQuality::Estimated,
        },
        source: "the CLI's own usage line".into(),
    }
}

impl Engine for Declarative {
    fn observe(&mut self, id: uuid::Uuid) {
        self.ids.observe(id);
    }

    fn start(&mut self, at: SystemTime, info: Value) -> Batch {
        let mut e = self.host_event(
            EventKind::AttemptStarted,
            Origin::AibusScheduler,
            Visibility::Observed,
            at,
        );
        e.data = info;
        Batch {
            events: vec![e],
            payload: None,
        }
    }

    fn stream_line(&mut self, line: &[u8], at: SystemTime) -> Batch {
        self.lines += 1;
        self.stdout_lines += 1;
        let key = format!("stdout:L{}", self.stdout_lines);
        let Ok(text) = std::str::from_utf8(line) else {
            return self.gap(&key, line, "invalid_utf8", line.len(), at);
        };
        let v: Value = match serde_json::from_str::<Value>(text) {
            Ok(v) if v.is_object() => v,
            Ok(_) => return self.gap(&key, line, "not_an_object", line.len(), at),
            Err(_) => return self.gap(&key, line, "malformed_json", line.len(), at),
        };
        let rule = self
            .rules
            .iter()
            .find(|r| v.pointer(&r.pointer).and_then(Value::as_str) == Some(r.equals.as_str()))
            .cloned();
        let Some(rule) = rule else {
            self.unknown += 1;
            let what = self
                .rules
                .first()
                .and_then(|r| pointer_text(&v, &r.pointer))
                .unwrap_or_default();
            let mut e = self.event(
                &key,
                line,
                EventKind::TraceWarning,
                Origin::CliStream,
                Visibility::Observed,
                at,
            );
            e.data = json!({ "reason": "unknown_variant", "what": what.chars().take(64).collect::<String>() });
            return Batch {
                events: vec![e],
                payload: Some(v),
            };
        };
        let kind = match rule.kind {
            RuleKind::MessageChunk => EventKind::MessageChunk,
            RuleKind::ToolStarted => EventKind::ToolStarted,
            RuleKind::ToolCompleted => EventKind::ToolCompleted,
            RuleKind::ToolFailed => EventKind::ToolFailed,
            RuleKind::UsageReported => EventKind::UsageReported,
        };
        let mut e = self.event(
            &key,
            line,
            kind,
            Origin::CliStream,
            Visibility::Observed,
            at,
        );
        e.native.native_type = Some(rule.equals.chars().take(128).collect());
        match rule.kind {
            RuleKind::MessageChunk => e.data = json!({ "bytes": line.len() }),
            RuleKind::ToolStarted | RuleKind::ToolCompleted | RuleKind::ToolFailed => {
                let id = rule.id_pointer.as_deref().and_then(|p| pointer_text(&v, p));
                if let Some(id) = &id {
                    if kind == EventKind::ToolStarted {
                        // Tool uses remembered as open; past this the engine stops tracking new ones.
                        if self.open_tools.len() < 4_096 {
                            self.open_tools.insert(id.clone());
                        }
                    } else {
                        self.open_tools.remove(id);
                    }
                }
                e.native.tool_use_id = id;
                e.data = json!({});
            }
            RuleKind::UsageReported => {
                e.data = json!({
                    "scope": "attempt",
                    "cumulative": false,
                    "metrics": [
                        metric(&v, rule.tokens_in_pointer.as_ref(), "input_tokens", self.usage_quality),
                        metric(&v, rule.tokens_out_pointer.as_ref(), "output_tokens", self.usage_quality),
                    ],
                });
            }
        }
        Batch {
            events: vec![e],
            payload: Some(v),
        }
    }

    fn hook_line(&mut self, _line: &[u8], _at: SystemTime) -> Batch {
        Batch::default()
    }

    /// stderr is counted and never stored: a CLI's login prompt must not reach a trace.
    fn stderr_line(&mut self, line: &[u8], at: SystemTime) -> Batch {
        if line.iter().all(u8::is_ascii_whitespace) {
            return Batch::default();
        }
        self.lines += 1;
        self.stderr_lines += 1;
        let key = format!("stderr:L{}", self.stderr_lines);
        let mut e = self.event(
            &key,
            line,
            EventKind::TraceWarning,
            Origin::CliStream,
            Visibility::Observed,
            at,
        );
        e.data = json!({ "reason": "stderr", "channel": "stderr", "bytes": line.len() });
        Batch {
            events: vec![e],
            payload: None,
        }
    }

    fn gap(&mut self, channel: Channel, reason: &str, bytes: u64, at: SystemTime) -> Batch {
        let origin = if channel == Channel::Host {
            Origin::AibusScheduler
        } else {
            Origin::CliStream
        };
        let mut e = self.host_event(EventKind::GapDetected, origin, Visibility::Unavailable, at);
        e.data = json!({
            "reason": reason.chars().take(64).collect::<String>(),
            "bytes": bytes,
            "channel": format!("{channel:?}").to_lowercase(),
        });
        Batch {
            events: vec![e],
            payload: None,
        }
    }

    fn finish(&mut self, ending: Ending, wall: Duration, at: SystemTime) -> Batch {
        if self.ended {
            return Batch::default();
        }
        self.ended = true;
        let mut out = self.close(ending, at);
        out.push(self.terminal(ending, wall, at));
        Batch {
            events: out,
            payload: None,
        }
    }
}

/// How an ending maps to the attempt's terminal state.
fn status_of(ending: Ending) -> (AttemptStatus, &'static str) {
    match ending {
        Ending::Exited(0) => (AttemptStatus::Completed, "completed"),
        Ending::Exited(_) => (AttemptStatus::Failed, "exit_code"),
        Ending::Signalled(_) => (AttemptStatus::Interrupted, "signal"),
        Ending::WallTime => (AttemptStatus::Interrupted, "wall_time"),
        Ending::OutputLimit => (AttemptStatus::Interrupted, "output_limit"),
        Ending::Cancelled => (AttemptStatus::Cancelled, "cancelled"),
    }
}

impl Declarative {
    /// What the end of the stream leaves behind, before the terminal event: the truncation gap of
    /// an interrupted attempt and one gap per tool use still open. The WASM host puts its guest's
    /// own end-of-stream events between this and [`Self::terminal`].
    pub(crate) fn close(&mut self, ending: Ending, at: SystemTime) -> Vec<TraceEvent> {
        let (status, _) = status_of(ending);
        let mut out = Vec::new();
        if matches!(
            status,
            AttemptStatus::Interrupted | AttemptStatus::Cancelled
        ) {
            let mut e = self.host_event(
                EventKind::GapDetected,
                Origin::CliStream,
                Visibility::Unavailable,
                at,
            );
            e.data = json!({ "reason": "stream_truncated" });
            out.push(e);
        }
        let open = std::mem::take(&mut self.open_tools);
        self.reconcile |= !open.is_empty();
        for id in open {
            let mut e = self.host_event(
                EventKind::GapDetected,
                Origin::Derived,
                Visibility::Unavailable,
                at,
            );
            e.native.tool_use_id = Some(id);
            e.data = json!({ "reason": "tool_outcome_unknown", "requires_reconciliation": true });
            out.push(e);
        }
        out
    }

    /// The attempt's one terminal event, with the counters of the run.
    pub(crate) fn terminal(
        &mut self,
        ending: Ending,
        wall: Duration,
        at: SystemTime,
    ) -> TraceEvent {
        let (status, reason) = status_of(ending);
        let reconcile = self.reconcile;
        let kind = if status == AttemptStatus::Completed {
            EventKind::AttemptCompleted
        } else {
            EventKind::AttemptFailed
        };
        let mut e = self.host_event(kind, Origin::AibusScheduler, Visibility::Observed, at);
        e.data = json!({
            "status": status,
            "reason": reason,
            "exit_code": match ending { Ending::Exited(c) => Some(c), _ => None },
            "signal": match ending { Ending::Signalled(s) => Some(s), _ => None },
            "requires_reconciliation": reconcile,
            "lines": self.lines,
            "malformed": self.malformed,
            "unknown": self.unknown,
            "metrics": [Metric {
                name: "wall_ms".into(),
                value: Some(wall.as_millis() as f64),
                unit: "ms".into(),
                quality: DataQuality::Derived,
                source: "aibus monotonic clock, spawn to exit".into(),
            }],
        });
        e
    }
}
