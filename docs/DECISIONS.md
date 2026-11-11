# Decisions

The contested calls from the spec panel, written up short. Each entry: context, decision, the alternatives that lost, and why. Numbering is stable; new entries append.

## D-01: Cargo workspace, not a single crate

**Context.** The Rust engineer wanted one crate until it hurt. The architecture expert wanted the engine to compile without ratatui and the TUI without reqwest as a build-time fact, not a convention. The security expert wanted provider dependency trees scoped so `cargo-deny` can reason about them one at a time.

**Decision.** A workspace: `aibus` (bin), `aibus-core`, `aibus-graph`, `aibus-provider`, `aibus-provider-anthropic`, `aibus-provider-openai`, `aibus-tui`, `aibus-testkit`. Provider implementations are separate crates registered by one line in the binary.

**Alternatives.** Single crate with feature flags (rejected: features do not stop `use ratatui` in core; a reviewer has to catch it). Providers as modules in one `aibus-providers` crate (rejected: every provider's deps compile for everyone, and a new provider edits a shared crate).

**Why.** Compile-time boundaries are cheaper than review discipline, and the extra `Cargo.toml` files are the whole cost.

## D-02: Causl contract behind a trait, two backends, in-house mini engine first

**Context.** The requirement is that aibus uses Causl as its state layer, consuming the Rust engine in-process. I verified the engine: `causl-engine-core`, `no_std` + `alloc`, `forbid(unsafe)`, real and usable from a plain binary, but distributed only from a private Gitea at version 0.0.0 with `publish = false` (the GitHub name I was given, `causljs/causl-wasm`, returns 404), and it exposes the state-machine layer (`State`, `Action::CommitWithValues`, `transition_phased_with_callbacks`, `ComputeCallback`, dense `NodeId`, `JsonValue`) rather than the §12 ergonomic surface. String ids, tracked `get`, closures, subscribe ergonomics and `snapshot`/`hydrate` live in the wasm bridge aibus does not want; `Persister` in core is `unimplemented!()`.

The Rust engineer: a young crate with stubbed traits, every value boxed to `JsonValue`, a learning curve for every contributor, and a private git dependency in the supply chain. The architecture expert: the glitch-free derived view model, a structured commit log for free, replay-based testing and snapshot persistence are exactly the hard parts of a TUI orchestrator, and they fall out of the model. The performance expert: fine, as long as heavy values never enter the graph.

**Decision.** `aibus-graph` defines `trait Graph` with the §12 semantics and a conformance suite. Two backends: `mini` (in-house, under 1,500 lines, default) and `causl` (a host layer over `causl-engine-core`, feature `causl-rs`, git dependency pinned by commit or vendored). A differential replay test keeps them byte-identical. The default flips to `causl-rs` at the M6 gate if the crate is obtainable under the supply-chain rules.

**Alternatives.** A plain `AppState` struct with manual invalidation (rejected: every derived value becomes a bug waiting for the case where two fields changed in one event; no audit log; snapshots by hand). `tokio::sync::watch` per field with derived tasks (rejected: glitches by construction, since two watches update at different instants; no atomic multi-field commit). Depend on `causl-engine-core` directly with no trait (rejected: the dependency is private and 0.0.0; the build must work from crates.io alone). Wait for the crate before building (rejected: the trait is cheap and the mini engine is a week).

**Why.** The semantics are the contract and they are stable; the crate's distribution is logistics. A trait lets the semantics land now and the real engine land when it can be pinned. The honest costs stay: boxing across the node boundary (mitigated by handles for heavy values), one more concept for contributors (mitigated by the `Tracker` API making tracked reads the only reads), and a second engine to keep in lockstep (the differential test is the price).

## D-03: Elm loop over a Causl graph, with actors for IO

**Context.** Three options for the event architecture: a classic Elm loop with a plain model; an actor model where every run, provider and widget is an actor passing messages; the hybrid.

**Decision.** One `Msg` channel into the main thread; `reduce(graph, msg) -> writes`; exactly one `graph.commit(intent = msg.name(), writes)` per `Msg`; an effects executor reads derived nodes after each commit and does IO; each run is a tokio task with a mailbox feeding `Msg`s back.

**Alternatives.** Pure actor model (rejected: UI state scattered over mailboxes, no single snapshot, ordering bugs between actors). Pure Elm with IO in `update` (rejected: Causl forbids side effects in compute and the panel wants the same rule for `reduce`, so the boundary has to be explicit).

**Why.** Single-owner graph, one commit per event, IO quarantined in one place. The commit log is then a complete history and replay is trivial.

## D-04: Heavy values live in a side store, the graph holds handles

**Context.** The performance expert's condition on D-02: a 100k-line transcript as a node value would be cloned on every retained commit and serialised into every snapshot.

**Decision.** `Value::Handle { store, id, version }`. Transcript blocks, wrapped-line caches and the session log ring live in `aibus_core::state::store`. The handle version is the change signal; compute reads content through the `Tracker` so the dependency is tracked.

**Alternatives.** Everything in the graph (rejected on measurement: snapshot size and commit cost both scale with content). Everything outside the graph with manual dirty flags (rejected: reintroduces the bugs D-02 exists to remove).

**Why.** The graph stays small and fast; the content stays where it is append-only anyway (the transcript file).

## D-05: Tool allow-lists enforced by the executor, OS sandbox deferred

**Context.** The security expert asked for honesty about what a tool allow-list is.

**Decision.** The allow-list is a boundary at the aibus process boundary: the model never receives schemas it cannot call, and the executor rejects unknown names and specifier mismatches. What a `bash` child does once started is not bounded in v1. An optional OS sandbox is M6, off by default until it has run a release.

**Alternatives.** Ship a sandbox in v1 (rejected: platform-specific, a release blocker, and the design is independent of the rest). Call the allow-list a sandbox (rejected: it is not one).

**Why.** Saying exactly where the boundary is lets users choose `isolation: worktree` and permission modes with open eyes.

## D-06: Credentials in the OS keychain only; never in config, graph or logs

**Context.** Two Anthropic accounts, Kimi, OpenAI, and a graph that snapshots itself to disk.

**Decision.** `keyring` as primary; `env` and `0600` file as explicit alternatives; the config `auth` table has no key field. `Secret<T>` with zeroising drop and no `Serialize`. The graph holds `AccountView` with a `key_ref`. The redactor registers live secrets and also pattern-matches.

**Alternatives.** Encrypted file with a passphrase (rejected: another secret to manage, and the keychain already does this). Plaintext fallback with a warning (rejected: warnings get ignored).

**Why.** The snapshot and commit log are the reason the graph cannot hold secrets; everything else follows.

## D-07: The model decides delegation; keyword triggers are hints only

**Context.** Claude Code lets the model match `description` fields. Continuous-Claude's `skill-rules.json` matches keywords and intent regexes and injects suggestions. The agentic AI expert wanted both; the architecture expert wanted one path.

**Decision.** The lead's `delegate` tool carries the roster; the model picks. `.aibus/triggers.toml` patterns are injected as `[hint]` system reminders, never acted on directly.

**Alternatives.** Deterministic keyword routing (rejected: brittle, and it bypasses the model's judgement that the whole system relies on). No hints at all (rejected: users of Continuous-Claude want the nudge).

**Why.** One decision path, with the hints as a visible input to it.

## D-08: The recording's "fork layer" is a router, not a conversation fork

**Context.** The recording's `JEV · fork layer` counts "forks" and makes small decisions with confidences. Claude Code's "fork" is a sub-agent that inherits the whole conversation. Same word, two things.

**Decision.** aibus has both: `fork` (the conversation-inheriting run) and the `router` role (the fork layer). The router's panel keeps the recording's "forks" counter label; its title defaults to `ROUTER · fork layer` with `router.label` to rename it.

**Alternatives.** Drop one of the two (rejected: both are in the brief).

**Why.** The behaviour in the frames (p values, sharp/split, "runs in code" vs "to Sonnet") is a routing layer; the name is incidental.

## D-09: JSONL transcripts plus a SQLite index, not SQLite for everything

**Context.** Transcripts are append-only streams; recall needs search; sessions need listing.

**Decision.** Transcripts and commit logs as JSONL (zstd); `index.sqlite` with FTS5 as a rebuildable cache for listing and recall.

**Alternatives.** SQLite as the only store (rejected: streaming appends and crash-safety are simpler with append-only files, and the files are greppable). Files only (rejected: recall over thousands of handoffs needs an index).

**Why.** Each format where it is strong; the index being a cache means it can be thrown away.

## D-10: tokio and crossterm

**Context.** Runtime and terminal backend.

**Decision.** tokio multi-thread; crossterm.

**Alternatives.** `smol` (rejected: provider HTTP crates assume tokio). `termion` (rejected: Unix only). `termwiz` (rejected: heavier, and crossterm is ratatui's default and the best-tested path).

**Why.** Ecosystem fit; nothing in aibus needs anything the defaults do not provide.

## D-11: Continuous-Claude's loop natively, without Python, Docker or PostgreSQL

**Context.** The repo implements continuity with TypeScript hooks, a Python wizard, a daemon, and PostgreSQL with pgvector.

**Decision.** Ledgers, handoffs (same YAML schema), memory directories, FTS5 recall, learning extraction inline at `session_end`, workflows as TOML chains. No daemon, no database server, no embeddings in v1; `MemoryIndex` is a trait.

**Alternatives.** Embed the same stack (rejected: the brief is a single Rust binary). Skip continuity until later (rejected: it is one of the three pillars).

**Why.** The ideas port cleanly; the infrastructure does not need to.

## D-02 gate at C2.4 (2026-11-08): `mini` stays the default

**Context.** D-02 flips the default to `causl-rs` only if the crate is obtainable under the supply-chain rules and the differential test has been green for a release. C2.4 built what is needed to check both.

**Decision.** The default stays `mini`. I checked the two conditions:

1. Obtainable under the supply-chain rules: not met. `causl-engine-core` is version 0.0.0, `publish = false`, on a private Gitea, pinned at `a6247b36b38dda4d97017d0577210aa8c724e4f2`. Building it needs a token, which only the manual `causl-differential` workflow has. `cargo deny` restricts sources to crates.io plus that one remote, but there is no token-free source: no public mirror and no vendored copy with a confirmed licence.
2. Differential green for a release: not met. The first green run is 2026-11-08 (conformance 13 of 13, differential 2 of 2, side-by-side bench, clippy 1.99.1, `cargo deny`). The same differential was also shown to fail with one side's commit intents deliberately reversed, then the change was reverted. One green run is not a release.

**Alternatives.** Flip now because the numbers are fine (rejected: causl is 1.4x to 2.4x mini at p50 on `commit_stream` and far under the 4 ms p99 budget, so speed is not the blocker, the supply chain is). Drop the second backend (rejected: it is cheap to keep and the differential keeps `mini` honest).

**Why.** Flipping would make the default build depend on a source nobody but the owner can fetch. The gate is re-evaluated at each release (Q12): flip once the crate is public or vendored under a confirmed licence and the differential has been green across a release.

## F4 error taxonomy (2026-11-11): body code first, reset as a span

**Context.** Issue #112 (epic #108) adds `Quota` and `ContentFilter` to `ProviderErrorKind` and feeds the rate-limit headers to the `Limiter`. Three choices were open.

**Decision.**

1. `ProviderError::from_http` takes the body's `error.code` (else `error.type`) as a fifth argument and checks it before the status: `insufficient_quota`, `usage_limit_reached`, `billing_error` and a 402 are `Quota`, `content_filter*` and `content_policy*` are `ContentFilter`. Anthropic's 400 "credit balance is too low" is the one message match left, because that shape has no code of its own. The OpenAI message sniff is gone.
2. `RateLimitInfo.resets_in` is a span from receipt, not a wall-clock `resets_at`. OpenAI sends spans and Anthropic sends RFC 3339 timestamps, and the limiter runs on tokio's clock, so a span is the one form its paused-clock tests can drive. Zero requests or tokens remaining blocks until `resets_in` without halving the limits; `tighten` stays for the 429 case.
3. A run that ends on `Quota` is exit 4, the same as a rejected credential (the account has to act). `ContentFilter` is exit 5, a provider failure, and never names the account. `StopReason::ContentFilter` replaces `Refusal` for `finish_reason = content_filter`; the run loop does nothing special with it yet.

**Alternatives.** A `Quota` exit code of its own (rejected: 06 section 3 has no spare code and the remedy is the same as for auth). Storing `resets_at` as `SystemTime` (rejected for the clock reason above). Sniffing messages for the content filter too (rejected: it is the sniff this issue removes).

**Why.** The run loop needs to act differently on a spent quota, a refused prompt and a busy minute, and the provider's own code is the only signal that tells them apart.

## D-12: rataflow for the centre column, behind a feature, with no events handed to it

**Context.** The epic (#85) wants the worker cards on a canvas so that more than three of them can be panned to. rataflow is a 0.1 crate with one maintainer and its own mouse model (left drag moves nodes, the wheel zooms).

**Decision.** Phase 1 moves to ratatui 0.30 (#86) and adds rataflow as an optional dependency behind `flow-diagram`, off by default (#87). One file, `diagram.rs`, names it. The flow is rebuilt every frame, locked, at zoom 1, so world cells are screen cells and no flow state outlives a frame. The mapping from our `Theme` to rataflow's palette lives in `diagram.rs` (`diagram::palette`), not in `theme.rs` as the epic first said, so that "only `diagram.rs` names rataflow" stays true and a test can enforce it. No mouse event is forwarded to the flow: the hit map resolves first as before, and panning is done by changing the offset the next frame is built from.

**Alternatives.** Keep a long-lived `Flow` and forward only pan events (rejected for now: state that can drift from the graph, and a locked flow turns the wheel into zoom). Put `palette` in `theme.rs` behind the feature (rejected: a second file naming the crate). Draw the overflow by hand in `widgets.rs` (not tried: the epic's point is to find out if the canvas earns its place).

**Why.** It keeps the default build exactly as it was, keeps the cost of backing out to one file, and keeps the graph as the only place state lives.

## E2 (2026-11-11): what `workflow validate` warns about, and what the schema leaves out

**Context.** #97 adds the workflow JSON Schema, `workflow::validate` and the two CLI commands. A few calls were open when I wrote it.

**Decision.** The last step's summary never counts as unread, since it is the workflow's result. A `steps.<agent>.summary` only counts as read when no step of the same name sits between the writer and the reader, because the later one replaces the earlier. `on_fail` on the first step is not warned about (a retry of the first step has no predecessor to re-run, which `execute` already handles). The schema checks shape only and says so; `prompt` and `gate` use the pattern `\S` so a whitespace-only value fails the way `parse` fails it. Template errors and unknown agents live in a fourth fixture directory, `fixtures/workflows/semantic/`, because the schema passes them and the issue's `invalid/` directory has to fail the schema. `validate` reports every step's roster problem even when `parse` stopped earlier, and the CLI adds `path` to each problem in `--json` (the core type has no path, the file is the CLI's business). `$id` is the only keyword added to the schema checker's `KNOWN` list.

**Alternatives.** Warn on the last step too (rejected: every workflow would carry one). Put `semantic` cases in `invalid/` and drop the schema-fails requirement for them (rejected: it weakens the acceptance check).

**Why.** Each choice keeps warnings to things an author can act on and keeps the schema honest about what it can express.
## G1 (2026-11-11): the command registry and the help screen

**Context.** The registry (`crates/aibus-tui/src/commands.rs`) is now the one place a command is declared. The issue asks for the help screen's command section to come from it, and also for no snapshot file to change. Those two cannot both hold: `shell_help_80x24.txt` and `shell_help_98x46.txt` draw the key map only, so a command section changes them.

**Decision.** The registry gives the help screen its lines (`commands::help_lines()`, with a test), but I did not draw them yet. The two help snapshots stay as they are, and the section lands with the regenerated snapshots on the release PR, where the suites run and I can see the frames. Two smaller calls went the same way: a `needs` miss on a `/fix`-style shorthand now says `/build is not available: no workflow named build` (it used to say `no command /build (try /help)`), and the argument shape check stays inside each entry's `run`, because the existing notices (`/ledger needs a section and text`) are worded per command and the issue says to keep them.

**Alternatives.** Draw the section now and update the two snapshots by hand (rejected: I cannot run the suites on this lane, and a hand-edited frame is a guess). Derive the shape check from the `args` hint (rejected: `/x needs <section> <text>` is a different notice from the current one).

**Why.** Behaviour first, frames with the suites.

## D-13: the card row pans a card at a time, and the flow gets no mouse events (#89)

**Context.** #89 asks for pan on the rataflow canvas, with `cards_x` "now a world offset" and `ViewportChanged` read back from the flow. But `ctrl:scroll:cards:x` is already the index of the first card in the window, the reducer already moves it when the selection leaves the window, and a locked flow turns the wheel into zoom and a left press into a pan without any hit test (D1.1, item 7).

**Decision.** `cards:x` stays a card index. A drag from empty canvas (`Drag::Pan` in `input.rs`) and Shift plus the wheel both change it through the existing `UiIntent::Scroll { sideways: true }`, one card and its gap (20 cells) per step, so the position persists and replays like before. The reducer clamps it to the last window when there are more than three cards (it keeps accepting anything for three or fewer, which the older `ui_intent` test relies on). On the flow path the cards beside the window are nodes at the same 20-cell stride, so a card the window reaches is exactly where the classic row would draw it, and the neighbours show at the column's edges. The flow is rebuilt every frame and never sees a mouse event, so zoom, node drag, connect and box select are unreachable, not just switched off.

**Alternatives.** A cell-granular world offset in the graph (rejected: it forks the reducer's selection logic and the replay fixtures for a smoother drag). Forward pan gestures to a long-lived `Flow` and read `ViewportChanged` (rejected: two sources of truth for one offset).

**Why.** It meets the behaviour the issue wants (more than three cards reachable by mouse and key, clamped, selection stays in view) without a second copy of the pan state.

## D-13: The MCP server uses `rmcp` 3.9 in a new `aibus-mcp` crate (E1, 2026-11-11)

**Context.** Epic #95 adds `aibus mcp`, a stdio MCP server. The spike (`docs/spikes/E1-mcp-server.md`) built a throwaway crate with `rmcp` 3.9.0 (`server`, `macros`, `transport-io`, no defaults) in docker on MARS.

**Decision.** Use `rmcp`, in a new `crates/aibus-mcp` that depends on `aibus-core` and is called from the `aibus` binary. It builds on rustc 1.89 with no `--precise` pin, `cargo deny check` passes with `deny.toml` unchanged (seven new `multiple-versions` warnings: `sha2`, `digest`, `block-buffer`, `crypto-common`, `darling*`), and the closure has no `reqwest`, `hyper`, `rustls`, `ratatui` or provider crate. It answers `server/discover` (`2026-07-28`) and `initialize` (`2025-11-25`) out of the box. Cost: 28 lock entries and about +2.1 MB stripped (+2.7 MB unstripped), +6 s on a musl release build. I took the recommended default here without asking.

**Alternatives.** A hand-rolled stdio server in the style of `aibus-core/src/mcp.rs` (rejected for now: it would own both protocol lifecycles and the schema generation for good; it stays the fallback if rmcp stops fitting the MSRV or a duplicate becomes a `deny` hit). Putting the handler in `aibus-core` or `aibus` (rejected: rmcp's tokio, schemars and chrono would reach every crate, or the handler would only be testable through the binary).

**Why.** The two protocol lifecycles and paging are the expensive part to get right and they work on the first try. A separate crate keeps the new dependencies and the closure test local, and makes the fallback a swap rather than a rewrite.
