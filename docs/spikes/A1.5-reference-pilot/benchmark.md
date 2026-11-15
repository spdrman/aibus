# Benchmark comparison: Conductor, LangGraph/Studio, Langfuse, Vibe Kanban (#285)

Built from the vendors' **public documentation only**, fetched 2026-11-15. I did not install, run or import any of them, and nothing here depends on their code. Each row says what the page states, what aibus has now (with the test or fixture that shows it), and the gap. A claim a page did not state is marked **unverified**: that means "I could not confirm it from the cited page", not "it is false". These are architectural patterns to learn from, not licensed code to copy; licences are noted where the page showed one, and need a real check before any reuse.

## Sources

| # | Page | Fetched |
|---|---|---|
| S1 | Conductor README, https://github.com/microsoft/conductor | 2026-11-15 |
| S2 | Conductor execution reference, https://github.com/microsoft/conductor/blob/main/plugins/conductor/skills/conductor/references/execution.md | 2026-11-15 |
| S3 | LangGraph persistence, https://docs.langchain.com/oss/python/langgraph/persistence | 2026-11-15 |
| S4 | LangSmith Studio, https://docs.langchain.com/langsmith/studio | 2026-11-15 |
| S5 | Langfuse observability best practices, https://langfuse.com/docs/observability/best-practices | 2026-11-15 |
| S6 | Langfuse prompt to trace linkage, https://langfuse.com/docs/prompt-management/features/link-to-traces | 2026-11-15 |
| S7 | Langfuse experiments via OpenTelemetry, https://langfuse.com/docs/evaluation/experiments/experiments-via-opentelemetry | 2026-11-15 |
| S8 | Vibe Kanban supported agents, https://github.com/BloopAI/vibe-kanban/blob/main/docs/supported-coding-agents.mdx | 2026-11-15 |
| S9 | Vibe Kanban monitoring task execution, https://github.com/BloopAI/vibe-kanban/blob/main/docs/core-features/monitoring-task-execution.mdx | 2026-11-15 |
| S10 | Vibe Kanban README, https://github.com/BloopAI/vibe-kanban | 2026-11-15 |

The pages were read through a summarising fetch, not as raw text, so quotes are short and anything load-bearing should be checked against the page.

Legend for the aibus column: **have** (tested, evidence named), **partial**, **not yet** (planned issue named).

## Conductor (validation and graph), MIT per S1

