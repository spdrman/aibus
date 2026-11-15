//! What a person reads of a recorded run (#284, spec A1.4 and sections 7.3 and 8.1): the fleet of
//! runs, one run's planned and execution graph, its timeline, the inspector for one node or one
//! event, and the offline replay of its state transitions. The CLI (`aibus runs`) and the TUI
//! (`/runs`) print these same strings, and the fleet's `RunRow` is the typed row a fleet with
//! actions (#290) builds on.
//!
//! Everything here reads files and nothing else: no provider, no process, no network. Every piece
//! of text that came from a trace goes through `clean` before it is returned, so an escape
//! sequence, a control or bidi character in an id, a name or a payload reaches the terminal as
//! visible `\u{..}` text and never as something the terminal acts on. Every edge and node shows
//! its evidence level, and every number its quality, as the projection carries them: a gap stays
//! a gap and an unknown cost stays `unknown`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    project, AttemptStatus, DataQuality, Edge, EventKind, Metric, Projection, ReadReport,
    ToolOutcome, TraceEvent, Visibility, SCHEMA_VERSION,
};

/// One run as it was recorded: the events in file order with the exact bytes of each line, what
/// the read skipped, and the plan the run was started from, when it has one.
pub struct Recorded {
    pub dir: PathBuf,
    pub events: Vec<TraceEvent>,
    /// `raw[i]` is the line (with its newline) `events[i]` was read from.
    pub raw: Vec<Vec<u8>>,
    pub report: ReadReport,
    /// `plan.json` next to the events (the #283 plan), when there is one.
    pub plan: Option<Value>,
    /// Why a `plan.json` that is there could not be read.
    pub plan_error: Option<String>,
}

/// Opens `path` for reading only when it is a regular file inside its directory: a symbolic link
/// is refused (`O_NOFOLLOW`), the open of a FIFO or a device returns at once (`O_NONBLOCK`) and is
/// then refused, so a hostile run directory cannot hang or redirect a view.
fn open_regular(path: &Path) -> io::Result<File> {
    let mut o = fs::OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags((rustix::fs::OFlags::NONBLOCK | rustix::fs::OFlags::NOFOLLOW).bits() as i32);
    }
    let file = o.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    Ok(file)
}

impl Recorded {
    /// Reads `<dir>/events.jsonl` and `<dir>/plan.json` without taking the writer's lock.
    pub fn load(dir: &Path) -> io::Result<Self> {
        /// The largest `events.jsonl` a view reads. The store's own soft limit is 256 MiB.
        const MAX_TRACE_BYTES: u64 = 512 << 20;
        let file = open_regular(&dir.join("events.jsonl"))?;
        if file.metadata()?.len() > MAX_TRACE_BYTES {
            return Err(io::Error::other(format!(
                "the trace is over {} MiB, the most a view reads",
                MAX_TRACE_BYTES >> 20
            )));
        }
        let mut bytes = Vec::new();
        file.take(MAX_TRACE_BYTES).read_to_end(&mut bytes)?;
        let mut rec = Self::from_bytes(dir, &bytes);
        let plan_path = dir.join("plan.json");
        // `symlink_metadata`: a link (even a dangling one) is something there that must be refused,
        // not an absent plan.
        if fs::symlink_metadata(&plan_path).is_ok() {
            let read = open_regular(&plan_path).and_then(|f| {
                let mut b = Vec::new();
                f.take((4 << 20) + 1).read_to_end(&mut b).map(|_| b)
            });
            match read {
                Ok(b) if b.len() > 4 << 20 => {
                    rec.plan_error = Some("plan.json is over 4 MiB".into())
                }
                Ok(b) => match serde_json::from_slice::<Value>(&b) {
                    Ok(v) => rec.plan = Some(v),
                    Err(e) => rec.plan_error = Some(format!("plan.json is not JSON: {e}")),
                },
                Err(e) => rec.plan_error = Some(format!("plan.json: {e}")),
            }
        }
        Ok(rec)
    }

    /// The events of `bytes`, read the way `TraceStore::read` reads a file, keeping each line.
    pub fn from_bytes(dir: &Path, bytes: &[u8]) -> Self {
        let mut report = ReadReport::default();
        let mut events = Vec::new();
        let mut raw = Vec::new();
        for line in bytes.split_inclusive(|b| *b == b'\n') {
            if !line.ends_with(b"\n") {
                report.torn_bytes += line.len() as u64;
                break;
            }
            let v = serde_json::from_slice::<Value>(line).ok();
            match v
                .as_ref()
                .and_then(|v| v.get("schema_version"))
                .and_then(Value::as_u64)
            {
                Some(s) if s == u64::from(SCHEMA_VERSION) => {
                    match serde_json::from_value::<TraceEvent>(v.unwrap_or_default()) {
                        Ok(e) => {
                            events.push(e);
                            raw.push(line.to_vec());
                        }
                        Err(_) => report.corrupt_lines += 1,
                    }
                }
                Some(_) => report.unsupported_schema += 1,
                None => report.corrupt_lines += 1,
            }
        }
        Self {
            dir: dir.to_owned(),
            events,
            raw,
            report,
            plan: None,
            plan_error: None,
        }
    }
}

/// A character a terminal acts on or does not show: controls (C0, DEL, C1), every character of
/// the Unicode Format category (bidi overrides and isolates, zero-width characters, the soft
/// hyphen, the byte order mark, tag characters), the line and paragraph separators, the
/// variation selectors and the blank Hangul fillers.
fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00ad}'
                | '\u{0600}'..='\u{0605}'
                | '\u{061c}'
                | '\u{06dd}'
                | '\u{070f}'
                | '\u{0890}'..='\u{0891}'
                | '\u{08e2}'
                | '\u{115f}'..='\u{1160}'
                | '\u{17b4}'..='\u{17b5}'
                | '\u{180b}'..='\u{180f}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{3164}'
                | '\u{fe00}'..='\u{fe0f}'
                | '\u{feff}'
                | '\u{ffa0}'
                | '\u{fff9}'..='\u{fffb}'
                | '\u{110bd}'
                | '\u{110cd}'
                | '\u{13430}'..='\u{1343f}'
                | '\u{1bca0}'..='\u{1bca3}'
                | '\u{1d173}'..='\u{1d17a}'
                | '\u{e0000}'..='\u{e0fff}'
        )
}

