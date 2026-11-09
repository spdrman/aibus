# 07. Persistence and config

## 1. Locations

XDG on every platform. On macOS aibus still uses the XDG layout under `$HOME` (`~/.config/aibus`, not `~/Library/Application Support`), because the files are meant to be found, edited and version-controlled by hand. The `directories` crate is not used for that reason; `etcetera` with the XDG strategy is.

| Purpose | Path (default) | Env override |
|---|---|---|
| config, user agents, user workflows | `~/.config/aibus/` | `XDG_CONFIG_HOME` |
| memory, learnings, index | `~/.local/share/aibus/` | `XDG_DATA_HOME` |
| sessions, snapshots, transcripts, logs | `~/.local/state/aibus/` | `XDG_STATE_HOME` |
| caches (model tables, tokenizer, worktrees) | `~/.cache/aibus/` | `XDG_CACHE_HOME` |
| project scope | `<project>/.aibus/` | |
| continuity files | `<project>/thoughts/` | `continuity.dir` in config |

Full tree:

```
~/.config/aibus/
├── config.toml
├── agents/*.md
├── workflows/*.toml
└── keys.toml                  # key map overrides only

~/.local/share/aibus/
├── agent-memory/<agent>/MEMORY.md, *.md
├── agent-memory/lead/LEARNINGS.md
└── index.sqlite               # sessions, runs, memory FTS5 (section 2.3)

~/.local/state/aibus/
├── sessions/<session-ulid>/
│   ├── session.json           # id, started, topic, cwd, lead agent, outcome
│   ├── graph.snapshot.json.zst
│   ├── commits.jsonl.zst      # rotated per 50k commits: commits.0001.jsonl.zst
│   ├── msgs.jsonl.zst         # redacted Msg recording for replay
│   └── runs/<run-ulid>/
│       ├── meta.json
│       ├── transcript.jsonl   # append-only, redacted (section 2.2)
│       ├── result.md          # when the summary was capped
│       └── artifacts/         # copies of files reported via report_artifact (opt-in)
└── logs/
    ├── aibus.log              # tracing, daily rotation, 7 files kept
    └── hooks/<session>/<event>-<ts>.log

~/.cache/aibus/
├── models/<provider>.json
└── worktrees/<session>/<run>/  # isolation: worktree checkouts

<project>/.aibus/
├── agents/*.md
├── workflows/*.toml
├── triggers.toml
├── rules/*.md
├── agent-memory/<agent>/
├── agent-memory-local/<agent>/   # gitignored
└── settings.toml                 # project overrides of config.toml (hooks, limits, defaults)

<project>/thoughts/
├── ledgers/CONTINUITY_<topic>.md
└── shared/handoffs/<topic>/<ts>_<slug>.yaml
```

`aibus init` creates `.aibus/` with a sample agent, `settings.toml`, and adds `.aibus/agent-memory-local/` to `.gitignore`.

## 2. Formats

### 2.1 Snapshot and commit log

`graph.snapshot.json.zst`: the `aibus-graph` `Snapshot` (03 section 4) as JSON, zstd level 3. Written when `model:persist:due` is true (every 64 commits or 30 s, whichever first) and on shutdown. Written atomically: temp file in the same directory, fsync, rename.

`commits.jsonl.zst`: one `Commit` per line, appended through a zstd frame writer that flushes every 100 lines or 2 s. A session's full history is the last snapshot plus the commits after its `time`.

`session.json` carries `schema_version`; a snapshot from an older version is migrated at hydrate (03 section 9).

### 2.2 Transcripts

`runs/<id>/transcript.jsonl`, one JSON object per line, append-only, redacted on write:

```json
{"t":"2026-11-07T23:48:05.012Z","type":"message","role":"user","blocks":[{"type":"text","text":"…"}]}
{"t":"…","type":"message","role":"assistant","blocks":[{"type":"tool_use","id":"toolu_…","name":"read","input":{"path":"README.md"}}]}
{"t":"…","type":"tool_result","id":"toolu_…","is_error":false,"bytes":1842,"content":"…"}
{"t":"…","type":"usage","input_tokens":1200,"output_tokens":80,"cache_read":900}
{"t":"…","type":"compact_boundary","trigger":"auto","pre_tokens":167189}
{"t":"…","type":"hook","event":"pre_tool_use","command":"./scripts/guard.sh","exit":0,"ms":12}
{"t":"…","type":"end","status":"completed","turns":7}
```

Tool result content over `limits.transcript_result_max_bytes` (64 KiB) is stored as the first 64 KiB plus `"truncated": true, "full": "runs/<id>/results/<call>.txt"`.

`aibus session export <id> --format md` renders the transcripts into one Markdown file per run.

### 2.3 Index

`index.sqlite` (rusqlite, bundled SQLite, WAL mode):

