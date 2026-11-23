# aibus mcp

`aibus mcp` is an MCP server. A client (an editor, a chat app, a coding assistant) starts it, and from then on it can read your workflows and your agent roster, check a draft, and see what a workflow would do, all without a model being called. It is the other direction from the `[mcp.<server>]` tables in your config: those make aibus a client of somebody else's tools, and this makes aibus a server for yours. The two do not share anything.

Out of the box the server is read-only. `--allow write` and `--allow run` add the write and run tools, each described below. When the client can be asked, the write and run tools also stop and ask you to confirm first (see Confirmation), and `--confirm never` turns that off.

## Setting it up

The server speaks stdio by default (and loopback HTTP on request, see the end of this page), so the client only needs the command:

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
| `--confirm elicit\|never` | whether a write or a run asks you first when the client can be asked; `elicit` is the default, see Confirmation | yes |
| `--allow session-list` | `session_list`: the live terminal sessions you can reach | yes |
| `--allow session-status` | `session_status`: lead, model, cards and open permission prompts of a live session | yes |
| `--allow session-send` | `session_send`: a prompt to the lead of a live session, as if typed | yes |
| `--allow session-command` | `session_command`: a slash command line in a live session, through the same parser as typing | yes |
| `--allow session-cancel` | `session_cancel`: cancel a run of a live session | yes |
| `--allow session-transcript` | `session_transcript`: a run's transcript and the session log, redacted | yes |
| `--allow permission` | `session_permission`: answer a permission prompt, only in a session started with `--control-permission` | yes |
| `--allow attach` | `aibus_attach`, `next_step`, `step_result`: attach an open agent session to a live session and run steps on it, only in a session started with `--control-attach` (see Attached sessions) | yes |
| `--allow present` | `session_present`: a read-only panel on the screen, only in a session started with `--control-present` (see Attached sessions) | yes |
| `--session <id>` | the live session the session tools act on; without it, the only live one (see Live sessions) | yes |
| `--http <ip:port>` | the same server over Streamable HTTP on loopback, see below | yes |
| `--origin <value>` | one more `Origin` the HTTP server accepts (repeatable, needs `--http`) | yes |

## Read tools

All seven are read-only (`readOnlyHint`), never destructive and never open-world. Each has an `outputSchema`, and the result is in `structuredContent`. The text block holds the same JSON under a one-line header saying it is data, with aibus's own markers escaped and a "cut at" note past 32 KiB. `structuredContent` is never cut.