/// `s` with every control, bidi and zero-width character written out as `\u{..}`, cut to `max`
/// characters. What comes back can be printed on a terminal as it is.
pub fn clean(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max * 2));
    for (n, c) in s.chars().enumerate() {
        if n == max {
            out.push_str("...");
            break;
        }
        if hidden(c) {
            let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
        } else {
            out.push(c);
        }
    }
    out
}

fn opt(s: Option<&str>) -> String {
    s.map_or_else(|| "-".into(), |t| clean(t, 160))
}

/// A recorded JSON line made safe for a terminal without changing what it says: every character
/// `clean` would escape becomes a JSON `\uXXXX` escape. Such characters can only stand inside a
/// string of a line that parsed, so the result is the same JSON value. Bytes that are not UTF-8
/// are not touched: a line that parsed has none.
pub fn terminal_safe_line(raw: &[u8]) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(raw) else {
        return raw.to_vec();
    };
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c != '\n' && hidden(c) {
            let mut units = [0u16; 2];
            for u in c.encode_utf16(&mut units) {
                let _ = write!(out, "\\u{u:04x}");
            }
        } else {
            out.push(c);
        }
    }
    out.into_bytes()
}

/// The wire name of an enum value (`tool_started`, `cli_stream`, ...).
fn wire(v: impl Serialize) -> String {
    match serde_json::to_value(v) {
        Ok(Value::String(s)) => s,
        _ => "?".into(),
    }
}

fn data_str<'a>(e: &'a TraceEvent, key: &str) -> Option<&'a str> {
    e.data.get(key).and_then(Value::as_str)
}

fn metric_line(m: &Metric) -> String {
    let value = m.value.map_or_else(|| "unknown".into(), |v| v.to_string());
    format!(
        "{} {value} {} ({}, {})",
        clean(&m.name, 160),
        clean(&m.unit, 160),
        wire(m.quality),
        clean(&m.source, 160)
    )
}

/// "observed 2, correlated 1": how many events of each level back a node.
fn levels<'a>(ev: impl Iterator<Item = &'a crate::Evidence>) -> String {
    let mut n: BTreeMap<Visibility, usize> = BTreeMap::new();
    for e in ev {
        *n.entry(e.visibility).or_default() += 1;
    }
    if n.is_empty() {
        return "no events".into();
    }
    n.iter()
        .map(|(v, c)| format!("{} {c}", wire(v)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn edge_line(e: &Edge) -> String {
    fn sources(ev: &[crate::Evidence]) -> String {
        ev.iter()
            .map(|e| format!("#{} {}/{}", e.sequence, wire(e.visibility), wire(e.origin)))
            .collect::<Vec<_>>()
            .join(" ")
    }

    let channels: BTreeSet<String> = e.sources.iter().map(|s| wire(s.origin)).collect();
    let checked = if e.cross_checked {
        format!(
            "cross-checked ({})",
            channels.into_iter().collect::<Vec<_>>().join(", ")
        )
    } else {
        format!(
            "one channel ({})",
            channels.into_iter().collect::<Vec<_>>().join(", ")
        )
    };
    format!(
        "edge {} {checked} via {} sources {}",
        wire(e.level),
        opt(e.via_tool_use_id.as_deref()),
        sources(&e.sources)
    )
}

/// The facts about one run that the views share: the projection plus what it does not keep.
struct Facts<'a> {
    rec: &'a Recorded,
    p: Projection,
    /// (attempt, agent) -> message chunks counted (not corroborations).
    messages: HashMap<(Uuid, String), usize>,
    /// (attempt, tool use) -> the reason a `tool_failed` gave (`permission_denied`, ...).
    denied: HashMap<(Uuid, String), String>,
    /// attempt -> its parent attempt, for aibus-managed lineage.
    parent_attempt: HashMap<Uuid, Uuid>,
    /// event id -> sequence, to show what a corroboration confirms.
    seq_of: HashMap<Uuid, u64>,
}

impl<'a> Facts<'a> {
    fn of(rec: &'a Recorded) -> Self {
        let p = project(&rec.events);
        let mut messages: HashMap<(Uuid, String), usize> = HashMap::new();
        let mut denied = HashMap::new();
        let mut parent_attempt = HashMap::new();
        let mut seq_of = HashMap::new();
        for e in &rec.events {
            seq_of.entry(e.event_id).or_insert(e.sequence);
            if let Some(pa) = e.parent_attempt_id {
                parent_attempt.entry(e.attempt_id).or_insert(pa);
            }
            if e.corroborates.is_some() {
                continue;
            }
            match e.kind {
                EventKind::MessageChunk => {
                    if let Some(a) = &e.agent_id {
                        *messages.entry((e.attempt_id, a.clone())).or_default() += 1;
                    }
                }
                EventKind::ToolFailed => {
                    if let (Some(t), Some(r)) = (&e.native.tool_use_id, data_str(e, "reason")) {
                        denied
                            .entry((e.attempt_id, t.clone()))
                            .or_insert_with(|| r.to_owned());
                    }
                }
                _ => {}
            }
        }
        Self {
            rec,
            p,
            messages,
            denied,
            parent_attempt,
            seq_of,
        }
    }

    fn message_note(&self, attempt: Uuid, agent: &str) -> String {
        match self.messages.get(&(attempt, agent.to_owned())) {
            Some(n) => format!("messages {n} observed"),
            None => "messages none recorded (unavailable)".into(),
        }
    }

    fn tool_line(&self, t: &crate::ToolNode) -> String {
        let outcome = match t.outcome {
            ToolOutcome::Completed => "completed".to_string(),
            ToolOutcome::Failed => match self.denied.get(&(t.attempt_id, t.tool_use_id.clone())) {
                Some(r) => format!("failed ({})", clean(r, 160)),
                None => "failed".into(),
            },
            ToolOutcome::Unknown => "outcome unknown (never reported)".into(),
        };
        format!(
            "tool {} {}: {outcome}  [{}]",
            clean(&t.tool_use_id, 160),
            opt(t.name.as_deref()),
            levels(t.evidence.iter())
        )
    }
}

/// How a run stands, for the fleet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    /// Waiting on an approval or a login, and no end recorded.
    Blocked,
    /// No end recorded: still running, or stopped without a word.
    Open,
    /// No attempt in the trace at all.
    Empty,
}

/// One row of the fleet: a compact, read-only summary of one run. #290 adds its actions to this.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RunRow {
    /// The run directory's name, cleaned.
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
    /// Delegation edges by level: observed, correlated, inferred, unavailable.
    pub edges: [usize; 4],
    pub gaps: usize,
    /// The attempt-wide estimated cost when one was reported, otherwise an `unknown` metric.
    pub cost: Metric,
    pub corrupt_lines: u64,
    pub torn_bytes: u64,
    pub unsupported_schema: u64,
    /// Why the run could not be read at all.
    pub error: Option<String>,
}

