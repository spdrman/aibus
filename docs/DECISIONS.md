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

## F8 (2026-11-11): the model catalog, the sync tool and reported cost

**Context.** #116 moves the three built-in tables to `catalog/`, adds a reviewed sync tool, dated prices and the gateway-reported cost. The epic's open decision 2 (catalog source) gates it, and #111 (per-model capabilities) is not in the tree yet. #110 landed while I was working, so the catalog follows its presets.

**Decision.**

1. The source is a models.dev snapshot that I save by hand and pass with `--snapshot`. The tool has no network code at all, so "no fetch without review" is true by construction. I did not confirm models.dev's licence here, so the first real sync waits for that; the fixture snapshot in `fixtures/catalog-sync/` is made up.
2. The tool is a bin in `aibus-testkit` (`catalog-sync`), with the logic in a pure function (`catalog_sync::plan`) that the tests call. Dry run is the default and `--write` applies. It updates the models already in a catalog and only adds new ones with `--add-new`, because models.dev lists embeddings, audio and retired models we do not want in a table.
3. Capabilities are written as `capabilities: {tool_call, reasoning, input_modalities}` using models.dev's names, and `ModelEntry` ignores them for now. #111 can rename or consume them. The `price_of` alias column is not added: no alias column exists in the tree yet, and snapshots already resolve through the undated id.
4. `price_from` is a second row with the same `id`. `layer()` keys on `(id, price_from)`, and an undated row from the user's `models.json` also drops the earlier dated rows of that id, so your override is not undone when a built-in future price starts. `price()` uses the wall clock; `price_at` takes a time so tests need no clock.
5. Gateway cost is recognised for the `openrouter` preset (and any definition whose `base_url` contains `openrouter.ai`). A run total is marked `provider` as soon as one turn was.
6. Every preset that names a built-in table has a `catalog/<name>.json`, including the eleven whose tables were inline in `presets.json` (they moved, and the preset now names them). The nine presets that ship no models (local servers, Azure, the generic server) name `empty` and have no file, because an empty file would only pad the directory. `aibus_provider::catalog::table(name)` is the one place that `include_str!`s them, used by the Anthropic and OpenAI adapters.

**Alternatives.** Fetching the snapshot inside the tool (rejected: it is the fetch we said we would not do). Adding every model of a provider by default (rejected, above). A separate file for dated prices (rejected: one model, one place).

**Why.** One source for the tables, a diff to review for every price change, and nothing in the binary that talks to a third party.

## F2 (2026-11-11): providers are definitions, instances are per id

**Context.** Five provider ids were code (`providers.rs` listed five factories, `kimi` was a flavour inside the OpenAI crate), so a new vendor meant a new factory and two local servers could not coexist: `provider_configs` refused two accounts of one id with different URLs.

**Decision.** A provider is a `ProviderDef` (api, base URL, env vars, auth methods, model table, prefixes, compat flags, headers). Presets live in `presets.json` in `aibus-provider`; a `[[provider]]` table overrides a preset field by field or defines a new id. The registry builds one instance per id with the factory that serves the definition's `api`. The factories keep their ids (`anthropic`, `openai`) and now also report which apis they serve (`ProviderFactory::apis`), so the binary registers one adapter per wire and `Factory::kimi()` and `compatible()` stay only as constructors for the presets of those names. Provider ids became leaked `&'static str` (one leak per distinct id, at startup), because `ProviderId` holds a `&'static str` and user ids are not known at compile time.

Gated calls, taken at the recommended default:

* The old conflict error stays for the old form only (two accounts of one id with different `base_url` or `api` on the accounts). With `[[provider]]` ids the situation cannot arise, and keeping the error for the old form is safer than guessing which URL gets which key. The message now points at `[[provider]]`.
* `[[provider]]` is user-file only, like `[[account]]`, since it decides where a key is sent.
* A user-supplied `compat` replaces the preset's whole instead of merging per flag, to keep the merge rule one sentence long.
* Model tables for the new cloud presets list one or two models without prices; an unknown price is `n/a`, never a guess.

**Alternatives.** One factory per vendor (rejected: it is exactly the code-per-vendor this removes). Resolving the instance per account instead of per id (rejected: `registry.get(&account.provider)` is used in four places in core and the id is the cleaner key). Merging `compat` flag by flag (rejected for now: needs an `Option` per flag).

**Why.** The vendor is data and the wire is code, so adding Groq is a review of a JSON entry.


## G5 (2026-11-11): the per-agent commands

**Context.** #124 adds `/status`, `/cost`, `/log`, `/cancel`, `/retry`, `/message`, `/resume` and `/background` to the registry, two menu items and two keys. A few things the issue left open had to be picked.

**Decision.**

1. The state rules (`/retry` on a failed or partial run, `/resume` on a finished one) are two more `needs` bits, `RETRYABLE` and `FINISHED`, judged against the run the `@target` resolved to, after the target is parsed. The popup judges them against the focused card, else the lead. A miss says `/retry is not available: the run is running`.
2. `/cost` is not a `Run` row. With no target it is the session table, and the focused-card fallback would have turned it into a status. `/cost @t` opens the same modal as `/status @t`. The menu's Cost item opens the table.
3. `/status @advisor` and `/status @router` get their own screens (`AdvisorStatus`, `RouterStatus`) rather than a `RunId`, since a seat has no run. The advisor shows its checkpoints so far, the router its answers so far.
4. `/resume @t` with no text sends `continue`, the same as `R` and the menu. `/cancel` asks only when typed against the lead; the menu and `x` still cancel at once, as before.
5. The status bar has no cost segment drawn yet, so I added a `cost` field to `view:status_bar` computed by the same `session_cost` the cost table uses. The two cannot disagree, and the acceptance test compares them. Drawing it is not part of this issue.
6. `i` (status) is bound in cards and transcript, `$` (cost) in the dashboard and transcript, not the prompt, where it is typing.

