//! The advisor's side of the engine (01 section 7.1): which events are checkpoints, and the
//! packet the advisor reads when one fires.
//!
//! `Triggers` is a plain state machine over the lead's events, `Ledger` is what every run of the
//! session writes its compacted transcript into, and `Ledger::packet` turns that into the text
//! the advisor sees. The call itself is made by the lead's run task (`run/assist.rs`).

use std::sync::{Mutex, MutexGuard, PoisonError};

use aibus_provider::tokens::CHARS_PER_TOKEN;
use aibus_provider::{ContentBlock, Message, MessageRole, ToolCallId};
use regex::Regex;
use smol_str::SmolStr;

use crate::agent::{CheckpointSpec, Role};
use crate::ids::{AgentName, RunId};
use crate::msg::Checkpoint;
use crate::run::RunStatus;

/// How much of a tool result, a worker's message or a worker's prompt the packet keeps.
const RESULT_HEAD: usize = 400;
/// How much of a tool call's arguments the packet keeps.
const ARGS_HEAD: usize = 160;
/// Plan headings are `# Plan`, `## plan:` and so on, at the start of a line.
const PLAN_HEADING: &str = r"(?im)^#+ *plan\b";

/// `text` cut to at most `max` characters, with an ellipsis when it was cut.
fn head(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}...", &text[..at]),
        None => text.to_owned(),
    }
}

/// An error message with what varies between two occurrences of the same failure taken out:
/// whitespace collapsed, anything path-shaped masked, then digits masked.
pub fn normalise_error(text: &str) -> String {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|w| {
            let core = w.trim_matches(|c: char| "\"'`,;:()[]".contains(c));
            if core.contains('/') || core.contains('\\') {
                return "<path>".to_owned();
            }
            let mut out = String::with_capacity(w.len());
            let mut digits = false;
            for c in w.chars() {
                if c.is_ascii_digit() {
                    if !std::mem::replace(&mut digits, true) {
                        out.push('#');
                    }
                } else {
                    digits = false;
                    out.push(c);
                }
            }
            out
        })
        .collect();
    words.join(" ")
}

/// Evaluates the checkpoint triggers on the lead's events. One per lead run.
pub struct Triggers {
    plan: Option<Regex>,
    same_error: bool,
    done: bool,
    custom: Vec<(SmolStr, Regex)>,
    /// The tool and normalised text of the last failed result, while the results since then have
    /// all been failures of one kind.
    last_error: Option<(SmolStr, String)>,
    /// Completed workers when `before_done` last fired: it fires again only for new work.
    done_at: usize,
}

impl Triggers {
    /// `specs` are the advisor definition's `checkpoints`, `enabled` the built-in names
    /// `[advisor] checkpoints` leaves on. A spec with an `on` and a `matcher` is a custom one.
    pub fn new(specs: &[CheckpointSpec], enabled: &[String]) -> Self {
        let on =
            |name: &str| specs.iter().any(|s| s.name == name) && enabled.iter().any(|e| e == name);
        let custom = specs
            .iter()
            .filter(|s| s.on.as_deref() == Some("tool_result"))
            .filter_map(|s| Some((s.name.clone(), Regex::new(s.matcher.as_deref()?).ok()?)))
            .collect();
        Self {
            plan: on("before_plan").then(|| Regex::new(PLAN_HEADING).expect("a fixed pattern")),
            same_error: on("same_error_x2"),
            done: on("before_done"),
            custom,
            last_error: None,
            done_at: 0,
        }
    }

    /// `before_plan`: the lead called `plan` or `enter_plan_mode`, or its text opens a plan
    /// heading.
    pub fn on_assistant(&mut self, msg: &Message) -> Option<(Checkpoint, String)> {
        let plan = self.plan.as_ref()?;
        for b in &msg.blocks {
            match b {
                ContentBlock::ToolUse { name, .. }
                    if name == "plan" || name == "enter_plan_mode" =>
                {
                    return Some((Checkpoint::BeforePlan, format!("the lead called {name}")));
                }
                ContentBlock::Text(t) if plan.is_match(t) => {
                    return Some((
                        Checkpoint::BeforePlan,
                        "the lead's text opens a plan".into(),
                    ));
                }
                _ => {}
            }
        }
        None
    }