impl RunRow {
    pub fn of(id: &str, rec: &Recorded) -> Self {
        let p = project(&rec.events);
        let ended: Vec<Option<AttemptStatus>> = p.attempts.iter().map(|a| a.status).collect();
        let waiting = {
            let mut asked = 0i64;
            let mut auth = false;
            for e in rec.events.iter().filter(|e| e.corroborates.is_none()) {
                match e.kind {
                    EventKind::ApprovalRequested => asked += 1,
                    EventKind::ApprovalResolved => asked -= 1,
                    EventKind::AuthRequired => auth = true,
                    _ => {}
                }
            }
            asked > 0 || auth
        };
        let state = if ended.is_empty() {
            RunState::Empty
        } else if ended.iter().any(Option::is_none) {
            if waiting {
                RunState::Blocked
            } else {
                RunState::Open
            }
        } else if ended.contains(&Some(AttemptStatus::Interrupted)) {
            RunState::Interrupted
        } else if ended.contains(&Some(AttemptStatus::Cancelled)) {
            RunState::Cancelled
        } else if ended.contains(&Some(AttemptStatus::Failed)) {
            RunState::Failed
        } else {
            RunState::Completed
        };
        let mut edges = [0usize; 4];
        for e in &p.edges {
            edges[e.level as usize] += 1;
        }
        let cost = p
            .attempts
            .iter()
            .flat_map(|a| a.usage.iter())
            .find(|m| m.name == "cost_usd")
            .cloned()
            .unwrap_or(Metric {
                name: "cost_usd".into(),
                value: None,
                unit: "usd".into(),
                quality: DataQuality::Unknown,
                source: "no cost in the trace".into(),
            });
        let seen: Vec<&str> = rec.events.iter().map(|e| e.observed_at.as_str()).collect();
        Self {
            id: clean(id, 160),
            workflow_run_id: rec.events.first().map(|e| e.workflow_run_id),
            provider: rec
                .events
                .iter()
                .find(|e| !e.provider_extension.is_empty())
                .map(|e| clean(&e.provider_extension, 160)),
            state,
            requires_reconciliation: p.attempts.iter().any(|a| a.requires_reconciliation),
            first_seen: seen.iter().min().map(|s| clean(s, 160)),
            last_seen: seen.iter().max().map(|s| clean(s, 160)),
            events: rec.events.len(),
            attempts: p.attempts.len(),
            agents: p.agents.len(),
            tools: p.tools.len(),
            edges,
            gaps: p.gaps.len(),
            cost,
            corrupt_lines: rec.report.corrupt_lines,
            torn_bytes: rec.report.torn_bytes,
            unsupported_schema: rec.report.unsupported_schema,
            error: None,
        }
    }
}

/// A run directory's name is one plain path component.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.starts_with('.')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// The run directories under `root` (each holds an `events.jsonl`), sorted by name.
fn run_dirs(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        // A link to a directory is not a run of this root (`file_type` does not follow it).
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| valid_id(n) && root.join(n).join("events.jsonl").is_file())
        .collect();
    out.sort();
    out
}

