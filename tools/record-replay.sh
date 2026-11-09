#!/usr/bin/env bash
# Records the fixtures under tests/replay/ with `aibus run --record` on the fake provider
# (10 section 5). Each one is a throwaway $HOME with a small project in it, a RON script the
# fake plays, and one command line. Nothing here touches a network or a real keychain.
#
#   tools/record-replay.sh                  record every scenario
#   tools/record-replay.sh retry-after-429  record one
#
# AIBUS=<path to the binary> (default target/debug/aibus), OUT=<fixture root> (default
# tests/replay). Run it in the build container, see docs/TESTING.md. Re-recording changes the
# run ids and so every hash: commit the whole directory of a fixture together.
set -euo pipefail

AIBUS=$(realpath "${AIBUS:-target/debug/aibus}")
OUT=$(realpath -m "${OUT:-tests/replay}")
ONLY=${1:-}
mkdir -p "$OUT"

# rec <name> <expected exit code> <script|-> [agent file contents|-] <aibus run args...>
# A "-" script plays the built-in one for the agent.
rec() {
  local name=$1 want=$2 script=$3 agent=$4
  shift 4
  if [ -n "$ONLY" ] && [ "$ONLY" != "$name" ]; then return 0; fi
  local home
  home=$(mktemp -d)
  mkdir -p "$home/proj/src"
  printf '# demo\n\nA tiny project for the replay fixtures.\n' >"$home/proj/README.md"
  printf 'fn main() {\n    println!("hello");\n}\n' >"$home/proj/src/main.rs"
  local args=(run --json --record "$OUT/$name")
  if [ "$script" != - ]; then
    printf '%s\n' "$script" >"$home/script.ron"
    args+=(--script "$home/script.ron")
  fi
  if [ "$agent" != - ]; then
    mkdir -p "$home/proj/.aibus/agents"
    printf '%s\n' "$agent" >"$home/proj/.aibus/agents/$(sed -n 's/^name: //p' <<<"$agent").md"
  fi
  local code=0
  (
    cd "$home/proj"
    env -u XDG_CONFIG_HOME -u XDG_DATA_HOME -u XDG_STATE_HOME -u XDG_CACHE_HOME -u AIBUS_CONFIG \
      HOME="$home" "$AIBUS" "${args[@]}" "$@" >"$home/out.ndjson" 2>"$home/err.txt"
  ) || code=$?
  if [ "$code" != "$want" ]; then
    echo "$name: exit $code, wanted $want" >&2
    cat "$home/err.txt" >&2
    FAILED=1
    return 0
  fi
  echo "recorded $name"
}

# ---- M1: one run, headless ----------------------------------------------------------------

# The built-in script of `aibus run --agent explore`.
rec explore-fake 0 - - --agent explore "map this repo"

rec read-then-answer 0 '[
    Text("Looking at the readme."),
    ToolCall(name: "read", input: {"path": "README.md"}),
    Usage(input: 120, output: 18),
    Stop(ToolUse),
    Text("It is a one-line readme for a demo project."),
    Usage(input: 210, output: 12),
]' - --agent explore "what does the readme say"

rec multi-tool-turns 0 '[
    Text("Three looks, two of them at once."),
    ToolCall(name: "ls", input: {"path": "."}),
    ToolCall(name: "glob", input: {"pattern": "**/*.rs"}),
    Usage(input: 150, output: 30),
    Stop(ToolUse),
    ToolCall(name: "grep", input: {"pattern": "fn main"}),
    Usage(input: 310, output: 14),
    Stop(ToolUse),
    ToolCall(name: "read", input: {"path": "src/main.rs"}),
    Usage(input: 380, output: 12),
    Stop(ToolUse),
    Text("main prints a greeting and nothing else."),
    Usage(input: 460, output: 11),
]' - --agent explore "find main"

rec unicode-stream 0 '[
    Text("Grüße, 世界! Ça va? Ünïcödé résumé, naïve café, 日本語のテキスト, and an emoji pair: 🚀✨. "),
    Text("The stream is cut into small chunks, never in the middle of a character."),
    Usage(input: 40, output: 60),
]' - --agent explore "say hello in many scripts"

rec long-stream 0 "[Text(\"$(for i in $(seq 1 600); do printf 'line %04d the quick brown fox jumps over the lazy dog\\n' "$i"; done)\"), Usage(input: 90, output: 4800)]" - --agent explore "print a long answer"

rec quiet-completion 0 '[Text("Nothing to do."), Usage(input: 20, output: 4)]' - --agent explore "anything to do?"

# ---- M1: errors and limits ----------------------------------------------------------------

rec retry-after-429 0 '[
    Error((kind: RateLimit, retryable: true, retry_after: Some((secs: 1, nanos: 0)), message: "slow down")),
    Text("recovered"),
    Usage(input: 30, output: 3),
]' - --agent explore "hello"

rec invalid-request-fails 5 '[Error((kind: InvalidRequest, retryable: false, retry_after: None, message: "bad schema"))]' - --agent explore "hello"

rec max-turns-partial 10 '[
    ToolCall(name: "ls", input: {"path": "."}), Stop(ToolUse),
    ToolCall(name: "ls", input: {"path": "src"}), Stop(ToolUse),
    ToolCall(name: "ls", input: {"path": "."}), Stop(ToolUse),
    Text("never reached"),
]' '---
name: quick
description: Two turns and out.
tools: ls
max_turns: 2
---
Look around, briefly.' --agent quick "look around"