    /// `same_error_x2` and the custom ones, for one tool result. A pair fires once: the result
    /// that completes it is not the start of the next pair.
    pub fn on_result(
        &mut self,
        tool: &str,
        content: &str,
        is_error: bool,
    ) -> Vec<(Checkpoint, String)> {
        let mut hit = Vec::new();
        if self.same_error {
            if is_error {
                let now = (SmolStr::new(tool), normalise_error(content));
                if self.last_error.take().as_ref() == Some(&now) {
                    let detail = format!("{tool} failed twice with: {}", head(content, 120));
                    hit.push((Checkpoint::SameErrorX2, detail));
                } else {
                    self.last_error = Some(now);
                }
            } else {
                self.last_error = None;
            }
        }
        for (name, matcher) in &self.custom {
            if matcher.is_match(tool) || matcher.is_match(content) {
                hit.push((
                    Checkpoint::Custom(name.clone()),
                    format!("{tool} matched {name}"),
                ));
            }
        }
        hit
    }

    /// `before_done`: the lead's turn ended with no tool call and a worker has completed since
    /// the last time this fired.
    pub fn on_idle(&mut self, workers_done: usize) -> Option<(Checkpoint, String)> {
        if !self.done || workers_done == 0 || workers_done <= self.done_at {
            return None;
        }
        self.done_at = workers_done;
        Some((
            Checkpoint::BeforeDone,
            format!("{workers_done} worker run(s) completed"),
        ))
    }
}

enum Kind {
    User(String),
    Text(String),
    Call {
        id: ToolCallId,
        name: SmolStr,
        args: String,
        result: Option<(String, bool)>,
    },
}

struct Entry {
    seq: u64,
    kind: Kind,
}

struct RunLog {
    run: RunId,
    agent: AgentName,
    role: Role,
    status: Option<RunStatus>,
    entries: Vec<Entry>,
    /// Messages of the run's conversation already taken in.
    synced: usize,
}

#[derive(Default)]
struct Inner {
    runs: Vec<RunLog>,
    seq: u64,
}

impl RunLog {
    fn push(&mut self, seq: &mut u64, kind: Kind) {
        *seq += 1;
        self.entries.push(Entry { seq: *seq, kind });
    }

