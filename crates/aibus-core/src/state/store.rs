//! The side arena for heavy values (03 section 8, D-04).
//!
//! The graph holds `Value::Handle { store, id, version }`; the content lives here. Sealed
//! blocks are immutable and shared by `Arc`, the version of a handle is the change signal.
//!
//! `Store` is a cheap clone of one shared arena. 03 section 8 has deriveds reach it through
//! `Tracker::store()`; the `Tracker` lives in `aibus-graph`, which knows nothing of the store,
//! so a derived captures a clone instead and reads the handle through the tracker for the
//! dependency.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use aibus_graph::{StoreHandle, Value};

use super::value::json_to_value;

/// A tail longer than this seals its oldest `SEAL_BYTES` into a block.
pub const TAIL_MAX: usize = 64 * 1024;
pub const SEAL_BYTES: usize = 32 * 1024;
pub const LOG_RING: usize = 2_000;
/// A finished run keeps this much of its start for the card and `session show`.
pub const SUMMARY_HEAD: usize = 2 * 1024;
/// Tool inputs and prompts over this go behind a handle (Q13).
pub const INLINE_MAX: usize = 4 * 1024;
/// How much of a result the expanded view keeps in memory; the rest is on disk.
pub const OUTPUT_KEEP: usize = 4 * 1024;

pub const TRANSCRIPT: &str = "transcript";
pub const TAIL: &str = "tail";
pub const LOG: &str = "log";
pub const TEXT: &str = "text";

/// Immutable once sealed.
#[derive(Debug)]
pub struct Block {
    pub id: u64,
    pub text: Arc<str>,
}

/// What a run did besides talk: the transcript view draws these between the text (05 section 3).
#[derive(Clone, Debug, PartialEq)]
pub enum Ev {
    /// A message from the user: the prompt that started the run, or one sent to it.
    User(String),
    Tool(Tool),
    /// The conversation was compacted away at this many tokens.
    Compact { pre_tokens: u64 },
    Note { error: bool, text: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    pub call: String,
    pub name: String,
    /// The one line a collapsed call shows: the command, the path.
    pub summary: String,
    pub input: String,
    pub done: Option<Done>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Done {
    pub ok: bool,
    pub ms: u64,
    pub bytes: usize,
    /// The start of the result, for the expanded view.
    pub output: String,
}

enum Chunk {
    /// A sealed run of text; `start` is the number of text bytes before it.
    Text { block: Arc<Block>, start: usize },
    Ev { ev: Ev, ver: u32 },
}

/// One row of the transcript view.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    Text(String),
    User(String),
    /// A tool call; `ordinal` counts the calls of the run from 0.
    Tool { ordinal: usize, tool: Tool, expanded: bool },
    Detail(String),
    Compact(u64),
    Note { error: bool, text: String },
}

/// A window of the rows of a transcript at one width.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rows {
    pub total: usize,
    /// Index of `rows[0]` among all rows.
    pub first: usize,
    pub rows: Vec<Row>,
    /// How many calls the transcript holds.
    pub tools: usize,
}

#[derive(Default)]
struct Transcript {
    chunks: Vec<Chunk>,
    blocks_version: u32,
    /// Text bytes in `chunks`; the tail starts here.
    sealed_bytes: usize,
    tail: String,
    tail_version: u32,
    head: String,
    /// `(tail_version, text bytes so far)` after each change, so a past version can be cut out.
    marks: Vec<(u32, usize)>,
    /// The content was dropped when the run finished.
    evicted: bool,
}

impl Transcript {
    fn seal(&mut self, block: Arc<Block>) {
        let start = self.sealed_bytes;
        self.sealed_bytes += block.text.len();
        self.chunks.push(Chunk::Text { block, start });
    }
}

#[derive(Default)]
struct Inner {
    next_id: u64,
    transcripts: HashMap<u64, Transcript>,
    texts: HashMap<u64, Arc<str>>,
    log: VecDeque<Value>,
    log_version: u32,
    /// Wrapped lines per sealed block at one width; a width change drops everything.
    wrap: HashMap<u64, Arc<[String]>>,
    wrap_width: usize,
    /// How many times text was wrapped (a cache miss, the tail, a cut): the tail-only test reads it.
    wrap_calls: u64,
}

