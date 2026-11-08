# 00. Overview

## What aibus is

`aibus` is a terminal app (Rust, ratatui, crossterm) that runs a lead AI agent, lets it delegate to sub-agents, keeps an advisor watching the whole session, and carries state across sessions through ledgers, handoffs and memory. It is one binary, one crate name, `aibus`.

It recreates three things and welds them into one program:

1. **The sub-agent model from Claude Code.** Agent definitions are Markdown files with YAML frontmatter. They live at project or user scope, carry a tool allow-list and a model choice, get delegated to automatically or invoked by name, run in the foreground or the background, can be resumed, and run in an isolated context whose only output back to the parent is a result message. Hooks fire around tool calls and around the agent's lifecycle.

2. **The continuity model from Continuous-Claude-v3.** "Continuous" there means: a ledger tracks state inside a session, a YAML handoff carries it to the next one, a memory store accumulates learnings, hooks save state before compaction and load it at session start, and meta-workflows chain agents (sleuth, then premortem, then kraken, then arbiter...). The mantra is "compound, don't compact". aibus keeps the shape and drops the Python, Docker and PostgreSQL.

3. **The dashboard UI from the screen recording.** A lead session box, a fork layer that makes cheap routing decisions, three swarm worker cards with progress bars, an advisor rail that wakes on checkpoints ("before a plan", "same error x2", "before done"), a session log and a status bar. Section 04 grounds every widget in the frames.

## Sources I read for this spec

- `https://code.claude.com/docs/en/sub-agents`, the whole page in three passes (frontmatter table, built-in agents, delegation, foreground/background, resume, context isolation, forks, memory scopes, hooks, examples, transcripts, compaction).
- `https://code.claude.com/docs/en/hooks`, the event table, stdin JSON shape, exit code semantics, hook types and timeouts.
- `parcadei/Continuous-Claude-v3`, default branch `main`: README (1288 lines), `.claude/hooks/README.md`, `.claude/hooks/src/pre-compact-continuity.ts`, `session-start-continuity.ts`, `.claude/skills/create_handoff/SKILL.md`, `continuity_ledger/SKILL.md`, `agent-context-isolation/SKILL.md`, `.claude/agents/scout.md`, `maestro.md`, `maestro.json`, `kraken.json`, `.claude/rules/proactive-delegation.md`, `agent-model-selection.md`, `.claude/settings.json`, `skills/skill-rules.json`, `docs/MULTI-SESSION-ARCHITECTURE.md`, and the full tree (1176 paths: 32 agents, 108 skills, 30 hooks).
- Frames `f01.png` to `f37.png` of the recording (720x900 each, one per second, terminal reported as `98x46`).

What I could not verify is listed in section 11 and in the PR body.

## Vocabulary

I use these words consistently across the spec.

| Word | Meaning |
|---|---|
| **agent definition** | A Markdown file with YAML frontmatter that names an agent, says when to use it, and restricts its tools and model. |
| **role** | One of `lead`, `worker`, `advisor`, `router`. The lead talks to the user and delegates. Workers are the sub-agents. The advisor reads everything and only speaks at checkpoints. The router is the fork layer: a cheap model that answers small routing questions. |
| **run** | One execution of an agent definition with one prompt: its own conversation, its own tool loop, its own result. A sub-agent invocation is a run whose parent is another run. |
| **session** | The top-level unit: a lead run plus every run it spawned, plus the session log, ledger and handoffs. One session per `aibus` launch unless resumed. |
| **checkpoint** | A named moment where the advisor is consulted: `before_plan`, `same_error_x2`, `before_done`, plus user-defined ones. |
| **fork** | A router decision. The recording calls these "forks" and counts them; aibus keeps the word. A fork is `sharp` (confidence at or above threshold, acted on in code) or `split` (escalated to the lead model). |
| **ledger** | `thoughts/ledgers/CONTINUITY_<topic>.md`, the within-session state file. |
| **handoff** | `thoughts/shared/handoffs/<session>/<timestamp>_<slug>.yaml`, the between-session state file. |
| **memory** | Per-agent persistent directory (`user`, `project` or `local` scope) plus a searchable index. |
| **account** | One login to one provider: a name, a provider id, a credential reference, optional base URL and model aliases. Two Anthropic accounts are two accounts. |
| **provider** | An implementation of the `Provider` trait: Anthropic, OpenAI, Kimi (Moonshot), the fake, and whatever comes later. |

## Design stance

- **The engine does not know about the terminal.** `aibus-core` compiles without ratatui. Every UI is a consumer of `Msg`s committed into the graph and a producer of `Command`s. This is what makes `aibus run --json` and the TUI the same program.
- **All state is one Causl graph.** One event, one atomic commit. Widgets are pure functions of derived nodes. The commit log is the audit trail, the snapshot is the save file, and a recorded `Msg` stream replays deterministically. Section 03 has the contract, what I verified about the Rust engine, and the two-backend decision.
- **The model decides delegation, the runtime enforces it.** Tool allow-lists are enforced by the executor, not by prompt text. The model never sees a tool it cannot call.
- **Nothing secret touches disk in plaintext under aibus's control.** Credentials live in the OS keychain. Logs and transcripts go through a redaction layer.
- **Compound, don't compact.** Before a context is compacted, a handoff is written. At session start, the latest handoff is loaded. Learnings are extracted into memory. This is the Continuous-Claude loop, implemented natively instead of with hooks and a daemon.
- **Everything is bounded.** Channels, result sizes, transcript windows, log files, token budgets. A session that runs for twelve hours must not grow without limit.

## Reading order

00 overview, 01 agents and continuity (the behaviour), 02 architecture (the shape of the code), 03 state (the Causl graph), 04 providers and accounts, 05 UI, 06 CLI, 07 persistence and config, 08 performance, 09 security, 10 testing, 11 delivery plan, 12 open questions. `docs/DECISIONS.md` records why the contested choices went the way they did.
