# Normal subagents vs native agent teams (Claude Code 2.1.311)

Short version: under `claude -p`, which is the only mode aibus can drive headlessly, native agent teams don't exist. A request for a team quietly turns into ordinary subagents, so everything aibus can observe natively is the subagent model in [evidence.md](evidence.md).

## What the docs say (fetched 2026-11-14)

From https://code.claude.com/docs/en/agent-teams:

> "Agent teams are experimental and disabled by default. Enable them by setting `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1` in your settings.json or environment."

> "Spawning teammates also requires an interactive session. In non-interactive mode with the `-p` flag, including Agent SDK sessions, Claude doesn't spawn teammates, and a subagent that Claude names runs as an ordinary subagent even with agent teams enabled."

The same page describes a team as a lead plus teammates sharing a task list and per-agent mailboxes, stored as files under the user's config dir (`teams/{team-name}/config.json`, `teams/{team-name}/inboxes/{agent-name}.json`, `tasks/{team-name}/`). Teammates run in-process or in tmux/iTerm2 split panes. Its listed limits: no session resumption with in-process teammates, task status can lag, shutdown can be slow, one team per session, no nested teams, no background subagents from in-process teammates, a fixed lead, no per-teammate permission mode at spawn. It also says the `--teammate-mode` flag "is experimental and doesn't appear in `claude --help`".

From https://code.claude.com/docs/en/sub-agents: subagents can nest "up to three layers below the main conversation", and by default 20 concurrent subagents is the cap. "With nothing configured, a subagent that Claude spawns from your main conversation starts in the background", and under `claude -p` "Claude can wait, on its own judgment or when you ask."

## What I saw

| Run | Setup | Result |
|---|---|---|
| `12-teams-probe` | `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`, default tools, trivial prompt | `system/init` tool list has `Task`, `SendMessage`, `ListAgents`, `TaskStop` and so on, with no team-creation tool |
| `13-teams-control` | Same without the env var | Same tool list. Setting the variable changed nothing visible in `-p` |
| `14-teams-attempt` | Env var set, prompt asks for a team with teammates `alpha` and `beta` | The lead called `Agent` twice with `name: "alpha"` / `"beta"`, `subagent_type: general-purpose`; both ran as `task_type: local_agent`, `spawn_depth: 1`, foreground. No team files, no teammate hooks seen (I hadn't subscribed to `TeammateIdle`/`TaskCreated`, so that part is untested rather than absent) |

So the docs and the runs agree: under `-p` you get subagents, whatever you ask for.

## Side by side

| | Normal subagents (`Agent` tool) | Native agent teams |
|---|---|---|
| Available headless (`-p`) | Yes, observed | No: documented, and observed in `14` |
| Enabled by | Always; custom ones via `--agents` JSON or agent files | `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1` plus an interactive session |
| Process model | In the lead's process (one pid in `08`) | In-process or a separate pane per teammate (docs; not observed) |
| Parent link in events | `task_started.tool_use_id`, child `parent_tool_use_id` and `agent_id` | Not observable headless. Docs mention `teammate_name`/`team_name` on task hooks, marked deprecated |
| Lifecycle events | `task_started`, `task_progress`, `task_updated`, `task_notification`; hooks `SubagentStart`/`SubagentStop` | Hooks `TeammateIdle`, `TaskCreated`, `TaskCompleted`; `SubagentStart` fires per teammate message for in-process teammates (docs) |
| Communication | Prompt in, final result out (one hand-back) | Shared task list plus mailboxes as files in the user's config dir |
| Host control | Whole-process only: aibus can kill the `claude` process, which ends every child | Interactive terminal only |
| Resume | Not for aibus: `--no-session-persistence` in every run, and the spec forbids assuming vendor resume anyway | Docs: no resumption with in-process teammates |

## Unavailable event and control paths

What aibus can't get from Claude Code 2.1.311 under `-p`, so the graph and inspector (#284) have to show these as gaps:

1. **Any agent-team topology.** No teammates, mailboxes or shared task list in headless mode. If an owner runs a team interactively, aibus sees none of it unless it reads the vendor's files, and it doesn't do that.
2. **Per-child cancel, pause, retry or steer from the host.** Native children live inside the lead's process. The only host lever is the whole process (spec 5.1's graceful-then-kill applies to the lot). `--input-format stream-json` and its control messages weren't tested in this spike, so whether they add anything is open for #282.
3. **Forcing foreground or background.** `run_in_background` is chosen by the model. A prompt can ask; the host can't require it.
4. **Hidden reasoning.** `thinking` blocks arrive empty with an opaque signature.
5. **Native timestamps** on lifecycle events (only `task_updated.end_time`).
6. **Teammate or task hooks under `-p`.** They fire only when a team exists, which it never does headless.
7. **Transcripts and task output files** belong to the vendor (under the user's config dir and `/private/tmp/claude-<uid>/`). Reading them isn't a documented integration path, and this spike didn't.
8. **Billed cost** for subscription users; only the client-side list-price estimate exists.

Spec 2.3's rule holds: aibus can observe native subagents well, but it controls only the process it launched. Anything that needs per-agent control or cross-provider children has to be aibus-managed (separate official CLI invocations with aibus run ids), and it must be labelled that way.
