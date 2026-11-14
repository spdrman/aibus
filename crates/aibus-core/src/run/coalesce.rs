//! Merges provider chunks into the batches that become one `Msg::Stream` (08 section 2).
//!
//! A batch goes out when it is 16 ms old, when it holds 4 KiB of deltas, or when a chunk that
//! marks a boundary arrives (a tool call starting or ending, the end of the turn). Adjacent
//! text deltas are concatenated, input fragments of one tool call are appended and usage is
//! summed, so a burst of small chunks costs one commit.

use std::time::Duration;

use aibus_provider::StreamChunk;
use tokio::time::Instant;

pub const FLUSH_AFTER: Duration = Duration::from_millis(16);
pub const FLUSH_BYTES: usize = 4096;

#[derive(Default)]
pub struct Coalescer {
    chunks: Vec<StreamChunk>,
    bytes: usize,
    since: Option<Instant>,
}

impl Coalescer {
    /// When the pending batch is due, if there is one.
    pub fn due(&self) -> Option<Instant> {
        self.since.map(|t| t + FLUSH_AFTER)
    }

    /// Adds `chunk`; returns the batch if this chunk completes it.
    pub fn push(&mut self, chunk: StreamChunk, now: Instant) -> Option<Vec<StreamChunk>> {
        self.since.get_or_insert(now);
        let boundary = matches!(
            chunk,
            StreamChunk::ToolUseStart { .. }
                | StreamChunk::ToolUseEnd { .. }
                | StreamChunk::Done(_)
        );
        match (self.chunks.last_mut(), chunk) {
            (Some(StreamChunk::TextDelta(t)), StreamChunk::TextDelta(more))
            | (Some(StreamChunk::ThinkingDelta(t)), StreamChunk::ThinkingDelta(more)) => {
                self.bytes += more.len();
                t.push_str(&more);
            }
            (
                Some(StreamChunk::ToolUseInputDelta { id, json_fragment }),
                StreamChunk::ToolUseInputDelta {
                    id: next,
                    json_fragment: more,
                },
            ) if *id == next => {
                self.bytes += more.len();
                json_fragment.push_str(&more);
            }
            (Some(StreamChunk::Usage(u)), StreamChunk::Usage(more)) => {
                u.input_tokens += more.input_tokens;
                u.output_tokens += more.output_tokens;
                u.cache_read += more.cache_read;
                u.cache_write += more.cache_write;
                u.cost_usd += more.cost_usd;
            }
            (_, chunk) => {
                self.bytes += match &chunk {
                    StreamChunk::TextDelta(t) | StreamChunk::ThinkingDelta(t) => t.len(),
                    StreamChunk::ToolUseInputDelta { json_fragment, .. } => json_fragment.len(),
                    _ => 0,
                };
                self.chunks.push(chunk);
            }
        }
        (boundary || self.bytes >= FLUSH_BYTES)
            .then(|| self.take())
            .flatten()
    }

    /// Whatever is pending, or `None` when nothing is.
    pub fn take(&mut self) -> Option<Vec<StreamChunk>> {
        self.bytes = 0;
        self.since = None;
        (!self.chunks.is_empty()).then(|| std::mem::take(&mut self.chunks))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aibus_provider::{StopReason, Usage};

    fn text(s: &str) -> StreamChunk {
        StreamChunk::TextDelta(s.into())
    }

    #[tokio::test(start_paused = true)]
    async fn text_merges_and_waits_for_the_timer() {
        let mut c = Coalescer::default();
        let t0 = Instant::now();
        assert!(c.push(text("ab"), t0).is_none());
        assert!(c.push(text("cd"), t0).is_none());
        assert_eq!(c.due(), Some(t0 + FLUSH_AFTER));
        assert_eq!(c.take(), Some(vec![text("abcd")]));
        assert_eq!(c.due(), None);
        assert_eq!(c.take(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn four_kib_flushes() {
        let mut c = Coalescer::default();
        let now = Instant::now();
        assert!(c.push(text(&"x".repeat(FLUSH_BYTES - 1)), now).is_none());
        let batch = c.push(text("y"), now).unwrap();
        assert_eq!(batch.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn boundaries_flush_with_pending_text_and_usage_sums() {
        let mut c = Coalescer::default();
        let now = Instant::now();
        let usage = |n| {
            StreamChunk::Usage(Usage {
                input_tokens: n,
                ..Usage::default()
            })
        };
        c.push(text("hi"), now);
        c.push(usage(1), now);
        c.push(usage(2), now);
        let batch = c.push(StreamChunk::Done(StopReason::EndTurn), now).unwrap();
        assert_eq!(batch.len(), 3);
        assert_eq!(batch[1], usage(3));
    }
}
