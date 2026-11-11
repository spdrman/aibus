# Testing

Tests and compiles run in a container (see `docs/BUILD.md`), never on a host. This file starts with
the layers and the commands that run each one, then covers the test kit, the engine integration
suite, the replay fixtures and how to point the suites at a second graph backend, and ends with
the TUI snapshots, because they are the part that needs a routine.

## Layers

These are the rows of 10 section 1. The local run on MARS in docker is the real gate: every "every
push" row below runs there, with these commands. `.github/workflows/ci.yml` has one job per row with
the same command, started by hand.

| Layer | Command | When |
|---|---|---|
| Unit | `cargo test --workspace --locked --lib --bins` | every push |
| Graph conformance | `cargo test --locked -p aibus-graph` (mini); `aibus-graph-causl` for the second backend | every push |
| Engine integration | `cargo test --locked -p aibus-core --test integration` | every push |
| TUI snapshots | `cargo test --locked -p aibus-tui` | every push |
| CLI | `cargo test --locked -p aibus --tests` | every push |
| Replay | `cargo test --locked -p aibus-testkit --test replay --test session` and `-p aibus-core --test replay` | every push |
| Provider contract | `cargo test --locked -p aibus-provider-anthropic -p aibus-provider-openai` (cassettes) | every push |
| Provider contract, live | `... --features live-tests --test live`, see `docs/PROVIDERS.md` | nightly, opt-in; locally, or `nightly.yml` by hand |
| Benchmarks | `cargo bench --locked -p aibus-core` and `-p aibus-tui` with `AIBUS_BENCH_GATE=1` | nightly, and when `state/` or `tui/` changed; locally, or `bench.yml` by hand |
| Fuzz | `cargo fuzz run <target>` for each target in `fuzz/` | nightly; `nightly.yml` by hand (a no-op until `fuzz/` exists) |

The conformance row says "both backends when available": the `causl-differential` job in
`.github/workflows/causl-spike.yml` (manual dispatch) runs the `aibus-graph-causl` conformance and
differential tests with `--features causl-rs`, plus the side-by-side `commit_stream` bench. The
crate is private, so the job's fetch step needs the repository secret `CAUSL_CI_BUILD_1` and is the
only step that sees it. See `docs/BUILD.md`.

## The test kit

`aibus-testkit` is where the shared fixtures live:

* `xdg_temp()` gives a throw-away `$HOME` with the four XDG directories under it and a `Paths`
  resolved from it (it never reads the process environment, so tests can run side by side).
  Dropping it greps every file below the root, including `.zst` files after decompressing them,
  for the fake key and for anything added with `forbid`, and panics on a hit.
* `capture_logs()` and `assert_no_secret_logged()` do the same for the log; `LogCapture` checks on
  drop.
* `FakeClock` is `tokio::time::pause()` as a value. Do not use it in a test that waits on a real
  process under a timeout you care about (hooks, for one), the paused clock jumps over it.
* `tui::render`, `assert_frame!` and `assert_styles!` are the frame snapshot helpers below.
* `big_session(mk)` builds the reference session of 03 section 11 (eight runs, 100,000 lines on
  the first, 2,000 log rows) on any backend. Run ids, the clock and every text are fixed, so two
  calls leave the same snapshot hash.
* `replay::{fixtures, assert_replays, assert_same_on_both}` is the replay harness, below.

## Engine integration

`crates/aibus-core/tests/integration/` has one test for each bullet of 10 section 4, named after
what it asserts. `rig.rs` is the orchestrated engine (a real lead, the orchestrator, the effects
executor, a launcher that gives each child its own scripted fake), `solo.rs` is one lead run for the
cases that need no orchestrator. Both sit on `xdg_temp()` and `capture_logs()`, so every test ends
with the "no credential in any file or log line" check, not only the one that is named for it.

The suite runs on the mini backend. `common::app()` builds a `MiniGraph`; running it on another
backend means making `Rig` and `Solo` generic over `Graph` and passing the constructor in, which
I did not do because the replay and conformance suites already cover the second backend and the
integration cases do not touch the graph beyond `App::step`.

## Replay fixtures

A fixture is a directory under `tests/replay/`:

* `msgs.jsonl`, the `Msg` stream the run sent, redacted (tool output and summaries are kept as
  lengths only);
* `script.ron`, the fake provider's script that produced it;
* `snapshot.hash`, the FNV-1a hash of the snapshot after replaying the stream;
* `views.json`, every `view:` node after the replay with the dashboard's views registered.

`aibus-testkit/tests/replay.rs` replays each one, asserts the hash and the `view:` nodes, and
fails a fixture that takes a second or more. There are 22. `tools/record-replay.sh` is the list of
scenarios, each a script plus one command line (the M1 to M5 exits: a plain run, tool turns, a
retry, a failure, `max_turns`, a budget stop, a denied permission, a hook that blocks, delegation,
the advisor, `same_error_x2`, the router sharp and split, compaction, a ledger and a handoff).

