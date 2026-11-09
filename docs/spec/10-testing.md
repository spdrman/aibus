# 10. Testing

The strategy leans on two things the architecture gives for free: a fake provider that makes runs deterministic, and a graph whose `Msg` stream can be recorded and replayed.

## 1. Layers

| Layer | Where | Runs on | Tool |
|---|---|---|---|
| Unit | each crate's `src/**/tests` | every push | `cargo test` |
| Graph conformance | `aibus-graph/tests/conformance/` | every push, both backends | `proptest` |
| Engine integration | `aibus-core/tests/` | every push | fake provider, temp XDG dirs |
| TUI snapshots | `aibus-tui/tests/snapshots/` | every push | `TestBackend` + `insta` |
| CLI | `crates/aibus/tests/cli/` | every push | `assert_cmd`, `predicates` |
| Replay | `tests/replay/` | every push | recorded `msgs.jsonl` fixtures |
| Provider contract | `aibus-provider-*/tests/` | every push (recorded), nightly (live, opt-in) | `wiremock` cassettes |
| Benchmarks | `benches/` | nightly and on PRs touching `state/`, `tui/` | `criterion`, CI gate on p99 |
| Fuzz | `fuzz/` | nightly | `cargo-fuzz` on frontmatter, handoff YAML, redactor, tool-pattern matcher |

## 2. Unit

Each module tests its own contract. The ones the panel called out:

- `agent::def::parse`: every frontmatter field, both spellings, unknown keys, the `name` regex, tools as list and as comma string, `bash(git *)` specifiers.
- `agent::discover`: precedence across all five scopes with same-named definitions; `extra_dirs` hook stripping.
- `tools::policy`: allow/deny/specifier matching, including the shell-word normalisation cases in 09 section 2.
- `hooks::io`: exit codes 0/2/other, JSON vs plain stdout, timeouts, `updated_input`.
- `run::result`: capping, escaping of markers, the wrapper header.
- `redact`: the pattern corpus plus registered-secret replacement, including base64 forms.
- `continuity::handoff`: round-trip of the YAML schema; `goal`/`now` required; the Continuous-Claude sample files in `fixtures/handoffs/` parse unchanged.
- `router`: verdict threshold, `p` from logprobs vs self-report, per-turn cap.
- `advisor`: checkpoint triggers (`same_error_x2` normalisation of error text), packet elision order.
- `state::reduce`: every `Msg` variant produces the expected writes and nothing else (table-driven).
- `persist::migrate`: every fixture snapshot version hydrates.

## 3. The fake provider

`aibus_provider::fake::FakeProvider` implements `Provider` with no network and deterministic behaviour driven by a script:

```rust
pub struct FakeProvider { script: Arc<Mutex<Script>>, clock: FakeClock, logprobs: bool }

pub enum Step {
    Text(&'static str),                                  // streamed in 8-byte chunks
    ToolCall { name: &'static str, input: serde_json::Value },
    Usage { input: u64, output: u64 },
    Error(ProviderError),                                // e.g. rate limit, retryable
    Delay(Duration),                                     // advances FakeClock, not wall time
    MatchPrompt { contains: &'static str, then: Vec<Step> },   // branch on the request
    RouterAnswer { choice: &'static str, p: f32 },       // for router runs
    Stop(StopReason),
}
```

Scripts are built per test (`Script::new().then(Text("..")).then(ToolCall{..})`) or loaded from `fixtures/scripts/*.ron`. The fake records every `ChatRequest` it received (redacted), so tests assert on what the model was asked: "the worker's system prompt contains the definition body and not the parent's messages" is a one-line assertion against the recorded request.

`FakeClock` is a `tokio::time::pause()` wrapper; all timeouts, backoffs and the 16 ms coalescer run against it, so a test of five retries with 30 s backoff takes milliseconds.

The fake's credential is the fixed string `fake-key-DO-NOT-LOG-7f3a9c`. A test subscriber captures every log line in every integration test and fails the suite if that string appears anywhere (04 section 7, condition 2).

### 3.1 Recorded cassettes for real providers

Each real provider crate ships `wiremock` cassettes of real responses (recorded once with `AIBUS_RECORD=1` against a live account, secrets stripped): a normal streamed completion, a tool call, a 429 with `retry-after`, a 401, a 529 overloaded, a context-too-long 400, a malformed SSE line. The mapping table to `ProviderError` is asserted against them. Live tests (`--features live-tests`, `AIBUS_LIVE_ACCOUNT=<name>`) run nightly only.

## 4. Engine integration

Each test boots `aibus_core::Engine` with temp XDG dirs, the fake provider, and a `Script`, then drives it with `Msg`s and asserts on the graph and the files. The required set:

