//! `runs/<id>/transcript.jsonl` and `meta.json` (07 section 2.2): what a run said and did, one
//! JSON object per line, redacted on the way out.
//!
//! The lines are built from the `Msg`s the run task sends, so a transcript costs the run
//! nothing. Thinking is kept only when `[persist] store_thinking` is on (09 section 4).

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use aibus_provider::StreamChunk;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::fs::{create_private_dir, write_atomic, write_private};
use super::time::rfc3339;
use crate::ids::{RunId, ToolCallId};
use crate::msg::{Msg, ToolResult};
use crate::redact::Redactor;
use crate::state::reduce::tool_summary;
use crate::state::store::{Done, Ev, Store, Tool, OUTPUT_KEEP, SUMMARY_HEAD};

/// `runs/<id>/meta.json`, and the row the index keeps per run.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct RunMeta {
    pub id: String,
    pub parent_id: Option<String>,
    pub agent: String,
    pub role: String,
    pub model: String,
    pub account: String,
    pub status: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub turns: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    pub summary_head: String,
}

enum Block {
    Text(String),
    Thinking(String),
    Tool { id: ToolCallId, name: String, input: String },
}

pub struct Transcript {
    dir: PathBuf,
    file: File,
    meta: RunMeta,
    blocks: Vec<Block>,
    redactor: Redactor,
    store_thinking: bool,
    result_max: usize,
}