#[derive(Clone, Default)]
pub struct Store(Arc<Mutex<Inner>>);

fn boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Wraps at `width` characters, one row per `\n`-separated line at least.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for line in text.split('\n') {
        let mut chars = line.chars().peekable();
        if chars.peek().is_none() {
            rows.push(String::new());
            continue;
        }
        while chars.peek().is_some() {
            rows.push(chars.by_ref().take(width).collect());
        }
    }
    rows
}

/// Row count of `wrap_text` without building the rows.
fn count_rows(text: &str, width: usize) -> usize {
    let width = width.max(1);
    text.split('\n')
        .map(|l| l.chars().count().div_ceil(width).max(1))
        .sum()
}

/// The rows below a call that is expanded: its input, then its result, each wrapped.
fn detail(tool: &Tool, width: usize) -> Vec<String> {
    let w = width.saturating_sub(4).max(1);
    let mut out: Vec<String> = wrap_text(&tool.input, w).into_iter().map(|l| format!("  {l}")).collect();
    if let Some(d) = &tool.done {
        out.extend(wrap_text(&d.output, w).into_iter().map(|l| format!("  {l}")));
    }
    out
}

fn event_rows(ev: &Ev, expanded: &[String], width: usize) -> usize {
    match ev {
        Ev::User(t) => count_rows(t, width.saturating_sub(2).max(1)),
        Ev::Tool(t) if expanded.contains(&t.call) => 1 + detail(t, width).len(),
        _ => 1,
    }
}

