# aibus mcp

`aibus mcp` is an MCP server. A client (an editor, a chat app, a coding assistant) starts it, and from then on it can read your workflows and your agent roster, check a draft, and see what a workflow would do, all without a model being called. It is the other direction from the `[mcp.<server>]` tables in your config: those make aibus a client of somebody else's tools, and this makes aibus a server for yours. The two do not share anything.

Today the server is read-only. The `--allow write` and `--allow run` flags parse, and the tools behind them are not in this release.

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
| `--allow write` | create and update workflows | not yet (the flag parses) |
| `--allow gates` | a written workflow may carry `gate` commands; needs `write` (exit 2 otherwise) | not yet |
| `--allow run` | start, watch and cancel runs | not yet (the flag parses) |
| `--allow-tool <pattern>` | pre-approves a tool for runs | not yet |
| `--http <ip:port>` | the same server over Streamable HTTP on loopback, see below | yes |

## Tools

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

## Resources

`resources/list` returns three and `resources/templates/list` one. Nothing else under `aibus://` exists: a uri outside this table, a workflow that is not there and a name with a path in it (`../x`, `%2e%2e`) are all `-32602`. No resource reaches config, accounts, sessions or logs.

| URI | Mime type | Body |
|---|---|---|
| `aibus://workflows` | `application/json` | the `workflow_list` result |
| `aibus://workflows/{scope}/{name}` | `application/toml` | the workflow file, redacted |
| `aibus://schema/workflow` | `application/schema+json` | `docs/schemas/workflow/v1.json`, byte for byte |
| `aibus://agents` | `application/json` | the `agent_list` result |

## Prompts

| Prompt | Arguments | For |
|---|---|---|
| `author_workflow` | `goal`, `scope?` | writing a new workflow: the format, every agent of the roster, the four shipped examples as fenced TOML marked as data, and the house rules; it ends by asking the model to call `workflow_validate` before `workflow_create` |
| `review_workflow` | `name`, `scope?` | looking for summaries nobody reads, gates that cannot block and `on_fail` in the wrong place; the file and the validator's findings are embedded as data |
| `explain_run` | `run` | explaining a run; says "runs are not enabled on this server" until `--allow run` exists |

## Limits and the audit log

* At most 60 tool calls a minute per process. Past that the call is an `isError` result saying how many seconds to wait. A call that is refused does not spend a call.
* Arguments are capped at 256 KiB. Resource reads and prompt gets are audited but do not use the tool-call budget.
* Every call leaves one line in `~/.local/state/aibus/logs/mcp/<ulid>.jsonl` (one file per process, mode 0600): `{t, client:{name, version}, method, tool, args, ok, ms}`. The first line is `server/start` with the flags. Arguments are redacted and cut at 4 KiB. A resource read has `method: "resources/read"` and the uri as its argument, a prompt `method: "prompts/get"`.
* Nothing that came out of a file, an agent definition or an argument reaches a client, the audit file or stderr without going through the redactor. stdout carries JSON-RPC and nothing else.

## What is not in this release

Writing workflows (`--allow write`), running them (`--allow run`) and the run resources, prompts that need a run, and elicitation. The README and this page will say so when that changes.

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