| Pattern | What the docs state | aibus | Gap |
|---|---|---|---|
| Source-controlled declarative workflows | YAML, "diff workflows in pull requests" (S1) | have: TOML workflows (#283), `fixtures/workflows/` | Syntax differs on purpose; not a dependency |
| Static validation before running | `conductor validate` checks YAML syntax, agent references, route targets and reachability "without executing" (S1, S2) | have: invalid workflows rejected before any CLI spawns, `workflow_cli.rs`, `workflow_plan.rs` | None of note |
| Dry run | `--dry-run` shows graph, order and configuration (S2) | have: `workflow plan --dry-run`, stable plan hash, case 09 reads it | aibus also shows runtime status and Jev candidates |
| Deterministic conditional routing | "First matching condition wins", no LLM in the orchestration loop (S1) | partial: the Jev decision schema and fixtures (#283) | Scheduler is #288 |
| Live DAG | Web dashboard with interactive DAG (S1) | partial: planned vs actual graph in the terminal, offline (#284) | No live feed or browser view; spec does not ask for one in Phase 1 |
| Approvals | Gates in the dashboard or TUI, `conductor gate respond`, `--skip-gates` auto-selects the first option (S1, S2) | partial: plan lists held steps (case 09); no gate runs | #288. aibus must never ship an equivalent of `--skip-gates` that approves for the user |
| Checkpoint and resume after failure | "automatically saves a checkpoint", `conductor resume` continues "from the exact agent that failed" (S2). S1 does not mention resume | not yet: restart reconstructs nothing, retries are new attempts | #289. Note Conductor resumes its own agent steps; aibus can only resume its own steps, never a vendor session |
| Replay with a timeline scrubber | `conductor replay <log_file>` replays a recorded run in the dashboard (S2) | have: `aibus runs replay`, offline, case 10 | No scrubber; text replay |
| Fleet overview | Fleet Manager TUI (S1) | have: `aibus runs list` (#284) | Read-only until #290 |
| OpenTelemetry export | OTLP to Jaeger or Tempo, Copilot span correlation (S1) | not yet | #292 |
| Limits | `max_iterations` (500 max), `timeout_seconds`, exit codes 3 and 4 (S2) | have: `max_agent_runs`, `max_parallel`, `max_wall_seconds` in the plan envelope; host wall-time limit (case 03) | Defaults of Conductor's limits are unverified (not stated) |
| Runtimes | Copilot, OpenAI, Claude stable; Claude Agent SDK, Hermes, ACA experimental (S1) | Claude Code CLI only; Conductor drives SDKs, aibus drives official CLIs | Different model, by design |

## LangGraph and LangSmith Studio (checkpoints and debug)

| Pattern | What the docs state | aibus | Gap |
|---|---|---|---|
| Checkpoints as durable state | Checkpointers "persist a thread's graph state as checkpoints"; in-memory ones are lost on restart, Postgres or SQLite recommended (S3) | partial: an append-only on-disk event store with repair (case 05); nothing is a checkpoint of scheduler state yet | #289; amendment C2 (Causl check) |
| Threads | `thread_id` identifies a run's state (S3) | have, in effect: `workflow_run_id` plus `attempt_id`, new attempt per retry | None |
| Time travel | Listed as a checkpointer use case (S3) and a Studio feature (S4). **How it works is unverified** from these pages | partial: replay of the recording only (case 10); no fork-from-a-point, which would be a new attempt | Re-execution is a non-goal; any "time travel" in aibus is a view of recorded state |
| Interrupts and human in the loop | Listed as a use case (S3); mechanics **unverified** from S3, and "interrupts" not stated on S4 | partial: denial is observed, approval events not produced yet | #288 |
| Fault tolerance | "recover from a failure" listed (S3); mechanics **unverified** | partial: torn-tail repair, `requires_reconciliation` | #289 |
| Separate definition and run graph | Not stated on S3 or S4 (**unverified**); the spec cites it as a LangGraph trait | have: planned graph vs execution graph (#284) | None |
| Graph visualisation, "nodes traversed, intermediate states" | S4 | have: inspector shows agents, edges, tools, evidence | No state editing, rerun from a node: S4 does not say either way (**unverified**), and aibus will not do it for vendor state |
| Local and deployed use | Studio works against a local Agent Server or LangSmith Cloud (S4) | local only, by invariant | Cloud use is a non-goal |

Key difference to keep in the Phase 2 design: LangGraph owns the whole graph state, so restoring a checkpoint restores everything. aibus owns only the scheduler's state; a Claude session cannot be restored (capability note). Anything copied from LangGraph's vocabulary has to be labelled with that limit.

## Langfuse (trace fidelity and prompt linkage)

| Pattern | What the docs state | aibus | Gap |
|---|---|---|---|
| Trace, observation, session | Traces are units of work, observations nest under them by `trace_id`, sessions group traces by `session_id` (S5) | have: `workflow_run_id` (session-like), `attempt_id` (trace-like), agent and tool nodes | Mapping to OTLP spans is #292 |
| Nesting | Tool calls nest under the agent observation, not the trace root (S5) | have: tool nodes belong to their agent, delegation edges carry the parent tool_use id | None |
| Typed observations | `generation`, `tool`, `agent`, `span` (S5) | partial: event kinds differ (`agent_*`, `tool_*`, `message_chunk`) | Map at export time |
| Metadata, tags, scores | Free-form metadata, immutable tags set at creation, scores for after-the-fact judgements (S5) | partial: `data` per event, metrics with quality; no scores | #293 |
| Prompt to trace linkage | Passing the prompt to a generation links the version; metrics per version (median latency, tokens, cost, count) (S6). Needs a Langfuse SDK or an integration | partial: the plan and manifest hash prompts (#283); events are not yet stamped with a prompt revision | #283 manifest to #292 export. Not a dependency: aibus links by hash and does not call Langfuse |
| Experiments over OpenTelemetry | Experiment attributes on OTEL spans; **the attribute list is not on the page (unverified)** (S7) | not yet | #292, #293 |
| OTLP ingest, data masking, self-hosting | **Unverified** from S5 to S7 (not stated) | opt-in export is spec'd, redaction exists (`Redactor`) | #292 |
| Data quality labels | Not stated (**unverified**) | have: every metric carries `reported`, `derived`, `estimated` or `unknown`, and events carry observed, correlated, inferred or unavailable | This is an aibus differentiator, not a claim about Langfuse |

## Vibe Kanban (worktree and review UX), Apache-2.0 per S10, **sunsetting**

S10 says "Vibe Kanban is sunsetting" with no date on the page. Treat it as a UX reference only, never a dependency.

| Pattern | What the docs state | aibus | Gap |
|---|---|---|---|
| User-owned coding CLIs | Lists Claude Code, Codex, Copilot, Gemini, Amp, Cursor, OpenCode, Droid, Claude Code Router, Qwen; "requires installation and authentication before use" (S8). Who owns the login is **unverified** | have for Claude Code: user's own install and sign-in, never touched (credentials test) | More CLIs: #287, #291 |
| Task attempts | Each attempt triggers worktree creation, setup script, agent run, streaming logs (S9) | partial: attempts are first-class with their own trace (case 05); no worktree | #290 |
| Per-attempt git worktree | Isolated, "ephemeral and automatically cleaned up" (S9) | not yet. #281 found a worktree's `.claude/` is untrusted input under `-p` | #290; aibus must not delete uncommitted work on cleanup, which is stricter than what S9 says |
| Live monitoring | Real-time logs, a Processes view (S9) | partial: offline replay and fleet; no live view | #290 |
| Diff and review | Reviewing is on another page, not fetched (**unverified**); commit messages come from the agent's last message (S9) | not yet | #290; no automatic merge |
| Agent control | Approve or deny for Codex only, Claude Code "coming soon"; pause or stop: not stated (S9) | partial: cancel works (case 04), approval untested | A control path through the CLI's own permission prompt would be new ground (#281) |

## What this comparison does and does not show

- It is a **capability comparison**, as the issue allows. There is no timing comparison: I ran none of these tools, and aibus's own latency is n=1 (see the capability note).
- aibus is ahead of what these pages state on one thing only: evidence labels on every edge and metric. That is a claim about the pages I read, not about the products.
- aibus is behind on checkpoint and resume (Conductor, LangGraph), live views (Conductor, Studio, Vibe Kanban), export (Conductor, Langfuse) and worktree review (Vibe Kanban). All of those are Phase 2 or 3 issues already.