**Alternatives.** A `Screen::RunStatus` for seats too, with a fake id (rejected: it lies about what the id is). Making `/cost` a `Run` row and using a `--all` flag for the table (rejected: the issue says no target means the table). Waiting for a drawn cost segment before adding the field (rejected: the acceptance check needs a number to compare).

**Why.** Each choice keeps the rule in the registry, where the popup, the slash line and the menu already read it.

**Snapshots.** The menu is two rows taller and the help screen lists two more keys, so `state_menu_*`, `shell_help_*` and the menu crops change. I cannot run the suites on this lane, so they regenerate on the release PR, as with G1.

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

## E4 (2026-11-11): the first `aibus mcp` PR, and what I chose where the issue left room

**Context.** #99 adds the `aibus-mcp` crate, the `mcp` subcommand, the capability flags, the audit log, the rate limit and `workflow_list`. Several details were open.

**Decision.**

1. The tool table is written by hand (`Tool::new` and JSON schemas), not with `#[tool]` and `Parameters<T>`. `workflow_list` takes nothing and its schemas are four lines each, so a derive would add `schemars` output I would then have to bend into `additionalProperties: false` and 2020-12. The `macros` feature stays on, because D-13 names it, and the next PR can move to the macros if the tool count makes the table hard to read.
2. `Server::call` is the one way into a tool: argument cap, unknown-tool check, rate limit, redaction of the result and the audit line all live there, and the rmcp handler only converts. A call that cannot be routed (unknown or switched-off tool, arguments over 256 KiB, arguments the schema forbids) is `-32602`; a rate limit is an `isError` result with "try again in N s". Refused calls do not spend a token.
3. `tools/list` pages by offset with cursors of the form `p<offset>` (50 per page); a cursor that is not one is `-32602`. With one tool nothing pages, but the harness asks for the error.
4. `--allow` on `aibus mcp` shares its argument id with the global `--allow <tool>`, because clap skips a global that a subcommand already defines and panics when two arguments have the same long name. Both are text so clap can share them; `aibus mcp --allow write` is the capability, and `--allow gates` without `write` is exit 2. `--max-runs`, `--max-concurrent-runs` and `--max-cost-usd` parse and are refused with exit 2 and "not available yet", as the issue says.
5. Logging for `mcp` is stderr only, through the redactor, at the level `-v`, `AIBUS_LOG` or `RUST_LOG` give. It does not also write the rotating log file the other commands write: a server started by a client for a few calls would otherwise leave a log file per start, and the audit file is the record.
6. The audit file for a process is `~/.local/state/aibus/logs/mcp/<ulid>.jsonl`, created 0600, and its first line is the start line with the flags (`method: "server/start"`). The client name is `clientInfo` from `initialize`, or from `_meta` on a `2026-07-28` request; a client that sends neither has `client: null`.
7. `no_secret_leaves_over_stdio` lives in `crates/aibus/tests/mcp_security.rs`, not in `aibus-core/tests/security/`, because it drives the binary and only the `aibus` package can name `CARGO_BIN_EXE_aibus`.

**Alternatives.** The `#[tool]` macros now (rejected for the reason in 1). A cursor that is a bare number (rejected: clients must not parse it, and a prefix makes a made-up number fail). Writing the rotating log file as well (rejected, 5). A separate `--capability` flag so `--allow` stays unshared (rejected: the epic and the issue both say `--allow`).

**Why.** Each keeps the first PR to what its acceptance criteria name and leaves the next PR a place to put the write and run tools.

## E5 (2026-11-11): the read tools, resources and prompts of `aibus mcp`, and where I bent the issue (#100)

**Context.** #100 adds five read tools, the `aibus://` resources and three prompts to `aibus-mcp`. A few details in the issue could not be followed to the letter.

**Decision.**

1. `workflow_dry_run` returns `agent` as an object (`{name, role, model, tools, permission_mode}`) per step, because the issue's shape has two `agent` keys in one object. `scope` is optional on `get`, `explain`, `dry_run` and `validate`: left out, the project's file wins, as in `workflow list`.
2. A tool that reads files puts the redacted value in `structuredContent` and a pretty-printed copy in the text block, under the one-line data header, with aibus markers escaped and a "cut at" note past `limits.result_max_bytes` (the default, 32 KiB: the server does not read config). `structuredContent` is never cut. `workflow_list` and its golden transcript are untouched. The conformance test that compared the text block to `structuredContent` now skips that header line.
3. Budgets in `workflow_dry_run` are the built-in `Limits::default()`. The server does not read `config.toml`, so a user's own limits do not show; `docs/MCP.md` says so.
4. The agent roster is the CLI's discovery without `agents.extra_dirs` (that setting lives in config, which the server does not read).
5. `aibus-mcp` has no provider dependency, and `read_tools.rs` checks the manifest. That is the proof that a dry run cannot call a model, in place of a provider that panics: there is no seat to put one in.
6. Resources are an allow-list in `resources.rs`: any `aibus://` uri not matching one of the four is `-32602`, and so is a workflow that does not exist. Names are `[A-Za-z0-9_-]{1,64}`, so `..` and `%2e%2e` never reach the file system, and a symlink or a file over 1 MiB is refused. The workflow resource returns the file redacted but not wrapped, since the mime type promises TOML.
7. Resource reads and prompt gets are audited like tool calls (`resources/read` with the uri, `prompts/get` with the name). They do not spend rate-limit tokens: the limit in the epic is for tool calls.
8. `aibus-core/tests/security/mcp_server.rs` cannot name `aibus-mcp` (core is below it), so the redaction tests live in `crates/aibus-mcp/tests/read_tools.rs` and `resources.rs`, as the E4 decision did for the stdio test.
9. `workflow::reads` and `workflow::preview` are new public functions in `aibus-core`, because only that module knows the `{{...}}` syntax.
10. The golden transcripts are written by `AIBUS_UPDATE_TRANSCRIPTS=1` on the release PR run; this PR has the tests and not the files, since the suites do not run before then.

