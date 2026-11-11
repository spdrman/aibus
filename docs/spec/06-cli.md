# 06. CLI

`aibus` is one binary. With no subcommand on a TTY it opens the dashboard. Everything the TUI can do has a subcommand or flag, so scripts and CI get the same engine.

## 1. Surface

```
aibus [OPTIONS] [PROMPT]                 open the TUI (optionally with a first prompt)
aibus run [OPTIONS] <PROMPT>             headless session, NDJSON events on stdout
aibus agent  list | show <name> | new <name> | validate [path] | import <dir> | where
aibus account add <name> --provider <id> [--key-stdin|--env VAR|--file PATH] [--base-url URL] [--label TEXT]
             list | show <name> | test <name> | remove <name> | enable|disable <name>
aibus session list | show <id> | resume <id> [--run <run-id>] [MESSAGE] | export <id> [--format md|jsonl] | log <id> | replay <id> | prune [--older-than 30d]
aibus handoff create [--topic T] [--outcome O] | list | show [PATH]
aibus ledger show | new <topic> | update <section> <text>
aibus memory recall <query> [-k N] | add <text> [--type T] [--scope S] | index
aibus workflow list | show <name> | validate [<name>|<path>] | run <name> <input>
aibus config path | show | init | validate | edit
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

## 2. `aibus run`

The headless mode. Stdout is NDJSON, one event per line, stderr is human-readable status. The prompt comes from the argument or stdin (`-` or no TTY on stdin).

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
| 12 | `agent validate` or `workflow validate` found errors | |

`session list --json` and friends exit 0 even when the list is empty.

## 4. `--json` outputs

| Command | Shape |
|---|---|
| `agent list` | `[{name, scope, path, role, model, tools, description}]` |
| `agent show` | the full definition as JSON (frontmatter fields plus `prompt`) |
| `agent validate` | `{ok, errors:[{path, field, message}], warnings:[..]}` |
| `workflow show` | `{name, path, description, steps:[{agent, prompt, gate?, on_fail?}]}` |
| `workflow validate` | `{ok, errors:[{path, step, field, message}], warnings:[..]}` |
| `account list` | `[{name, provider, label, auth:"keyring|env|file|none", default_model, status}]` |
| `account test` | `{name, ok, label, org, models:[..], latency_ms}` or `{name, ok:false, error:{kind, message}}` |
| `session list` | `[{id, started_at, ended_at, topic, runs, outcome, cost_usd}]` |
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
