# 06. CLI

`aibus` is one binary. With no subcommand on a TTY it opens the dashboard. Everything the TUI can do has a subcommand or flag, so scripts and CI get the same engine.

## 1. Surface

```
aibus [OPTIONS] [PROMPT]                 open the TUI (optionally with a first prompt)
aibus run [OPTIONS] <PROMPT>             headless session, NDJSON events on stdout
aibus agent  list | show <name> | new <name> | validate [path] | import <dir> | where
aibus account add <name> --provider <id> [--key-stdin|--env VAR|--file PATH] [--base-url URL] [--label TEXT]
             list | show <name> | test <name> | remove <name> | enable|disable <name> | revoke <name>   (revoke: OpenRouter keys only)
aibus session list | show <id> | resume <id> [--run <run-id>] [MESSAGE] | export <id> [--format md|jsonl] | log <id> | replay <id> | prune [--older-than 30d]
aibus handoff create [--topic T] [--outcome O] | list | show [PATH]
aibus ledger show | new <topic> | update <section> <text>
aibus memory recall <query> [-k N] | add <text> [--type T] [--scope S] | index
aibus workflow list | show <name> | validate [<name>|<path>|<file>.aibus] | run <name> <input>
               pack <name>... -o <file>.aibus [--description T] [--with-agents] [--force]
               install <file>.aibus [--user] [--force] [--allow gates]
               plan <target> --dry-run [--locked <plan.json>] | diff <a> <b> | manifest <target> [--input <text>|-] | export <target> [--dir <dir>] [--apply <hash>]
aibus runs list | inspect <run> [--node ID | --event N] | replay <run> [--offline] [--events] | rerun <run>   recorded runs (15-recorded-runs.md)
aibus sessions [list] | tap <session> [--follow] | hooks install|uninstall --client claude | allow-transcripts claude [--revoke]   Claude Code sessions open in other terminals, read-only (17-tap-in.md)
aibus extensions search <query> | install <id>[@ver] | verify|update|disable|enable|pin|unpin|rollback|remove <id> | list | test <path-or-id> --fixtures | publish <dir>   extensions for official CLIs (16-extensions.md)
aibus schedule print [<name>] [--bin <path>]   the config's [[schedule]] runs as a crontab line, a launchd plist and systemd units; prints, installs nothing (#391, docs/MCP.md)
aibus config path | show | init | validate | edit
aibus mcp [--allow write|gates|run|session-*|permission]... [--session <id>] [--allow-tool <pattern>]... [--http <ip:port> [--origin <o>]...]   MCP server (section 8, docs/MCP.md)
aibus doctor
aibus completions <bash|zsh|fish|powershell>
aibus version
```

Global options (before or after the subcommand):

