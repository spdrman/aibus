# Changelog

All notable changes go here, newest first. The section for a version is what the release workflow
uses as the release notes, so every release needs its entry before it is merged into `release`.

## [0.2.0] - unreleased

### Added

* `aibus mcp`: an MCP server over stdio, in a new `aibus-mcp` crate on `rmcp` 3.9 (D-13). It answers `2026-07-28` (`server/discover`) and `2025-11-25` (`initialize`) as `aibus` plus the crate version, starts read-only, and lists the read tools below. `--allow write|gates|run` and `--allow-tool` are parsed (`gates` needs `write`, exit 2 otherwise; the run budget flags are refused as not available yet). Every call leaves one redacted line in `~/.local/state/aibus/logs/mcp/<ulid>.jsonl` (0600), 60 calls a minute are allowed and arguments are capped at 256 KiB. stdout carries JSON-RPC only; the log goes to stderr. `aibus doctor` lists the audit directory.
* `aibus mcp` read-only surface (E5, #100): `workflow_get` (text, SHA-256 revision, parsed steps), `workflow_explain`, `workflow_validate` (an existing workflow or a draft, against the real roster), `workflow_dry_run` (prompts rendered with an input, gates, resolved agents, budgets; no model is called) and `agent_list`, all read-only with an `outputSchema`. Resources `aibus://workflows`, `aibus://workflows/{scope}/{name}`, `aibus://schema/workflow` and `aibus://agents` (an allow-list: anything else is `-32602`), and the prompts `author_workflow`, `review_workflow` and `explain_run`. Text from a file reaches the client wrapped as data and redacted. See `docs/MCP.md`.
* `aibus workflow show <name>` prints a workflow as it will run, and `aibus workflow validate [<name>|<path>]` checks files against the format and the agent roster (exit 12 on errors, `--json` for scripts).
* `aibus mcp --http 127.0.0.1:<port>`: the same server over Streamable HTTP for clients that cannot spawn a process (E11, #106). Loopback only (any other address is exit 2, no override), a bearer token on every request (`AIBUS_MCP_TOKEN`, or 32 random bytes shown once on stderr), 403 for a foreign `Origin` or `Host`, 400 for an unsupported `MCP-Protocol-Version`, and the stdio rate limit and audit log, with the client address on each audit line. `--origin <value>` adds an accepted origin. See `docs/MCP.md` for the SSH tunnel recipe.
* `aibus mcp --allow write` adds `workflow_create` and `workflow_update` (E8, #103): names not paths, validation first, no gate without `--allow gates`, size and step caps, a credential check, atomic 0600 writes with a `# written through aibus mcp` first line (`--no-provenance` turns it off), and an update needs the SHA-256 revision it read. 10 writes a minute. `--allow run` adds `workflow_run`, `run_status` (with `wait_ms`) and `run_cancel` (E9, #104): a run is a child `aibus run` in `dont-ask` with only the `--allow-tool` pre-approvals, capped by `--max-runs` (10), `--max-concurrent-runs` (1) and `--max-cost-usd` (`limits.max_cost_usd`); closing stdin cancels live runs. `AIBUS_MAX_COST_USD` is a new variable that sets `limits.max_cost_usd` for one process. The `explain_run` prompt works. Details in `docs/MCP.md` and D-15.

* `aibus mcp --confirm elicit|never` (E12, #107): when the client declared `elicitation`, the server asks the user to retype the workflow name (and accept a cost under the cap for a run) before `workflow_create`, `workflow_update` and `workflow_run`. `2025-11-25` clients get an `elicitation/create` mid-call; `2026-07-28` clients get `input_required` with an HMAC-sealed `requestState` bound to the call, good once for 10 minutes. `--confirm never` and clients without the capability keep the flags as the only gate. Relaying a run's permission prompts is a recorded no-go (see DECISIONS.md).
* `/workflow list`, `show <name>`, `save <name> [--user] [--force]` and `run <name> <input>` in the TUI (G7, #126). `list` and `show` open modals (the rows, or the file with the validator's summary under it, `Enter` on a row fills the prompt with `/workflow run <name> `). `save` takes the latest drafted workflow, validates it against the real roster, and writes `.aibus/workflows/<name>.toml` (or the user's directory with `--user`, 0600, atomic) starting with `# schema: aibus://schema/workflow v1` and `# written through aibus, <RFC 3339 time>`. The name is checked against `^[a-z0-9][a-z0-9_-]{0,63}$` before the disk is touched, an existing file needs `--force`, and the prompt's `/fix` family now reads the workflows on disk. `/workflow run` is `/workflow <name> <input>` under a second name, and in a running session it makes the lead call the `workflow` tool at its next turn.
* `aibus mcp --allow run` adds the run resources (E10, #105): `aibus://runs/{run}` is the `run_status` body without the tail and `aibus://runs/{run}/log` the run's output, redacted, wrapped as data and cut at the result cap. Only runs the server started are readable; the `explain_run` prompt points at them.
* A built-in `workflow-author` agent and `/workflow new <goal>` (G6, #125). The agent is read-only (`read`, `glob`, `grep` and `mcp:aibus:*`) and talks to this program's own `aibus mcp` over stdio, which the binary adds as `[mcp.aibus]` when you have none (no `--allow`, so no write or run tool exists for it; only the author is offered those tools). `/workflow new` makes the lead delegate the goal to it in the foreground, so the report comes back as data and the card shows the author like any worker. Its last fenced `toml` block that validates against your agents is kept as the draft `/workflow save` writes. It has a cost cap of a quarter of `limits.max_cost_usd` unless its definition names one, and `aibus doctor` has a `workflow author` row. See `docs/MCP.md`.

### Accounts and models in the terminal

* `/model` with no reference opens a picker of what the connected accounts serve, with context window, prices (`n/a` when the table has none) and capabilities; it filters by the target's role and sets the lead, a run or a seat (G3, #122).
* `/login [provider]`, `/logout <account>` and `/accounts`: pick a provider, name the account, type the key into a masked field (a `Secret` from the first keystroke). The verify runs off the UI loop, and the new account is usable at once for a session that started with accounts (G4, #123).

### Providers

* Gemini has a native adapter, `aibus-provider-gemini`, behind a new `gemini` preset (`api = "gemini-generate-content"`, key in `x-goog-api-key`, `GEMINI_API_KEY`). It keeps thought signatures across tool turns, streams thought summaries, reports reasoning and cached tokens, counts tokens through `countTokens`, and has a priced model table. `gemini-openai` (the OpenAI shim) is unchanged. See `docs/PROVIDERS.md` and D-14.
* `ContentBlock::ToolUse` has an optional `signature`; the other providers leave it empty.

### Models

* The built-in model tables moved from the provider crates to `catalog/<provider>.json`, and every priced row now writes all four buckets (`null` for none). Nothing about `models()` or `price()` changes.
* `catalog-sync` (in `aibus-testkit`) refreshes the catalog from a saved models.dev snapshot as a reviewable diff. It never fetches and never writes without `--write`.
* A model row can carry `price_from`, so an announced price sits beside the current one and starts on its date.
* For OpenRouter the response's `usage.cost` replaces the estimate, and `Usage.cost_source` says which one it was.

### Build

* The TUI is on ratatui 0.30 and crossterm 0.29, and `deny.toml` has no ignored advisories any more: `paste` (RUSTSEC-2024-0436) left the tree with ratatui 0.29. `crossterm` joins `ratatui` in the one-version-only ban list.
* The dashboard's centre column (lead box, fork layer, connectors, cards, return box) is drawn with rataflow 0.1 now. It is a plain dependency of `aibus-tui` and adds one crate to the lock (+0.6 % binary size). The hand-drawn connectors are gone, and so is the `flow-diagram` feature, so a build from source has no way to turn rataflow off. See `docs/spikes/D1.1-rataflow.md`.
* The frames are the ones the hand-drawn path drew, cell for cell: the same boxes, the same dotted connectors with their travelling dots and pulsing arrow head, the same card hit rects. The 100 frames of the README demo were byte-identical between the two paths when the hand-drawn one was removed.
* With more than three cards the row pans: a drag from empty space in the centre column moves it a card at a time, Shift plus the wheel still does, and the row stops at the first and the last card. The cards beside the window are on the canvas too (their edge and connector show) and come in as it pans.

## [0.1.0] - 2026-11-09

First release. What is in it:

* The `aibus` binary: a ratatui dashboard (advisor rail, lead session, fork layer, worker cards,
  session log, status bar) and a headless `aibus run`.
* A lead agent that delegates to sub-agents defined as Markdown files with YAML frontmatter, an
  advisor that speaks at checkpoints, a router for small decisions, and ledgers, handoffs and
  memory that carry state from one session to the next.
* The whole application state as one Causl graph, with the commit log as the audit trail and
  snapshots as the session save.
* Anthropic and OpenAI-compatible (OpenAI, Kimi/Moonshot) providers, several accounts per
  provider, credentials in the OS keychain. The OAuth method and the Bedrock and Vertex
  transports are in the code but not usable yet (see Known issues).
* `aibus version` prints the release tag and commit, `aibus doctor` checks the setup.
* Release builds for aarch64 and x86_64 macOS, x86_64 and aarch64 Linux (glibc), x86_64 Linux
  (musl) and x86_64 Windows, with a CycloneDX SBOM, `SHA256SUMS` and keyless cosign signatures.

### Known issues

* The Bedrock and Vertex transports exist as constructors, but `auth = "sigv4"` and
  `auth = "gcloud"` cannot be selected from the config file yet.

Not proven yet: no real provider has been exercised against a live account by the release
pipeline, and the Windows build has not been run on a Windows machine. `docs/RELEASE.md` lists
the manual checks.

### Build

* MSRV is Rust 1.89 (`rust-toolchain.toml`), up from 1.85, because `rusqlite` 0.41 needs it. I moved to 0.41 for the bundled SQLite 3.54 (RUSTSEC-2026-0459).