fn event_window(ev: &Ev, ordinal: usize, expanded: &[String], width: usize, range: std::ops::Range<usize>) -> Vec<Row> {
    let all: Vec<Row> = match ev {
        Ev::User(t) => wrap_text(t, width.saturating_sub(2).max(1)).into_iter().map(Row::User).collect(),
        Ev::Tool(t) => {
            let open = expanded.contains(&t.call);
            let mut rows = vec![Row::Tool { ordinal, tool: t.clone(), expanded: open }];
            if open {
                rows.extend(detail(t, width).into_iter().map(Row::Detail));
            }
            rows
        }
        Ev::Compact { pre_tokens } => vec![Row::Compact(*pre_tokens)],
        Ev::Note { error, text } => vec![Row::Note { error: *error, text: text.clone() }],
    };
    all.into_iter().skip(range.start).take(range.len()).collect()
}

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A new, empty transcript. Returns its entry id.
    pub fn new_transcript(&self) -> u64 {
        let mut g = self.lock();
        g.next_id += 1;
        let id = g.next_id;
        g.transcripts.insert(id, Transcript::default());
        id
    }

    /// `(blocks, tail)` handles of a transcript at their current versions.
    pub fn handles(&self, id: u64) -> (StoreHandle, StoreHandle) {
        let g = self.lock();
        let t = g.transcripts.get(&id);
        let (bv, tv) = t.map_or((0, 0), |t| (t.blocks_version, t.tail_version));
        (
            StoreHandle {
                store: TRANSCRIPT.into(),
                id,
                version: bv,
            },
            StoreHandle {
                store: TAIL.into(),
                id,
                version: tv,
            },
        )
    }

    /// Appends streamed text. Returns true when a block sealed (the blocks handle moved).
    /// The tail handle moves on every append.
    pub fn append(&self, id: u64, text: &str) -> bool {
        let mut guard = self.lock();
        let Inner {
            transcripts,
            next_id,
            ..
        } = &mut *guard;
        let Some(t) = transcripts.get_mut(&id) else {
            return false;
        };
        if t.head.len() < SUMMARY_HEAD {
            let cut = boundary(text, SUMMARY_HEAD - t.head.len());
            t.head.push_str(&text[..cut]);
        }
        t.tail.push_str(text);
        t.tail_version = t.tail_version.wrapping_add(1);
        let mut sealed = false;
        while t.tail.len() > TAIL_MAX {
            let mut cut = boundary(&t.tail, SEAL_BYTES);
            // Prefer to cut after a newline so blocks wrap on line boundaries.
            if let Some(nl) = t.tail[..cut].rfind('\n') {
                cut = nl + 1;
            }
            let rest = t.tail.split_off(cut);
            let head = std::mem::replace(&mut t.tail, rest);
            *next_id += 1;
            t.seal(Arc::new(Block {
                id: *next_id,
                text: head.into(),
            }));
            sealed = true;
        }
        let total = t.sealed_bytes + t.tail.len();
        t.marks.push((t.tail_version, total));
        if sealed {
            t.blocks_version = t.blocks_version.wrapping_add(1);
        }
        sealed
    }

    /// Puts an event after the text so far. Both handles move.
    pub fn push_event(&self, id: u64, ev: Ev) {
        let mut guard = self.lock();
        let Inner {
            transcripts,
            next_id,
            ..
        } = &mut *guard;
        let Some(t) = transcripts.get_mut(&id) else {
            return;
        };
        if !t.tail.is_empty() {
            *next_id += 1;
            let text = std::mem::take(&mut t.tail);
            t.seal(Arc::new(Block { id: *next_id, text: text.into() }));
        }
        t.tail_version = t.tail_version.wrapping_add(1);
        t.blocks_version = t.blocks_version.wrapping_add(1);
        t.marks.push((t.tail_version, t.sealed_bytes));
        t.chunks.push(Chunk::Ev { ev, ver: t.tail_version });
    }

    /// Fills in the result of the call `call`. Both handles move.
    pub fn finish_tool(&self, id: u64, call: &str, done: Done) {
        let mut g = self.lock();
        let Some(t) = g.transcripts.get_mut(&id) else { return };
        let found = t.chunks.iter_mut().rev().find_map(|c| match c {
            Chunk::Ev { ev: Ev::Tool(tool), .. } if tool.call == call => Some(tool),
            _ => None,
        });
        if let Some(tool) = found {
            tool.done = Some(done);
            t.tail_version = t.tail_version.wrapping_add(1);
            t.blocks_version = t.blocks_version.wrapping_add(1);
        }
    }

    /// The id of the `ordinal`th call of the transcript.
    pub fn tool_call(&self, id: u64, ordinal: usize) -> Option<String> {
        let g = self.lock();
        g.transcripts
            .get(&id)?
            .chunks
            .iter()
            .filter_map(|c| match c {
                Chunk::Ev { ev: Ev::Tool(t), .. } => Some(t.call.clone()),
                _ => None,
            })
            .nth(ordinal)
    }

    /// The sealed text blocks, oldest first.
    pub fn blocks(&self, id: u64) -> Vec<Arc<Block>> {
        self.lock().transcripts.get(&id).map_or_else(Vec::new, |t| {
            t.chunks
                .iter()
                .filter_map(|c| match c {
                    Chunk::Text { block, .. } => Some(block.clone()),
                    Chunk::Ev { .. } => None,
                })
                .collect()
        })
    }

    pub fn tail(&self, id: u64) -> String {
        self.lock()
            .transcripts
            .get(&id)
            .map_or_else(String::new, |t| t.tail.clone())
    }

    pub fn head(&self, id: u64) -> String {
        self.lock()
            .transcripts
            .get(&id)
            .map_or_else(String::new, |t| t.head.clone())
    }

    /// A finished run keeps only its `SUMMARY_HEAD`. Both handles move.
    pub fn evict_finished(&self, id: u64) {
        let mut g = self.lock();
        let Some(t) = g.transcripts.get_mut(&id) else {
            return;
        };
        let gone: Vec<u64> = t
            .chunks
            .drain(..)
            .filter_map(|c| match c {
                Chunk::Text { block, .. } => Some(block.id),
                Chunk::Ev { .. } => None,
            })
            .collect();
        t.tail = String::new();
        t.sealed_bytes = 0;
        t.marks.clear();
        t.evicted = true;
        t.blocks_version = t.blocks_version.wrapping_add(1);
        t.tail_version = t.tail_version.wrapping_add(1);
        for b in gone {
            g.wrap.remove(&b);
        }
    }

    /// Whether the content was dropped when the run finished and has not been brought back.
    pub fn is_evicted(&self, id: u64) -> bool {
        self.lock().transcripts.get(&id).is_some_and(|t| t.evicted)
    }

    /// Starts bringing an evicted transcript back from disk: true once, for whoever reads the
    /// file. Text goes in with `append` and events with `push_event`, as they came the first time.
    pub fn begin_restore(&self, id: u64) -> bool {
        let mut g = self.lock();
        match g.transcripts.get_mut(&id) {
            Some(t) if t.evicted => {
                t.evicted = false;
                true
            }
            _ => false,
        }
    }

    /// Immutable text behind a handle (prompts, large tool inputs, agent bodies).
    pub fn put_text(&self, text: &str) -> StoreHandle {
        let mut g = self.lock();
        g.next_id += 1;
        let id = g.next_id;
        g.texts.insert(id, text.into());
        StoreHandle {
            store: TEXT.into(),
            id,
            version: 0,
        }
    }

    pub fn text(&self, h: &StoreHandle) -> Option<Arc<str>> {
        self.lock().texts.get(&h.id).cloned()
    }

    /// The text inline if it fits in `INLINE_MAX`, else a handle to it.
    pub fn inline_or_handle(&self, text: &str) -> Value {
        if text.len() <= INLINE_MAX {
            Value::Str(text.into())
        } else {
            Value::Handle(self.put_text(text))
        }
    }

    /// Q13: a tool input stays inline (so `explain` is useful) up to `INLINE_MAX` serialised
    /// bytes, and goes behind a handle above that.
    pub fn tool_input(&self, input: &serde_json::Value) -> Value {
        let text = input.to_string();
        if text.len() <= INLINE_MAX {
            json_to_value(input)
        } else {
            Value::Handle(self.put_text(&text))
        }
    }

    pub fn log_push(&self, row: Value) -> StoreHandle {
        let mut g = self.lock();
        if g.log.len() == LOG_RING {
            g.log.pop_front();
        }
        g.log.push_back(row);
        g.log_version = g.log_version.wrapping_add(1);
        StoreHandle {
            store: LOG.into(),
            id: 0,
            version: g.log_version,
        }
    }

    pub fn log_clear(&self) -> StoreHandle {
        let mut g = self.lock();
        g.log.clear();
        g.log_version = g.log_version.wrapping_add(1);
        StoreHandle {
            store: LOG.into(),
            id: 0,
            version: g.log_version,
        }
    }

    pub fn log_handle(&self) -> StoreHandle {
        StoreHandle {
            store: LOG.into(),
            id: 0,
            version: self.lock().log_version,
        }
    }

    pub fn log_len(&self) -> usize {
        self.lock().log.len()
    }

    /// The last `n` rows, oldest first.
    pub fn log_rows(&self, n: usize) -> Vec<Value> {
        let g = self.lock();
        let skip = g.log.len().saturating_sub(n);
        g.log.iter().skip(skip).cloned().collect()
    }

    /// Wrapped lines of one sealed block at `width`, cached.
    pub fn wrapped(&self, block: &Block, width: usize) -> Arc<[String]> {
        let mut g = self.lock();
        Self::wrapped_locked(&mut g, block, width)
    }

    fn wrapped_locked(g: &mut Inner, block: &Block, width: usize) -> Arc<[String]> {
        if g.wrap_width != width {
            g.wrap.clear();
            g.wrap_width = width;
        }
        if let Some(hit) = g.wrap.get(&block.id) {
            return hit.clone();
        }
        g.wrap_calls += 1;
        let lines: Arc<[String]> = wrap_text(&block.text, width).into();
        g.wrap.insert(block.id, lines.clone());
        lines
    }

    /// How many times text was wrapped, for the test that a stream commit wraps only the tail.
    pub fn wrap_calls(&self) -> u64 {
        self.lock().wrap_calls
    }

    /// `(sealed_rows, tail_rows)` of a transcript's text at `width`. Sealed blocks come from the
    /// cache; only the tail is walked.
    pub fn rows(&self, id: u64, width: usize) -> (usize, usize) {
        let mut g = self.lock();
        let blocks: Vec<Arc<Block>> = match g.transcripts.get(&id) {
            Some(t) => t
                .chunks
                .iter()
                .filter_map(|c| match c {
                    Chunk::Text { block, .. } => Some(block.clone()),
                    Chunk::Ev { .. } => None,
                })
                .collect(),
            None => return (0, 0),
        };
        let sealed = blocks
            .iter()
            .map(|b| Self::wrapped_locked(&mut g, b, width).len())
            .sum();
        let tail = g
            .transcripts
            .get(&id)
            .map_or(0, |t| count_rows(&t.tail, width));
        (sealed, tail)
    }

    /// The rows of a transcript at `width`: text wrapped, events one row each (a call in
    /// `expanded` adds its input and result). `upto` cuts the transcript back to what it held at
    /// that tail version (time travel). `window` is `(first row, count)`; a `None` first row
    /// takes the last `count` rows. Wrapping a sealed block is cached; the tail is wrapped each time.
    pub fn transcript_rows(
        &self,
        id: u64,
        width: usize,
        expanded: &[String],
        upto: Option<u32>,
        window: (Option<usize>, usize),
    ) -> Rows {
        let mut g = self.lock();
        let Some(t) = g.transcripts.get(&id) else { return Rows::default() };
        let limit = upto.map(|v| {
            t.marks
                .iter()
                .rev()
                .find(|(ver, _)| *ver <= v)
                .map_or(0, |(_, bytes)| *bytes)
        });
        // The pieces in order: text (a sealed block, or a cut of it or of the tail) and events.
        enum Piece {
            Block(Arc<Block>),
            Cut(String),
            Ev(Ev),
        }
        let mut pieces = Vec::new();
        for c in &t.chunks {
            match c {
                Chunk::Text { block, start } => match limit {
                    Some(l) if l <= *start => {}
                    Some(l) if l < start + block.text.len() => {
                        pieces.push(Piece::Cut(block.text[..boundary(&block.text, l - start)].to_owned()));
                    }
                    _ => pieces.push(Piece::Block(block.clone())),
                },
                Chunk::Ev { ev, ver } => {
                    if upto.is_none_or(|v| *ver <= v) {
                        pieces.push(Piece::Ev(ev.clone()));
                    }
                }
            }
        }
        let tail = match limit {
            Some(l) if l <= t.sealed_bytes => String::new(),
            Some(l) => t.tail[..boundary(&t.tail, (l - t.sealed_bytes).min(t.tail.len()))].to_owned(),
            None => t.tail.clone(),
        };
        if !tail.is_empty() {
            pieces.push(Piece::Cut(tail));
        }
        // Row counts first, so the window can be placed; then only the rows in it are built.
        let mut counts = Vec::with_capacity(pieces.len());
        let mut text_rows: Vec<Option<Arc<[String]>>> = Vec::with_capacity(pieces.len());
        let mut tools = 0;
        for p in &pieces {
            let (n, lines) = match p {
                Piece::Block(b) => {
                    let lines = Self::wrapped_locked(&mut g, b, width);
                    (lines.len(), Some(lines))
                }
                Piece::Cut(text) => {
                    g.wrap_calls += 1;
                    let lines: Arc<[String]> = wrap_text(text, width).into();
                    (lines.len(), Some(lines))
                }
                Piece::Ev(ev) => {
                    tools += usize::from(matches!(ev, Ev::Tool(_)));
                    (event_rows(ev, expanded, width), None)
                }
            };
            counts.push(n);
            text_rows.push(lines);
        }
        let total: usize = counts.iter().sum();
        let (first, count) = match window {
            (Some(f), c) => (f.min(total), c),
            (None, c) => (total.saturating_sub(c), c),
        };
        let end = (first + count).min(total);
        let mut rows = Vec::with_capacity(end - first);
        let (mut at, mut ord) = (0usize, 0usize);
        for (i, p) in pieces.iter().enumerate() {
            let n = counts[i];
            let this = ord;
            ord += usize::from(matches!(p, Piece::Ev(Ev::Tool(_))));
            let (lo, hi) = (first.max(at), end.min(at + n));
            if lo < hi {
                match (p, &text_rows[i]) {
                    (_, Some(lines)) => rows.extend(lines[lo - at..hi - at].iter().cloned().map(Row::Text)),
                    (Piece::Ev(ev), None) => rows.extend(event_window(ev, this, expanded, width, lo - at..hi - at)),
                    _ => {}
                }
            }
            at += n;
        }
        Rows { total, first, rows, tools }
    }

    /// Bytes of content held (blocks, tails, heads, texts, log rows excluded).
    pub fn content_bytes(&self) -> usize {
        let g = self.lock();
        let t: usize = g
            .transcripts
            .values()
            .map(|t| t.sealed_bytes + t.tail.len() + t.head.len())
            .sum();
        t + g.texts.values().map(|x| x.len()).sum::<usize>()
    }
}