To record or re-record, run it in the build container after building `aibus`:

```
AIBUS=/tmp/target/debug/aibus tools/record-replay.sh            # all of them
AIBUS=/tmp/target/debug/aibus tools/record-replay.sh router-split   # one
```

For a new scenario add a `rec <name> <expected exit code> <script> <agent file or -> <args>` line.
The command under it is `aibus run --json --record tests/replay/<name> --script <file> ...`, which
is the interface to use by hand too: `--record <dir>` writes the four files when the lead has
finished. A re-recorded fixture gets new run ids and so a new hash, and its four files change
together, so commit the directory as a whole.

What the recorder keeps is what passes through the run loop: run, stream, tool, usage, permission,
hook, router, advisor, checkpoint and compaction messages. `HandoffWritten` at session end is
stepped by the session code and not recorded; the CLI test in `continuity_cli.rs` covers it.

### Running the suites against another graph backend

The second backend is `aibus-graph-causl` (outside the workspace, with its own lockfile). Three
things use the harness; the first two already run there, the third is C2.4's to switch on.

1. Conformance: `crates/aibus-graph-causl/tests/causl.rs` includes the suite file with `#[path]`
   and calls `conformance::<case>(CauslGraph::new)` for each case. A new case in
   `aibus-graph/tests/conformance/mod.rs` needs one more name in the `cases!` list there.
2. Replay, hash and differential: `crates/aibus-graph-causl/tests/differential.rs` replays every
   fixture on mini and on causl and asserts equal snapshot bytes, graph time and commit intents,
   plus the recorded hash on causl. It only uses `aibus-core`, so it needs no new dependency.
   Both are behind `--features causl-rs`.
3. Replay, `view:` nodes (item 3 of 10 section 5): add
   `aibus-testkit = { path = "../aibus-testkit" }` under `[dev-dependencies]` in
   `crates/aibus-graph-causl/Cargo.toml`, refresh that crate's `Cargo.lock` (it needs the token,
   so do it in the causl workflow or on a machine that has it), and add:

   ```rust
   use aibus_graph::mini::MiniGraph;
   use aibus_graph_causl::CauslGraph;
   use aibus_testkit::replay::{assert_replays, assert_same_on_both, fixtures};

   #[test]
   fn every_fixture_replays_on_causl() {
       for fx in fixtures() {
           assert_replays(&fx, CauslGraph::new);                       // hash and view: nodes
           assert_same_on_both(&fx, MiniGraph::new, CauslGraph::new);  // the differential
       }
   }
   ```

   Both functions take a constructor (`fn() -> G`), so the backend is the only thing that changes.
   `aibus_testkit::big_session(CauslGraph::new)` builds the reference session on it the same way.

The integration suite is not backend-generic, see above.

## Provider cassettes

Every provider adapter runs the same suite, `aibus_testkit::conformance::run(provider, server,
fixtures)`. It starts a scripted fake server on a loopback port (no network, no keychain, no real
provider), builds the adapter with the server's `base_url`, and plays one fixture per case. The
cases are fixed: text, tool use, parallel tool calls, the stop reasons, every error status
(400 invalid, 400 context, 401, 403, 404, 413, 429 with and without `retry-after`, 500, 503, 529),
an error event inside a 200 stream, a malformed SSE line, a stream that ends early, a cancel while
the server is still sending (must come back `Cancelled` within 100 ms), the model list, `verify`
and `count_tokens`. A fixture set that leaves one out fails, unless the case is listed in
`Fixtures::skipped` with a reason. Every case also checks that the fake key only travels in the
adapter's one auth header, never in a URL, a body or a log line, and the suite checks that the
redactor leaves every fixture unchanged.

A fixture comes in two forms and the kit reads both:

* `tests/cassettes/<name>.http`, the recorded form: `HTTP <status>`, header lines, a blank line,
  the body. The redaction test in `aibus-core` walks these too.
* `tests/fixtures/<name>.json`, the scripted form, for what a recording cannot say. It names the
  `status`, `headers`, an optional `delay_ms`, and either a `body` or a list of `steps`
  (`{"event": "..", "data": {..}, "delay_ms": 20}`, or `{"raw": ".."}` for a deliberately broken
  frame), then `"end": "close" | "drop" | "hang"`. `drop` ends the connection with no final
  chunk (a real broken stream) and `hang` holds it open until the client leaves, which is what the
  cancel case uses (`Fixture::hang_after(n)` turns a recording into one).

