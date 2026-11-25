# 15. Recorded runs: graph, timeline, inspector, replay and the fleet (EPIC A, #284)

This is what #284 built for spec A1.4 (`epic_a_aibus_workflow_engineering_v3.md`, sections 2.2, 7.3, 8.1 and 8.3) on top of the trace of #282 (`14-evidence-trace.md`). It covers where recorded runs live, the views of a run, the commands and the `/runs` screen that show them, and what replay is and is not. The decisions behind it are D-284 in `docs/DECISIONS.md`; the threats and their tests are in `docs/SECURITY.md`.

## Where runs live

One run is one directory, `<state>/traces/<run>/` (`~/.local/state/aibus/traces/` by default), holding the store's `events.jsonl`, its `objects/` when payloads were kept, and a `plan.json` when the run was started from a compiled workflow (the plan of #283, written by whoever launches the run). Every command takes `--root <dir>` to read another directory instead. A run id is the directory's name: letters, digits, `.`, `_` and `-`, never a path. A unique prefix is enough.

Nothing in aibus writes there yet: the supervisor (#282) takes its store directory from its caller, and the scheduler that launches runs is #288. The reference pilot (#285) is the first writer.

## The views

All of them live in `aibus_trace::view`, read files and nothing else, and are the same strings in the CLI and the TUI. Every piece of text that came from a trace goes through `view::clean` before it is returned (see "Terminal safety").

- **Header.** The run, its workflow run id, the provider extension, how many events were read, the first and last `observed_at` (aibus's clock), and the data quality: corrupt lines, torn bytes and lines of another schema that the read skipped. A legend says what the four evidence levels mean.
- **Plan** (the definition graph). From `plan.json`: the plan hash, each step with its agent, mode, runtime and prompt hash, and each edge with its kind and condition. Next to each step, `scheduled #n` when a `step_scheduled` event names it, otherwise `not in the trace`. Without a plan it says `not recorded`. The plan is intent, not proof of execution (spec 2.2), and the heading says so.
- **Execution** (the execution graph). Per attempt: its status (or `no end recorded`), `needs reconciliation` when a tool never reported back, its usage with each number's quality and source, and its lineage: `aibus-managed child of attempt <id> (linked by the scheduler)` when the events carry a parent attempt, otherwise `provider-native`. Under it the lead and, indented, every child it delegated to; under each child the delegation edge with its level, whether it is cross-checked and by which channels, the tool use it came through, and the sequence, level and origin of every source event; under each agent its tools with their outcome (`completed`, `failed (permission_denied)`, `outcome unknown (never reported)`). Each node says how many of its events are at each level. An agent with no message says `messages none recorded (unavailable)`; an attempt with no usage says `usage: not reported (unknown)`; a billed cost the CLI does not report is `unknown`, never a number.
- **What cannot be placed.** A child nothing links to a parent is an `unavailable` edge with a `parent_unavailable` gap; a parent that is not in the trace is named as such; a parent link that points at itself says so; agents whose parent links form a cycle are listed under their own heading, each once. Indentation stops at depth 16 and deeper nodes say their depth, so a chain thousands deep stays readable and is drawn without recursion.
- **Gaps.** Every `gap_detected` and every `parent_unavailable`, with its position and reason.
- **Timeline.** Every event in `event_id` order: position (its rank, for display), `observed_at`, the vendor's `occurred_at` or `-`, kind, level, origin, agent, tool use, a short note from its metadata, and `corroborates` for a second report. It filters by agent, tool use, kind and level (spec 8.1).
- **Inspector.** For an agent: attempt, role, status, parent and the edge with its sources, who it delegated to, elapsed time (derived from aibus's clock), the model and CLI version the vendor reported, the prompt (`not in the trace`: a plan keeps prompt hashes, text only under payload retention), its messages, its usage with sources, its tools, permission events (denials, approvals, logins), its gaps, and every event behind it with its native type, parser, raw line hash and payload reference. For a tool use: the same, with its input and output payloads. For an attempt id: status, reconciliation, usage and its lifecycle events. Payloads are shown only when the trace kept them (`PayloadPolicy::Retain`), cut at 4 KiB; otherwise the inspector says `text not retained`.
- **Event.** One event in full, as JSON, plus its payload when kept: the "open the supporting raw events" of A1.4's acceptance criteria.
- **Fleet.** One line per run: state (`completed`, `failed`, `cancelled`, `interrupted`, `blocked` when an approval or login is pending and no end was recorded, `open` when no end was recorded, `empty`), a `!` when a tool never reported back, events, agents, tools, edges by level (observed/correlated/inferred/unavailable), gaps, the estimated cost with its quality (or `unknown`), the last time seen, and the data quality. `RunRow` is the typed row behind it, and `--json` prints those rows.

## Commands

```text
aibus runs list [--root DIR]                       the fleet; --json for the rows
aibus runs inspect <run> [--root DIR]              header, plan, execution, gaps, timeline
                   [--agent-id ID] [--tool-use ID] [--kind KIND] [--level LEVEL]   timeline filters
                   [--node ID]                     the inspector of one agent, tool use or attempt
                   [--event SEQUENCE]              one event in full
aibus runs replay <run> [--offline] [--events] [--root DIR]
aibus runs compare <a> <b> [--root DIR]            two runs side by side (#294)
                   [--result-a FILE] [--result-b FILE]   an EvaluationResult JSON for a run: its tests and revision
aibus runs repro <run> [--root DIR]                what a replay and what a rerun would need (#294)
aibus runs rerun <run> [--isolated] [--root DIR]   refused for now (exit 2)
```

A missing run, an ambiguous prefix, a run id that is not a plain name, an unknown node, event, kind or level is a usage error (exit 2). The `--agent-id` and `--tool-use` filters are not called `--agent`, because `--agent` is a global flag.

## Replay is the recording, never a rerun

`aibus runs replay` (spec 7.3, spelled the way #280 decided) reads the trace and prints every recorded state transition in `event_id` order (`attempt started`, `agent a1d3... started, child of 66c1... via toolu_...`, `tool ... completed`, `#29 confirms #3; no change`, ...), then the graph they add up to, which is byte for byte the `inspect` graph. It makes no model call, starts no process and opens no connection: it reproduces the recorded topology and transitions, not anything the model kept to itself. `--offline` is accepted for the spec's spelling; replay is always offline. `--events` prints the stored event lines exactly as they are in `events.jsonl` (byte for byte into a pipe or a file); on a terminal, a character the terminal would act on is written as its JSON `\uXXXX` escape, which reads as the same JSON.

`aibus runs rerun` would start a new attempt of the CLI, and that needs the scheduler (#288, #289). Until then it is refused, and the refusal says that a rerun's outcome may differ from the recording and points at `replay`. It never claims a rerun would come out the same.

## Comparing two runs, the typed inspector and reproducibility (#294)

`aibus runs compare <a> <b>` (`aibus_trace::compare`) reads two run directories and answers the five reviewer questions from one text: **what ran** (state, provider, counts, edges by level, gaps), **routing** (the `route_decided` events as recorded), **outputs** (message chunks and bytes, retained payloads, artifacts), **quality** (the checks of an `EvaluationResult` handed in with `--result-a` and `--result-b`: passed, failed and not run, where a check nobody ran never counts as a pass) and **git and worktree**. After those come latency, tokens and cost with `b - a`, the tool counts by name and outcome, and the environment.

Everything the trace does not carry is a line that says `gap:` and why. A cost nobody reported stays `unknown`, and a change is computed only when both numbers are known and in the same unit. A trace carries no git revision and no diff: the revision comes only from an evaluation result, and the worktree diff is always a gap.

The environment rows are the provider extension, the parsers, the CLI version, the model, the permission mode, the workflow plan (its `plan_hash`, or a hash of `plan.json`), the repository revision and the extension version. Two runs are `comparable` only when every row is known and equal; any row that differs makes the text say `not comparable as like for like` and name the rows, and a row one side does not know makes it say `comparability unknown`. Plain recordings never reach `comparable`, because none of them has a `plan.json` (the workflow plan is a gap), and the revision comes only from an evaluation result. Values are compared as the trace has them, never as they are cut or escaped for display, and a field with several values (a run that spans two providers) compares as the whole set.

An evaluation result counts only for the run it belongs to: its `result_version` must be the one this build reads and its `workflow_run_ids` must name the run. `aibus runs compare` refuses a result that does not (usage error, exit 2); the library shows the quality of such a run as a gap and takes nothing from it. Results of different suites, suite versions or cases make the runs `not comparable`. A trace read with skipped lines, a torn tail or lines of another schema says so per side, and its runs are never `comparable`.

Usage is summed over the attempts of a run only when every attempt reports the number once, in one unit. When some attempts report nothing, the total is `unknown` and says how many of the attempts did report; a sum that is not finite is unknown too, and message sizes that add up past 2^64 are a gap.

`aibus_trace::causal::inspector` is the inspector of one agent, tool use or attempt as typed fields (`aibus runs inspect <run> --node <id> --json`): prompt and output as `Retained` text when the run kept the payload (the prompt is the payload of the delegating tool use), otherwise a gap that says why; native ids; the delegation link with its evidence level; every source event with its parser, raw hash and payload reference; the trace gaps; elapsed time (derived); reported usage; linked artifacts; and the worktree diff (a gap). Without `--json` the text inspector is unchanged.

`aibus runs repro <run>` keeps recorded replay and re-execution apart. The replay section says it is offline and lists what makes it incomplete (skipped lines, a torn tail, payloads not kept). The rerun section is labelled `rerun`, says it is not a replay and that its outcome may differ, says it is not available yet (#288, #289), and lists what a fresh run would need with each input as a value from the trace or a gap: CLI version, model, permission mode, plan, prompts, repository revision and the environment.

## The `/runs` screen

`/runs` in the dashboard opens the fleet, `/runs inspect <run>` the run's graph and timeline, `/runs inspect <run> <node>` the inspector, `/runs replay <run>` the replay, `/runs compare <a> <b>` the comparison and `/runs repro <run>` the reproducibility report (the screen has no way to hand in evaluation results, so its comparison shows quality as a gap). The terminal loop reads the trace store (`Command::Runs`, answered with `Msg::RunsLoaded`), and the screen shows the same text the commands print, up to 5,000 lines, with Up/Down (`k`/`j`), PgUp/PgDn (space), `g`/`G` and the wheel to scroll and Esc to close. The evidence levels are words in the text, so the screen adds no colour of its own and no new contrast pair. An error (no such run, not a run id) goes to the session log and opens nothing.

## Terminal safety

A trace is untrusted text: a name, an id, a timestamp or a payload can hold an escape sequence. `view::clean` writes every control character (C0, DEL, C1), bidi override and isolate, zero-width character and byte order mark as visible `\u{..}` text, and cuts single values at 160 characters, so a 20 MB name is a short line. JSON from a trace (an event, a payload) is cleaned line by line after it is pretty-printed. Every text field of the typed inspector (usage names, units and sources included) is cleaned before it is serialised, and the `--json` output of `inspect --node`, `compare` and `repro` goes through the same escaping as `replay --events` on a terminal, so a bidi or C1 character is never written raw. A payload is opened only when its reference is exactly `local://objects/<64 hex>.json` inside the run's own directory.

## How it sits next to the rest

- **#282.** The views are a module of `aibus-trace` and use its `project`, `Projection`, `Edge` and `TraceEvent` as they are; the trace format is unchanged. `Recorded::load` reads a run the way `TraceStore::read` does and keeps each line's bytes, which is what makes the replay byte for byte.
- **#290.** The fleet's `RunRow` and `view::fleet` are the shared view model: #290 adds pause and stop to it, through the `CancellationToken` that `supervise` takes, next to the control socket of #274. Nothing here can stop or pause a run.
- **#283.** The plan section reads `plan.json` as JSON (`steps[].key`, `agent`, `mode`, `runtime`, `prompt.sha256`, `edges[].from`, `to`, `kind`, `when`), so it does not depend on the compiler's types.
- **#294.** Done in the reduced form above. The graphical client, redacted run bundles and the rest are in `docs/DEFERRED.md`.

## Not here

Comparison of two runs (#294), stop and pause (#290), a rerun (#288, #289), an editable plan view (the deferred designer, spec 15), live updates while a run is being written (the views read a run as it is when they are asked).
