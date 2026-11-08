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

#[derive(Default)]
struct Transcript {
    blocks: Vec<Arc<Block>>,
    blocks_version: u32,
    tail: String,
    tail_version: u32,
    head: String,
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
            t.blocks.push(Arc::new(Block {
                id: *next_id,
                text: head.into(),
            }));
            sealed = true;
        }
        if sealed {
            t.blocks_version = t.blocks_version.wrapping_add(1);
        }
        sealed
    }

    pub fn blocks(&self, id: u64) -> Vec<Arc<Block>> {
        self.lock()
            .transcripts
            .get(&id)
            .map_or_else(Vec::new, |t| t.blocks.clone())
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
        let gone: Vec<u64> = t.blocks.drain(..).map(|b| b.id).collect();
        t.tail = String::new();
        t.blocks_version = t.blocks_version.wrapping_add(1);
        t.tail_version = t.tail_version.wrapping_add(1);
        for b in gone {
            g.wrap.remove(&b);
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
        g.wrap
            .entry(block.id)
            .or_insert_with(|| wrap_text(&block.text, width).into())
            .clone()
    }

    /// `(sealed_rows, tail_rows)` of a transcript at `width`. Sealed blocks come from the cache;
    /// only the tail is walked.
    pub fn rows(&self, id: u64, width: usize) -> (usize, usize) {
        let mut g = self.lock();
        let blocks = match g.transcripts.get(&id) {
            Some(t) => t.blocks.clone(),
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

    /// Bytes of content held (blocks, tails, heads, texts, log rows excluded).
    pub fn content_bytes(&self) -> usize {
        let g = self.lock();
        let t: usize = g
            .transcripts
            .values()
            .map(|t| {
                t.blocks.iter().map(|b| b.text.len()).sum::<usize>() + t.tail.len() + t.head.len()
            })
            .sum();
        t + g.texts.values().map(|x| x.len()).sum::<usize>()
    }
}