```sql
CREATE TABLE sessions (id TEXT PRIMARY KEY, started_at TEXT, ended_at TEXT, cwd TEXT, topic TEXT,
                       lead_agent TEXT, outcome TEXT, cost_usd REAL, runs INTEGER);
CREATE TABLE runs     (id TEXT PRIMARY KEY, session_id TEXT, parent_id TEXT, agent TEXT, role TEXT,
                       model TEXT, account TEXT, status TEXT, started_at TEXT, ended_at TEXT,
                       turns INTEGER, input_tokens INTEGER, output_tokens INTEGER, cost_usd REAL, summary_head TEXT);
CREATE VIRTUAL TABLE memory_fts USING fts5(path, scope, kind, content, tokenize = 'porter unicode61');
CREATE TABLE memory_files (path TEXT PRIMARY KEY, scope TEXT, kind TEXT, mtime INTEGER, hash TEXT);
```

`memory_fts` indexes `agent-memory/**/*.md`, `LEARNINGS.md`, `thoughts/ledgers/*.md`, `thoughts/shared/handoffs/**/*.yaml` for the current project and user scopes. `aibus memory index` rebuilds; the `session_start` and `session_end` steps re-index changed files by mtime and hash. `recall` is `bm25` ranked with a snippet.

The index is a cache. Deleting it loses nothing; `aibus memory index` and `aibus session list --rebuild` recreate it from the files.

### 2.4 Agent definitions, workflows, handoffs, ledgers

Markdown with YAML frontmatter (01 section 1), TOML (01 section 8.4), YAML (01 section 8.2), Markdown (01 section 8.1). All plain text, all meant to be committed.

## 3. Config file

TOML. User config at `~/.config/aibus/config.toml`, project overrides at `.aibus/settings.toml`, merged with project winning per key (tables merge, arrays replace, except `[[account]]` which is user-only and `[hooks]` which concatenates). `aibus config show` prints the merged result; `aibus config validate` exits 3 with the path and line of the first error.

```toml
[defaults]
account = "anthropic-work"
model = "sonnet"
advisor_model = "opus"
router_model = "fast"
swarm_model = "haiku"
permission_mode = "default"
effort = "high"

[models]
sonnet = "claude-sonnet-4-5"
opus = "anthropic-work/claude-opus-4-1"
haiku = "claude-haiku-4-5"
fast = "kimi/kimi-k2-turbo"

[[account]]            # see 04 section 6
name = "anthropic-work"
provider = "anthropic"
auth = { keyring = true }

[agents]
extra_dirs = [".claude/agents"]
trusted_dirs = []
reload_on_change = true

[limits]
max_turns = 50
max_tokens_total = 2_000_000
max_cost_usd = 20.0
max_spawn_depth = 3
max_concurrent_runs = 8
max_runs_per_session = 500
result_max_bytes = 32768
transcript_result_max_bytes = 65536
auto_compact_pct = 80

[router]
enabled = true
threshold = 0.85
max_per_turn = 10
label = "ROUTER"

[advisor]
enabled = true
checkpoints = ["before_plan", "same_error_x2", "before_done"]
packet_max_tokens = 400_000

[continuity]
dir = "thoughts"
auto_handoff_on_compact = true
handoff_on_end = true
extract_learnings = true
ask_outcome = true

[memory]
scopes = ["user", "project"]
recall_k = 5

[tools]
web = false
bash_timeout_secs = 120
bash_max_output_bytes = 262144
read_max_lines = 2000

[hooks]
session_start = [{ command = ".aibus/hooks/load-context.sh", timeout = 10 }]
pre_tool_use = [{ matcher = "bash", command = ".aibus/hooks/guard.sh" }]

[persist]
snapshot_every_commits = 64
snapshot_every_secs = 30
commit_history_cap = 4096
session_retention_days = 30
log_files_kept = 7

[ui]
theme = "recording"
ascii = false
mouse = true
hints = true
log_rows = 4
tick_hz = 30

[keys]                  # overrides; names are the actions in 05 section 4
"stop_run" = "x"
"open_palette" = ":"
```

Unknown keys are warnings with the line number. Every field has a default; an empty file is valid. Secrets are structurally impossible: the `auth` table has no `key` field, and a `key = "..."` entry is a validation error with the message `credentials do not go in config; run: aibus account add <name> --key-stdin`.

## 4. Retention and rotation

| Thing | Policy |
|---|---|
| `aibus.log` | `tracing-appender` daily rotation, `log_files_kept` (7) files, each capped at 64 MiB by a size guard that rotates early |
| hook logs | deleted with their session |
| sessions | `aibus session prune` removes sessions older than `session_retention_days` (30) unless `session.json` has `"pinned": true`; also runs at startup if the last prune was over a day ago |
| commit logs | rotated per 50k commits inside the session dir; pruned with the session |
| worktrees | removed at run end if clean; otherwise kept and listed by `aibus doctor` |
| model cache | refreshed on `account test` and when older than 7 days |
| memory | never pruned automatically |

## 5. Concurrency between aibus processes

Two `aibus` processes in the same project at once is normal (two terminals). Each has its own session directory. Shared files use advisory locks: `index.sqlite` via SQLite's own locking (busy timeout 5 s), ledgers and handoffs via `fs4` file locks held for the duration of one write. Both see the latest handoff the way Continuous-Claude's status line does: the newest file by name wins, and both status bars update on the next `session_start`-style reload, which a file watcher on `thoughts/shared/handoffs/` triggers.
