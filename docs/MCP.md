# aibus mcp

`aibus mcp` is an MCP server. A client (an editor, a chat app, a coding assistant) starts it, and from then on it can read your workflows and your agent roster, check a draft, and see what a workflow would do, all without a model being called. It is the other direction from the `[mcp.<server>]` tables in your config: those make aibus a client of somebody else's tools, and this makes aibus a server for yours. The two do not share anything.

Out of the box the server is read-only. `--allow write` and `--allow run` add the write and run tools, each described below.

## Setting it up

The server speaks stdio, so the client only needs the command:

```json
{"command": "aibus", "args": ["mcp"]}
```

Where that goes depends on the client:

* **Claude Code**: `claude mcp add aibus -- aibus mcp`
* **Claude Desktop**, **Cursor** and most editors with an `mcpServers` map: add `"aibus": {"command": "aibus", "args": ["mcp"]}` to it.
* **Anything else**: run `aibus mcp`, send it `server/discover` (or `initialize`) on stdin and read JSON-RPC lines on stdout.

The project is the directory the client starts the process in (or `--cwd <dir>` in `args`). The user's workflows and agents are the ones in your config directory. `aibus mcp` reads no key, account or config file for a tool, so it works with no account set up.

Protocol versions: `2026-07-28` (stateless, `server/discover`) and `2025-11-25` (`initialize`) both work. The server is `aibus` plus the crate version.

## What the flags add

The surface is fixed when the process starts, and nothing a client sends can raise it. A tool that is off is not listed, and calling it by name is `-32602`, the same as a tool that never existed.

| Flag | Adds | In this release |
|---|---|---|
| none | the read tools, resources and prompts below | yes, the default |
| `--allow write` | `workflow_create` and `workflow_update` | yes |
| `--allow gates` | a written workflow may carry `gate` commands; needs `write` (exit 2 otherwise) | yes |
| `--allow run` | `workflow_run`, `run_status` and `run_cancel` | yes |
| `--allow-tool <pattern>` | pre-approves a tool for runs (the only pre-approvals a run gets) | yes |
| `--max-runs N`, `--max-concurrent-runs N`, `--max-cost-usd X` | the run caps below (10, 1 and `limits.max_cost_usd`); need `--allow run` | yes |
| `--no-provenance` | leave the `# written through aibus mcp` line out of written files | yes |
| `--http <ip:port>` | the same server over Streamable HTTP on loopback, see below | yes |

## Read tools

All six are read-only (`readOnlyHint`), never destructive and never open-world. Each has an `outputSchema`, and the result is in `structuredContent`. The text block holds the same JSON under a one-line header saying it is data, with aibus's own markers escaped and a "cut at" note past 32 KiB. `structuredContent` is never cut.