rec budget-stop 8 '[
    Text("pricey"),
    Usage(input: 1000000, output: 0),
    ToolCall(name: "ls", input: {"path": "."}), Stop(ToolUse),
    Text("never reached"),
]' '---
name: pricey
description: Stops at fifty cents.
tools: ls
max_cost_usd: 0.5
---
Look around.' --agent pricey "look around"

rec permission-denied-no-tty 0 '[
    Text("I will write a note."),
    ToolCall(name: "write", input: {"path": "notes.txt", "content": "hello"}), Stop(ToolUse),
    Text("The write was refused, so I stopped."),
]' - "write a note"

rec hook-exit2-blocks 0 '[
    ToolCall(name: "ls", input: {"path": "."}), Stop(ToolUse),
    Text("The listing is off limits, so I read the readme instead."),
]' '---
name: guarded
description: Listing is blocked by a hook.
tools: ls, read
hooks:
  pre_tool_use:
    - matcher: ls
      command: "echo listing is off limits >&2; exit 2"
---
Look around.' --agent guarded "look around"

# ---- M2: sub-agents -----------------------------------------------------------------------

# The scripted fake is shared: the child takes the first turn, the lead the second.
rec delegate-mention 0 '[Text("the child report"), Stop(EndTurn), Text("all done")]' - "@agent-explore map the repo"

rec delegate-two-sequential 0 '[
    Text("Two searches, one after the other."),
    ToolCall(name: "delegate", input: {"agent": "explore", "prompt": "map src", "background": false}), Stop(ToolUse),
    Text("src has a main.rs"), Stop(EndTurn),
    ToolCall(name: "delegate", input: {"agent": "explore", "prompt": "map the docs", "background": false}), Stop(ToolUse),
    Text("the docs are one README"), Stop(EndTurn),
    Text("Both reports are in: a main.rs and a README."),
]' - "map src and the docs"

# ---- M4: advisor and router ---------------------------------------------------------------

rec advisor-before-plan 0 '[
    Text("# Plan\n1. Now I will list the files."), Stop(EndTurn),
    Text("plan skips auth"), Stop(EndTurn),
    ToolCall(name: "ls", input: {"path": "."}), Stop(ToolUse),
    Text("done"),
]' - --advisor fake-large "plan the work"

rec same-error-x2 0 '[
    ToolCall(name: "read", input: {"path": "missing.txt"}), Stop(ToolUse),
    ToolCall(name: "read", input: {"path": "missing.txt"}), Stop(ToolUse),
    Text("try ls before reading"), Stop(EndTurn),
    ToolCall(name: "ls", input: {"path": "."}), Stop(ToolUse),
    Text("found the file list"),
]' - --advisor fake-large "read missing.txt"

rec router-sharp 0 '[
    Text("Now I will list the files."), Stop(EndTurn),
    RouterAnswer(choice: "ls", p: 0.95), Stop(EndTurn),
    ToolCall(name: "ls", input: {"path": "."}), Stop(ToolUse),
    Text("done"),
]' - --router fake-small "look at the files"

rec router-split 0 '[
    Text("Now I will list the files."), Stop(EndTurn),
    RouterAnswer(choice: "ls", p: 0.52), Stop(EndTurn),
    ToolCall(name: "ls", input: {"path": "."}), Stop(ToolUse),
    Text("done"),
]' - --router fake-small "look at the files"

rec advisor-and-router 0 '[
    Text("# Plan\n1. Now I will list the files."), Stop(EndTurn),
    Text("plan skips auth"), Stop(EndTurn),
    RouterAnswer(choice: "ls", p: 0.95), Stop(EndTurn),
    ToolCall(name: "ls", input: {"path": "."}), Stop(ToolUse),
    Text("done"),
]' - --advisor fake-large --router fake-small "plan the work"

rec compaction-auto-handoff 0 '[
    Usage(input: 500, output: 5),
    ToolCall(name: "ls", input: {"path": "."}), Stop(ToolUse),
    MatchPrompt(contains: "Summarise the conversation below", then: [Text("SUMMARY: listed the directory"), Stop(EndTurn)]),
    Text("done after compacting"),
]' '---
name: compacting
description: Compacts early.
tools: ls
auto_compact_at: 100
---
Look around.' --agent compacting "a long task"

# ---- M5: continuity -----------------------------------------------------------------------

rec ledger-and-handoff 0 '[
    ToolCall(name: "ledger_update", input: {"section": "Goal", "text": "Move auth to middleware"}),
    ToolCall(name: "ledger_update", input: {"section": "In Progress", "text": "run the migration\nadd a test"}),
    Stop(ToolUse),
    Text("The ledger is up to date."), Stop(EndTurn),
    Text("[]"),
]' - --topic auth --outcome PARTIAL_PLUS "start the auth work"

rec memory-write 0 '[
    ToolCall(name: "memory_write", input: {"path": "LEARNINGS.md", "content": "Run the failing test alone first."}), Stop(ToolUse),
    Text("Saved the note."), Stop(EndTurn),
    Text("[]"),
]' - "remember how to debug"

[ -z "${FAILED:-}" ]
