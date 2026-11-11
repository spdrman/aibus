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

### Commands in the session

* One registry declares every command (G1, #120): the slash line, the palette, the card menu, the completion popup and the help text read the same table, and `@target` (`@lead`, `@advisor`, `@router`, an agent name, a run id prefix) is parsed in one place. The README and `docs/spec/05-ui.md` carry that table, generated from it (G10, #129).
* Fixed: `/model` and `/advisor` did nothing. The line was sent to the lead as words, or dropped. They are `Command::SetModel` now, for the lead, a seat (`@advisor`, `@router`, or `/advisor` and `/router`) or one run (`/model @grep haiku`), and the change takes effect from the next request and lasts for the session (G2, #121).
* Per-agent commands (G5, #124): `/status`, `/cost`, `/log`, `/cancel` (asks first on the lead), `/retry`, `/message`, `/resume` and `/background`, with `i` and `$` on a card and `Status` and `Cost` in its menu. A command on a run in the wrong state is a notice naming the state.
* Security tests for the commands (G9, #128): a slash line in a hook's output, a model's or tool's result, a message or a resumed run never runs as a command; no key from `/login` reaches a file, a frame or a notice; the workflow author's child never gets `gates` or `run` and starts from the scrubbed environment; every write tool of `aibus mcp --allow write` asks every time. A new `commands_send` fuzz target covers `send()` and is seeded with every documented command.

### Accounts and models in the terminal

* `/model` with no reference opens a picker of what the connected accounts serve, with context window, prices (`n/a` when the table has none) and capabilities; it filters by the target's role and sets the lead, a run or a seat (G3, #122).
* `/login [provider]`, `/logout <account>` and `/accounts`: pick a provider, name the account, type the key into a masked field (a `Secret` from the first keystroke). The verify runs off the UI loop, and the new account is usable at once for a session that started with accounts (G4, #123).

* `/login openrouter` offers the browser login in a modal: the address and the device code the flow prints, `o` to open the browser, `Esc` to cancel the flow. A session that started with no account now runs on the account `/login` adds, with no restart; the fake key and the fake's default model never go to the new account (G4b, #161).
* `/workflow new --write <goal>` and `/workflow edit <name> <change>` let the author save (G8, #127), per command and never as a standing setting. They start a second `aibus mcp --allow write` child (never `gates`, never `run`) for that one author run, registered as `aibus-write`; without the flag no write tool exists for the author. Every `workflow_create` and `workflow_update` asks in the permission modal with a summary line (`create review (project), 4 steps, no gate`, `update review, revision ab12cd34...`), a pre-approved pattern does not skip it, and "always" is not offered. `edit` reads the file with `workflow_get` and updates with that revision, so a file changed meanwhile is a conflict. The log gets `workflow <name> written by workflow-author`.

### Providers

* Account fallback (F9, #117): `[[account]] fallback = ["b", "c"]` and `[defaults] fallback_on = ["quota", "auth", "overloaded"]` move a run to the next account when the first fails before its first token, with the model resolved again there, a fresh retry budget per account and the conversation unchanged. The header shows `↷ account`, the transcript keeps a `switch` line, and an agent definition can say `fallback: []` to stay put. An account that triggered a switch sits out of selection for a while (30 minutes or the reset the headers named after a quota, 60 seconds after an overload, until `aibus account test` after an auth failure), in memory only. The advisor and router seats fall back the same way. Off unless configured.
* Gemini has a native adapter, `aibus-provider-gemini`, behind a new `gemini` preset (`api = "gemini-generate-content"`, key in `x-goog-api-key`, `GEMINI_API_KEY`). It keeps thought signatures across tool turns, streams thought summaries, reports reasoning and cached tokens, counts tokens through `countTokens`, and has a priced model table. `gemini-openai` (the OpenAI shim) is unchanged. See `docs/PROVIDERS.md` and D-14.
* `ContentBlock::ToolUse` has an optional `signature`; the other providers leave it empty.
* Claude on Bedrock and Vertex can be chosen from the config file (F5, #113). Two more `api` values of the Anthropic adapter, `anthropic-bedrock` and `anthropic-vertex`, behind the `bedrock` and `vertex` presets. An account says `auth = { sigv4 = true }`, `{ aws_env = true }` or `{ aws_profile = "name" }` for Bedrock and `{ gcloud = true }` for Vertex, with `region` (and `project` for Vertex) on the account or the `[[provider]]`; the wrong auth for an api is a config error with the line. `aws_env` reads the usual `AWS_*` variables at request time, `aws_profile` reads static keys from a `0600` `~/.aws/credentials`, and a Bedrock API key (`AWS_BEARER_TOKEN_BEDROCK`) goes through `env`. The Vertex token comes from `GOOGLE_OAUTH_ACCESS_TOKEN` or `gcloud auth print-access-token` (argv only, 10 s timeout, cached 50 minutes in the credential store). `aibus account test` makes one real request on the smallest model and shows what it cost, and `aibus doctor` shows the region and project. A model row can say `price_of`, so a Bedrock inference profile costs what the plain Claude model does.

### Models

* The built-in model tables moved from the provider crates to `catalog/<provider>.json`, and every priced row now writes all four buckets (`null` for none). Nothing about `models()` or `price()` changes.
* `catalog-sync` (in `aibus-testkit`) refreshes the catalog from a saved models.dev snapshot as a reviewable diff. It never fetches and never writes without `--write`. It writes aibus's own capability names (models.dev's `tool_call` is `capabilities.tools`, an `image` input is `capabilities.images`, `reasoning` is the row's `reasoning`), keeps capabilities set by hand, and drops the models.dev-named keys an earlier run wrote.
* A model row can carry `price_from`, so an announced price sits beside the current one and starts on its date.
* For OpenRouter the response's `usage.cost` replaces the estimate, and `Usage.cost_source` says which one it was.

### Build

* The TUI is on ratatui 0.30 and crossterm 0.29, and `deny.toml` has no ignored advisories any more: `paste` (RUSTSEC-2024-0436) left the tree with ratatui 0.29. `crossterm` joins `ratatui` in the one-version-only ban list.
* The dashboard's centre column (lead box, fork layer, connectors, cards, return box) is drawn with rataflow 0.1 now. It is a plain dependency of `aibus-tui` and adds one crate to the lock (+0.6 % binary size). The hand-drawn connectors are gone, and so is the `flow-diagram` feature, so a build from source has no way to turn rataflow off. See `docs/spikes/D1.1-rataflow.md`.
* The frames are the ones the hand-drawn path drew, cell for cell: the same boxes, the same dotted connectors with their travelling dots and pulsing arrow head, the same card hit rects. The 100 frames of the README demo were byte-identical between the two paths when the hand-drawn one was removed.
* With more than three cards the row pans: a drag from empty space in the centre column moves it a card at a time, Shift plus the wheel still does, and the row stops at the first and the last card. The cards beside the window are on the canvas too (their edge and connector show) and come in as it pans.

### Known issues

* The `?` screen shows the key map and not the commands; type `/` for the list, or read the table in the README. Drawing the commands there needs a second page, which changes every help snapshot (see G10 in `docs/DECISIONS.md`).
* `/logout <text>` repeats the text in its notice when no account has that name, and the prompt history keeps the line. `/login` has no such echo.

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

Not proven yet: no real provider has been exercised against a live account by the release
pipeline, and the Windows build has not been run on a Windows machine. `docs/RELEASE.md` lists
the manual checks.

### Build

* MSRV is Rust 1.89 (`rust-toolchain.toml`), up from 1.85, because `rusqlite` 0.41 needs it. I moved to 0.41 for the bundled SQLite 3.54 (RUSTSEC-2026-0459).