fn cut(text: &str, max: usize) -> &str {
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

impl Transcript {
    /// Creates `<runs_dir>/<run>/` and writes the first `meta.json`.
    pub fn start(
        runs_dir: &Path,
        meta: RunMeta,
        redactor: Redactor,
        store_thinking: bool,
        result_max: usize,
    ) -> io::Result<Self> {
        let dir = runs_dir.join(&meta.id);
        create_private_dir(&dir)?;
        let mut opts = fs::OpenOptions::new();
        opts.append(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let file = opts.open(dir.join("transcript.jsonl"))?;
        let this = Self {
            dir,
            file,
            meta,
            blocks: Vec::new(),
            redactor,
            store_thinking,
            result_max,
        };
        this.write_meta()?;
        Ok(this)
    }

    fn write_meta(&self) -> io::Result<()> {
        let mut v = serde_json::to_value(&self.meta).map_err(io::Error::other)?;
        self.redactor.redact_json(&mut v);
        write_atomic(
            &self.dir.join("meta.json"),
            serde_json::to_string_pretty(&v).map_err(io::Error::other)?,
        )
    }

    fn line(&mut self, at: SystemTime, mut v: Value) -> io::Result<()> {
        v["t"] = rfc3339(at).into();
        self.redactor.redact_json(&mut v);
        writeln!(self.file, "{v}")
    }

    /// The user's message: the prompt that started the run, or one mailed to it. `via` is the
    /// client of `aibus mcp` that sent it (#274); a typed one has none.
    pub fn user(&mut self, at: SystemTime, text: &str, via: Option<&str>) -> io::Result<()> {
        let mut line = json!({"type": "message", "role": "user", "blocks": [{"type": "text", "text": text}]});
        if let Some(client) = via {
            line["via"] = "mcp".into();
            line["client"] = client.into();
        }
        self.line(at, line)
    }

    fn assistant(&mut self, at: SystemTime) -> io::Result<()> {
        let blocks: Vec<Value> = std::mem::take(&mut self.blocks)
            .into_iter()
            .map(|b| match b {
                Block::Text(text) => json!({"type": "text", "text": text}),
                Block::Thinking(text) => json!({"type": "thinking", "text": text}),
                Block::Tool { id, name, input } => {
                    let input = serde_json::from_str(&input).unwrap_or(Value::String(input));
                    json!({"type": "tool_use", "id": id.0.to_string(), "name": name, "input": input})
                }
            })
            .collect();
        if blocks.is_empty() {
            return Ok(());
        }
        self.line(at, json!({"type": "message", "role": "assistant", "blocks": blocks}))
    }

    fn stream(&mut self, at: SystemTime, chunks: &[StreamChunk]) -> io::Result<()> {
        for chunk in chunks {
            match chunk {
                StreamChunk::TextDelta(t) => match self.blocks.last_mut() {
                    Some(Block::Text(s)) => s.push_str(t),
                    _ => self.blocks.push(Block::Text(t.clone())),
                },
                StreamChunk::ThinkingDelta(t) if self.store_thinking => match self.blocks.last_mut() {
                    Some(Block::Thinking(s)) => s.push_str(t),
                    _ => self.blocks.push(Block::Thinking(t.clone())),
                },
                StreamChunk::ToolUseStart { id, name } => self.blocks.push(Block::Tool {
                    id: *id,
                    name: name.to_string(),
                    input: String::new(),
                }),
                StreamChunk::ToolUseInputDelta { id, json_fragment } => {
                    for b in &mut self.blocks {
                        if let Block::Tool { id: tid, input, .. } = b {
                            if tid == id {
                                input.push_str(json_fragment);
                            }
                        }
                    }
                }
                StreamChunk::Usage(u) => self.line(
                    at,
                    json!({"type": "usage", "input_tokens": u.input_tokens,
                           "output_tokens": u.output_tokens, "cache_read": u.cache_read}),
                )?,
                StreamChunk::Done(_) => self.assistant(at)?,
                _ => {}
            }
        }
        Ok(())
    }

    /// Past `result_max` bytes the result is cut and the whole text goes to
    /// `results/<call>.txt`. Redaction comes first, so a secret on the cut cannot leave its
    /// first half behind.
    fn tool_result(
        &mut self,
        at: SystemTime,
        call: ToolCallId,
        result: &ToolResult,
    ) -> io::Result<()> {
        let text = self.redactor.redact(&result.content).into_owned();
        let id = call.0.to_string();
        let mut v = json!({"type": "tool_result", "id": id, "is_error": result.is_error,
                           "bytes": result.content.len()});
        if text.len() > self.result_max {
            let rel = format!("results/{id}.txt");
            create_private_dir(&self.dir.join("results"))?;
            write_private(self.dir.join(&rel), &text)?;
            v["content"] = cut(&text, self.result_max).into();
            v["truncated"] = true.into();
            v["full"] = format!("runs/{}/{rel}", self.meta.id).into();
        } else {
            v["content"] = text.into();
        }
        self.line(at, v)
    }

    fn end(&mut self, at: SystemTime, status: &str, turns: u32) -> io::Result<()> {
        self.assistant(at)?;
        self.meta.status = status.into();
        self.meta.ended_at = Some(rfc3339(at));
        self.line(at, json!({"type": "end", "status": status, "turns": turns}))?;
        self.write_meta()
    }

    /// Adds `msg` to the transcript if it belongs to this run and has a line of its own.
    pub fn record(&mut self, msg: &Msg, at: SystemTime) -> io::Result<()> {
        match msg {
            Msg::Stream { chunks, .. } => self.stream(at, chunks),
            Msg::ToolCallFinished { call_id, result, .. } => self.tool_result(at, *call_id, result),
            Msg::Compacted { pre_tokens, .. } => self.line(
                at,
                json!({"type": "compact_boundary", "trigger": "auto", "pre_tokens": pre_tokens}),
            ),
            Msg::HookFired { event, outcome, .. } => {
                let mut v = json!({"type": "hook", "event": event.as_str(), "exit": outcome.exit_code});
                if let Some(m) = &outcome.message {
                    v["message"] = m.as_str().into();
                }
                self.line(at, v)
            }
            Msg::RunFinished { result, .. } => {
                self.meta.turns = result.turns;
                self.meta.input_tokens = result.usage.input_tokens;
                self.meta.output_tokens = result.usage.output_tokens;
                self.meta.cost_usd = result.usage.cost_usd;
                let head = cut(&result.summary, SUMMARY_HEAD);
                self.meta.summary_head = self.redactor.redact(head).into_owned();
                self.end(at, &result.status.as_value(), result.turns)
            }
            Msg::RunFailed { .. } => {
                self.end(at, "failed", self.meta.turns)
            }
            _ => Ok(()),
        }
    }
}

/// What one line of `transcript.jsonl` adds to a run's view.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Text(String),
    Ev(Ev),
    /// The result of the call `.0`.
    Done(String, Done),
}

/// The view items of one line; a line that is not one of ours, or is cut short, gives none.
pub fn items(line: &str) -> Vec<Item> {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return Vec::new() };
    let text_of = |b: &Value| b.get("text").and_then(Value::as_str).unwrap_or("").to_owned();
    match v.get("type").and_then(Value::as_str) {
        Some("message") => {
            let blocks = v.get("blocks").and_then(Value::as_array).cloned().unwrap_or_default();
            if v.get("role").and_then(Value::as_str) == Some("user") {
                let text = blocks.iter().map(text_of).collect::<Vec<_>>().join("\n");
                return if text.is_empty() { Vec::new() } else { vec![Item::Ev(Ev::User(text))] };
            }
            blocks
                .iter()
                .filter_map(|b| match b.get("type").and_then(Value::as_str) {
                    Some("text") => Some(Item::Text(text_of(b))),
                    Some("tool_use") => {
                        let name = b.get("name").and_then(Value::as_str).unwrap_or("tool");
                        let input = b.get("input").cloned().unwrap_or(Value::Null);
                        Some(Item::Ev(Ev::Tool(Tool {
                            call: b.get("id").and_then(Value::as_str).unwrap_or("").to_owned(),
                            name: name.to_owned(),
                            summary: tool_summary(name, &input),
                            input: serde_json::to_string_pretty(&input)
                                .unwrap_or_default()
                                .chars()
                                .take(OUTPUT_KEEP)
                                .collect(),
                            done: None,
                        })))
                    }
                    _ => None,
                })
                .collect()
        }
        Some("tool_result") => {
            let content = v.get("content").and_then(Value::as_str).unwrap_or("");
            vec![Item::Done(
                v.get("id").and_then(Value::as_str).unwrap_or("").to_owned(),
                Done {
                    ok: !v.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                    ms: 0,
                    bytes: v.get("bytes").and_then(Value::as_u64).map_or(content.len(), |b| b as usize),
                    output: content.chars().take(OUTPUT_KEEP).collect(),
                },
            )]
        }
        Some("compact_boundary") => vec![Item::Ev(Ev::Compact {
            pre_tokens: v.get("pre_tokens").and_then(Value::as_u64).unwrap_or(0),
        })],
        _ => Vec::new(),
    }
}