- lead delegates to a background worker, result arrives as a tool result wrapped and capped
- foreground delegate blocks the parent's tool call until the child ends
- nesting stops at `max_spawn_depth`; `delegate` is absent from the tool list at the limit
- concurrency cap: the ninth run stays `Queued` until one finishes, `model:orchestrator:runnable` says so
- permission `ask` in `default` mode pauses the run; `PermissionDecided` resumes it; `dont_ask` denies
- `pre_tool_use` hook exit 2 blocks and the model sees the stderr text
- `same_error_x2` triggers the advisor exactly once per pair, the advice lands as a system message on the lead's next request
- router `sharp` applies in code, `split` lands in the lead's request as a note
- `max_turns` yields `Partial`, `resume_run` continues with history intact
- compaction writes an auto-handoff and a `compact_boundary`, the request after it is shorter
- `session_end` writes a handoff with the outcome, extracts learnings, indexes them, `recall` finds them
- two Anthropic accounts: a definition with `account: anthropic-personal` uses that credential (the fake runs as two accounts with two keys; the recorded request carries the right one)
- credential never appears in any file under the temp XDG dirs (grep after every test)
- a retryable provider error retries with backoff and succeeds; a non-retryable one fails the run with exit mapping 5
- budget: `max_cost_usd` stops the run with `Budget`

## 5. Replay

`tests/replay/<name>/` holds `msgs.jsonl` (recorded `Msg` stream, redacted), `script.ron` (the fake's script that produced it) and `snapshot.hash`. The test replays the stream through `reduce` and both graph backends and asserts:

1. the final `snapshot()` hash equals the recorded one (determinism, theorem 1);
2. the mini and causl backends produce byte-identical snapshots and identical `commit_log` intents (the differential gate);
3. every `view:` node at the end equals the recorded one.

Recording a new fixture is `aibus run --record tests/replay/<name> ...` with the fake provider. Replay runs in under a second per fixture; there are about 20.

## 6. TUI snapshots

`ratatui::backend::TestBackend` at 98x46 (the recording's size), plus 80x24 and 120x50. Each test builds a graph state (via a replay fixture or by hand), renders once, and `insta::assert_snapshot!` compares the buffer as text. Styles are checked with `assert_snapshot!` on a second rendering that prints `fg/bg/modifier` per cell for the key widgets (cards, fork layer rows, status bar), so a colour regression fails.

Required snapshots: the recorded screen at frames f01 (before_plan active, no split), f02 (split row), f03 (search done), f04 (same_error_x2, search border purple), f09 (before_done, return box purple), f21 (audit done); the permission modal; the transcript view with a collapsed and an expanded tool call; the agents screen; 80x24 with the rail collapsed; `ascii = true`; `mono` theme.

Animation frames are made deterministic by injecting the tick counter.

## 7. CLI

`assert_cmd` tests per subcommand: `--json` shapes validate against JSON schemas in `docs/schemas/`; exit codes per 06 section 3; non-TTY behaviour by running with piped stdio; `account add --key-stdin` stores into a fake keychain (`keyring`'s mock credential store) and `config show` never contains it; `agent validate` on the fixture corpus (valid, invalid, warnings).

## 8. Graph conformance

`aibus-graph/tests/conformance/` is run against both backends via a generic `fn suite<G: Graph>(mk: fn() -> G)`:

- determinism: random DAGs (proptest, 1,000 trials minimum), random commits, same inputs give same values
- glitch-freedom: diamond shapes, a derived over two deriveds of one input sees them agree at every commit
- atomicity: a compute that returns an error mid-commit leaves `snapshot()` byte-identical and fires no subscriber
- time: `now` advances by exactly one per commit and per hydrate
- subscriber order: insertion order per node, then changed-node order
- dynamic deps: a derived that stops reading an input no longer recomputes when it changes
- retention: `read_at` before `oldest_retained` returns `Evicted`
- layering: no `view:` node depends on `ctrl:` through a `model:` node (walk `dependencies` from every `model:` node and assert no `ctrl:` ancestor)

## 9. Benchmarks as gates

`criterion` benches with saved baselines in `benches/baselines/` per CI runner class. A PR that regresses `commit_stream`, `render_dashboard` or `hydrate_snapshot` by more than 20 % at p99 fails. The reference session fixture (100k lines, 8 runs, 2,000 log rows) is generated deterministically by `aibus-testkit::big_session()`.

## 10. What is not tested automatically

Real keychain behaviour per OS (manual checklist in `docs/RELEASE.md`), real terminal emulators (manual smoke on iTerm2, Terminal.app, kitty, WezTerm, Windows Terminal), and the OS sandbox (M6, when it exists).