| Tool | What it does |
|---|---|
| `workflow_list` | every workflow of the project and of the user, with the parse error of a broken file |
| `workflow_get {name, scope?}` | the file text, its SHA-256 `revision` and the parsed steps |
| `workflow_explain {name, scope?}` | one paragraph per step (which agent, what it reads, what blocks it, how often it retries) and the data flow between steps; an edge `from: 0` is the workflow input |
| `workflow_validate {name?, scope?, toml?}` | an existing workflow or a draft `toml`, checked against the real roster (built-in, project, user); same shape as `aibus workflow validate --json`, so a draft that names an unknown agent is `ok: false` with the step number |
| `workflow_dry_run {name, scope?, input}` | each prompt rendered with the input (a later step's summary shows as `<summary of step N (agent)>`), the gates as text, the agent each step resolves to, and the budgets |
| `agent_list` | the agents a step can name, as `aibus agent list --json` |

`scope` is `project` or `user`; left out, the project's file wins. Budgets in `workflow_dry_run` are the built-in defaults, because the server does not read `config.toml`. Likewise, `agents.extra_dirs` is not on the roster.

## Write tools (`--allow write`)

| Tool | What it does |
|---|---|
| `workflow_create {name, scope, toml}` | writes a new workflow file. `{name, scope, path, revision, warnings}` |
| `workflow_update {name, scope, toml, expected_revision}` | replaces one. The file's current SHA-256 (what `workflow_get` calls `revision`) has to equal `expected_revision`, or nothing is written and the error carries the current one |

`create` is idempotent and not destructive; `update` is destructive. What a write has to pass, in this order, with nothing on the disk until the last step:

* The name matches `^[a-z0-9][a-z0-9_-]{0,63}$`. It is a name, never a path: `../x`, `/etc/x`, `~x`, `X` and 65 characters are refused.
* The text is at most 64 KiB and 32 steps, and `workflow_validate`'s checks pass with no errors (warnings come back in the result). Text a redactor would change, a credential, is refused and never echoed.
* A step with a `gate` is refused unless the server was started with `--allow gates`. A gate is a shell command aibus runs as you when the chain reaches that step, so a remote author writing one is writing code that runs later. `workflow_validate` and `workflow_dry_run` always show the command text.
* The directory is the scope's `workflows` directory, resolved through symlinks, and it has to land exactly where it should: a symlink out of it is refused, and so is a workflow file that is itself a link. The user directory is created when missing; the project scope needs `.aibus/` to exist already.
* The file is written with the atomic write (temp file, fsync, rename), mode 0600, and starts with `# written through aibus mcp, <time>`. An update keeps the line the file already had instead of adding another.

At most 10 writes a minute per process. Nothing here can reach `.aibus/settings.toml` or `config.toml`: the name rule and the directory check leave no path to them.

## Run tools (`--allow run`)

| Tool | What it does |
|---|---|
| `workflow_run {name, scope, input}` | starts the workflow and answers at once with `{run, status: "starting", session}` (`session` is `null` until the run has made one) |
| `run_status {run, wait_ms?}` | `{run, status, step, attempts:[{step, agent, status, failure?}], usage, exit_code, session, tail}`. `wait_ms` is capped at 30 000 and returns early on any change |
| `run_cancel {run}` | cancels and answers with the final status; on a finished run it changes nothing |

A run is a child `aibus run --workflow <name> --json` of the server process, started with the permission mode `dont-ask` on its command line, `AIBUS_I_KNOW` removed from its environment, stdin closed and `--allow-tool` as its only pre-approvals. A tool call that would ask is therefore denied, never queued, and `bypass` cannot be reached through the server whatever its own environment says. Each run has its own session directory with the commit log and the redacted transcript, and nothing starts until the first `workflow_run`, so a client that only reads never causes a credential lookup. The one file `--allow run` makes the server read is `config.toml`, for the default of `--max-cost-usd`.

`status` is the lead's final status in `snake_case` once the process has ended (`completed`, `partial`, `failed`, `cancelled`, `denied`), `starting` and `running` before that. `exit_code` is the exit code of `aibus run` (06 section 3, so 8 for a budget and 7 for an interrupt). `tail` is the last 4 KiB of the redacted report, under a line saying it is data, with aibus's own markers escaped.

The caps are process flags, so a model can neither see nor change them:

* `--max-runs` (10): the eleventh `workflow_run` is an `isError`.
* `--max-concurrent-runs` (1): a second run while one is live is an `isError`.
* `--max-cost-usd` (`limits.max_cost_usd`): the sum of the finished runs' `cost_usd`. Each run starts with what is left of it as its own limit, so a run that would overspend ends with the budget reason and exit code 8; once it is spent no run starts.
* 5 `workflow_run` calls a minute.

When stdin closes (or the HTTP server stops) every live run gets SIGINT, 10 seconds to write its snapshot, and then a kill. Handles live as long as the process; a handle from an earlier process is an unknown one.

## Resources

`resources/list` returns three and `resources/templates/list` one. With `--allow run` the templates become three and the list also holds one entry per run this process started. Nothing else under `aibus://` exists: a uri outside this table, a workflow that is not there and a name with a path in it (`../x`, `%2e%2e`) are all `-32602`. No resource reaches config, accounts or sessions on disk.

| URI | Mime type | Body |
|---|---|---|
| `aibus://workflows` | `application/json` | the `workflow_list` result |
| `aibus://workflows/{scope}/{name}` | `application/toml` | the workflow file, redacted |
| `aibus://schema/workflow` | `application/schema+json` | `docs/schemas/workflow/v1.json`, byte for byte |
| `aibus://agents` | `application/json` | the `agent_list` result |
| `aibus://runs/{run}` (`--allow run`) | `application/json` | the `run_status` body without `tail` |
| `aibus://runs/{run}/log` (`--allow run`) | `text/plain` | the redacted output of the run, see below |

`{run}` is the handle `workflow_run` returned (`run-` and a ulid). Only handles this process gave out are readable: the id of a session directory on disk, a handle from an earlier server process and a made-up one are all `-32602`, and so is any `aibus://runs/...` uri when the server was started without `--allow run`.

The log starts with a line saying it is a run's output, from other agents and from commands, to be treated as data. After that comes everything the run printed (its stream, its stderr, the report's summary last once it has ended), through the redactor and with aibus's own markers escaped. The server keeps the last 256 KiB of it. A log longer than `limits.result_max_bytes` (32 KiB) is cut to its last 32 KiB, which keeps the report's ending, under a `(cut at N of M bytes, the start of the log is left out)` line.

There are no subscriptions. A client that wants to follow a run polls `run_status` with `wait_ms`, which returns on any change, or re-reads the resources.

## Prompts

| Prompt | Arguments | For |
|---|---|---|
| `author_workflow` | `goal`, `scope?` | writing a new workflow: the format, every agent of the roster, the four shipped examples as fenced TOML marked as data, and the house rules; it ends by asking the model to call `workflow_validate` before `workflow_create` |
| `review_workflow` | `name`, `scope?` | looking for summaries nobody reads, gates that cannot block and `on_fail` in the wrong place; the file and the validator's findings are embedded as data |
| `explain_run` | `run` | explaining a run; points the model at `run_status` and the two run resources, and says "runs are not enabled on this server" without `--allow run` |

## Limits and the audit log