/// The directory of run `id` under `root`: the exact name, or the one name it starts.
pub fn resolve(root: &Path, id: &str) -> Result<PathBuf, String> {
    if !valid_id(id) {
        return Err(format!(
            "{}: not a run id (letters, digits, '.', '_' and '-')",
            clean(id, 160)
        ));
    }
    let names = run_dirs(root);
    if names.iter().any(|n| n == id) {
        return Ok(root.join(id));
    }
    let hits: Vec<&String> = names.iter().filter(|n| n.starts_with(id)).collect();
    match hits.as_slice() {
        [one] => Ok(root.join(one)),
        [] => Err(format!("no run {id} under {}", root.display())),
        many => Err(format!(
            "{id} names {} runs: {}",
            many.len(),
            many.iter()
                .take(5)
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Every run under `root`, most recently seen first.
pub fn fleet(root: &Path) -> Vec<RunRow> {
    let mut rows: Vec<RunRow> = run_dirs(root)
        .iter()
        .map(|name| match Recorded::load(&root.join(name)) {
            Ok(rec) => RunRow::of(name, &rec),
            Err(e) => {
                let mut row = RunRow::of(name, &Recorded::from_bytes(&root.join(name), b""));
                row.error = Some(clean(&e.to_string(), 160));
                row
            }
        })
        .collect();
    rows.sort_by(|a, b| b.last_seen.cmp(&a.last_seen).then_with(|| a.id.cmp(&b.id)));
    rows
}

/// The fleet as a table, one run a line.
pub fn render_fleet(rows: &[RunRow]) -> String {
    let mut s = String::new();
    if rows.is_empty() {
        s.push_str("no recorded runs\n");
        return s;
    }
    let _ = writeln!(
        s,
        "{:<28} {:<12} {:>6} {:>6} {:>6} {:<16} {:>5} {:<32} {:<24} QUALITY",
        "RUN", "STATE", "EVENTS", "AGENTS", "TOOLS", "EDGES o/c/i/u", "GAPS", "COST", "LAST SEEN"
    );
    for r in rows {
        let mut state = wire(r.state);
        if r.requires_reconciliation {
            state.push('!');
        }
        let cost = format!(
            "{} {} ({})",
            r.cost
                .value
                .map_or_else(|| "unknown".into(), |v| v.to_string()),
            clean(&r.cost.unit, 160),
            wire(r.cost.quality)
        );
        let quality = match &r.error {
            Some(e) => format!("unreadable: {e}"),
            None if r.corrupt_lines + r.torn_bytes + r.unsupported_schema == 0 => "complete".into(),
            None => format!(
                "{} corrupt lines, {} torn bytes, {} other schema",
                r.corrupt_lines, r.torn_bytes, r.unsupported_schema
            ),
        };
        let _ = writeln!(
            s,
            "{:<28} {:<12} {:>6} {:>6} {:>6} {:<16} {:>5} {:<32} {:<24} {quality}",
            r.id,
            state,
            r.events,
            r.agents,
            r.tools,
            format!(
                "{}/{}/{}/{}",
                r.edges[0], r.edges[1], r.edges[2], r.edges[3]
            ),
            r.gaps,
            cost,
            r.last_seen.as_deref().unwrap_or("-"),
        );
    }
    s.push_str("state! = a tool never reported back; edges by evidence level: observed/correlated/inferred/unavailable\n");
    s
}

/// `aibus runs inspect <run>`: the header, the planned graph, the execution graph and the gaps.
pub fn graph(rec: &Recorded, name: &str) -> String {
    fn header(s: &mut String, rec: &Recorded, name: &str) {
        let _ = writeln!(s, "run {}", clean(name, 160));
        if let Some(e) = rec.events.first() {
            let _ = writeln!(s, "  workflow run {}", e.workflow_run_id);
        }
        let providers: BTreeSet<String> = rec
            .events
            .iter()
            .filter(|e| !e.provider_extension.is_empty())
            .map(|e| clean(&e.provider_extension, 160))
            .collect();
        if !providers.is_empty() {
            let _ = writeln!(
                s,
                "  provider {}",
                providers.into_iter().collect::<Vec<_>>().join(", ")
            );
        }
        let seen: Vec<&str> = rec.events.iter().map(|e| e.observed_at.as_str()).collect();
        let _ = writeln!(
            s,
            "  recorded {} events, observed {} to {} (aibus clock)",
            rec.events.len(),
            seen.iter()
                .min()
                .map_or_else(|| "-".into(), |t| clean(t, 160)),
            seen.iter()
                .max()
                .map_or_else(|| "-".into(), |t| clean(t, 160)),
        );
        let r = &rec.report;
        if r.corrupt_lines + r.torn_bytes + r.unsupported_schema == 0 {
            let _ = writeln!(s, "  data quality: every line read");
        } else {
            let _ = writeln!(
                s,
                "  data quality: {} corrupt lines, {} torn bytes, {} lines of another schema were skipped",
                r.corrupt_lines, r.torn_bytes, r.unsupported_schema
            );
        }
        let _ = writeln!(s, "  levels: observed = a native event, correlated = linked across ids or channels, inferred = a labelled guess, unavailable = a gap");
    }

    /// The planned graph, from `plan.json`, with what the trace shows of each step.
    fn plan_section(s: &mut String, rec: &Recorded) {
        s.push_str("plan (the definition graph: intent, not proof of execution)\n");
        let Some(plan) = &rec.plan else {
            match &rec.plan_error {
                Some(e) => {
                    let _ = writeln!(s, "  unreadable: {}", clean(e, 160));
                }
                None => s.push_str("  not recorded: this run has no plan.json\n"),
            }
            return;
        };
        let text = |v: &Value, k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .map_or_else(|| "-".into(), |t| clean(t, 160))
        };
        if let Some(h) = plan.get("plan_hash").and_then(Value::as_str) {
            let _ = writeln!(s, "  plan_hash {}", clean(h, 160));
        }
        let scheduled: HashMap<String, u64> = rec
            .events
            .iter()
            .filter(|e| e.kind == EventKind::StepScheduled && e.corroborates.is_none())
            .filter_map(|e| Some((data_str(e, "step")?.to_owned(), e.sequence)))
            .collect();
        let steps = plan
            .get("steps")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        if steps.is_empty() {
            s.push_str("  no steps in plan.json\n");
        }
        if steps.len() > 500 {
            let _ = writeln!(s, "  {} steps; the first 500 follow", steps.len());
        }
        for st in steps.iter().take(500) {
            let key = st.get("key").and_then(Value::as_str).unwrap_or("-");
            let seen = scheduled
                .get(key)
                .map_or_else(|| "not in the trace".into(), |q| format!("scheduled #{q}"));
            let _ = writeln!(
                s,
                "  step {} agent {} {}/{} prompt {}  {seen}",
                clean(key, 160),
                text(st, "agent"),
                text(st, "mode"),
                text(st, "runtime"),
                st.get("prompt")
                    .map_or_else(|| "-".into(), |p| text(p, "sha256")),
            );
        }
        let edges = plan
            .get("edges")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        if edges.len() > 1000 {
            let _ = writeln!(s, "  {} edges; the first 1000 follow", edges.len());
        }
        for e in edges.iter().take(1000) {
            let when = e
                .get("when")
                .and_then(Value::as_str)
                .map(|w| format!(" when {}", clean(w, 160)))
                .unwrap_or_default();
            let _ = writeln!(
                s,
                "  {} {} -> {}{when}",
                text(e, "kind"),
                text(e, "from"),
                text(e, "to")
            );
        }
    }

    /// The execution graph: per attempt, the lead and every child it delegated to, with the edge's
    /// level and sources, the tools each agent ran, and whatever cannot be placed.
    fn execution_section(s: &mut String, x: &Facts) {
        fn indent(depth: usize) -> String {
            /// Indentation stops growing past this depth; deeper nodes say their depth instead.
            const MAX_INDENT: usize = 16;
            if depth > MAX_INDENT {
                format!("{}[depth {depth}] ", "| ".repeat(MAX_INDENT))
            } else {
                "| ".repeat(depth)
            }
        }

        s.push_str("execution (what the trace shows)\n");
        if x.p.attempts.is_empty() && x.p.agents.is_empty() && x.p.tools.is_empty() {
            s.push_str("  no attempt recorded\n");
        }
        // Attempts in the order the projection met them, then any attempt that only agents or tools
        // name (its start and end were never recorded).
        let mut ids: Vec<Uuid> = x.p.attempts.iter().map(|a| a.attempt_id).collect();
        let mut have: HashSet<Uuid> = ids.iter().copied().collect();
        for id in
            x.p.agents
                .iter()
                .map(|n| n.attempt_id)
                .chain(x.p.tools.iter().map(|t| t.attempt_id))
        {
            if have.insert(id) {
                ids.push(id);
            }
        }
        // Index once by attempt, so a trace with many attempts costs one pass, not one per attempt.
        let mut attempt_of: HashMap<Uuid, &crate::AttemptView> = HashMap::new();
        for a in &x.p.attempts {
            attempt_of.entry(a.attempt_id).or_insert(a);
        }
        let mut agents_of: HashMap<Uuid, Vec<&crate::AgentNode>> = HashMap::new();
        for n in &x.p.agents {
            agents_of.entry(n.attempt_id).or_default().push(n);
        }
        let mut edges_of: HashMap<Uuid, Vec<&Edge>> = HashMap::new();
        for e in &x.p.edges {
            edges_of.entry(e.attempt_id).or_default().push(e);
        }
        let mut tools_of: HashMap<Uuid, Vec<&crate::ToolNode>> = HashMap::new();
        for t in &x.p.tools {
            tools_of.entry(t.attempt_id).or_default().push(t);
        }
        for attempt in ids {
            match attempt_of.get(&attempt).copied() {
                Some(a) => {
                    let status = a.status.map_or_else(|| "no end recorded".into(), wire);
                    let reconcile = if a.requires_reconciliation {
                        ", needs reconciliation"
                    } else {
                        ""
                    };
                    let _ = writeln!(
                        s,
                        "attempt {attempt} {status}{reconcile}  [{}]",
                        levels(a.evidence.iter())
                    );
                    if a.usage.is_empty() {
                        s.push_str("  usage: not reported (unknown)\n");
                    }
                    for m in &a.usage {
                        let _ = writeln!(s, "  usage {}", metric_line(m));
                    }
                }
                None => {
                    let _ = writeln!(
                        s,
                        "attempt {attempt} start and end not recorded  [no events]"
                    );
                    s.push_str("  usage: not reported (unknown)\n");
                }
            }
            match x.parent_attempt.get(&attempt) {
                Some(pa) => {
                    let _ = writeln!(
                        s,
                        "  lineage: aibus-managed child of attempt {pa} (linked by the scheduler)"
                    );
                }
                None => s.push_str(
                    "  lineage: provider-native, as the CLI's stream and hooks show it\n",
                ),
            }
            let agents: &[&crate::AgentNode] = agents_of.get(&attempt).map_or(&[], Vec::as_slice);
            let edges: &[&Edge] = edges_of.get(&attempt).map_or(&[], Vec::as_slice);
            let known: HashSet<&str> = agents.iter().map(|n| n.agent_id.as_str()).collect();
            let mut children: HashMap<&str, Vec<&Edge>> = HashMap::new();
            let mut edge_of: HashMap<&str, &Edge> = HashMap::new();
            for e in edges {
                edge_of.entry(e.child.as_str()).or_insert(e);
                if let Some(p) = e
                    .parent
                    .as_deref()
                    .filter(|p| known.contains(p) && *p != e.child)
                {
                    children.entry(p).or_default().push(e);
                }
            }
            let mut tools: HashMap<&str, Vec<&crate::ToolNode>> = HashMap::new();
            let mut loose_tools = Vec::new();
            for t in tools_of.get(&attempt).map_or(&[][..], Vec::as_slice) {
                match t.agent_id.as_deref().filter(|id| known.contains(id)) {
                    Some(id) => tools.entry(id).or_default().push(*t),
                    None => loose_tools.push(*t),
                }
            }
            // Roots: agents no known parent delegated to.
            let roots: Vec<&str> = agents
                .iter()
                .map(|n| n.agent_id.as_str())
                .filter(|id| {
                    edge_of
                        .get(id)
                        .and_then(|e| e.parent.as_deref())
                        .is_none_or(|p| !known.contains(p) || p == *id)
                })
                .collect();
            let node: HashMap<&str, &&crate::AgentNode> =
                agents.iter().map(|n| (n.agent_id.as_str(), n)).collect();
            let mut seen: HashSet<&str> = HashSet::new();
            let mut stack: Vec<(&str, usize)> = roots.iter().rev().map(|r| (*r, 0)).collect();
            // After the roots, whatever is left sits on a cycle of parent links: each such component
            // starts from its first agent in trace order, under its own heading.
            let mut next_left = 0;
            let mut cycle_heading = false;
            loop {
                let Some((id, depth)) = stack.pop() else {
                    while next_left < agents.len()
                        && seen.contains(agents[next_left].agent_id.as_str())
                    {
                        next_left += 1;
                    }
                    let Some(n) = agents.get(next_left) else {
                        break;
                    };
                    if !cycle_heading {
                        cycle_heading = true;
                        s.push_str(
                            "  not reachable from a root (the parent links form a cycle):\n",
                        );
                    }
                    stack.push((n.agent_id.as_str(), 1));
                    continue;
                };
                if !seen.insert(id) {
                    continue;
                }
                let Some(n) = node.get(id) else { continue };
                let pad = indent(depth);
                let role = n
                    .role
                    .as_deref()
                    .map_or_else(|| "role unknown".into(), |t| clean(t, 160));
                let _ = writeln!(
                    s,
                    "  {pad}agent {} {role}, {}  {}  [{}]",
                    clean(id, 160),
                    n.status
                        .as_deref()
                        .map_or_else(|| "no stop recorded".into(), |t| clean(t, 160)),
                    x.message_note(attempt, id),
                    levels(n.evidence.iter())
                );
                if let Some(e) = edge_of.get(id) {
                    match e.parent.as_deref() {
                        Some(p) if known.contains(p) && p != id => {
                            let _ = writeln!(s, "  {pad}  {}", edge_line(e));
                        }
                        Some(p) if p == id => {
                            let _ = writeln!(
                                s,
                                "  {pad}  parent link points at itself; {}",
                                edge_line(e)
                            );
                        }
                        Some(p) => {
                            let _ = writeln!(
                                s,
                                "  {pad}  parent {} is not in the trace; {}",
                                clean(p, 160),
                                edge_line(e)
                            );
                        }
                        None => {
                            let _ = writeln!(
                                s,
                                "  {pad}  parent unavailable: nothing in the trace says who delegated; {}",
                                edge_line(e)
                            );
                        }
                    }
                }
                for t in tools.get(id).map_or(&[][..], Vec::as_slice) {
                    let _ = writeln!(s, "  {pad}| {}", x.tool_line(t));
                }
                if let Some(kids) = children.get(id) {
                    for e in kids.iter().rev() {
                        stack.push((e.child.as_str(), depth + 1));
                    }
                }
            }
            if !loose_tools.is_empty() {
                s.push_str("  tools with no known agent:\n");
                for t in loose_tools {
                    let _ = writeln!(s, "  | {}", x.tool_line(t));
                }
            }
        }
    }

    fn gaps_section(s: &mut String, p: &Projection) {
        s.push_str("gaps (what the trace does not show)\n");
        if p.gaps.is_empty() {
            s.push_str("  none recorded\n");
        }
        for g in &p.gaps {
            let _ = writeln!(
                s,
                "  #{} {} (unavailable) attempt {}",
                g.sequence,
                clean(&g.reason, 160),
                g.attempt_id
            );
        }
    }

    let x = Facts::of(rec);
    let mut s = String::new();
    header(&mut s, rec, name);
    plan_section(&mut s, rec);
    execution_section(&mut s, &x);
    gaps_section(&mut s, &x.p);
    s
}

/// Which timeline rows to show. Each field that is set has to match.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    pub agent: Option<String>,
    pub tool: Option<String>,
    pub kind: Option<EventKind>,
    pub level: Option<Visibility>,
}

/// A short note of what one event says, from its metadata.
fn note(e: &TraceEvent) -> String {
    let mut parts = Vec::new();
    for k in [
        "tool",
        "role",
        "subagent_type",
        "status",
        "reason",
        "reason_code",
        "scope",
    ] {
        if let Some(v) = data_str(e, k) {
            parts.push(format!("{k}={}", clean(v, 160)));
        }
    }
    parts.join(" ")
}

/// The events in sequence order, one a line, with their level and origin.
pub fn timeline(rec: &Recorded, filter: &Filter) -> String {
    let mut order: Vec<&TraceEvent> = rec.events.iter().collect();
    order.sort_by_key(|e| (e.sequence, e.event_id));
    let mut s = String::from(
        "timeline (sequence, observed by aibus, reported by the vendor, kind, level, origin)\n",
    );
    let mut shown = 0usize;
    for e in order {
        if filter
            .agent
            .as_deref()
            .is_some_and(|a| e.agent_id.as_deref() != Some(a))
            || filter
                .tool
                .as_deref()
                .is_some_and(|t| e.native.tool_use_id.as_deref() != Some(t))
            || filter.kind.is_some_and(|k| e.kind != k)
            || filter.level.is_some_and(|l| e.visibility != l)
        {
            continue;
        }
        shown += 1;
        let corr = if e.corroborates.is_some() {
            " corroborates"
        } else {
            ""
        };
        let tool = e
            .native
            .tool_use_id
            .as_deref()
            .map(|t| format!(" tool_use={}", clean(t, 160)))
            .unwrap_or_default();
        let n = note(e);
        let _ = writeln!(
            s,
            "#{} {} {} {} {} {} agent={}{tool}{}{corr}",
            e.sequence,
            clean(&e.observed_at, 160),
            opt(e.occurred_at.as_deref()),
            wire(e.kind),
            wire(e.visibility),
            wire(e.origin),
            opt(e.agent_id.as_deref()),
            if n.is_empty() {
                String::new()
            } else {
                format!(" {n}")
            },
        );
    }
    if shown == 0 {
        s.push_str("  no events match\n");
    }
    s
}

fn push_payload(s: &mut String, rec: &Recorded, payload_ref: &str, pad: &str) {
    /// The most of one payload the inspector shows.
    const MAX_PAYLOAD: usize = 4096;
    /// `local://objects/<64 hex>.json` and nothing else names a payload.
    fn payload_path(rec: &Recorded, payload_ref: &str) -> Option<PathBuf> {
        let name = payload_ref
            .strip_prefix("local://objects/")?
            .strip_suffix(".json")?;
        (name.len() == 64
            && name
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        .then(|| rec.dir.join("objects").join(format!("{name}.json")))
    }

    let Some(path) = payload_path(rec, payload_ref) else {
        let _ = writeln!(
            s,
            "{pad}payload {} is not a local object reference; not opened",
            clean(payload_ref, 160)
        );
        return;
    };
    // The `objects` directory itself must not be a link out of the run.
    let in_run = fs::symlink_metadata(rec.dir.join("objects")).is_ok_and(|m| m.is_dir());
    let body = (if in_run {
        open_regular(&path)
    } else {
        Err(io::Error::other("objects is not a directory of the run"))
    })
    .and_then(|file| {
        let mut b = Vec::new();
        file.take(1 << 20).read_to_end(&mut b).map(|_| b)
    });
    match body {
        Err(_) => {
            let _ = writeln!(
                s,
                "{pad}payload {} is not in the run directory (unavailable)",
                clean(payload_ref, 160)
            );
        }
        Ok(b) => {
            let text = serde_json::from_slice::<Value>(&b)
                .ok()
                .and_then(|v| serde_json::to_string_pretty(&v).ok())
                .unwrap_or_else(|| String::from_utf8_lossy(&b).into_owned());
            let _ = writeln!(
                s,
                "{pad}payload {} (redacted when it was stored):",
                clean(payload_ref, 160)
            );
            let mut used = 0;
            for line in text.lines() {
                if used > MAX_PAYLOAD {
                    let _ = writeln!(s, "{pad}  ... cut at {MAX_PAYLOAD} bytes");
                    break;
                }
                used += line.len() + 1;
                let _ = writeln!(s, "{pad}  {}", clean(line, 400));
            }
        }
    }
}

fn evidence_row(s: &mut String, x: &Facts, e: &TraceEvent) {
    let corr = e
        .corroborates
        .map(|c| match x.seq_of.get(&c) {
            Some(q) => format!(" confirms #{q}"),
            None => " confirms an event not in the trace".into(),
        })
        .unwrap_or_default();
    let _ = writeln!(
        s,
        "  #{} {} {} {} native {} parser {} raw {} payload {}{corr}",
        e.sequence,
        wire(e.kind),
        wire(e.visibility),
        wire(e.origin),
        opt(e.native.native_type.as_deref()),
        clean(&e.source.parser, 160),
        e.source
            .raw_sha256
            .as_deref()
            .map_or_else(|| "-".into(), |h| format!("sha256:{}", clean(h, 160))),
        opt(e.payload_ref.as_deref()),
    );
}

/// The payloads of `events`, or why there are none.
fn payloads(s: &mut String, x: &Facts, events: &[&TraceEvent], what: &str) {
    let refs: BTreeSet<&str> = events
        .iter()
        .filter_map(|e| e.payload_ref.as_deref())
        .collect();
    if refs.is_empty() {
        let _ = writeln!(
            s,
            "  {what}: text not retained (a metadata-only trace; payloads are kept only when asked for)"
        );
        return;
    }
    let _ = writeln!(s, "  {what}:");
    for r in refs.iter().take(6) {
        push_payload(s, x.rec, r, "    ");
    }
    if refs.len() > 6 {
        let _ = writeln!(
            s,
            "    ... {} more payloads, open them with --event",
            refs.len() - 6
        );
    }
}

fn elapsed(events: &[&TraceEvent]) -> String {
    /// Milliseconds since the epoch of `YYYY-MM-DDTHH:MM:SS[.mmm]Z`, the form aibus writes.
    fn millis(ts: &str) -> Option<i64> {
        let b = ts.as_bytes();
        if b.len() < 20 || !b.is_ascii() || b[4] != b'-' || b[10] != b'T' || !ts.ends_with('Z') {
            return None;
        }
        let num = |r: std::ops::Range<usize>| ts.get(r)?.parse::<i64>().ok();
        let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
        let (hh, mm, ss) = (num(11..13)?, num(14..16)?, num(17..19)?);
        let ms = if b.get(19) == Some(&b'.') {
            num(20..23).unwrap_or(0)
        } else {
            0
        };
        if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 60 {
            return None;
        }
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let mp = (m + 9) % 12;
        let doy = (153 * mp + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe - 719_468;
        Some(((days * 24 + hh) * 60 + mm) * 60_000 + ss * 1000 + ms)
    }

    let ms: Vec<i64> = events
        .iter()
        .filter_map(|e| millis(&e.observed_at))
        .collect();
    match (ms.iter().min(), ms.iter().max()) {
        (Some(a), Some(b)) => format!(
            "{:.3} s (derived from aibus's clock, first to last event)",
            (b - a) as f64 / 1000.0
        ),
        _ => "unknown".into(),
    }
}

/// The inspector for one agent, tool use or attempt: who it is, how it is linked, what it did,
/// its usage with sources, its gaps, and every event behind it. `None` when nothing has that id.
pub fn inspect(rec: &Recorded, node: &str) -> Option<String> {
    let x = Facts::of(rec);
    let mut s = String::new();
    for a in x.p.agents.iter().filter(|a| a.agent_id == node) {
        let mine: Vec<&TraceEvent> = rec
            .events
            .iter()
            .filter(|e| e.attempt_id == a.attempt_id && e.agent_id.as_deref() == Some(node))
            .collect();
        let _ = writeln!(s, "agent {}", clean(node, 160));
        let _ = writeln!(s, "  attempt {}", a.attempt_id);
        let _ = writeln!(s, "  role {}", opt(a.role.as_deref()));
        let _ = writeln!(
            s,
            "  status {}",
            a.status
                .as_deref()
                .map_or_else(|| "no stop recorded".into(), |t| clean(t, 160))
        );
        match x
            .p
            .edges
            .iter()
            .find(|e| e.attempt_id == a.attempt_id && e.child == node)
        {
            Some(e) => {
                let _ = writeln!(s, "  parent {}", opt(e.parent.as_deref()));
                let _ = writeln!(s, "  {}", edge_line(e));
            }
            None if a.role.as_deref() == Some("lead") => {
                s.push_str("  parent none: the lead is the process aibus started\n")
            }
            None => s.push_str("  parent unavailable\n"),
        }
        let kids: Vec<String> =
            x.p.edges
                .iter()
                .filter(|e| e.attempt_id == a.attempt_id && e.parent.as_deref() == Some(node))
                .map(|e| format!("{} ({})", clean(&e.child, 160), wire(e.level)))
                .collect();
        if !kids.is_empty() {
            let _ = writeln!(s, "  delegated to {}", kids.join(", "));
        }
        let _ = writeln!(s, "  elapsed {}", elapsed(&mine));
        if let Some(init) = mine
            .iter()
            .find(|e| e.kind == EventKind::AgentStarted && e.corroborates.is_none())
        {
            for k in ["model", "cli_version", "permission_mode"] {
                if let Some(v) = data_str(init, k) {
                    let _ = writeln!(s, "  {k} {} (reported)", clean(v, 160));
                }
            }
        }
        s.push_str("  prompt: not in the trace (a plan keeps prompt hashes; text only under payload retention)\n");
        let _ = writeln!(s, "  {}", x.message_note(a.attempt_id, node));
        let chunks: Vec<&TraceEvent> = mine
            .iter()
            .copied()
            .filter(|e| e.kind == EventKind::MessageChunk)
            .collect();
        if !chunks.is_empty() {
            payloads(&mut s, &x, &chunks, "messages");
        }
        let usage: Vec<&TraceEvent> = mine
            .iter()
            .copied()
            .filter(|e| {
                e.kind == EventKind::UsageReported
                    && e.corroborates.is_none()
                    && e.data["scope"] == "agent"
            })
            .collect();
        if usage.is_empty() {
            s.push_str("  usage: not reported for this agent (unknown)\n");
        }
        for u in usage {
            let ms: Vec<Metric> =
                serde_json::from_value(u.data["metrics"].clone()).unwrap_or_default();
            for m in ms {
                let _ = writeln!(s, "  usage {}", metric_line(&m));
            }
        }
        let tools: Vec<String> =
            x.p.tools
                .iter()
                .filter(|t| t.attempt_id == a.attempt_id && t.agent_id.as_deref() == Some(node))
                .map(|t| x.tool_line(t))
                .collect();
        if tools.is_empty() {
            s.push_str("  tools: none recorded\n");
        }
        for t in tools {
            let _ = writeln!(s, "  {t}");
        }
        permission_and_gaps(&mut s, &mine);
        s.push_str("  evidence (open one with --event <sequence>):\n");
        for e in &mine {
            evidence_row(&mut s, &x, e);
        }
    }
    for t in x.p.tools.iter().filter(|t| t.tool_use_id == node) {
        let mine: Vec<&TraceEvent> = rec
            .events
            .iter()
            .filter(|e| {
                e.attempt_id == t.attempt_id && e.native.tool_use_id.as_deref() == Some(node)
            })
            .collect();
        let _ = writeln!(s, "{}", x.tool_line(t));
        let _ = writeln!(s, "  attempt {}", t.attempt_id);
        let _ = writeln!(s, "  agent {}", opt(t.agent_id.as_deref()));
        let _ = writeln!(s, "  elapsed {}", elapsed(&mine));
        payloads(&mut s, &x, &mine, "input and output");
        permission_and_gaps(&mut s, &mine);
        s.push_str("  evidence (open one with --event <sequence>):\n");
        for e in &mine {
            evidence_row(&mut s, &x, e);
        }
    }
    // A delegation: the tool use that started a child.
    for e in
        x.p.edges
            .iter()
            .filter(|e| e.via_tool_use_id.as_deref() == Some(node))
    {
        let _ = writeln!(
            s,
            "delegation {} -> {}",
            opt(e.parent.as_deref()),
            clean(&e.child, 160)
        );
        let _ = writeln!(s, "  {}", edge_line(e));
    }
    for a in
        x.p.attempts
            .iter()
            .filter(|a| a.attempt_id.to_string() == node)
    {
        let mine: Vec<&TraceEvent> = rec
            .events
            .iter()
            .filter(|e| e.attempt_id == a.attempt_id)
            .collect();
        let _ = writeln!(s, "attempt {}", a.attempt_id);
        let _ = writeln!(
            s,
            "  status {}",
            a.status.map_or_else(|| "no end recorded".into(), wire)
        );
        let _ = writeln!(s, "  requires reconciliation {}", a.requires_reconciliation);
        let _ = writeln!(s, "  elapsed {}", elapsed(&mine));
        for m in &a.usage {
            let _ = writeln!(s, "  usage {}", metric_line(m));
        }
        let _ = writeln!(
            s,
            "  {} events, {} agents",
            mine.len(),
            x.p.agents
                .iter()
                .filter(|n| n.attempt_id == a.attempt_id)
                .count()
        );
        permission_and_gaps(&mut s, &mine);
        s.push_str("  evidence (open one with --event <sequence>):\n");
        for e in mine.iter().filter(|e| {
            matches!(
                e.kind,
                EventKind::AttemptStarted | EventKind::AttemptCompleted | EventKind::AttemptFailed
            )
        }) {
            evidence_row(&mut s, &x, e);
        }
    }
    (!s.is_empty()).then_some(s)
}

fn permission_and_gaps(s: &mut String, events: &[&TraceEvent]) {
    let perms: Vec<&&TraceEvent> = events
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                EventKind::ApprovalRequested
                    | EventKind::ApprovalResolved
                    | EventKind::AuthRequired
            ) || data_str(e, "reason") == Some("permission_denied")
        })
        .collect();
    if perms.is_empty() {
        s.push_str("  permission events: none recorded\n");
    }
    for e in perms {
        let _ = writeln!(
            s,
            "  permission #{} {} {}",
            e.sequence,
            wire(e.kind),
            note(e)
        );
    }
    for e in events.iter().filter(|e| e.kind == EventKind::GapDetected) {
        let _ = writeln!(
            s,
            "  gap #{} {} (unavailable)",
            e.sequence,
            opt(data_str(e, "reason"))
        );
    }
}

/// One event in full (`--event <sequence>`), with the payload it points at when it was kept.
pub fn event(rec: &Recorded, sequence: u64) -> Option<String> {
    let x = Facts::of(rec);
    let mut s = String::new();
    for (e, raw) in rec
        .events
        .iter()
        .zip(&rec.raw)
        .filter(|(e, _)| e.sequence == sequence)
    {
        let _ = writeln!(
            s,
            "event #{} ({} bytes in events.jsonl)",
            e.sequence,
            raw.len()
        );
        evidence_row(&mut s, &x, e);
        let pretty = serde_json::to_string_pretty(e).unwrap_or_default();
        for line in pretty.lines() {
            let _ = writeln!(s, "  {}", clean(line, 400));
        }
        if let Some(r) = &e.payload_ref {
            push_payload(&mut s, rec, r, "  ");
        } else {
            s.push_str("  payload: not retained\n");
        }
    }
    (!s.is_empty()).then_some(s)
}

/// The offline replay (spec 7.3): every recorded state transition in sequence order, then the
/// graph they add up to. It reads the trace and nothing else; it re-executes nothing and makes
/// no model call, so it shows what was recorded and not what a model would do again.
pub fn replay(rec: &Recorded, name: &str) -> String {
    /// What one event changes, in words.
    fn transition(e: &TraceEvent, x: &Facts) -> String {
        let agent = opt(e.agent_id.as_deref());
        let tool = opt(e.native.tool_use_id.as_deref());
        if let Some(c) = e.corroborates {
            let what = x
                .seq_of
                .get(&c)
                .map_or_else(|| "an event not in the trace".into(), |q| format!("#{q}"));
            return format!("{} confirms {what}; no change", wire(e.kind));
        }
        match e.kind {
            EventKind::AttemptStarted => format!("attempt {} started", e.attempt_id),
            EventKind::AttemptCompleted | EventKind::AttemptFailed => {
                format!(
                    "attempt {} ended {}",
                    e.attempt_id,
                    opt(data_str(e, "status"))
                )
            }
            EventKind::AgentStarted => match (
                e.parent_agent_id.as_deref(),
                e.parent_tool_use_id.as_deref(),
            ) {
                (None, None) => format!(
                    "agent {agent} started as {}",
                    opt(data_str(e, "role").or(data_str(e, "subagent_type")))
                ),
                (p, via) => format!(
                    "agent {agent} started, child of {} via {}",
                    opt(p),
                    opt(via)
                ),
            },
            EventKind::AgentStopped => {
                format!("agent {agent} stopped: {}", opt(data_str(e, "status")))
            }
            EventKind::TaskDelegated => {
                format!(
                    "agent {agent} delegates via {tool} ({})",
                    opt(data_str(e, "subagent_type"))
                )
            }
            EventKind::ToolStarted => format!(
                "agent {agent} starts tool {tool} {}",
                opt(data_str(e, "tool"))
            ),
            EventKind::ToolCompleted => format!("tool {tool} completed"),
            EventKind::ToolFailed => format!("tool {tool} failed: {}", opt(data_str(e, "reason"))),
            EventKind::MessageChunk => format!("agent {agent} message chunk"),
            EventKind::UsageReported => format!("usage reported ({})", opt(data_str(e, "scope"))),
            EventKind::GapDetected => format!("gap: {}", opt(data_str(e, "reason"))),
            EventKind::TraceWarning => format!("warning: {}", opt(data_str(e, "reason"))),
            k => format!("{} {}", wire(k), note(e)),
        }
    }

    let x = Facts::of(rec);
    let mut order: Vec<&TraceEvent> = rec.events.iter().collect();
    order.sort_by_key(|e| (e.sequence, e.event_id));
    let mut s = String::new();
    let _ = writeln!(
        s,
        "replay of {}: {} recorded events, offline (no provider, no process; the recorded run, not a new one)",
        clean(name, 160),
        order.len()
    );
    for e in &order {
        let _ = writeln!(
            s,
            "#{} {} {}: {}",
            e.sequence,
            wire(e.visibility),
            wire(e.origin),
            transition(e, &x)
        );
    }
    s.push_str("final state\n");
    s.push_str(&graph(rec, name));
    s
}
