# 08. Performance

Budgets are numbers with a benchmark behind each one. "Fast" is not a budget.

## 1. Rendering budget

| Item | Budget | Measured by |
|---|---|---|
| Frame rate while animating | 30 fps (`ui.tick_hz`); 60 fps allowed when the terminal is small | `benches/render_dashboard.rs` with `TestBackend` 98x46 |
| One dashboard render (view read + widget draw + ratatui diff) | under 3 ms p99 | same |
| Idle CPU with no runs | 0 % (no tick scheduled) | `tests/idle_no_tick.rs` asserts no `Msg::Tick` for 5 s |
| Resize to re-layout | under 10 ms | bench |
| Time to first frame on launch | under 150 ms after config load, 400 ms cold including SQLite open and definition discovery of 100 files | `tests/startup_time.rs` (soft, logs only) |

ratatui is immediate-mode: every frame redraws the whole buffer and diffs against the previous one. That is fine; what must not happen each frame is recomputing the inputs to the widgets. The graph's `view:` nodes are memoised by the engine (equality cutoff), so a frame with nothing changed reads cached values and the diff is empty. Frames are skipped entirely when `view:dirty` is empty and no animation is active.

## 2. Streaming tokens

Provider chunks arrive at up to a few hundred per second per run. The pipeline:

1. The provider task pushes `StreamChunk`s into a per-run `mpsc` (capacity 1024).
2. The run task drains that channel into a coalescer: text deltas are concatenated, tool-input fragments appended, usage merged. Every 16 ms (or when 4 KiB accumulated, or on a non-text chunk) it emits one `Msg::Stream { run, chunks }`.
3. `reduce` writes `model:run:<id>:tail` (append) and `model:run:<id>:usage`. When the tail passes 64 KiB, the oldest 32 KiB are sealed into an immutable `Block` in the store and the handle version bumps.
4. The transcript derived (03 section 8) re-wraps only the tail string and blocks missing a cache entry for the current width. Wrapped-line cache per block is `Arc<[Line]>` keyed by `(block_id, width)`; cache entries for other widths are dropped on resize after a 2 s grace.
5. The card and status bar derive from `usage` and `status`, not from the tail, so they do not recompute per token.

Measured budget: a `Msg::Stream` commit with recompute, on a run with 100k transcript lines open in the transcript view, under 2 ms p99 (`benches/commit_stream.rs`). Eight runs streaming at once, dashboard open: under 30 % of one core.

## 3. Bounded channels and back-pressure

| Channel | Capacity | On full |
|---|---|---|
| `mpsc<Msg>` into the loop | 4096 | producers `await` (async) or `blocking_send` (threads); the tick timer drops and increments `eng:ticks_dropped` |
| per-run `mpsc<StreamChunk>` | 1024 | provider task awaits; the HTTP body stops being read, which is TCP back-pressure to the provider |
| per-run `mpsc<RunMail>` | 64 | sender awaits |
| hook stdout/stderr capture | 1 MiB each, then truncated with a marker | |
| tool output | `tools.bash_max_output_bytes` (256 KiB), tail kept, head marker | |

Nothing in the system is unbounded except the on-disk transcript, which is append-only by design and rotated per session.

## 4. Graph and commit costs

- Per commit: Kahn recompute over the affected sub-DAG only. The dashboard graph has about 300 nodes plus 8 per run; the affected set for a stream commit is under 10.
- Equality cutoff: a derived whose recomputed value equals the old one stops propagation. `Value` equality for `Handle` is version equality, so a sealed block compares in O(1).
- Commit history: `persist.commit_history_cap` (4096) commits retained in memory. Retained deltas are `(Node, Value)` pairs; with handles for heavy content a retained commit averages under 1 KiB.
- Target from 03 section 11: commit plus recompute under 4 ms p99 on the reference session (100k lines, 8 runs, 2,000 log rows). Mini and causl backends both run the bench; a regression over 20 % fails CI.

## 5. Memory limits for long sessions

A session running for 12 hours with 300 runs must stay under 500 MiB resident. The budget:

| Component | Cap | Mechanism |
|---|---|---|
| Live conversations | one per running or waiting run; finished runs drop their `Conversation` after `RunFinished` (the transcript is on disk) | run task ends, `Conversation` dropped |
| Store blocks | the open transcript's blocks plus the last 64 KiB tail of every running run; finished runs keep only a `summary_head` of 2 KiB | `store.evict_finished(run)` on `RunFinished`; reopening a finished transcript streams it back from `transcript.jsonl` |
| Session log ring | 2,000 rows | ring buffer |
| Commit history | 4096 commits | ring |
| Wrapped-line caches | current width only, open transcript only | dropped on close and on width change |
| Snapshot | handles only; target under 2 MiB on disk for a day | 03 section 9 |
| tokio tasks | one per run plus one per hook process; `max_concurrent_runs` (8) and `max_runs_per_session` (500) bound them | orchestrator |
| Model/tokenizer caches | under 20 MiB | lazy, per provider |

`aibus doctor --session <id>` prints the live numbers; `eng:mem` is sampled every 10 s into the graph so the status bar can show `mem 212M` when `-v`.

## 6. Tracing

`--trace <file>` writes Chrome trace events: one span per commit (`intent`, `changed`, `recompute_us`), per effect, per provider call (`ttfb_ms`, `tokens`), per tool call, per hook. `chrome://tracing` or Perfetto opens it. The `tracing` subscriber for the log file is level-filtered by `AIBUS_LOG`; the trace writer is separate and off unless asked for, so it costs nothing normally.

## 7. Provider-side costs

- Prompt caching: the system prompt, project rules and preloaded skills are stable per run and get the cache breakpoint on providers that support it; a fork's first request is byte-identical to the parent's prefix on purpose.
- The advisor packet (01 section 7.1) is capped by `advisor.packet_max_tokens` (400k); beyond that the oldest tool results are elided first, then oldest messages.
- The router is capped at `router.max_per_turn` (10) questions and uses `max_output_tokens = 16`.
- Token counting uses the provider endpoint when there is one, else a per-family heuristic with a 10 % safety margin on `auto_compact_at`.

## 8. Startup

Definition discovery reads at most 1,000 files per scope and stops with a warning. SQLite opens lazily on first query. Provider model tables come from the cache; refresh is background. The TUI draws its first frame before accounts are verified.