| Flag | Meaning |
|---|---|
| `--model <alias\|id>` | lead model for this session |
| `--advisor <alias\|id\|off>` | advisor model, or disable |
| `--router <alias\|id\|off>` | router model, or disable |
| `--account <name>` | explicit account for the lead (04 section 4.1 step 1) |
| `--agent <name>` | run the session as this agent (01 section 4.2) |
| `--agents <json>` | inline definitions, highest precedence |
| `--permission-mode <mode>` | `default`, `accept-edits`, `dont-ask`, `bypass`, `plan` |
| `--allow <tool>` / `--deny <tool>` | adjust the lead's tool policy for the session (repeatable) |
| `--topic <name>` | ledger/handoff topic |
| `--cwd <dir>` | project directory |
| `--config <path>` | config file override |
| `--json` | machine output where it applies (section 4) |
| `--no-hooks` | do not run any hooks (prints a warning) |
| `--theme <name>`, `--ascii`, `--no-mouse` | UI |
| `-v`, `-vv`, `-q` | log verbosity to the log file (never to the terminal in TUI mode) |
| `--trace <file>` | write a Chrome trace of commits and effects (08 section 6) |
| `--no-control` | the terminal session opens no control socket for `aibus mcp --session` (section 8) |
| `--control-permission` | a client of `aibus mcp --allow permission` may answer this session's permission prompts; off, only the keyboard can |
| `--control-attach` | an agent session may attach through `aibus mcp --allow attach`, accepted by a typed `/attach accept <code>` (#322) |
| `--control-present` | a client of `aibus mcp --allow present` may put a read-only panel on the screen, the first one accepted at the keyboard (#322) |

## 2. `aibus run`

The headless mode. Stdout is NDJSON, one event per line, stderr is human-readable status. The prompt comes from the argument or stdin (`-` or no TTY on stdin). An argument is visible to every local user in `ps` while the run lives, and in shell history when typed, so pipe anything secret (`aibus mcp` always does, for the input of a workflow run).

```bash
aibus run --agent code-reviewer --json "review the diff on this branch" > events.jsonl
echo "summarise README.md" | aibus run --model haiku --permission-mode dont-ask
aibus run --workflow fix --outcome PARTIAL_PLUS "login fails silently"
```

Event schema (one JSON object per line, `type` discriminated, same names as `Msg`):

```json
{"type":"RunStarted","t":"2026-11-07T23:48:05.012Z","run":"01JC…","agent":"lead","model":"claude-sonnet-4-5","account":"anthropic-work"}
{"type":"Stream","run":"01JC…","text":"I'll start by reading"}
{"type":"ToolCallRequested","run":"01JC…","call":{"id":"toolu_…","name":"read","input":{"path":"README.md"}}}
{"type":"ToolCallFinished","run":"01JC…","call_id":"toolu_…","ok":true,"bytes":1842,"ms":3}
{"type":"RouterAnswered","fork":1315,"kind":"which_file","choice":"src/auth.ts","p":0.94,"verdict":"sharp"}
{"type":"AdvisorSpoke","checkpoint":"before_plan","advice":"plan skips auth","tokens_read":122000}
{"type":"PermissionNeeded","id":"01JC…","run":"01JC…","tool":"bash","input":{"command":"cargo test"}}
{"type":"RunFinished","run":"01JC…","status":"completed","turns":7,"usage":{"input_tokens":41200,"output_tokens":3100,"cost_usd":0.19},"summary":"…"}
{"type":"SessionEnd","session":"01JC…","exit_code":0,"handoff":"thoughts/shared/handoffs/general/2026-11-07_23-52_review.yaml"}
```

Without `--json`, `run` prints the lead's text to stdout as it streams and the final summary, with tool calls as one-line notices on stderr.

Permissions in headless mode: `--permission-mode` decides. `default` with no TTY means `ask` becomes `deny` with a `PermissionNeeded` event followed by `PermissionDecided{decision:"denied",reason:"no tty"}`; use `accept-edits` or `dont-ask` deliberately, or `--allow bash(cargo *)` to pre-approve. `bypass` requires `AIBUS_I_KNOW=1` in the environment as well, so it cannot be enabled by a flag alone.

Stdin after the prompt is a control channel when `--json` is set: lines like `{"type":"Permission","id":"…","decision":"allow"}` or `{"type":"SendMessage","run":"…","text":"…"}` are accepted. That is how an outer orchestrator (or a test) drives a headless session.

## 3. Exit codes

| Code | Meaning | Source |
|---|---|---|
| 0 | success; for `run`, the lead finished `Completed` | |
| 1 | unexpected error (IO, panic caught at the top) | `AibusError::Io` and unknowns |
| 2 | usage error (clap) or invalid arguments | `Usage` |
| 3 | config or agent definition invalid | `Config` |
| 4 | credential missing or rejected, no account for model | `Auth` |
| 5 | provider failed after retries | `Provider` |
| 6 | a tool was denied or blocked and the run could not proceed | `Denied` |
| 7 | interrupted (SIGINT/SIGTERM, `Ctrl+c` twice) | `Interrupted` |
| 8 | token or cost budget exceeded | `Budget` |
| 10 | the lead run ended `Failed`, `Partial` or `Cancelled` | `Run` |
| 11 | `doctor` found problems | |
| 12 | `agent validate`, `workflow validate`, or `workflow plan`, `diff`, `manifest` or `export` found errors (the workflow gets no plan) | |
| 13 | `workflow plan --locked` found the plan differs from the saved one (drift) | |

`session list --json` and friends exit 0 even when the list is empty.

## 4. `--json` outputs

| Command | Shape |
|---|---|
| `agent list` | `[{name, scope, path, role, model, tools, description}]` |
| `agent show` | the full definition as JSON (frontmatter fields plus `prompt`) |
| `agent validate` | `{ok, errors:[{path, field, message}], warnings:[..]}` |
| `workflow list` | `[{name, path, description, steps, scope, bundle, source, runnable, not_runnable?}]`, or `{name, path, error, scope, bundle, source, runnable}` for a broken file or a refused bundle |
| `workflow show` | `{name, path, description, steps:[{agent, prompt, gate?, on_fail?}], source, not_runnable?}` |
| `workflow validate` | `{ok, errors:[{path, step, field, message}], warnings:[..]}` |
| `workflow plan --dry-run` | the compiled plan, `docs/schemas/workflow/plan-v1.json` (or the validate shape with `ok: false` and exit 12) |
| `workflow diff` | `{same, a, b, changes:[{path, a, b}]}`, `a` and `b` the two plan hashes |
| `workflow manifest` | `run-manifest.json`, `docs/schemas/workflow/run-manifest-v1.json` (always JSON) |
| `workflow export` | `{dir, files, diff, hash}`, or `{written}` with `--apply` |
| `account list` | `[{name, provider, label, auth:"keyring|env|file|none", default_model, status}]` |
| `account test` | `{name, ok, label, org, models:[..], latency_ms}` or `{name, ok:false, error:{kind, message}}` |
| `session list` | `[{id, started_at, ended_at, topic, runs, outcome, cost_usd}]` |
| `runs list` | `[{id, workflow_run_id, provider, state, requires_reconciliation, first_seen, last_seen, events, attempts, agents, tools, edges:[observed, correlated, inferred, unavailable], gaps, cost:{name, value, unit, quality, source}, corrupt_lines, torn_bytes, unsupported_schema, error}]` |
| `session show` | `{session, runs:[{id, agent, status, turns, usage}], handoff, ledger}` |
| `session log` | NDJSON of the commit log: `{time, intent, changed, at}` |
| `handoff list/show` | the YAML parsed to JSON |
| `memory recall` | `[{path, score, snippet}]` |
| `doctor` | `{checks:[{name, ok, detail}]}` |
| `config show` | the effective merged config (no secrets exist in it) |
| `run` | NDJSON stream (section 2) |

Timestamps are RFC 3339 UTC. Ids are ULIDs as strings. Nothing in any `--json` output contains a credential or a raw transcript body longer than `limits.result_max_bytes`; longer bodies are paths.

## 5. Non-TTY behaviour

- stdout not a TTY and no subcommand: behave as `aibus run` with the prompt from stdin, and print a one-line stderr note saying so.
- stdin not a TTY and no prompt argument: read the prompt from stdin.
- `NO_COLOR` set or stdout not a TTY: no ANSI in human output.
- `TERM=dumb`: `run` only; the TUI refuses with exit 2 and a message.
- SIGTERM/SIGHUP: graceful shutdown (handoff, snapshot) with a 10 s budget, then exit 7.
- SIGWINCH: resize `Msg`.

## 6. Completions and help

`aibus completions zsh > ~/.zfunc/_aibus`. Agent names, account names, session ids and workflow names complete dynamically (`aibus agent list --names` is the hidden helper the completion scripts call). `aibus help <sub>` and `-h` are clap's; `--help` on the root also prints the key map summary.

## 7. Environment variables

| Variable | Effect |
|---|---|
| `AIBUS_CONFIG` | config path |
| `AIBUS_ACCOUNT_<NAME>_KEY` | credential for account `<NAME>` (upper-cased, `-` to `_`), when its auth is `env` |
| `AIBUS_MAX_SPAWN_DEPTH`, `AIBUS_MAX_CONCURRENT_RUNS` | limits (01 section 4.4) |
| `AIBUS_OUTCOME` | answers the outcome prompt headlessly |
| `AIBUS_I_KNOW` | required for `--permission-mode bypass` |
| `AIBUS_LOG` | `tracing` filter for the log file, e.g. `aibus_core=debug` |
| `AIBUS_PROJECT_DIR`, `AIBUS_SESSION_ID`, `AIBUS_RUN_ID` | set for hooks and tools, not read by aibus itself |
| `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME` | 07 section 1 |
| `NO_COLOR`, `COLORTERM`, `TERM`, `EDITOR` | standard meanings |

No environment variable carries a credential except the explicit `env` auth method, and aibus never enumerates the environment into logs.

## 8. `aibus mcp`

An MCP server for other programs to read workflows through. It starts read-only, speaks stdio unless `--http` is given, and answers `2026-07-28` and `2025-11-25`.

| Flag | Meaning |
|---|---|
| `--allow write\|gates\|run` | add to what the server may do (repeatable); `gates` needs `write`, otherwise exit 2 |
| `--allow session-list\|session-status\|session-send\|session-command\|session-cancel\|session-transcript\|permission` | one live-session tool each, acting on a terminal session that is already open (#274) |
| `--session <id>` | the live session those tools act on; without it the only live one, and a refusal listing them when there are several; needs a session tool, otherwise exit 2 |
| `--allow-tool <pattern>` | pre-approve a tool for runs (repeatable) |
| `--http <ip:port>` | Streamable HTTP on a loopback address instead of stdio; any other address is exit 2 |
| `--origin <origin>` | accept this `Origin` as well as the loopback ones (needs `--http`) |
| `--max-runs`, `--max-concurrent-runs`, `--max-cost-usd` | parse, and exit 2 with "not available yet" |

The tools, resources and prompts are in `docs/MCP.md`. Logging goes to stderr, and each call leaves a redacted line under `<state>/logs/mcp/`.

A terminal session (`aibus` with no subcommand) opens a control socket for the session tools unless `--no-control` is given: `$XDG_RUNTIME_DIR/aibus/sessions/<session>.sock`, or `$TMPDIR/aibus-<uid>/sessions/` without `XDG_RUNTIME_DIR`, with a token file and an info file next to it, all removed on exit. One JSON object per line each way, like the headless control channel in section 2. What a client sends is stepped as if typed and tagged `via: "mcp"` (05 section 2.7, 07 section 2.2). docs/MCP.md, Live sessions, has the checks and the limits.

## 9. Where workflows come from, and `.aibus` bundles

aibus reads two directories, in this order: the project's `.aibus/workflows/` (the project is `--cwd`, or the directory the session starts in; aibus does not walk up from it) and the user's `workflows/` in the config directory (`~/.config/aibus/workflows/`). In each it reads the loose `*.toml` files and the `*.aibus` bundles. A name in the project hides the same name in the user's directory; inside one directory a loose file hides a bundled workflow of the same name, and an earlier bundle (by file name) a later one. The list is read again every time anything lists or runs, so a file copied in mid-session shows up on the next `/workflow list` without a restart.

`workflow list` says where each one came from in its last column (`--json`: `scope`, `bundle`, `source`): `project`, `user`, or `project bundle kit.aibus`. A workflow written as a project workflow is only seen by sessions started in that same project directory; one that every session should see belongs in the user scope (`workflow install --user`, or `scope: "user"` through MCP).

A bundle is a plain, uncompressed tar with the extension `.aibus` (always `<name>.aibus`, never a bare `.aibus`, which is the project directory's name):

```
manifest.toml            format = 1, name, description, workflows = [...], agents = [...]
workflows/<name>.toml    one per workflow the manifest lists, byte for byte as on disk
agents/<name>.md         optional: agent definitions the workflows name
README or README.md      optional
```

`aibus workflow pack` writes one. By hand, list the files by name and keep to ustar: GNU `tar --format=ustar -cf kit.aibus manifest.toml workflows` (GNU's default format works too), or on macOS `COPYFILE_DISABLE=1 tar --format=ustar -cf kit.aibus manifest.toml workflows`, so no `._` or extended-attribute entries go in. A pax archive (`tar --format=posix`, or any tar that adds pax headers) is refused, and so is `tar -cf kit.aibus .`, whose entries start with `./`.

Loose `.toml` files stay the editable source: `/workflow save` and the MCP write tools write loose files, `pack` builds a bundle from them, `install` unpacks one. A bundle is read in memory and never unpacked to disk just to be listed. It is refused whole, with nothing of it listed or run, when any of these holds: it is over 1 MiB or has more than 64 entries; an entry is over 64 KiB, a symlink, a hard link, a device, a fifo, a pax or GNU extension header, an absolute path, a path with `..`, a duplicate, or anything outside the layout above; a name does not match `^[a-z0-9][a-z0-9_-]{0,63}$`; a header is cut short, has a bad checksum or a size that is not plain octal; a file is not UTF-8, holds a control character (raw, or through a TOML or YAML escape, so no escape sequence can reach a terminal) or text the redactor would change; the manifest does not list exactly what is there; a workflow does not parse, has more than 32 steps or a `name` that is not its file name; an agent file does not parse or names another agent. A refused bundle shows in `workflow list` as one row named after its file, with the reason. At most 64 bundles are read in one directory; past that each further bundle is a row saying it was not read.

Every file is opened without blocking and has to be a regular file of a sane size (a bundle 1 MiB, a loose workflow 1 MiB), so a fifo, a link to a device or a huge file in a workflows directory is a row with the reason, never a hang. What `workflow list`, `show` and `validate` print, and what the dashboard draws, has escape sequences and control characters taken out, since a loose file can hold them too.

A bundled workflow with a `gate` is listed but does not run (`workflow run`, `run --workflow`, `/workflow`, the `workflow` tool and MCP `workflow_run` all refuse it, and `workflow validate` warns): a gate is a shell command from someone else that runs as you. You trust it by installing the bundle with `--allow gates`, which writes the workflow as a loose file of yours.

- `workflow pack <name>... -o <file>.aibus` puts the named workflows (whichever source wins each name) into a bundle named after the file. `--with-agents` adds the project or user definition of each agent the steps name (built-ins need none). It never replaces a file without `--force`, and the bundle it writes is read back through every check first. The output is deterministic: the same files give the same bytes.
- `workflow install <file>.aibus` writes the workflows (and agents) into the project's `.aibus/` or, with `--user`, the user's config directory, through the checks of `workflow_create`: the name rule, 64 KiB, 32 steps, `workflow validate` against the agents you have plus the bundle's own, the redactor, and the gate rule. `--allow gates` is also what an agent needs that has hooks, a permission mode, an `account`, a non-empty `fallback`, `memory: user` or an `initial_prompt` (`memory: project` and `local` stay in the project and are not gated); its `tools` are not gated, since every tool call still goes through the permission rules. An agent with a built-in's name (`lead`, `explore`, ...) replaces that built-in for every session, so it needs `--force` and install says which built-in it replaces. Every check runs before anything is written. A file that is already there is only replaced with `--force`. The workflows and agents directories, and the `.aibus` (or config) directory above them, have to be real directories: a symlink at any of them is refused (the user's config directory itself may be a link, which install resolves first). Install stages every file under a random name made with `O_CREAT|O_EXCL` (which never follows a symlink), mode 0600, then renames them into place, and a failure removes the files and directories it made, but a file replaced with `--force` before a later rename failed cannot be put back. The files are byte for byte as in the bundle, so pack then install gives identical files.
- `workflow validate <file>.aibus` checks a bundle; `workflow validate` with no argument checks every loose file and bundle in both directories.

## 10. Compiling a workflow (#283)

`aibus workflow plan <target> --dry-run` compiles a workflow (a name, found as `workflow run` finds it, or a path) against the agents discovery finds and prints the plan: every step with its key, agent, runtime, resolved mode, prompt hash and the invocation it would get, the typed edges, fan-out groups, the envelope, the approval boundary and the routing decisions, under one `plan_hash`. Nothing runs, no gate included. `--dry-run` is required: running a plan is the scheduler's (#288), and plan without it is exit 2. A workflow that does not validate, has a step that `requires` what its agent's tools don't grant, has more than 256 steps or holds something credential-shaped gets no plan and exit 12. The same input gives the same bytes on any host (`fixtures/workflows/plans/` is checked on each one).

`--locked <plan.json>` holds the plan to one saved earlier with `plan --dry-run --json`; any difference (a prompt edited, an agent definition changed, a route added) is printed as paths like `steps.tests.prompt.sha256` and is exit 13. A saved plan whose `plan_hash` does not match its content was edited by hand, which is said on stderr.

`aibus workflow diff <a> <b>` compares two workflows or saved plans (`.json`). Steps are matched by key and edges by their ends, so moving a step in the file is not a change. It always exits 0.

`aibus workflow manifest <target> [--input <text>|-]` prints the `run-manifest.json` a run of the plan starts from (spec section 4.3): the plan hash and every prompt and agent hash, the task input as a hash and a length (`-` reads it from stdin), the aibus version, OS and architecture, the runtimes, configured models, the routing decisions, the repository's commit and whether it is dirty (git runs with the repository's hooks and filesystem monitor off), and environment variable names, never values. What only a started run can know is `{"unknown": "<why>"}`; what does not apply is `{"not_applicable": "<why>"}`. Writing one per attempt lands with the attempt store (#282).

`aibus workflow export <target>` shows the Claude Code agent files (`.claude/agents/<key>.md`, or `--dir`) the workflow's native subagents would become, as a diff against what is there, and writes nothing. `--apply <hash>` writes them only when `<hash>` is the hash of the diff that preview printed and the files are still what it showed; a symlink or anything not a plain file is refused. A plan needs none of this: the invocation passes native subagents inline with `--agents`, which writes no file.

The format is in `docs/spec/01-agents-and-continuity.md` section 8.4 and `docs/schemas/workflow/v1.json`.