`crates/aibus-testkit/tests/conformance_broken.rs` keeps the suite honest: a wrapper that drops
`ToolUseEnd`, swallows cancel, or logs the key must turn it red. Each adapter has one conformance
test (`tests/conformance.rs` in its crate); what is specific to a provider (cache breakpoints,
thinking signatures, effort headers, Kimi's reasoning round trip) stays in its `cassettes.rs`.

To record a case from a real account, run the live suite with `AIBUS_LIVE_ACCOUNT=<name>`, the
account's key in `AIBUS_LIVE_KEY_<NAME>` and `AIBUS_RECORD=1`:

```
AIBUS_RECORD=1 AIBUS_LIVE_ACCOUNT=work AIBUS_LIVE_KEY_WORK=... \
  cargo test -p aibus-provider-anthropic --features live-tests --test live
```

The answers land in `tests/cassettes/live/<case>.http` with the key and the account's ids
stripped. Read them, then move the ones you want up into `tests/cassettes/` and add a `Case` for
them in the crate's `tests/conformance.rs`.

## The MCP conformance harness

`crates/aibus/tests/common/mcp_client.rs` is a scripted MCP client: it starts `aibus mcp <args>` in the `xdg_temp()` home, speaks newline JSON-RPC, and keeps every stdout line so a test can check that nothing but JSON-RPC came out. `McpClient::v2026_07_28` sends `_meta` (protocol version, client capabilities, client info) on every request and has no handshake, `McpClient::v2025_11_25` runs `initialize` and `notifications/initialized`. `discover`, `list_tools` (it follows cursors), `call`, `read_resource`, `list_prompts`, `get_prompt` and `raw` cover the rest. A server that exits before it answers is reported with its exit status and its stderr, so a binary without the subcommand fails every test with "unrecognized subcommand".

`common/transcript.rs` records the exchanges and compares them with `fixtures/mcp-server/<scenario>.jsonl`. It drops ids, the request `_meta`, and the `resultType`, `ttlMs` and `cacheScope` framing, and turns ULIDs, timestamps and the temp home into `<ulid>`, `<time>` and `<home>`, so one golden file serves both protocols. `AIBUS_UPDATE_TRANSCRIPTS=1` rewrites the file; read the diff before you commit it.

`common/schema.rs` gained the 2020-12 keywords the tool schemas use (see its header) and `lint_2020_12`, which checks a whole schema rather than the branches a value reaches.

`tests/mcp_server.rs` is the suite the server has to pass, `tests/mcp_harness.rs` tests the harness itself against `fixtures/mcp/server.sh` and needs no server. Both run in the `cargo test --locked -p aibus --tests` row.

## Frame snapshots

A frame snapshot is the text of a `TestBackend` buffer, one row per line with the trailing blanks
trimmed, compared with a file in `crates/aibus-tui/tests/snapshots/`. A second file next to it,
`<name>.styles.txt`, holds the `fg`, `bg` and modifier runs per row, so a colour regression fails
too. The animation is driven by the tick counter the test injects, so a frame is the same on every
run and every machine.

The helpers are `assert_frame!` and `assert_styles!` in `aibus-testkit`
(`crates/aibus-testkit/src/tui.rs`), and `Rig` in `crates/aibus-tui/tests/common/mod.rs` for
frames that come out of synthetic key and mouse events.

The spec asks for `cargo insta`. I did not add it: the checker above was already in place from
C1.1 and does the same job (a named text file per frame, rewritten on request, reviewed as a diff)
without a new dependency or a second review tool. The workflow is the same shape:

```
# 1. run the tests; a changed frame fails and prints the old and the new one
cargo test -p aibus-tui

# 2. when the change is on purpose, rewrite the files
AIBUS_UPDATE_SNAPSHOTS=1 cargo test -p aibus-tui

# 3. read the diff before committing it
git diff crates/aibus-tui/tests/snapshots
```

Review the diff like code. A cursor that moved, a border that changed colour or a column that
shifted is a real change. A snapshot that is missing fails with a message that names the file; the
first run with `AIBUS_UPDATE_SNAPSHOTS=1` creates it.

## What is covered

* `c13_frames.rs`: f01, f02, f03, f04, f09 and f21 at 98x46, the style runs of f02 and f04 (cards,
  fork rows, status bar), the 80x24 collapsed rail, 120x50 with the ledger, `ascii = true`, `mono`,
  the permission modal, the transcript collapsed and expanded, the agents screen.
* `c13_interact.rs`: what keys and the mouse do, with each button tested against its key.
* `snapshots_dashboard.rs`, `snapshots_states.rs`, `shell.rs`, `dashboard_widgets.rs`: the
  dashboard and its interaction states from C1.1 and C1.2.

## Contrast

`crates/aibus-tui/tests/theme_contrast.rs` measures every text and background pair of the themes
and pins the number. A palette change shows up there as a failing ratio.