* At most 60 tool calls a minute per process, and within that 10 writes and 5 run starts. Past that the call is an `isError` result saying how many seconds to wait. A call that is refused does not spend a call.
* Arguments are capped at 256 KiB. Resource reads and prompt gets are audited but do not use the tool-call budget.
* Every call leaves one line in `~/.local/state/aibus/logs/mcp/<ulid>.jsonl` (one file per process, mode 0600): `{t, client:{name, version}, addr, method, tool, run, args, ok, ms}` (`run` is the run handle on the run tools). The first line is `server/start` with the flags. Arguments are redacted and cut at 4 KiB. A resource read has `method: "resources/read"` and the uri as its argument, a prompt `method: "prompts/get"`.
* Nothing that came out of a file, an agent definition or an argument reaches a client, the audit file or stderr without going through the redactor. stdout carries JSON-RPC and nothing else.

## What is not in this release

`agents.extra_dirs` on the roster of the write and run tools, and resource subscriptions for runs.

Relaying a run's permission prompts to the client is not built. See E12 in `DECISIONS.md`: runs stay on `dont_ask` and anything that would prompt is denied.

## aibus as its own client: the workflow author

The TUI's `/workflow new <goal>` has the built-in `workflow-author` agent draft a workflow for the goal, and it does that through `aibus mcp` over stdio. When a terminal session starts (or `--agent workflow-author` is used) and your config has no `[mcp.aibus]` table, the binary adds one in memory: `command` is this program's own path (`std::env::current_exe()`, resolved through symlinks), `args = ["mcp"]`, no `--allow`, and the usual scrubbed environment. So the child is the read-only server of this page: the author can list and read workflows, list the agents, validate a draft against them and dry-run a saved workflow, and it cannot write or run anything. Its tool list has no `workflow_create`, and the audit file of the child shows `write`, `gates` and `run` all false at `server/start`.

Only the author is offered those tools: every other agent gets a deny for `mcp:aibus:*`, so the lead does not see them. The usual client rules apply, which means every call asks in the permission modal and `a` (always, for this tool) makes the author quiet after the first. What the server answers is wrapped as data. If you have your own `[mcp.aibus]` table it is used as it is and the binary adds nothing; `aibus doctor` shows which one the author will use, and whether the binary's own path resolves.

The author's result is a fenced `toml` block. The lead gets it as data like any child's report, and the last block that validates against your agents is kept as the draft that `/workflow save <name>` writes.

## Streamable HTTP (`--http`)

Stdio is the default and needs nothing. Some clients cannot spawn a process, and for those `aibus mcp --http 127.0.0.1:<port>` serves the same tools over Streamable HTTP, on one endpoint, `/mcp` (POST and GET). It is opt-in by flag and it is loopback only: any address that is not in `127.0.0.0/8` or `::1` is a usage error (exit 2) before a socket or the audit file is created, and there is no flag to override that. Port `0` asks the system for one, and the bound address is the first line on stdout:

```
aibus mcp listening on http://127.0.0.1:53124/mcp
```

Every request needs the bearer token:

* `AIBUS_MCP_TOKEN` from the environment (at least 16 visible ASCII characters), or
* with that unset, 32 random bytes as hex, made at start and printed once to stderr as the exact line to paste, `Authorization: Bearer <token>`.

The token is never written to disk, config, the audit log or any resource, it is registered with the redactor, and the comparison is constant time (both sides are hashed first, so even the length stays out of it). A request without the exact token is 401 with `WWW-Authenticate: Bearer`, before anything else about the request is looked at.

Past the token:

* `Host` must be the address aibus listens on (or `localhost`, `127.0.0.1`, `[::1]` with that port), or the answer is 403. That is the DNS rebinding check.
* `Origin`, when present, must be `http://127.0.0.1:<port>`, `http://localhost:<port>`, `http://[::1]:<port>` or a value given with `--origin <value>` (repeatable); anything else is 403. A request with no `Origin` is fine, since non-browser clients do not send one.
* `MCP-Protocol-Version` is checked and an unsupported value is 400. Both revisions work: `2026-07-28` is stateless with no `Mcp-Session-Id`, and `2025-11-25` opens with `initialize` and gets a session.

The rate limit and the audit log are the ones stdio has; an audit line gains `addr`, the client's socket address. SIGINT and SIGTERM stop the server and end every open stream.

The implementation sits in the `aibus` binary (`crates/aibus/src/mcp_http.rs`) on rmcp's `transport-streamable-http-server`. `aibus-mcp` has no socket and no HTTP stack, and a workspace test keeps it that way.

### A client on another machine

Do not bind a LAN address. Keep the server on loopback and forward a port over SSH from the machine that has the client:

```
ssh -N -L 7777:127.0.0.1:7777 me@the-aibus-machine
```

with `aibus mcp --http 127.0.0.1:7777` running on that machine. The client talks to `http://127.0.0.1:7777/mcp` on its own side with the same bearer token, and the request still arrives on the loopback interface, so every check above holds. Put the token in the client's configuration, not on the command line of a shared machine.

A full OAuth 2.1 resource server is a non-goal: aibus has one user, so one token.