| Tool | What it does |
|---|---|
| `workflow_list` | every workflow of the project and of the user, loose files and `.aibus` bundles, with the parse error of a broken file; `bundle` names the bundle a workflow came from, and `ok` and `runnable` are false for a broken file and for a bundled workflow with a gate (its `error` says how to trust it; an older client that reads `ok` as "can run" stays right) |
| `workflow_get {name, scope?}` | the file text, its SHA-256 `revision` and the parsed steps |
| `workflow_explain {name, scope?}` | one paragraph per step (which agent, what it reads, what blocks it, how often it retries) and the data flow between steps; an edge `from: 0` is the workflow input |
| `workflow_validate {name?, scope?, toml?}` | an existing workflow or a draft `toml`, checked against the real roster (built-in, project, user); same shape as `aibus workflow validate --json`, so a draft that names an unknown agent is `ok: false` with the step number |
| `workflow_dry_run {name, scope?, input}` | each prompt rendered with the input (a later step's summary shows as `<summary of step N (agent)>`), the gates as text, the agent each step resolves to, and the budgets |
| `agent_list` | the agents a step can name, as `aibus agent list --json` |
| `workflow_plan {name?, scope?, toml?}` | the compiled plan of an existing workflow or a draft, the same JSON `aibus workflow plan --dry-run --json` prints (`docs/schemas/workflow/plan-v1.json`): steps with prompt and agent hashes, typed edges, fan-out, envelope, approvals, the invocation each step would get, the routing decisions and a `plan_hash`. A workflow that does not validate, misses a capability or carries something credential-shaped gets an `isError` and no plan |

`workflow_plan` came with the compiler (#283) and is listed after the six older tools, which keep their names, arguments and output schemas. For a workflow that uses routes they answer inside the same schemas: `workflow_get` shows the steps' four old fields, and `workflow_explain` and `workflow_dry_run` follow the routes (a step's `{{prev.summary}}` is the step its one route comes from). `workflow_validate` also reports a step that `requires` a capability its agent has no tool for. `workflow_run` refuses a workflow that only the scheduler (#288) can run, before it starts anything.

`scope` is `project` or `user`; left out, the project's file wins. The read tools see the workflows inside `*.aibus` bundles in both directories, read in memory through every bundle check (`docs/spec/06-cli.md` section 9); inside one directory a loose file wins over a bundled one of the same name, and `path` is the bundle's. Budgets in `workflow_dry_run` are the built-in defaults, because the server does not read `config.toml`. Likewise, `agents.extra_dirs` is not on the roster.

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
* The file is written with the atomic write (a temp file under a random name made with `O_CREAT|O_EXCL`, so a symlink planted in the directory is never written through; fsync; rename; then a check that the result is a plain file), mode 0600, and starts with `# written through aibus mcp, <time>`. An update keeps the line the file already had instead of adding another.

`workflow_create` and `workflow_update` write single loose files, never a bundle. The project scope is the directory the server was started in (`--cwd`, or its working directory), and aibus does not walk up from it: a session started in another directory, a subdirectory included, does not see a project workflow written here. That is why a workflow written through MCP could be missing from `/workflow list`. `workflow_list` and `aibus workflow list` say where each workflow came from; use `scope: "user"` for a workflow every session should see.

At most 10 writes a minute per process. Nothing here can reach `.aibus/settings.toml` or `config.toml`: the name rule and the directory check leave no path to them.

## Run tools (`--allow run`)

| Tool | What it does |
|---|---|
| `workflow_run {name, scope, input}` | starts the workflow and answers at once with `{run, status: "starting", session}` (`session` is `null` until the run has made one) |
| `run_status {run, wait_ms?}` | `{run, status, step, attempts:[{step, agent, status, failure?}], usage, exit_code, session, tail}`. `wait_ms` is capped at 30 000 and returns early on any change |
| `run_cancel {run}` | cancels and answers with the final status; on a finished run it changes nothing |

A run is a child `aibus run --workflow <name> --json` of the server process, started with the permission mode `dont-ask` on its command line, `AIBUS_I_KNOW` removed from its environment and `--allow-tool` as its only pre-approvals. The input of a run is not on that command line: the server starts the child as `aibus run --workflow <name> -- -`, writes the input to the child's stdin and closes it, so nothing can answer a prompt through it either. A command line is readable by every local user with `ps` for as long as the process lives, and the input may hold a key. No file is written for it, and every trailing newline and carriage return of the input is dropped (`aibus run` does that to any prompt it reads from stdin). The argument form of `aibus run` (`aibus run --workflow <name> <input>`) still works for people typing it, and it is visible to every local user in `ps` and in shell history; the server never uses it (`docs/spec/06-cli.md` says the same). A tool call that would ask is therefore denied, never queued, and `bypass` cannot be reached through the server whatever its own environment says. Each run has its own session directory with the commit log and the redacted transcript, and nothing starts until the first `workflow_run`, so a client that only reads never causes a credential lookup. The one file `--allow run` makes the server read is `config.toml`, for the default of `--max-cost-usd`.

`workflow_run` does not start a workflow from an `.aibus` bundle that has a `gate`: a bundle is someone else's file and the gate is a shell command that would run as you, so it waits until the user installs the bundle with `aibus workflow install <file> --allow gates` (which writes it as a loose file of theirs). Nothing a model can call trusts a bundle.

`status` is the lead's final status in `snake_case` once the process has ended (`completed`, `partial`, `failed`, `cancelled`, `denied`), `starting` and `running` before that. `exit_code` is the exit code of `aibus run` (06 section 3, so 8 for a budget and 7 for an interrupt). What the server redacts of a child's events: the strings of an attempt (its name, status and failure), the run id and the session are redacted whole, as the child sent them, before they are stored (the failure before its 300 byte cut). The text for the tail and the log (the stream, a summary, a failure of no attempt, stdout and stderr lines) is stored raw, about 28 to 36 KiB more than what is shown, and is redacted when it is read: the whole buffer goes through the redactor first and is cut to the last 4 KiB (the tail) or the last 256 KiB (the log) after. So a key the child sent in pieces over several stream events, a private key block sent a line at a time and a token of any length are whole when the redactor sees them, and a cut falls in text that is already replaced. The result goes through the redactor once more on the way out. What this does not cover:

* Text from another source between the pieces of a secret (a stderr line between two stream pieces) leaves no pattern to match, as it never did.
* The context patterns (`bearer`, `api-key-field`, `assignment`) match a name together with its value, so they still miss a value whose name was cut off, which can happen at the start of the stored window.
* When the cut falls just before a context name that was whole but inside a token (`x.|Bearer <value>`, `x.|api_key: <value>`), the move past the token takes the name with it, and a value that only its name marks (no key shape of its own) is then shown, if the redaction also shrinks the buffer so that the start of the window is shown (one unterminated `-----BEGIN PRIVATE KEY-----` line does that). A `name=value` pair goes whole only when the value is token characters to its end (`=` counts as one). With a character outside the token set in it, such as a quote, `!` or `@` (`x.api_key="v"`, `--api-key="v"`, `x.password=hunter!v`, `x.token=ab@v`), a cut inside the name moves on to just after that character, and the rest of the value shows.
* The start of the stored window can hold the rest of something that began before it. The window starts after the next line start, or else (when the cut falls inside a token) after the next character a key cannot contain, within 4 KiB of the cut, which removes the rest of a key. But one unterminated `-----BEGIN PRIVATE KEY-----` line is enough to make the redaction shrink the buffer so that the start of the window is shown, and the body lines of a private key whose BEGIN line was cut off then show.
* A read in the middle of a run can show the first piece of a key that is still arriving, since a piece of a key too short for any pattern matches none.
* The raw text sits in the server's memory until it is cut off.
* The server's redactor holds the patterns but no registered secrets (the server process registers none), so a secret that only the child knows by value is covered only by the child's own redaction of each event.

`tail` is the last 4 KiB of the redacted report, under a line saying it is data, with aibus's own markers escaped.

The caps are process flags, so a model can neither see nor change them:

* `--max-runs` (10): the eleventh `workflow_run` is an `isError`.
* `--max-concurrent-runs` (1): a second run while one is live is an `isError`.
* `--max-cost-usd` (`limits.max_cost_usd`): the sum of the finished runs' `cost_usd`. Each run starts with what is left of it as its own limit, so a run that would overspend ends with the budget reason and exit code 8; once it is spent no run starts.
* 5 `workflow_run` calls a minute.

When stdin closes (or the HTTP server stops) every live run gets SIGINT, 10 seconds to write its snapshot, and then a kill. Handles live as long as the process; a handle from an earlier process is an unknown one.

## Live sessions (`--allow session-*`)

The session tools act on a terminal session that is already open (`aibus` on a terminal), not on a run the server starts. Whatever a client sends goes into my session as if I had typed it, shows on my screen at once and is tagged as sent through MCP. Each tool has its own `--allow` value (table above) and is not listed without it; calling one that is off is `-32602`.

| Tool | What it does |
|---|---|
| `session_list` | `{sessions:[{id, cwd, lead, model, started}]}`, oldest first |
| `session_status` | `{lead, cards:[{run, agent, model, status, via?, client?}], permissions:[{id, run, agent, tool, summary}], client}` |
| `session_send {text}` | the text as a prompt on my prompt line: a message to the running lead, or the lead's first prompt. A slash line is refused (use `session_command`) |
| `session_command {line}` | a slash line through the parser the prompt line uses, so the same refusals apply: `/login <provider> <key>` is refused with the key `[redacted]`, and `/login <provider>` is refused too because it opens a masked key entry. On top of that, what a line would do decides what stays mine, never its name, so aliases and later commands are covered: ending the session (`/quit`, `/exit`, `/q`), any confirmation (`/logout`, `/clear`, `/cancel` on the lead), writing files (`/workflow save`, `/handoff`, `/ledger`), opening a screen or menu with live letter keys (`/accounts`, `/model`, `/agents`, `/workflow list`) or moving the focus, which is refused even when I am idle because my next keystroke would drive it, and opening or closing an inert screen (`/help`, `/cost`, `/status`) while I have a screen or a menu open, a field being edited, or text on the prompt line. Each refusal is one line, on my screen too |
| `session_cancel {run}` | `/cancel @<run>` as typed: an id, a prefix of one, an agent or `lead`. A worker is cancelled at once; the lead asks first, so a client cannot cancel it (only the keyboard answers a confirmation) |
| `session_transcript {run?}` | the run's transcript (the lead's without `run`), its last 32 KiB, wrapped as data with aibus's markers escaped, and the last 200 session log rows |
| `session_permission {id, decision}` | answers the permission prompt my screen shows (`allow`, `allow_session`, `deny`). Only with `--allow permission` here **and** a session started with `aibus --control-permission`; without the second, the session refuses and the prompt stays mine |

**Which session.** `--session <id>` names one. Without it the server uses the only live session, and when there are several it refuses each call and lists them (id, directory, start time), so a client never lands in the wrong one by accident. No session is looked up or connected to until the first session call.

**The socket.** A terminal session opens a Unix socket at `$XDG_RUNTIME_DIR/aibus/sessions/<session>.sock` (where there is no `XDG_RUNTIME_DIR`, as on macOS, `$TMPDIR/aibus-<uid>/sessions/`, or `/tmp/aibus-<uid>/sessions/` when the `$TMPDIR` one would put a socket past about 100 bytes, which macOS's default `$TMPDIR` does, because `sun_path` takes 104 there; `XDG_RUNTIME_DIR` is always used as it is). The `sessions` directory and the one above it are 0700 and have to be mine, and the directory above those (`XDG_RUNTIME_DIR`, the temporary directory or `/tmp`) has to be mine or root's and sticky if others can write to it; anything else is refused, never repaired, the socket and the two files next to it are 0600: `<session>.token` (32 random bytes as hex) and `<session>.json` (id, pid, directory, lead, model, start time). All three go when the session ends, and a session that died without cleaning up leaves a socket nobody answers on, which the next session to start removes, along with its two files. A new socket is bound under a temporary name and linked to its real one only once it listens, so two sessions starting at once never sweep each other, and a session only ever removes the socket it made. There is no TCP and no remote option. `aibus --no-control` opens no socket at all. A session whose socket cannot be opened says so in its log pane (``io error in control socket, so `aibus mcp` will not find this session: ...``, with the home directory shown as `~`) as well as in the log file, and carries on without it.

**Who gets in.** In this order: the peer has to be my own user (the kernel's peer credentials on the connection), its first line has to be `{"hello":1, "token":..., "client":...}` with the token from the file (compared in constant time, within 5 seconds), and no other client may hold the session: one controlling client at a time, and a second one is refused until the first goes. At most 16 connections may be waiting to say hello; past that a new one is closed at once. A client that says nothing for 10 minutes is disconnected so the next one can come in. On its side `aibus mcp` checks the same directories, that the socket file is mine, and (from the kernel's peer credentials) that the process behind it is mine, before it reads the token or sends it. The screen says when a client connects and when it goes (`mcp <client> connected`), and the status bar shows `mcp <client>` while it is there.

**Framing and limits.** One JSON object per line each way, like the headless control channel: `{"id":n, "op":"status"|"send"|"command"|"cancel"|"transcript"|"permission", ...}` and `{"id":n, "ok":true, "result":...}` or `{"id":n, "ok":false, "error":"..."}`. A line is at most 400 KiB (a 64 KiB text even with every character escaped, plus the envelope); a longer one is refused and the connection closed. The session takes 60 lines a minute, and 10 of them may change it; the limits belong to the session, so reconnecting does not refill them. A call the session does not answer within 30 seconds (an editor open, say) is answered with an error saying nothing was done, and the session drops it rather than doing it late, so a retry never acts twice. On the server, a text or a line over 64 KiB is an `isError` that sends nothing, and `session_send`, `session_command`, `session_cancel` and `session_permission` share a limit of 10 a minute per process.

**The tag.** Everything a client sends reaches the session as `Msg::Via { client, msg }`, where `msg` is exactly what the keyboard would have produced for the same line, and it takes the same path a key does. Its session log rows carry `via: "mcp"` and the client's name and show an `mcp` badge in the actor column; a run it starts carries the same two fields in its meta; the transcript line of its prompt or message has them too, and `aibus session export --format md` heads that message `user, via mcp (<client>)`. The commit log has them in `model:last_msg`. The server's audit line for a session tool adds `via: "mcp"` and `session`.

**Redaction.** The session redacts every answer with its own redactor (the patterns and every secret the session has registered) before it writes it to the socket, and the server redacts it again before it leaves, like `run_status`. The transcript is wrapped as data. What this leaves: text that no pattern recognises and that the session never registered as a secret (a password typed into a prompt, say) is shown as it is, as it would be on the screen; and the transcript is the last 32 KiB of the run's text, so a long secret cut at that edge loses its start, which can leave the rest unrecognisable.

Answering a permission prompt from a client is not the relay that E12 in `DECISIONS.md` rules out for the runs the server starts: those stay on `dont_ask`. Here it is my own session, it takes an opt-in at both ends (`--allow permission` on the server, `--control-permission` on the session), a client can only answer the prompt my screen shows, and the answer is tagged like everything else.

**Untrusted input.** A client is not me. Its text is a user message from a client, tagged as such; a tool result that quotes it is data like any other; and with `--confirm elicit` the four tools that change the session ask first (see Confirmation). Its name is cut to ASCII letters, digits, space, `.`, `_` and `-` (32 at most) before it reaches my screen, the log or an export heading, and what it sends wears an `[mcp <client>]` badge in the run's transcript on screen.

## Attached sessions (Phase 1 spike, #322)

An agent session that is already open (Claude Code, say) can attach to a live aibus session. It is still
experimental: the pull leg only. #353 adds `runtime = "attached"` to workflows (below) and a live check against Claude
Code (`docs/spikes/353-attached-claude-code.md`).

**Attaching.** The agent calls `aibus_attach {label?, lineage?}`. The session needs `--control-attach`. The screen
logs `<client> wants to attach (pid, cwd), code 7Q2K`; the tool's answer does not carry it, so the user reads it off the screen. Only a typed
`/attach accept 7Q2K` at the aibus keyboard attaches it (60 s, then nothing changed); `/attach deny` refuses. A client has
no way to send that line: `session_command` goes through the prompt's parser, which has no `/attach`. One agent holds a
session at a time.

**aibus runs a step on the agent.** `/attach step <text>` queues one step (10 minutes to answer). The agent pulls it
with `next_step {wait_ms?}` (it asks the session every 3 s for up to 20 s) and gets it redacted and wrapped as data. It
carries the step out in its own context and answers `step_result {token, status, summary}` with `done`, `declined` or
`failed`. `/attach cancel` marks the step cancelled (and queues an interrupt for the agent, below): Claude Code has no
documented push interrupt, and a result that arrives after a cancel, a timeout or a first answer is refused. If the agent goes
away the step is `unavailable`. The answer carries the evidence: `dispatched` observed, `received` observed (the pull
answered), `started` and `tool_calls` unavailable, `summary` inferred (the agent said so), tokens and cost unavailable with
quality `unknown`, duration observed with quality `derived`. Every row it causes is tagged `mcp` and the status bar says
`mcp <client> attached`.

**Ack, tool calls and interrupt (#346).** Three more calls on the same pull leg, all behind `--allow attach`. `step_ack {token}`
says the agent has begun the step it pulled (once), so `started` becomes observed in the evidence. `step_tool_call {token, name}`
reports one tool call by name (no arguments, no output): aibus matches it to the step by token and labels it `correlated`, never
`observed`, because it did not see the call. Names are redacted, cleaned and cut to 40 characters, 100 are kept and the count stops at
10,000. A step with no reports says `unavailable`, not zero. `/attach cancel` on a step the agent already pulled is now an interrupt:
nothing can push to the agent, so aibus keeps it and the agent sees `{"interrupt": true}` once, as the answer to its next `step_ack`
or `step_tool_call` (or `next_step`), and the server drops its in-flight mark. A result after the cancel is still refused, and an agent
that never calls again is not stopped. That is as far as the pull leg goes; a real push is in `DEFERRED.md`.

**A workflow step can name the attached session (#353).** In a workflow file a step with `runtime = "attached"` is not
started as a run: the `workflow` tool hands its rendered prompt to the attached session, which pulls it with `next_step`
and answers with `step_result`, exactly like a step queued with `/attach step`. The step's `agent` is only its name (the
key `{{steps.<agent>.summary}}` reads); the agent's tools, `requires` and `[approvals]` do not bound what the attached
session does, and its permission prompts stay with you in that session. The plan shows the step (`workflow plan
--dry-run`: runtime `attached`, invocation `attached_session`, a note about permissions) and it is in the plan hash. A
workflow with such a step starts nothing, and says why, when the session was not started with `--control-attach`, nobody
is attached, or a step is already in flight. `aibus run --workflow` and `workflow_run` refuse it too: they have no
attached session to give the step to. The answer comes back wrapped as data and redacted, as a result always does; a
`declined` answer ends the step as `denied`, `failed` as `failed`, and a deadline (10 minutes) or an agent that left as a
failed step with the reason. Cancelling the workflow cancels the step and queues the same interrupt. The step is
one at a time like every step here, and it carries the same `mcp` tag and audit lines as a keyboard-queued one.

**The agent controls aibus.** Besides the #274 tools, `session_present {title, message?, columns?, rows?}` (with
`--allow present` and a session started with `--control-present`) shows plain, capped, redacted text on a panel over the
log, badged `[mcp <client>]`. It is not a screen: no live keys, no focus change, Esc closes it. The first panel of a client
waits for `/attach accept <code>` as well, and while the user has a screen or menu open, a field being edited or text on the
prompt line the call is refused as busy. Nothing in #274's refusals changes. To set up a workflow the agent uses
`workflow_validate` and `workflow_create` (`--allow write`), then `session_present` to show it; getting it into the live
session still means the user opening it (`/workflow list` stays the keyboard's).

**No loops.** While a step is in flight on a client, that client's `workflow_run`, `session_send`, `session_command` and a
second `next_step` are refused with `would loop (step <id> is in flight on this attachment)`, in this server and again in the
session. Every session has a lineage token and a step carries it; `aibus_attach {lineage}` from a session that echoes this
session's token, or any lineage as long as the bound (1), is refused. The echo is cooperative, the in-flight rule is not.

**Turn-end hook.** `examples/claude-code-attach/stop-hook.sh` is an example Stop hook that asks Claude Code to call
`next_step` when a turn ends, bounded and off until you create a marker file. aibus never installs it, and it never answers a
permission prompt or an elicitation. Channels (`--dangerously-load-development-channels`) are a later opt-in and are not here.

## Confirmation

`workflow_create`, `workflow_update` and `workflow_run` can stop and ask you before they do anything, and so can the four session tools that change a live session. This is a second check on top of the flags, not a replacement: a tool that is off is still not listed, whatever you answer.

* **When.** Only when the tool is listed (its flag is on) and the client declared `elicitation` in form mode when it connected. A client that did not declare it is never asked, and the call goes on as it always did, because the flags are then the only gate. A bare `elicitation: {}` counts as form mode.
* **What is asked.** A form with one field, the workflow name, which you have to retype exactly. For `session_send`, `session_command`, `session_cancel` and `session_permission` it is the last 8 characters of the live session's id (its random part), which the form shows. For `workflow_run` under a cost cap it also holds `max_cost_usd`, with the cap as its maximum, so the cost line of a run is in front of you before it starts. Only `accept` with the exact name goes on. `decline`, `cancel`, a different name or a cost that is missing or over the cap ends the call as an `isError` and nothing is written or started. The form never asks for a credential, and the answer is compared on the server.
* **How, per protocol.** A `2025-11-25` client gets an `elicitation/create` request in the middle of the call. A `2026-07-28` client gets an `input_required` result (multi round-trip requests, MRTR) carrying a `requestState`, and retries the same call with its answer. The `requestState` is sealed with an HMAC under a key made when the process starts and never stored. It is bound to the tool, the exact arguments and the client, it is good once for 10 minutes, and a state that was edited, expired, replayed or taken from another client is one `isError`.
* **Turning it off.** `aibus mcp --confirm never` skips the form, for scripted use where nobody is there to answer. The flags stay the only gate. The default is `--confirm elicit`.

Each ask and each refusal leaves a `confirm/ask` or `confirm/refused` line in the audit log.

Relaying a run's own permission prompts to the client through the same mechanism was considered and is a recorded no-go (E12 in `DECISIONS.md`): a run stays on `dont_ask`, and a tool call that would ask is denied. There is no flag for it.

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
* Every call leaves one line in `~/.local/state/aibus/logs/mcp/<ulid>.jsonl` (one file per process, mode 0600): `{t, client:{name, version}, addr, method, tool, run, args, ok, ms}` (`run` is the run handle on the run tools; a session tool adds `via: "mcp"` and `session`). The first line is `server/start` with the flags. Arguments are redacted and cut at 4 KiB. A resource read has `method: "resources/read"` and the uri as its argument, a prompt `method: "prompts/get"`.
* Nothing that came out of a file, an agent definition or an argument reaches a client, the audit file or stderr without going through the redactor. stdout carries JSON-RPC and nothing else.

## What is not in this release

`agents.extra_dirs` on the roster of the write and run tools, and resource subscriptions for runs.

Relaying a run's permission prompts to the client is a recorded no-go, not a gap to fill later by flag. See Confirmation and E12 in `DECISIONS.md`: runs stay on `dont_ask` and anything that would prompt is denied.

## aibus as its own client: the workflow author

The TUI's `/workflow new <goal>` has the built-in `workflow-author` agent draft a workflow for the goal, and it does that through `aibus mcp` over stdio. When a terminal session starts (or `--agent workflow-author` is used) and your config has no `[mcp.aibus]` table, the binary adds one in memory: `command` is this program's own path (`std::env::current_exe()`, resolved through symlinks), `args = ["mcp"]`, no `--allow`, and the usual scrubbed environment. So the child is the read-only server of this page: the author can list and read workflows, list the agents, validate a draft against them and dry-run a saved workflow, and it cannot write or run anything. Its tool list has no `workflow_create`, and the audit file of the child shows `write`, `gates` and `run` all false at `server/start`.

Only the author is offered those tools: every other agent gets a deny for `mcp:aibus:*`, so the lead does not see them. The usual client rules apply, which means every call asks in the permission modal and `a` (always, for this tool) makes the author quiet after the first. What the server answers is wrapped as data. If you have your own `[mcp.aibus]` table it is used as it is and the binary adds nothing; `aibus doctor` shows which one the author will use, and whether the binary's own path resolves.

`/workflow new --write <goal>` and `/workflow edit <name> <change>` are how the author is allowed to save, and they are per command: nothing stays on afterwards, and there is no config key for it (see G8 in `DECISIONS.md`). They start a second child, `aibus mcp --allow write` (and no `--allow gates`, no `--allow run`), registered as the server `aibus-write` and offered to that one author run in place of the read-only child, so `workflow_create` and `workflow_update` exist for the author only then. Every call of either asks in the permission modal, which shows `create review (project), 4 steps, no gate` or `update review, revision ab12cd34...`; `y` writes, `n` denies and the author sees the denial as data, and "always" is not offered (pre-approved patterns and a stray `a` do not skip the question either). `edit` tells the author to read the file with `workflow_get` first and pass that revision to `workflow_update`, so a file you changed in between is the server's conflict error and not an overwrite. A draft with a `gate` is refused by the server, and the author is told to leave gates to you. The TUI logs one line when a write lands: `workflow review written by workflow-author`.

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