    /// Takes in one message of the conversation. The lead's text is kept whole, everyone
    /// else's is cut to a head.
    fn take(&mut self, seq: &mut u64, msg: &Message) {
        let lead = self.role == Role::Lead;
        let cut = |t: String| if lead { t } else { head(&t, RESULT_HEAD) };
        match msg.role {
            MessageRole::System => {}
            MessageRole::Assistant => {
                let text: String = msg
                    .blocks
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text(t) => Some(t.as_str()),
                        _ => None,
                    })
                    .collect();
                if !text.is_empty() {
                    let text = cut(text);
                    self.push(seq, Kind::Text(text));
                }
                for b in &msg.blocks {
                    if let ContentBlock::ToolUse {
                        id, name, input, ..
                    } = b
                    {
                        let args = head(&input.to_string(), ARGS_HEAD);
                        let call = Kind::Call {
                            id: *id,
                            name: name.clone(),
                            args,
                            result: None,
                        };
                        self.push(seq, call);
                    }
                }
            }
            MessageRole::User => {
                for b in &msg.blocks {
                    match b {
                        ContentBlock::Text(t) => {
                            let text = cut(t.clone());
                            self.push(seq, Kind::User(text));
                        }
                        ContentBlock::ToolResult {
                            id,
                            content,
                            is_error,
                        } => {
                            let found =
                                self.entries
                                    .iter_mut()
                                    .rev()
                                    .find_map(|e| match &mut e.kind {
                                        Kind::Call {
                                            id: got, result, ..
                                        } if got == id => Some(result),
                                        _ => None,
                                    });
                            if let Some(result) = found {
                                *result = Some((head(content, RESULT_HEAD), *is_error));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

/// Every run's transcript, compacted as it happens: user and assistant text, and for each tool
/// call its name, an arguments summary and the head of its result.
#[derive(Default)]
pub struct Ledger(Mutex<Inner>);

/// What the advisor reads.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Packet {
    pub text: String,
    pub tokens: u64,
}

fn tokens_of(chars: usize) -> u64 {
    (chars as f64 / CHARS_PER_TOKEN * 1.1).ceil() as u64
}

impl Ledger {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A run starts with `task` (its first message). Messages from `synced` on are taken in by
    /// `sync`.
    pub fn begin(&self, run: RunId, agent: &AgentName, role: Role, task: &str, synced: usize) {
        let mut g = self.lock();
        let mut log = RunLog {
            run,
            agent: agent.clone(),
            role,
            status: None,
            entries: Vec::new(),
            synced,
        };
        let text = if role == Role::Lead {
            task.to_owned()
        } else {
            head(task, RESULT_HEAD)
        };
        log.push(&mut g.seq, Kind::User(text));
        g.runs.push(log);
    }

    /// Takes in what the run's conversation gained since the last call: its replies, the
    /// results of its tool calls and what was said to it. A conversation that got shorter was
    /// compacted, and starts over from where it is.
    pub fn sync(&self, run: RunId, messages: &[Message]) {
        let mut g = self.lock();
        let Inner { runs, seq } = &mut *g;
        let Some(log) = runs.iter_mut().find(|l| l.run == run) else {
            return;
        };
        let from = log.synced.min(messages.len());
        for m in &messages[from..] {
            log.take(seq, m);
        }
        log.synced = messages.len();
    }

    pub fn finish(&self, run: RunId, status: RunStatus) {
        if let Some(l) = self.lock().runs.iter_mut().find(|l| l.run == run) {
            l.status = Some(status);
        }
    }

    /// Worker runs that ended `Completed`.
    pub fn completed_workers(&self) -> usize {
        self.lock()
            .runs
            .iter()
            .filter(|l| l.role == Role::Worker && l.status == Some(RunStatus::Completed))
            .count()
    }

    /// The whole session as text, elided to `max_tokens` (Q5): tool results go first, oldest
    /// first, and then the oldest entries.
    pub fn packet(&self, max_tokens: u64) -> Packet {
        struct Line {
            run: usize,
            seq: u64,
            text: String,
            result: Option<String>,
            gone: bool,
        }
        let g = self.lock();
        let headers: Vec<String> = g
            .runs
            .iter()
            .map(|l| {
                let state = l.status.map_or("running".to_owned(), RunStatus::as_value);
                format!("## {} ({:?}) run {} [{state}]", l.agent, l.role, l.run)
            })
            .collect();
        let mut lines: Vec<Line> = Vec::new();
        for (run, log) in g.runs.iter().enumerate() {
            for e in &log.entries {
                let (text, result) = match &e.kind {
                    Kind::User(t) => (format!("user: {t}"), None),
                    Kind::Text(t) => (format!("assistant: {t}"), None),
                    Kind::Call {
                        name, args, result, ..
                    } => (
                        format!("tool {name} {args}"),
                        result.as_ref().map(|(r, bad)| {
                            if *bad {
                                format!("-> ERROR: {r}")
                            } else {
                                format!("-> {r}")
                            }
                        }),
                    ),
                };
                lines.push(Line {
                    run,
                    seq: e.seq,
                    text,
                    result,
                    gone: false,
                });
            }
        }
        const ELIDED: &str = "-> [result elided]";
        let size = |l: &Line| {
            if l.gone {
                0
            } else {
                l.text.chars().count() + 1 + l.result.as_ref().map_or(0, |r| r.chars().count() + 1)
            }
        };
        let fixed: usize = headers.iter().map(|h| h.chars().count() + 2).sum();
        let mut total: usize = fixed + lines.iter().map(size).sum::<usize>();
        let limit = (max_tokens as f64 * CHARS_PER_TOKEN / 1.1) as usize;
        let mut order: Vec<usize> = (0..lines.len()).collect();
        order.sort_by_key(|&i| lines[i].seq);
        for &i in &order {
            if total <= limit {
                break;
            }
            let l = &mut lines[i];
            if let Some(r) = &l.result {
                if r != ELIDED {
                    total -= r.chars().count();
                    total += ELIDED.chars().count();
                    l.result = Some(ELIDED.to_owned());
                }
            }
        }
        for &i in &order {
            if total <= limit {
                break;
            }
            total -= size(&lines[i]);
            lines[i].gone = true;
        }

        let mut text = String::new();
        for (run, header) in headers.iter().enumerate() {
            text.push_str(header);
            text.push('\n');
            let mut skipped = 0;
            for l in lines.iter().filter(|l| l.run == run) {
                if l.gone {
                    skipped += 1;
                    continue;
                }
                if skipped > 0 {
                    text.push_str(&format!("[{skipped} earlier entries elided]\n"));
                    skipped = 0;
                }
                text.push_str(&l.text);
                text.push('\n');
                if let Some(r) = &l.result {
                    text.push_str(r);
                    text.push('\n');
                }
            }
            if skipped > 0 {
                text.push_str(&format!("[{skipped} entries elided]\n"));
            }
            text.push('\n');
        }
        let tokens = tokens_of(text.chars().count());
        Packet { text, tokens }
    }
}

#[cfg(test)]
mod tests;
