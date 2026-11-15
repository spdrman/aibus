# Evidence events: what Claude Code 2.1.311 exposes headless

Every claim here is tied to a fixture under `fixtures/claude-code/spike-281/`. `L12` means line 12 of that run's `.stdout.jsonl`; `H7` means line 7 of its `.hooks.jsonl` (the hook file has a blank line after each payload because the hook command appended one, so payloads sit on odd lines). Levels follow spec 7.1: **observed** is a native explicit event, **correlated** links independent IDs or channels, **inferred** is my heuristic, **unavailable** is a gap.

I used two channels:

- **stream**: stdout of `claude -p --output-format stream-json --verbose`, plus `--include-hook-events` and `--forward-subagent-text` where noted.
- **hooks**: command hooks for `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `SubagentStart`, `SubagentStop`, `Stop` and `SessionEnd`, each appending its stdin JSON to a file. They came from a project `.claude/settings.json` in the scratch cwd, loaded with `--setting-sources project`. I didn't subscribe to `TeammateIdle`, `TaskCreated`, `TaskCompleted`, `PostToolUseFailure` or `Notification`.

## The acceptance fixture: `11-foreground`

One lead, two native children (`reader-a`, `reader-b`, defined inline with `--agents`), two `Read` tool calls, one final `result`, exit 0, 3.9 s wall time. List-price estimate $0.00104.

| Edge | Source evidence | Level |
|---|---|---|
| lead -> child A (spawn) | L6 lead `assistant` `tool_use` `name=Agent`, `id=toolu_01Mogri4...`, `input.subagent_type=reader-a`, `input.run_in_background=false` | observed |
| | L9 `system/task_started` `task_id=a51dc098ebaef1926`, `tool_use_id=toolu_01Mogri4...`, `task_type=local_agent`, `spawn_depth=1`, `is_backgrounded=false` | observed |
| | H7 `SubagentStart` `agent_id=a51dc098ebaef1926`, `agent_type=reader-a` (same opaque id as `task_id`, from a different channel) | correlated |
| child A messages | L11, L23, L27, L43, L44 carry `parent_tool_use_id=toolu_01Mogri4...` and `agent_id=a51dc098ebaef1926` | observed |
| child A -> tool | L23 `tool_use name=Read id=toolu_01JeiGno...` inside child A; L27 its `tool_result` (`ALPHA-7731`) | observed |
| | H13/H15 `PreToolUse`/`PostToolUse` with `agent_id=a51dc098...` and `tool_use_id=toolu_01JeiGno...` | correlated |
| child A done | L47 `task_updated` `status=completed` with `end_time`; L48 `task_notification` `status=completed`, `summary`, `usage`; L51 lead's `tool_result` for `toolu_01Mogri4...` with `tool_use_result.agentId=a51dc098...`, `usage`, `totalDurationMs`, `toolsUsed` | observed |
| | H25 `SubagentStop` `agent_id=a51dc098...`, `last_assistant_message`; H27 `PostToolUse` for the `Agent` call whose `tool_response.agentId` is `a51dc098...` | correlated |
| lead -> child B | Same chain: L13, L17, L19, L30, L34, L35, L38, L39, L42; hooks H9, H11, H17, H19, H21, H23 | observed + correlated |
| final result | L52 lead text, L55 `result` `subtype=success`, `num_turns=3`, `subagent_stats.spawned=2`, `completed=2`, `by_type={reader-a:1, reader-b:1}` | observed |

So each child edge has at least two independent sources (stream IDs, and the hook channel's `agent_id`), and the hook channel can rebuild the tree alone at completion time through `PostToolUse(Agent).tool_response.agentId`. The Agent SDK reference says to match a child's messages to task events on `agent_id` rather than `parent_tool_use_id`, because a resumed subagent keeps its original `parent_tool_use_id` while new task events carry the resuming call's id. Every child message here carries `agent_id`, so #282 should key on that.

`07-deleg` is the same delegation with nothing said about background, and it's just as well evidenced; it's the multi-`result` case below.

## Evidence table

| What | Where it comes from | Level |
|---|---|---|
| CLI identity and version | `--version` (`2.1.311 (Claude Code)`), `system/init.claude_code_version`, codesign authority `Developer ID Application: Anthropic PBC (Q6L2SF6YDW)` | observed |
| Auth state without credentials | `auth status --json`: `loggedIn`, `authMethod=claude.ai`, `apiProvider=firstParty`, `subscriptionType`; plus `email`, `orgId`, `orgName` (redacted in `05`) | observed |
| Session identity | `session_id` on every stream event and every hook payload, identical in both | observed |
| Lead tool calls | `assistant` `tool_use` blocks with `id`, `name`, `input`; `user` `tool_result` blocks | observed |
| Native child spawn | `system/task_started` (`task_id`, `tool_use_id`, `subagent_type`, `task_type=local_agent`, `spawn_depth`, `is_backgrounded`, `prompt`), `background_tasks_changed` | observed |
| Child messages and child tool calls | `assistant`/`user` with `parent_tool_use_id`, `agent_id`, `subagent_type`, `task_description`, `timestamp` | observed |
| Child progress and completion | `task_progress` (usage so far, `last_tool_name`), `task_updated` (`status`, `end_time` in epoch ms), `task_notification` (`status`, `summary`, `usage`, `output_file`) | observed |
| Per-child usage | `tool_use_result.usage`, `totalTokens`, `totalDurationMs`, `toolStats` on the lead's `Agent` result | observed (vendor-reported) |
| Run totals | `result`: `num_turns`, `duration_ms`, `usage`, `modelUsage`, `subagent_stats`, `permission_denials`, `terminal_reason` | observed |
| Cost | `total_cost_usd` with `modelUsage.*.costBasis="list"`; the docs call it a "client-side cost estimate, which can differ from your bill" | observed value, but it's an **estimate**, not billing |
| Rate limits | `rate_limit_event` (`rateLimitType=seven_day`, `utilization`, `resetsAt`, overage state) | observed |
| Hook lifecycle in stream | `system/hook_started` and `hook_response` with `hook_name`, `hook_event`, `exit_code`, `outcome`. No `agent_id` or `tool_use_id` in these | observed (names only) |
| Hook payloads | `agent_id`/`agent_type` on everything that fires inside a child; `tool_use_id`, `tool_input`, `tool_response`, `duration_ms` on tool hooks; `background_tasks` on `Stop`/`SubagentStop`; `agent_transcript_path` on `SubagentStop` | observed (hook channel) |
| Stream task id = hook agent id | Same opaque string in two channels | correlated |
| Denied tool | `system/permission_denied` (`tool_name`, `tool_use_id`, `decision_reason_type`, `decision_reason`), an error `tool_result`, `result.permission_denials[]`. `PreToolUse` fired and no `PostToolUse` did (`09`) | observed |
| Lead and children share one OS process | `ps` sampled during `08` showed a single `claude` pid whose parent is the harness (`08-control.ps.txt`, deduplicated); the full series in `15-killed-midrun.ps.tsv` shows one `claude` pid in every sample (about every 300 ms) while both children were working | observed by the host, at roughly 300 ms grain |
| Killing the process mid-delegation | `15`: killed at 2.2 s while reader-a had done one of three reads and reader-b none. The stream stops at L27 with no `task_notification`, no `result` and no `SessionEnd`, and the `ps` series shows no `claude` process in any sample from the first one after the kill until 2 s later | observed |
| The children ended with it | They had no process of their own (row above), so nothing was left to keep running. No event says so, because the process that would have reported it is gone | inferred |
| Killed after the children finished | `10`: killed at 3 s, after both `task_notification`s (L53, L58); the stream stops after a fresh `system/init` with no final `result` and no `SessionEnd` | observed |
| Background completion re-enters the lead as a new prompt | `07` H29/H35 `UserPromptSubmit` whose prompt starts `<task-notification>`; a fresh `system/init` and an extra `result` per wake-up | observed |
| Prompt asked for foreground, so children ran in foreground | `11` `run_in_background=false` in the tool input and `is_backgrounded=false`. That the prompt caused it is my reading | inferred |
| Lead's answer about why no team formed (`14` L57) | Model-written text; it agrees with the docs but it isn't telemetry | inferred |
| Timestamps on system, hook, task and result events | None, apart from `task_updated.end_time` and `rate_limit_event.resetsAt` | **unavailable**; aibus stamps `observed_at` |
| Hidden reasoning | `thinking` blocks arrive with an empty `thinking` string and an opaque `signature` (`11` L43) | **unavailable** |
| Parent id in `SubagentStart` payload | Only `agent_id`/`agent_type`; the parent `tool_use_id` has to be correlated through the stream or `PostToolUse(Agent)` | **unavailable** natively |
| Billed cost for subscription users | Only the list-price estimate | **unavailable** |
| Per-child cancel or stop from the host | No documented host control in text-input `-p`. The model has a `TaskStop` tool; `--input-format stream-json` control messages weren't tested | **unavailable** (untested path noted) |
| Agent-team teammates under `-p` | Not spawned (docs, plus `14`) | **unavailable** |
| Transcripts | Paths reported by hooks; not read (policy), and I don't know whether `--no-session-persistence` kept them off disk | **unavailable** to this spike |
| `task_progress.summary` | Only with the SDK's `agentProgressSummaries` option; I found no CLI flag | **unavailable** |

## Stream behaviour #282 has to handle

- **Several `result` events per process.** `07` has three (`num_turns` 3, 1, 1), `08` has three (including an empty `result` with `num_turns=0`), `10` has two before the kill. Each has `result_index`, but the attempt's outcome is the last `result` before a clean exit. A consumer that stops at the first `result` would record the lead saying "results haven't come back yet" as the answer.
- **Several `system/init` events per process**, one per wake-up.
- **Unknown or undocumented variants** turn up in normal runs: `rate_limit_event`, `system/thinking_tokens`, `system/status`, `background_tasks_changed`, `task_updated`. Spec 5.1 already says to keep unknown events; this confirms it happens on every run.
- **Hook events in the stream interleave with the work they describe**, and `hook_started`/`hook_response` pairs overlap (`07` L11 to L14).
- **`SessionStart` hook events show up even without `--include-hook-events`** (`08` L1, L2), which matches the CLI reference.
- **Child text without `--forward-subagent-text`.** In `08` (background children, flag off) the children's tool calls and final text still appeared. The CLI reference says the flag matters for foreground children's text and thinking, so foreground runs need it if aibus wants the child's narrative.
- **Truncation.** `10` ends after a `system/init` line with no final `result`, and `15` ends in the middle of a child's work with no `result` at all. Both have to become `interrupted` with `requires_reconciliation`, never `completed`.
- **The `init` tool list uses `Task`, tool calls use `Agent`.** Normalise both to one name.
- **`stderr` was empty in every run** (the `.stderr.txt` files are committed, all zero bytes). Errors and denials arrive as stream events.

## Privacy observations

- No token, cookie or key appeared in any output. The stream has no email, org id or org name; `auth status --json` does, so aibus must redact those three fields before storing the status.
- With `--permission-prompts none` nothing ever prompted, so no auth or permission terminal was involved.
- Hook and stream payloads carry file contents (`ALPHA-7731` came back through `tool_response.file.content`), full prompts and absolute paths. Those are payloads, so they're opt-in under spec 10.3.
- `system/init` lists the user's installed skills, slash commands and built-in plugins (including a `cc-plugin-telemetry@builtin` plugin) and `analytics_disabled: false`. The CLI has its own telemetry to Anthropic; aibus can't see or change that from the stream, and I didn't test the documented opt-outs.
- The CLI writes its own state as it runs: `memory_paths.auto` and transcript paths point under the user's config dir, and background task output files land under `/private/tmp/claude-<uid>/<cwd-slug>/<session>/tasks/`. aibus should treat those as vendor-owned and never read them as a matter of course.
- Under `-p`, project hooks and `.mcp.json` run even in a folder nobody trusted (the CLI help and headless docs both say so). Running Claude Code inside an untrusted repo means running that repo's hooks. #282 should pass `--setting-sources` explicitly, and #290 should treat a worktree's `.claude/` as untrusted input.