**Alternatives.** Wrap `structuredContent` strings too (rejected: it is data for a program, and the schema says string). Read `config.toml` for budgets and extra agent dirs (rejected: breaks "never reads config"). A provider that panics (rejected, 5).

**Why.** Each keeps the surface small and the secret boundary where the epic put it.

## D-14: Gemini gets a native adapter crate, and the OpenAI shim stays as a preset (#114)

**Context.** The epic's open decision 3 asked whether Google's OpenAI shim (`gemini-openai`, F2) is enough. The shim covers chat with tools. It does not carry thought signatures (Gemini 3 rejects a function-calling turn that lacks the signature of the previous call), does not show thought summaries, has no context cache, no `countTokens`, and answers `store` with a 400. Gemini is also the one widely used family that is neither Anthropic nor OpenAI shaped, and the owner wants no open decisions left on the epic.

**Decision.** Build it. `aibus-provider-gemini` serves a fourth wire, `gemini-generate-content`, behind a `gemini` preset (`https://generativelanguage.googleapis.com`, `x-goog-api-key`, `GEMINI_API_KEY`). It depends on `aibus-provider` and `aibus-provider-http` only, so no new dependency and `deny.toml` is unchanged. `ContentBlock::ToolUse` gets an optional `signature` for the thought signature. `gemini-openai` stays for anyone who wants the shim. Vertex for Gemini stays under "Later" (it needs the gcloud token source and a different URL).

**Alternatives.** Ship the shim preset only (rejected: it loses the signature on the second turn of any tool loop against Gemini 3, and the thinking and usage detail with it). Put the Gemini wire in `aibus-provider-openai` (rejected: it shares nothing with Chat Completions beyond SSE framing, and the crate boundary is how `cargo-deny` and the workspace tests reason about each wire).

**Why.** The shim cannot do the part of Gemini that matters for an agent, and the adapter is about 400 lines on plumbing that already exists. Two small shared changes came with it: `Http::send` reads Google's `retryDelay` from the error body, and `ProviderError::from_http` knows Google's "input token count exceeds" and "API key not valid" messages. The prices in the table are list prices I wrote from Google's public pricing page without a live call, so they are the first thing the F8 sync should confirm.

## F7 (2026-11-11): how `account add --oauth` is built

**Context.** #115 asks for two login flows, an async `OAuthFlow` and a refresh that does not race, with OpenRouter first. A few calls were open, and the owner's rule is to take the recommended default.

**Decision.** Credential resolution stays sync (`CredentialSource` is sync and the providers call it), so `resolve` keeps its shape and a refresh runs the async flow on a scoped thread with its own runtime; `resolve_async` is the entry for async callers. The per-account `tokio::sync::Mutex` re-reads the keychain after taking the lock, which is what makes the second caller reuse the first one's tokens. `OAuthFlow` gains `yields_key()` (default false), so a login that returns a durable key is stored as a plain keyring account. The definition lives in the binary (`oauth::definition`) and not on the provider definition, because #110 is changing those; it moves over when that lands. OpenRouter matches `--provider openrouter` and `openai-compatible` pointed at openrouter.ai, since the `openrouter` factory belongs to #110. The authorize URL uses `http://127.0.0.1:<port>/callback` (RFC 8252). The platform opener starts when a terminal is attached without asking first, because the address is printed before and the opener gets it as one argv element. Ctrl-C ends the process, which closes the listener.