/// Reads `path` back into the store entry `entry`, `chunk` lines at a time, calling `after` once
/// per chunk so a view can draw what has arrived. Returns the number of lines read.
pub fn reload(store: &Store, entry: u64, path: &Path, chunk: usize, mut after: impl FnMut()) -> io::Result<usize> {
    use std::io::BufRead;
    let file = io::BufReader::new(File::open(path)?);
    let mut n = 0;
    let mut open_text = false;
    for line in file.lines() {
        let line = line?;
        n += 1;
        for item in items(&line) {
            match item {
                Item::Text(t) => {
                    if open_text {
                        store.append(entry, "\n");
                    }
                    store.append(entry, &t);
                    open_text = true;
                }
                Item::Ev(ev) => {
                    store.push_event(entry, ev);
                    open_text = false;
                }
                Item::Done(call, done) => store.finish_tool(entry, &call, done),
            }
        }
        if n % chunk.max(1) == 0 {
            after();
        }
    }
    after();
    Ok(n)
}

/// The run a message belongs to, for the messages a transcript records.
pub fn run_of(msg: &Msg) -> Option<RunId> {
    match msg {
        Msg::Stream { run, .. }
        | Msg::ToolCallFinished { run, .. }
        | Msg::Compacted { run, .. }
        | Msg::RunFinished { run, .. }
        | Msg::RunFailed { run, .. } => Some(*run),
        Msg::HookFired { run, .. } => *run,
        _ => None,
    }
}
