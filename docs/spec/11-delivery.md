# 11. Delivery plan

Milestones are cumulative. Each one ends with something runnable and a tag. Estimates are deliberately absent; the order and the exit criteria are what matter.

## 1. Milestones

### M0: skeleton and contracts

- Workspace with all crates compiling, `clippy -D warnings`, `cargo-deny` and `cargo-audit` in CI, MSRV pinned, `#![forbid(unsafe_code)]`.
- `aibus-graph`: the `Graph` trait, the `mini` backend, the conformance suite green.
- `aibus-provider`: `Provider` trait, `AccountRegistry`, `CredentialStore` with keyring and env and file backends, the fake provider with scripts and `FakeClock`.
- `aibus-core`: ids, `AgentDef` parsing and discovery, config loading, XDG paths, the redactor, `Msg`/`reduce` for the agent and account facts.
- `aibus version`, `aibus config path|show|validate|init`, `aibus agent list|show|validate`, `aibus account add|list|test|remove`, `aibus doctor`.
- Exit: `aibus account add fake --provider fake --key-stdin` stores into the keychain; `aibus agent validate` on the fixture corpus matches the expected output; no secret in any file under the temp dirs in the test suite.

### M1: one run, headless

- The run loop, `Conversation`, tool executor with `read`, `write`, `edit`, `glob`, `grep`, `ls`, `bash`, tool policy enforcement, permission modes, `pre_tool_use`/`post_tool_use`/`stop` hooks.
- `aibus-provider-anthropic` with cassettes; `ProviderError` mapping; retries.
- `aibus run` with NDJSON output, the stdin control channel, exit codes.
- Transcript JSONL, session dir, `session.json`, SQLite index, `aibus session list|show|export`.
- Exit: `aibus run --agent explore "map this repo"` works against a real Anthropic account and against the fake; the replay fixture for it is recorded and green.

### M2: sub-agents

- `delegate`, `fork`, `resume_run`, `send_message`, background and foreground, nesting and concurrency limits, `model:orchestrator:runnable`, result wrapping and capping, worktree isolation, `subagent_start`/`subagent_stop` hooks, built-in `explore`, `plan`, `general`, `search`, `grep`, `audit`.
- `aibus-provider-openai` with the `openai`, `kimi` and `openai-compatible` dialects; account selection chain with two accounts of one provider tested.
- `aibus-graph::causl` host backend behind `causl-rs`, the differential replay test, `docs/BUILD.md` on how to obtain the crate.
- Exit: a lead on Anthropic delegates to three workers on Kimi in parallel, one in a worktree, results come back wrapped; the same session replays identically on both graph backends.

### M3: the dashboard

- `aibus-tui`: the recorded screen (header, legend, rail, lead box, fork layer, cards, return box, session log, prompt, status bar), transcript view, permission modal, palette, help, themes, key map, `TestBackend` snapshots for the frame set in 10 section 6.
- Streaming pipeline with coalescing and the block store; the `commit_stream` and `render_dashboard` benches with baselines.
- Snapshot persistence, `hydrate` on resume, `aibus session resume`.
- Exit: the dashboard matches the f01/f02/f03/f04/f09/f21 snapshots; a 100k-line replay fixture renders under budget; `aibus session resume` restores the dashboard mid-session.

### M4: advisor and router

- Advisor role, the three checkpoints, packet builder with elision, advice injection, the rail's typewriter and connectors, `checkpoint` hook.
- Router role, the three question sites, `sharp`/`split`, logprob-based `p` where available, the fork counter, the fork layer panel.
- Compaction with auto-handoff.
- Exit: the `same_error_x2` fixture shows the rail highlight and the purple card border in the snapshot; the router fixture shows a split row; both headless events appear in `--json`.

### M5: continuity

- Ledger, handoff (write, load at start, outcome prompt), memory scopes and tools, FTS5 recall, learning extraction at `session_end`, workflows (`fix`, `build`, `review`, `explore` shipped), triggers as hints, `agent import` from `.claude/agents`.
- `prompt` and `http` hook types, `web_fetch`, MCP tools (`mcp:<server>:<tool>`) via the stdio transport.
- Exit: a session ends, the handoff YAML parses with the Continuous-Claude schema, the next session's lead sees `[handoff] goal/now`, `recall` finds last session's learning; `/fix` runs the chain with gates.

### M6: hardening and release

- Optional OS sandbox for `bash` (`bwrap`, `sandbox-exec`), OAuth auth method for providers that offer it, Bedrock/Vertex transport for Anthropic.
- Prune, log rotation verification over a simulated 12-hour session, memory budget test, fuzz targets running nightly.
- Signed release binaries for macOS (arm64, x86_64), Linux (x86_64, aarch64, musl), Windows (x86_64, with the TUI tested on Windows Terminal), Homebrew tap, `cargo install aibus`.
- Decision gate: switch the default graph backend to `causl-rs` if the crate is obtainable under the supply-chain rules and the differential test has been green since M2; otherwise `mini` stays default and the gate is re-evaluated per release.

## 2. Out of scope for v1

Explicitly not in v1, so nobody designs around them by accident:

- An OS-level sandbox as the default (M6 ships it optional; default on is a later decision).
- OAuth / subscription logins (M6 adds the method; v1 is API keys).
- Vector/embedding memory, pgvector, a learning daemon (Continuous-Claude's PostgreSQL memory). `MemoryIndex` is a trait; FTS5 is the only v1 impl.
- The TLDR five-layer code analysis (AST, call graph, CFG, DFG, PDG) and the semantic index. aibus ships `grep`, `glob`, `read`; structural search is a tool someone can add.
- The math and proof stack (SymPy, Z3, Lean).
- Braintrust/Perplexity/Nia/Morph/Firecrawl integrations.
- Agent teams across processes (Claude Code's teammate feature), cross-terminal file claims and a shared blackboard. Two aibus processes share only handoffs and the index.
- Windows keychain beyond what `keyring` provides, and Windows OS sandboxing.
- A plugin system for third-party agents, tools or providers loaded at runtime. Providers are compile-time crates; agents are files; tools are compile-time.
- Remote or cloud execution of runs.
- Image input beyond passing through what a provider accepts; no screenshots, no computer use.
- Voice, notifications, desktop integration.
- Localisation.

## 3. Dependencies the plan assumes

| Crate | Use | Risk |
|---|---|---|
| `ratatui`, `crossterm` | TUI | low |
| `tokio`, `tokio-util` | runtime, cancellation | low |
| `reqwest` (rustls), `eventsource-stream` | providers | low |
| `serde`, `serde_json`, `serde_yaml`, `toml` | formats | low; `serde_yaml` is unmaintained, `serde_yml` or `yaml-rust2` is the fallback, decided at M0 |
| `clap` | CLI | low |
| `keyring` | credentials | medium: platform backends vary; `doctor` reports |
| `rusqlite` (bundled, fts5) | index | low |
| `ulid`, `smol_str`, `bytes`, `zstd`, `fs4`, `notify`, `etcetera`, `globset`, `grep` (ripgrep's crates), `tracing`, `tracing-appender`, `thiserror`, `zeroize`, `insta`, `proptest`, `criterion`, `wiremock`, `assert_cmd` | utilities and tests | low |
| `causl-engine-core` | graph backend (feature) | medium: private Gitea source, version 0.0.0, `Persister` unimplemented; mitigated by the trait and the mini backend |