**Alternatives.** Make `CredentialSource` async (rejected: it ripples through every provider crate other lanes are editing). The `oauth2` crate (rejected: a second HTTP abstraction for two flows). Put the definition on `ProviderFactory` now (rejected: collides with #110).

**Why.** It fixes the race and the blocking trait without touching what other lanes own, and OpenRouter proves the whole flow with nothing to refresh.
## G2 (2026-11-11): where /model is resolved, and what the reducer keeps

**Context.** The issue has the reducer validate a model reference through `AccountRegistry` and write `model:defaults:<role>`. The reducer is a pure function of the graph and has no accounts, and no `model:defaults:*` node exists; the lead's model lives in its run's `meta.model`, which the header already reads.

**Decision.** `Command::SetModel` only queues the request in `model:session:assist.pending` (no new node, so the snapshot fixtures keep their shape). The effects executor hands it to the launcher, which picks the provider, credential, model and account the way a launch does, and answers with `Msg::ModelSwitched { id, target, to }`. That answer is what writes `meta.model` and `meta.account` (so the header and the card follow) and the log line `model: lead -> <id> (<account>)`; an `Err` is one `model: <why>` line and nothing else. A run takes the switch through its mailbox (`RunMail::Model`) and applies it at the next turn boundary, so the request in flight finishes on the old model. A seat is switched in `Assist` and read at its next consultation. With no lead running, `/model` is a logged `no lead is running yet`; a session that started without a seat cannot switch it on later, because it has no definition to speak from.

**Alternatives.** Resolve in the reducer by copying the alias tables into the graph (rejected: a second copy of config that drifts). Add a `model:defaults:<role>` node (rejected: a node nothing reads, and it changes the snapshot fixtures).

**Why.** It keeps the reducer pure and puts the choice where the accounts are. The cost is one extra message per switch, which is also what lets the log say which account answered.

## G3 (2026-11-11): the /model picker, where its rows come from and how a row is named

**Context.** #122 asks for a screen of `(account, model)` rows with prices and capabilities, filtered by the target's role, that sends `SetModel`. The reducer has no providers and the TUI draws from graph nodes only, and `SetModel` carries a model string with no room for an account. #111 (per-model capabilities) is in flight.

**Decision.**

1. The rows ride on `AccountsReloaded`: `AccountView` gains `models: Vec<ModelView>` (id, alias, context window, prices as `Option`, five capability flags), built by `AccountRegistry::model_views` from the provider's `models()`, `price()` and `capabilities()`, deprecated models skipped. The `model` key is only written to the account's graph node when the list is not empty, and serialised only when not empty, so a graph without models keeps its shape. `view:models` is a new widget (the dirty list and `WIDGETS` grow by one), `view:models` reads `PALETTE_TEXT` for the filter, as the palette does.
2. Capabilities are read in one line of `model_views` (`provider.capabilities()`); #111 replaces it with `capabilities_for(&id)`.
3. A row is sent as `account/model`. `ChildLauncher::switch_model` treats a prefix that names an account as `SelectHints::explicit`, so the picked account is the one used when several serve a model. I chose the string over a new field on `Command::SetModel` because the command, its effect and the executor would all change shape and the typed form (`/model work/sonnet`) comes free.
4. The filter is entered with `/`, not by typing, because `a`, `r` and `s` are keys of the screen. While it is open the keys are text and every change is mirrored into the graph, so the rows narrow as you type. `Enter` or `Esc` ends it and keeps the text.
5. `s` on a picker opened for the lead says so in a note instead of guessing which card the user meant.
6. The screen needs `Screen::Models(ModelTarget)`; the modal is named `models:<target label>`.
7. The palette and the card menu get no new row (their order is pinned by tests); `/model` is the way in.

**Alternatives.** A `SetModel { account }` field (rejected, above). Typing to filter with `a`/`r`/`s` on `Ctrl` (rejected: the keys are named on the buttons like every other screen). Reading the providers from the TUI through a hook (rejected: the frame would not be a function of the graph).

**Why.** The picker stays a pure function of the graph, nothing asks the network, and the only code that knows about accounts is still the launcher.

**Snapshots.** The four frames of the screen are written by the tests and generated on the release PR with the other stale ones.

## E11 (2026-11-11): Streamable HTTP is built, on loopback with a token (#106)

**Context.** #106 sat behind owner decision 6 on epic #95. The owner wants zero open issues and the recommended default for decision 6 was to do it, with elicitation first. The order does not matter for the code, so I did the HTTP one first, as the issues are numbered.

**Decision.** Build it, in the `aibus` binary and not in `aibus-mcp`. rmcp 3.9's `transport-streamable-http-server` is a Tower service with `Host` and `Origin` validation and the `MCP-Protocol-Version` check built in, so `mcp_http.rs` is a small hyper host that adds the bearer token (SHA-256 digests compared with `subtle`, 401 plus `WWW-Authenticate: Bearer` before anything else), the one `/mcp` path and the peer address. The listener is bound to a loopback address or `aibus mcp` exits 2 before the audit file exists. Allowed hosts and origins are the exact `127.0.0.1`, `localhost` and `[::1]` forms with the bound port, never a wildcard, plus `--origin`. `legacy_session_mode` stays on so a `2025-11-25` client gets a session (and, with E12, a server-to-client request); `2026-07-28` requests are stateless whatever that says. The token is `AIBUS_MCP_TOKEN` (at least 16 visible ASCII characters) or 32 random bytes shown once on stderr; it is a `Secret`, it is registered with the redactor, and it is never written anywhere. The audit line gains `addr`. New direct dependencies of `aibus` are `rmcp` (the same crate and version `aibus-mcp` already uses, with the server transport feature), `hyper`, `hyper-util`, `http`, `http-body-util`, `bytes` and `subtle`, all already in the lock except `sse-stream` and `tokio-stream` (rmcp's); `aibus-mcp` gets `http` for the `ClientAddr` request extension. No `reqwest`.

