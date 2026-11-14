# 14. The CLI supervisor and the evidence trace (EPIC A, #282)

This is what #282 built for spec A1.2 (`epic_a_aibus_workflow_engineering_v3.md`, sections 5.1, 5.3, 7.1 to 7.3) on top of the Claude Code spike (#281, `docs/spikes/A1.1-claude-code/`). It covers the two crates, the event envelope, the store, the projection and what the supervisor promises about the CLI it runs. The decisions behind it are D-282 in `docs/DECISIONS.md`; the threats and their tests are in `docs/SECURITY.md`.

## Crates

| Crate | What it holds | Depends on |
|---|---|---|
| `aibus-trace` | The envelope (`TraceEvent` and its enums), `Metric` with `DataQuality`, `TraceStore`, `project` and `Projection` | `aibus-core` (the redactor, private files, timestamps), `serde`, `sha2`, `uuid`, `fs4` |
| `aibus-supervisor` | `cli` (discovery, probes, `auth status`, argv, bounded env), `host` (`supervise`), `claude` (`Normalizer`) | `aibus-trace`, `aibus-core`, `tokio` with `process`, `rustix` |

The split is so the inspector (#284) and durable state (#289) can read and replay a trace with no process code and no tokio in sight. Nothing in either crate opens a socket. The supervisor opens no file at all; the store opens only its own run directory.

## The envelope, schema 1

Every field of spec 7.1 is there with the spec's name and meaning, plus four more:

| Field | Meaning |
|---|---|
| `corroborates` | The `event_id` of the event this one confirms. A projection counts only events without it, so a hook that repeats what the stream said (or a duplicate hook) never counts twice, and its evidence still lands |
| `native` | The vendor's own ids (`session_id`, `tool_use_id`) and the native type (`system/task_started`, `hook/PreToolUse`, ...) |
| `source` | Parser id and version (`claude-code-stream-json/1`), `redaction_version`, and the sha256 of the raw line |
| `data` | Small per-kind metadata: names, statuses, sizes, `Metric`s. Never message text, tool input or output |

`kind`, `origin` and `visibility` are exactly the spec's lists. A vendor event that maps to none of them is a `trace_warning` with `reason: unknown_variant` naming the native type.

- `event_id` is a version 8 UUID derived from the attempt, the channel, the raw line's hash and the event's index in that line, so feeding the same line twice gives the same id and the store keeps one. `workflow_run_id` and `attempt_id` are the caller's (v7 is the natural choice).
- `sequence` is assigned by the store, one counter per run, and continues across restarts.
- `observed_at` is aibus's clock when the line arrived. `occurred_at` is the vendor's own timestamp where there is one (`timestamp` on messages, `end_time` on `task_updated`) and `null` everywhere else. It is never a copy of `observed_at`.
- `schema_version` is checked on read: a line of another version is counted (`unsupported_schema`) and not read as an event. A new field is added with `#[serde(default)]` under the same version; a changed meaning is a new version.

### Numbers

Every number is a `Metric { name, value, unit, quality, source }`, `quality` one of `reported`, `derived`, `estimated`, `unknown` (invariant 9). Token counts and durations from `result` and `task_notification` are `reported`; `total_cost_usd` is `estimated` (the CLI's own docs call it a client-side estimate, and it comes with `costBasis: list`); a billed cost is listed as `unknown` with no value; the attempt's wall time is `derived` from aibus's monotonic clock.

## What the Claude Code stream becomes

The table is at the top of `crates/aibus-supervisor/src/claude.rs`. The points the spike made, and where they live:

- **The attempt ends at process exit.** One process prints two or three `result` lines when children run in the background. Each `result` is a cumulative `usage_reported`; `attempt_completed` or `attempt_failed` comes only from `Normalizer::finish`, which the host calls once the process has exited.
- **Exit to status.** Exit 0 with a last `result` that is not an error is `completed`. Exit 0 without a `result`, or with an error one, is `failed`, as is any other exit code. A kill aibus did not send, the wall-time limit and the output limit are `interrupted`; the user's cancel is `cancelled`. A tool that started and never reported back is a `gap_detected` (`tool_outcome_unknown`) and sets `requires_reconciliation`.
- **Children die with the process.** Children still open at exit get an `agent_stopped` that is `inferred` (origin `derived`, status `ended_with_process`). The lead is the process, so its stop at exit is `observed` by the host.
- **`Task` and `Agent`.** `system/init` lists the delegation tool as `Task` and calls name it `Agent`; both become `Agent`.
- **Children are keyed on `agent_id`** (the stream's `task_id`, the hooks' `agent_id`), never on `parent_tool_use_id` alone.
- **Hidden reasoning** (`thinking` blocks with an empty body) is a `gap_detected` (`reasoning_not_exposed`), never a message.
- **Hooks are a second channel.** `hook_line` maps hook payloads the same way. A fact the stream already gave becomes a `correlated` corroboration; a fact the hooks give first is `observed` and the stream's report corroborates it, so the graph is the same in either order. The supervisor does not turn hooks on yet (see D-282).

## The store

```text
<run dir>/events.jsonl            one TraceEvent per line, append-only, 0600
<run dir>/objects/<sha256>.json   redacted payloads, content-addressed, 0600 (only under Retain)
<run dir>/events.jsonl.torn-<n>   bytes a crash left after the last whole line
```

- One run per directory, one writer at a time (an `fs4` lock that goes with the process). Readers never take the lock.
- Each `append` writes all of a batch's lines in one `write` and then `sync_data`, so a crash leaves at most one torn line at the end. `open` moves those bytes aside, cuts the file back to the last whole line and appends a `trace_warning` (`torn_tail_recovered`). A complete line that is not an event is counted and left where it is.
- An event whose id is already in the run is skipped, so appending is idempotent.
- Redaction happens in the store, before the write, through the existing `aibus_core::Redactor`: every string in `data`, and the whole payload. `redaction_version` on each event says which rules applied.
- Payloads are off by default (`PayloadPolicy::MetadataOnly`, spec 10.3). With `Retain { max_bytes }` each line's parsed payload is redacted and written once by hash, and every event of that line points at it (`payload_ref: local://objects/<sha256>.json`); past the budget the payload is dropped with one `gap_detected` (`payload_budget_exhausted`). The raw line of an unknown event is kept this way, which is the spec's "bounded, optional raw archive".
- Limits: an event line over `max_event_bytes` keeps its envelope and loses its `data` (marked `truncated`); past `soft_max_bytes` the store stops writing `message_chunk`, `usage_reported` and `trace_warning` and says so once with a `gap_detected` (`storage_limit`), and every lifecycle event still lands.

The default location is the caller's to choose; #284 will put runs under the XDG state directory next to the session store.

## The projection (offline replay)

`project(&events)` sorts by sequence and rebuilds attempts (status, reconciliation, the last reported usage), agents (role, status, every event that backs them), delegation edges, tools (outcome `completed`, `failed` or `unknown`), gaps and the timeline. It needs no provider and no process. An edge always lists its source events; its `level` is the strongest among them, `cross_checked` says more than one channel backs it, and a child nothing links to a parent is an `unavailable` edge with a `parent_unavailable` gap. `Projection::render` prints it as stable text; the goldens in `fixtures/claude-code/normalized-282/` are those renderings for every spike run.

## The supervisor

- `cli::discover` lists every `claude` it finds in the directories it is given, resolves links, refuses unsafe candidates (relative directory, not a regular file, not executable, writable by others, in a directory others can write, owned by another user), runs `--version` on the rest with the bounded environment and picks the first at or above 2.1.0. Every candidate stays in the result with its reason.
- `cli::probe` reads `--version` and `--help`, requires `--print`, `--output-format`, `--verbose`, `--permission-prompts` and `--setting-sources`, flags any version other than the tested 2.1.311 (it runs, with the probe's `tested: false`), and refuses a help text that says bare mode is the default for `-p`.
- `cli::argv` is always `-p --output-format stream-json --verbose --permission-prompts none --setting-sources <..>`; `--model`, `--max-budget-usd`, `--include-hook-events`, `--forward-subagent-text` and `--agents` are added only when asked for, and asking for one the probe did not find is an error. The prompt is never in argv.
- `cli::bounded_env` is the child's whole environment (see `docs/SECURITY.md`).
- `host::supervise` spawns the absolute executable in its own process group with that environment, writes the prompt to stdin and closes it, and reads stdout and stderr in two tasks into a bounded channel. Limits (`Limits`): line size (an over-long line is skipped to its newline and recorded as `line_too_long` with its size), total stdout (past it the attempt is stopped as `output_limit`), stderr kept, lines a second (past it the loop waits; the channel and then the pipe push back, nothing is dropped) and wall time. A stop is SIGTERM to the group, then SIGKILL after `grace`; after the CLI exits, anything left in its group is killed.
- stderr lines are `trace_warning`s with their text as a payload, except a line that looks like a login or credential prompt, which is `auth_required` with no text at all.

## How it sits next to the rest of aibus

- **`aibus run --json` events** (`crates/aibus/src/events.rs`) are the NDJSON view of aibus's own provider runs and stay as they are. The trace is the record for runs of an external CLI. Both go through the same redactor type. Projecting aibus's own runs into trace events (origin `aibus_scheduler`) is for the scheduler work in #288 and #289.
- **The live-session control of #274** (`aibus-control`, PR #276) drives a TUI session over a Unix socket. The supervisor does not depend on it; a fleet stop (#290) or a control client can end an attempt through the `CancellationToken` that `supervise` takes.
- **Causl.** The trace is a plain file aibus owns, not graph state. The spec 7.4 durability check belongs with #289, which is where Causl snapshots would carry aibus-owned attempt state.

## Not here

The `aibus runs list|inspect|replay` commands and the graph view are #284. Turning hooks on for a live run, the extension manifest and the final extension id are #286. Retrying and resuming attempts are #289.