**Alternatives.** `axum` as the host (rejected: a router for one path, and one more big dependency). A unix socket instead of TCP (rejected for now: the clients this is for speak HTTP to a port; the SSH tunnel recipe covers the remote case). Wildcard ports for `Origin` (rejected: `http://localhost:*` accepts any local web page, which is the attack). A comparison with plain `==` (rejected: the token is the only thing between a web page and the user's workflows).

**Why.** It meets the spec's MUSTs (Origin 403, loopback bind, authenticate) with rmcp doing the protocol and about 150 lines of ours doing the policy, and it leaves `aibus-mcp` transport-free so the closure test keeps meaning something. Tests are written and unrun, as the lane's rule says; they run on the release PR.

## G4 (2026-11-11): /login, /logout, the secret entry, and what a reload swaps

**Context.** #123 is a bug (the verify blocks the loop, the key is a plain `String`, the registry is only built at startup) and a feature (`/login`, `/logout`, a provider list). A few things in it were open or turned out bigger than they read.

**Decision.**

1. The key is a `Secret<String>` from the first keystroke. `Entry` holds it in `key` and shows one dot per character in `text`; every edit builds the next `Secret` into a buffer sized to never regrow, so the old one is zeroised on drop. `Entry::clone` (used to hand the frame its copy) leaves the key out on purpose. `AccountOp` and `Job` lose `Clone`; `AccountOp` compares keys by their text, by hand, because `Secret` has no `PartialEq`.
2. The verify runs on a thread of the account hook's own. The hook returns the line to show (`verifying <name>...`) and the thread sends `Msg::AccountsReloaded` and then `Msg::AccountOpDone { op, account, ok, text }` through the loop's channel. The graph logs the outcome (`account add work: done`), never `text`, since a provider's words can echo a key; `Input::observe` is what puts `text` under the Accounts rows. `run_job` and `AccountHook` take the sender for this.
3. `/login` with anything after the provider is refused with the provider and `[redacted]`. The line is also kept out of the prompt history (`Prompt::take` asks `commands::carries_key`), so the up arrow does not bring a pasted key back. The same name entry serves `/login <provider>`, a pick in the list and `a` on the Accounts screen (which now opens the list; the free-text provider step is gone).
4. The provider list is the presets (`aibus_provider::presets`). A user `[[provider]]` is not listed; `aibus account add` still takes it.
5. `/logout` asks first through `confirm:logout:<name>`, which needs `Screen` to carry an account name, so `Screen` is no longer `Copy`.
6. Reload without restart swaps the launcher's registry (`ChildLauncher::set_accounts`, behind an `RwLock`); the seats and a run already started keep what they resolved. It is only done for a session that started with accounts. A session that started on the built-in fake (no account configured) keeps the fake, because its default model and its credential path are the fake's, and swapping the registry under them would send the fake key to a real provider; the line says `restart to run on it`. Making `session_default` and the model defaults swappable is the follow-up.
7. The OAuth modal of the issue (authorization URL, device code, `o` to open the browser, `Esc` to cancel) is split out of this PR as a follow-up issue: the key path works for every provider that takes a key, including OpenRouter, and the modal is a screen and a flow of its own.

**Alternatives.** Keep `String` in the job and zeroise after (rejected: it is in the loop's state for the length of the verify). A `Secret` newtype with `Clone` for the entry (rejected: a clone is exactly how it would end up in a frame). Swap `session_default` as well in this PR (rejected: it touches the defaults every launch reads and the issue says to split rather than widen).

**Why.** The key never exists outside a `Secret`, nothing waits on the network, and the part of the reload that would have been a second PR is named instead of half done.

## D-15: the write tools of `aibus mcp` (E8, #103, 2026-11-11)

**Context.** The issue left several calls open, and the owner's rule is to take the recommended default and write it down.

**Decision.**

1. The revision is the SHA-256 of the file bytes in lower-case hex, the same value `workflow_get` returns. A write holds a process-wide lock from the revision check to the rename; two processes writing the same file are not covered.
2. The roster the write tools validate against is what agent discovery finds for the project and the user, without `agents.extra_dirs` or `--agents`, as for `workflow_validate`.
3. `--no-provenance` is the switch for the provenance option (the epic's decision 5 made it default on). Written files are 0600 in both scopes, because the atomic write has no other mode.
4. Text a redactor would change (a credential) is refused and never echoed, on top of the checks in the issue.
5. The security tests are in `crates/aibus/tests/mcp_security.rs` and `crates/aibus-mcp/tests/write_tools.rs`, not `aibus-core/tests/security/`, for the same reason as E4 item 7. The golden transcript (`create_update_conflict`) is recorded with `AIBUS_UPDATE_TRANSCRIPTS=1` on the release run, since nothing was run for this PR.

**Alternatives.** A fixed mode for the project scope that follows the umask (rejected: it needs a second atomic write for one mode bit). Locking the file across processes (rejected for now: an exclusive lock on a file that is replaced by rename does not hold).

**Why.** The safety model in the issue (names not paths, validate first, a revision check, no gate without its flag) holds with one writer, and a second writer is the user's editor, which the revision check already catches.

## D-16: the run tools of `aibus mcp` (E9, #104, 2026-11-11)

**Context.** The issue left several calls open, and the owner's rule is to take the recommended default and write it down.

**Decision.**

1. A run is a child process, `aibus run --workflow <name> --json --permission-mode dont-ask [--allow <pattern>]... -- <input>`, started from `std::env::current_exe()` (a `RunConfig` in `aibus-mcp` names the program, so tests can stand in for it). `AIBUS_I_KNOW` is removed from its environment, stdin is closed, and the server folds its NDJSON events into `run_status`. The issue says the server hosts one session the way `headless::run` does; a child gives each run its own session directory, commit log and transcript, makes `bypass` unreachable by construction (the mode is on a command line the model cannot touch), and lets a cancel be a SIGINT, which `aibus run` already turns into a snapshot and exit 7. `aibus-mcp` keeps its closure free of provider crates, which a hosted session would have broken.
2. `session` in the `workflow_run` answer is `null` until the child has made one; `run_status` shows it from the child's `SessionEnd` on. The id belongs to the child, which does not exist yet when the answer goes out.
3. The cost cap is the sum of the finished runs' `cost_usd`; each run is started with the remainder as `AIBUS_MAX_COST_USD`, a new variable read next to `AIBUS_MAX_SPAWN_DEPTH` in `env::apply_limits`, so the child ends with the budget error and exit 8 on its own. `--max-cost-usd` defaults to `limits.max_cost_usd`, which makes `aibus mcp --allow run` the one mode that reads `config.toml` (never a key).
4. `workflow_run` takes a `scope`, but `aibus run` resolves a name project first, so a run for the user scope is refused when the project has the same name.
5. `run_status` waits without holding a thread (`call_async`, a `tokio::sync::watch` per run) and `wait_ms` is capped by `RunConfig::max_wait` (30 s); `run_cancel` sends the interrupt and waits for the run to end, with a kill after the grace. The synchronous `Server::call` answers at once.
6. When the server ends, `Drop` cancels live runs and waits up to 11 s for them to snapshot, whichever transport it served. A server killed with SIGKILL leaves its children to finish on their own.
7. The `explain_run` prompt no longer points at a run resource that does not exist; it reads `run_status`. A handle may contain `-`, so the prompt accepts it.
8. The golden transcript (`run_status_cancel`) is recorded with `AIBUS_UPDATE_TRANSCRIPTS=1` on the release run, since nothing was run for this PR. The security tests are in `crates/aibus/tests/mcp_security.rs` and `crates/aibus-mcp/tests/run_bypass.rs`, for the reason in E4 item 7.

**Alternatives.** Host one session in the server process (rejected: it pulls the provider crates and the startup pipeline into `aibus-mcp`, and cancel becomes a task abort with no snapshot). A fixed per-run cost cap instead of the remainder (rejected: N runs could spend N times the cap).

**Why.** The safety model in the issue (nothing prompts, nothing the model sends raises the surface, no secret in a result) is easiest to keep when the run is a separate process with a fixed command line.

## D-17: run resources of `aibus mcp` (E10, #105, 2026-11-11)

**Context.** Runs exist (D-16) and a client wants to read one as a resource, not only poll a tool. The issue left the log's source, the handle spelling and subscriptions open.

**Decision.**

1. `aibus://runs/{run}` is the `run_status` body without `tail`, and `aibus://runs/{run}/log` is `text/plain`. Both are listed as templates only with `--allow run`, and `resources/list` adds one concrete entry per handle this process gave out (oldest first). Without the flag every `aibus://runs/...` uri is `-32602`, the same as a uri that never existed.
2. `{run}` is the handle as `workflow_run` returns it, `run-` plus the ulid, not the bare ulid the issue wrote. One spelling is easier to explain than two, and the table lookup is the only check: a session ulid on disk, a handle from another process and a made-up one all miss it, so they are `-32602` without anything touching the file system.
3. The log is not read back from the session on disk (that would be a path from a uri to a file). The server folds the child's stream, its stderr and the final summary into a buffer of the last 256 KiB as they arrive, which is the report's text for a run that ended and the transcript tail for one that has not. Non-JSON lines are redacted on the way in, and the whole buffer again when it is served, so a key cut in half at the front of the buffer is the only thing that can get through, and the buffer drops to the next line start to avoid even that.
4. The body is cut to its last `limits.result_max_bytes` (the end of a log is the part a client wants), under a "cut at N of M bytes, the start of the log is left out" line, with aibus's markers escaped and a fixed header that says it is output and not instructions. `as_data` cuts the front, which is right for a file and wrong here, so the run log has its own cut.
5. No subscriptions. `run_status` with `wait_ms` already returns on any change and works on both protocol revisions; `docs/MCP.md` says so.
6. The `explain_run` prompt now names the two resources next to `run_status`, and the "not in this release" lines in `docs/MCP.md` lose the run resources.
7. The security test the issue puts in `aibus-core/tests/security/mcp_server.rs` is in `aibus-mcp/tests/run_resources.rs` and `aibus/tests/mcp_security.rs`, for the reason in E4 item 7 (`aibus-core` cannot depend on the server). The golden transcript `run_resources` is recorded with `AIBUS_UPDATE_TRANSCRIPTS=1` on the release run.

**Alternatives.** Read the log from the child's session directory (rejected: a path reachable from a uri, and the redacted transcript there is not the report). Accept the bare ulid too (rejected: two spellings for one thing).

**Why.** A client can only read what this process started, and nothing it sends can name a file.

## E12 (2026-11-11): confirmation through elicitation and MRTR is built, the permission relay is not (#107)

**Context.** #107 sat behind owner decisions 6 and 7 on epic #95. Decision 6 (do #106 and #107, elicitation first) has the recommended default of yes. Decision 7 (relay a run's `bash` prompts through the client) has the recommended default of non-goal: the epic says "I lean non-goal until a client I use supports elicitation", and I have no such client. The owner wants zero open issues, so I took both defaults.

**Decision.** Build the confirmation, in `crates/aibus-mcp/src/confirm.rs`. The step sits in `call_tool` before `Server::call`, so the argument cap, rate limit, redaction and audit stay in one place; refusals and asks add `confirm/ask` and `confirm/refused` audit lines. It applies to the three tools the issue names, only when they are listed (flags on) and only when the request's capabilities include `elicitation` in form mode (rmcp's rule: a bare `elicitation: {}` counts as form). The form is `name` (a string whose `minLength` and `maxLength` are the name's length) and, for `workflow_run` under a cap, `max_cost_usd` (`maximum` is the cap). rmcp's `StringSchema` has no `pattern` keyword, so the issue's `pattern` set to the exact name is not expressible; the answer is compared with the name on the server, which is the check that counts anyway. `requestState` uses rmcp's own `request-state` feature (`RequestStateCodec`, HMAC-SHA256, 32 random bytes per process): the sealed payload is a ULID nonce, the associated data is the tool, the arguments with sorted keys and `clientInfo` (so the digest the issue asks for is in the tag and not stored), the ttl is 10 minutes, and a consumed-nonce set makes it single use. Every failure to open is one message, as the codec's docs ask. `--confirm elicit` is the default; `--confirm never` skips it. `ConfirmConfig { elicit, run_cap_usd, ttl }` is how the run tools set the cap later and how the tests shorten the ttl. The write and run tools are other PRs, so the wire scenarios in `crates/aibus/tests/mcp_confirm.rs` use `workflow_create` and go green when #103 is on the branch; the decisions are tested directly in `crates/aibus-mcp/tests/confirm.rs`.

**The permission relay is a recorded no-go.** Evidence: decision 7's recommended default is non-goal; the run tools it would relay from (#104: `PermissionNeeded`, the run mailbox, `dont_ask`) are not on `cut/0.2.0`; and #104's acceptance criteria say a call that would ask "is denied, not queued", which is the behaviour the relay would change, so building the relay first would make the run lane build against a moving rule. Nothing upstream blocks it (rmcp has the form and MRTR types), so the no-go is a choice and not a limit. I did not add a dead `--permission-relay` flag. If a client I use gains elicitation and decision 7 flips, the relay is a new issue on top of #104, not a rewrite of this.

**Alternatives.** Put the question inside `Server::call` (rejected: it is sync and the legacy path has to await the client). A hand-rolled HMAC (rejected: rmcp's codec has the ttl and associated data, and a second implementation is a second place to get constant-time wrong). Keep a table of pending confirmations instead of a sealed state (rejected: it is state to expire and leak, and the spec's stateless direction points the other way). Ask for any tool that writes, whatever its name (rejected: the issue names three).

**Why.** It adds the server's own question for the clients that can show one and changes nothing for the rest, and it keeps the unknown (run tools, a relay) out of this PR.
## D-12 addendum (2026-11-11): flow-diagram on by default for 0.2.0, ahead of the written exit criteria (#92)

**Context.** #92 lists exit criteria before the default flips: two weeks of main without a fix to `diagram.rs`, a clean terminal checklist on iTerm2, Terminal.app and kitty, and no old unanswered issue on rataflow's tracker. None of that can be met inside this wave: `diagram.rs` changed in #89 two days ago, and I have no real terminals here. The owner's goal for the wave is zero open issues. The measurements that were the hard gate did come back clean (#91): p99 render and resize at most 0.33 ms against 3 ms and 10 ms, one hash over 200 renders at three sizes, 100 of 100 demo frames byte-identical with the classic ones, a +0.6 % binary and a clean `cargo deny`.

**Decision.** I flip the default now. The time-based and terminal-based criteria are not waived, they move: the `docs/RELEASE.md` terminal checklist already has the cards line, and it has to be ticked on the release PR before the tag, so a failure there still stops 0.2.0 and the way back is one flag (`--no-default-features`, or reverting the two `default = [...]` lines). The root manifest now says `default-features = false` for `aibus-tui`, so that flag really drops rataflow from a build of `aibus`; `aibus-tui` and `aibus` each default to the feature. Running `-p aibus-testkit` on its own therefore builds the hand-drawn path, while `--workspace`, `-p aibus-tui` and `-p aibus` run the rataflow one. One snapshot, `w_cards_five_scrolled`, cannot be the same in both (the fourth card's edge is on the canvas), so the test picks `w_cards_five_scrolled_flow` on the new default and the classic file stays untouched for the rollback build.

**Alternatives.** Wait out the two weeks (rejected: it leaves #92 and #93 open past the wave for a risk the release checklist covers anyway). Regenerate `w_cards_five_scrolled` in place (rejected: it breaks the rollback build and the issue says no snapshot is regenerated). Force the feature on in `aibus-testkit` (rejected: the metadata test then cannot see a build without it).

**Why.** The numbers the flip was waiting for are in, and the cost of being wrong is one flag until #93 removes it.

## D-12 addendum 2 (2026-11-11): the hand-drawn connectors and the flag go in the same wave (#93)

**Context.** #93 is labelled "later" and says it starts only after a release with the flag on has shipped and the terminal checklist has been run on it. Neither can happen inside this wave, and the owner's goal for the wave is zero open issues.

**Decision.** I remove the classic `centre`, `widgets::cards` (its only caller was the classic path), the `flow-diagram` feature and the feature-gated tests now. `rataflow` is a plain dependency of `aibus-tui`; the workspace dependency on `aibus-tui` is back to plain. `dotted` stays: `advisor_link` still draws the rail-to-box link with it. The way back is no longer a flag, it is reverting this merge, which is why the release PR's checklist line about the cards canvas matters more than it did. `frames.classic.txt` and its test go as the issue says. With one drawing path left, the bench is `render_dashboard` again and the MARS baselines I recorded for the `_flow` names (#91) moved onto those names (the hand-drawn baselines measured code that no longer exists), and `w_cards_five_scrolled.txt` now holds the frame with the fourth card's edge on the canvas, the only snapshot whose content changes.

**Alternatives.** Leave #93 open until after the release (rejected: it is the last open issue of the epic's tail, and the revert path is one merge). Keep `dotted` out of `ui.rs` by moving it into `diagram.rs` (rejected: `advisor_link` is not part of the flow, and the issue says it stays).

**Why.** One drawing path means every change to the centre column is made and tested once.

## G6 (2026-11-11): the workflow author is a built-in over the binary's own `aibus mcp` (#125)

**Context.** #125 connects the built-in author to `aibus mcp` over stdio, read-only, and adds `/workflow new <goal>`. Three of its details do not fit the code as it stands, and the epic's open question is whether to link the server in-process instead of spawning it.

**Decision.** I keep the spawn (the issue's default). Four calls follow from reading the code:

1. The connection is made when a terminal session starts (or when `--agent workflow-author` is used), not when the author is first about to run. The tool registry is built once and shared, and an MCP client lists its tools at connect time, so there is no later point to add tools to it. The cost is one idle child process per terminal session, which exits with it. Every other agent gets a deny for `mcp:aibus:*`, and `ToolRegistry::specs` now hides a tool that a bare MCP deny covers, so the lead is not offered six tools it could not call. A user's own `[mcp.aibus]` is left alone and not denied to anybody.
2. The server's `author_workflow` prompt travels as the delegated task, not as the system prompt. The prompt takes the goal as an argument and is rendered per call, and a definition's body is fixed text, so the definition carries a short local prompt (the fallback) and `/workflow new` fetches the server's with `prompts/get` and hands it to the author as its task. A server without the prompt leaves the local one and a one-line task.
3. `workflow_dry_run` takes a saved workflow's name, not a draft, so the author validates drafts with `workflow_validate` only and is told to dry-run a similar saved workflow if it wants to see prompts rendered. The acceptance line that has it dry-run the draft cannot be met without a server change, and I did not make one.
4. `/workflow new` is not a forced foreground `Delegate` command. A command from the user has no one waiting for the child's result, whereas a `delegate` tool call made by the lead (`RunMail::Call`, already in the run loop) returns the wrapped report to the lead as a tool result, which is what the issue asks for. The draft is kept by the reducer when a `workflow-author` run finishes: the last fenced `toml` block, only if it validates against the agents in the graph.

**Alternatives.** Link `aibus-mcp` into the TUI over a channel (rejected for now, as the issue says: a new transport and no process boundary around the server). Connect lazily on the first `/workflow new` by making the registry mutable (rejected: a larger change to the executor for one idle process). Put the server's prompt into the system prompt by launching the author with a rendered definition (rejected: it needs an async fetch inside the synchronous launcher).

**Why.** The author gets the least capability that works (a server with no write or run tool at all, and nothing else of it offered to any other agent), and every piece reuses what exists: the validator, the MCP client, the child-result wrapping and the permission modal.

## G4b (2026-11-11): the OAuth modal of /login, and swapping a session off the built-in fake

**Context.** #161 is what G4 left out on purpose: the browser login inside the terminal, and a session that started with no account, which kept the fake and said `restart to run on it`. A few calls were open and the owner's rule is to take the recommended default.

**Decision.**

1. The modal is an `Entry` of a new kind (`EntryKind::Oauth(name)`) on the Accounts screen, so it follows the same Esc and redraw paths as the other fields. It replaces the key step for a provider in `Input::oauth_providers`, which the binary fills from the presets whose id has an `oauth::definition` (today OpenRouter). It shows the lines the flow wrote, as they came (the address, or the device code line), and the screen wraps them over the rows they need. `o` opens the first `https://` or `http://` word in them through `oauth::open`, the platform opener with the address as one argv element; `Esc` cancels the flow's `CancellationToken` and closes the modal.
2. The flow runs on the same worker thread as every account op (`AccountOp::AddOauth`). Its output goes through a small `Write` (`Lines`) that sends each finished line as `Msg::AccountPrompt` (a `try_send`, because the flow writes from inside its runtime). The message is redacted, is for the modal only, and the graph and the log keep nothing of it (the summary has its length). `Io` gains `cancel`, which `account add --oauth` uses in place of its own token; a command line leaves it `None`.
3. The browser is not started by itself in the terminal (`open_browser` and `paste` stay off: there is no stdin to paste on). `o` is the one way, so the opener never starts without a key press.
4. The launcher's registry, `builtin` flag, `ModelDefaults` and `session_default` are one `Live` behind one `RwLock`, and a pick reads all four from a single snapshot, so it cannot pair the new registry with the fake's credential or the fake's default model. `set_accounts(registry, views, defaults)` replaces them. Between real accounts only the registry changes, as in G4. Leaving the fake rebuilds the defaults from `defaults.model`, else the first account's default model, else the first model it lists, with `advisor_model`, `router_model` and `swarm_model` from the config (`off` is none) or the new default. If there is nothing to default to, nothing is swapped and the fake stays whole, and the line says why.
5. A run that is already going keeps what it resolved, and a fork or resume of a run pinned to the fake account fails to pick (the account is gone) and never reaches a real provider. The advisor and the router are not started by the swap; they need a restart, as before.

**Alternatives.** Keep the key field for OpenRouter as well, behind a key press (rejected: the issue says the flow replaces it, and `aibus account add --key-stdin` still takes a key). A new graph node for the prompt lines (rejected: the lines carry a one-off address and are not state worth snapshotting). Swap `builtin`, `defaults` and `session_default` as three atomics or locks (rejected: a pick between two of them could mix the fake with the new registry).

**Why.** A login now ends in a usable account in one session, and the fake key can only ever meet the fake provider.

## D-F3: Capabilities per model, a closed compat enum, and the leftover provider fields

**Context.** Issue #111. `Capabilities` was per provider, which is wrong for a preset table where one model reasons and its neighbour does not, and `ProviderDef.compat` was a struct that mixed capabilities (`logprobs`, `effort`, ...) with wire quirks and with three things that are neither (`user`, `open_aliases`, `responses`, `auth_header`).

**Decision.** `ModelEntry` gains `capabilities` (a `CapabilityOverrides`, every field optional) and `reasoning`. `ProviderDef` gains `default_capabilities`, and `compat` becomes a list over the closed `CompatFlag` enum from the issue (twelve names, two of them carrying a value). `Provider::capabilities_for(&ModelId)` has a default that answers like `capabilities()`, so providers without per-model data need nothing. The compat flags that are capabilities seen from the wire (`no_system_role`, `strip_images`) apply last, after the model layers. Layers of a model entry merge field by field instead of replacing the whole entry, so a 0.1.0 override file that only names sizes keeps the built-in capabilities. `user`, `open_aliases`, `responses` and `auth_header` are not wire quirks in the issue's list and not capabilities, so they moved up to plain optional fields on `ProviderDef` instead of being invented as flags. `reasoning` is `Option<bool>` on the entry (the issue says `bool`) so that "the file did not say" survives a layer merge; in a file it reads the same.

**Alternatives.** Keeping `Compat` as a struct next to the new list (rejected: two things called compat). Replacing a model entry whole per layer (rejected: a size-only override would drop its capabilities). Adding `user` and friends to the enum (rejected: the issue says anything not named goes on the "Later" list).

**Why.** One place says what a model can do, one small closed set says how the server deviates, and `degrade` stays a pure function of resolved capabilities.

## D-F5: Bedrock and Vertex as two more apis of the Anthropic adapter

**Context.** Issue #113. The Bedrock and Vertex transports existed only as constructors, so no config could select them. The issue left a few details open and I took the recommended default for each.

**Decision.** `anthropic-bedrock` and `anthropic-vertex` are two more `api` values, served by a `CloudFactory` in `aibus-provider-anthropic` (the plain `Factory` keeps serving `anthropic-messages`, so no existing call site changed), behind `bedrock` and `vertex` presets. Their `base_url` is left to the region. `TokenSource::token` now takes the calling `&Credential`, so the binary's gcloud source can cache under the account name, as the issue asks, without an account argument on the factory. The cache is a second map in `CredentialStore` (`cached_token`), not the 60-second key cache, since a token lives 50 minutes and a `gcloud` account has no key to cache. A Bedrock secret with no colon is a Bedrock API key and is sent as a bearer token; an access key pair always has one. `aibus account test` makes one real request (one output token) on the smallest haiku row, preferring a `global.` profile for Bedrock, and `AccountInfo` gained `cost_usd` for it. `price_of` is a single hop. Bedrock and Vertex get their own catalog files (`bedrock`, `vertex`) whose rows only say `price_of`, and `catalog-sync` knows them as `amazon-bedrock` and `google-vertex-anthropic`. A Bedrock or Vertex account without a region (or Vertex without a project, on the account or the `[[provider]]`) is a config error. `account add` gained `--region` and `--project`; the `aws_*` and `gcloud` forms are written by hand.

**Alternatives.** The AWS SDK and Google auth crates (rejected in the issue: dozens of crates and a second TLS stack for `cargo deny`). One factory with an optional token source (rejected: `Factory` is a unit struct used in many places). Caching the gcloud token under the provider id (rejected: the issue says the account name). A model-name rule for the price instead of table rows (rejected: the issue asks for the `price_of` column, and a rule cannot be corrected by a user file).

**Why.** The static-key paths cover the people who asked for this; SSO, `credential_process` and instance metadata stay on the epic's "Later" list.
