# Capability note: Claude Code as an external runtime (#285)

Evidence date 2026-11-15. Every number carries its data quality (invariant 9): **reported** (the vendor said so), **derived** (aibus measured or counted it), **estimated** (a list-price or client-side estimate), **unknown**. Evidence levels on events are the spec's: observed, correlated, inferred, unavailable. The machine-readable version of most of this is `fixtures/claude-code/reference-285/scorecard.json` (version 1.0.0).

## Demonstrated

| Capability | Evidence | Quality of the evidence |
|---|---|---|
| One unmodified `claude -p --output-format stream-json --verbose` as a supervised process (absolute path, argv vector, bounded env, prompt on stdin) | Case 01 and the live run | Recorded real (2.1.311) and one live run on the same version |
| Lead plus two native subagents, each with a tool, a final result | Case 01 | 12 of 12 delegation edges in the ten recordings are observed (native `tool_use.id`, `task_started.tool_use_id`, `parent_tool_use_id`, `agent_id`); 12 of 12 are also cross-checked by hook events where hooks were recorded |
| A failed tool does not fail the attempt | Case 02 | Recorded real with one changed field (`is_error`) |
| Wall-time interruption, user cancel (SIGTERM first, kill after the grace period, whole process group) | Cases 03, 04 | Stand-in CLI behind a recorded prefix |
| Crash and restart: torn tail repaired, new attempt, old attempt kept, open tool flagged `requires_reconciliation` | Case 05 | Stand-in plus the real store |
| Overlapping children and a concurrent stderr stream | Case 06 | Recorded 07-deleg; **limited**: see #324 for the case where output is dropped |
| Malformed, truncated, invalid-UTF-8 and unknown events become labelled gaps and warnings, graph unchanged | Case 07 | Recorded 06-basic with damage inserted |
| Missing usage stays `unknown` in the store, the projection and the fleet row; nothing is zero-filled | Case 08 | Recorded with usage removed |
| Denied permission is an observed `tool_failed`; aibus never invents an approval; the argv never carries a bypass flag | Case 09 | Recorded real (09-denied) plus the compiled plan |
| Offline replay of a stored trace: no provider, no process, empty `PATH`, byte for byte repeatable, `--events` equals the store; rerun refused | Case 10 | Recorded real, stored by the real store |
| Credentials: never read, copied or stored | `aibus-supervisor/tests/credentials.rs` (#282) | Test |

## Unsupported or not shown

| Item | Status |
|---|---|
| Deterministic re-execution | Not claimed. A rerun is a new attempt and may differ; `runs rerun` is refused |
| Vendor-state or session resume | Not claimed. Native children run inside the lead process (one `claude` process in the spike's `ps` samples) and die with it; they cannot be signalled or resumed one at a time |
| Interactive approval through aibus | Untested. Only `--permission-prompts none` (auto-deny) is used and recorded |
| Native agent teams under `-p` | Not available: a "team" request ran as two ordinary subagents (#281, runs 12 to 14) |
| Hook delivery in a live run | Not shown. The recordings have hooks (written by a project settings file in the spike); the live run saw the stream only. Inline `--settings` is untried |
| Foreground or background subagents as a host control | Not available. `run_in_background` is a model field; asking in the prompt worked once |
| Per-child cost and model-level cost | Per-child usage comes from `task_notification` (reported); model-level cost is not exposed. `total_cost_usd` is a list-price **estimate**, billed cost **unknown** |
| Timestamps on system and hook events | None from the CLI; `observed_at` is aibus's clock, `occurred_at` stays empty rather than copied |
| Transcript reading (`transcript_path`) | Not attempted (it lives under the user's config dir) |
| A full workflow run through the scheduler | Not possible until #288: `repo-review` plans (`runnable_now: false`) |
| Other CLIs | codex 0.167.0 and kimi 2.5.2 present, gemini not on PATH; no extension until #287 |
| OpenRouter | Not a CLI, not in scope before #287 |

## Event-fidelity coverage

From the ten spike recordings fed through the normaliser (with hooks where recorded), 12 of the spec's 21 event kinds are produced (counts here are derived: aibus counted them from the stored traces): `attempt_started`, `attempt_completed`, `attempt_failed`, `agent_started`, `agent_stopped`, `task_delegated`, `message_chunk`, `tool_started`, `tool_completed`, `tool_failed`, `usage_reported`, `gap_detected`. The cases add `trace_warning` (stderr, unknown event types, store repairs) for 13.

Not produced, and why:

| Kind | Reason |
|---|---|
| `workflow_started`, `step_scheduled`, `route_decided` | Scheduler (#288) |
| `approval_requested`, `approval_resolved` | Approval gates (#288), plus the untested host approval mode |
| `checkpoint_written` | Durable state (#289) |
| `artifact_created` | Worktrees and artifacts (#290) |
| `auth_required` | The recordings were all signed in. It is covered by `host.rs` tests (a login prompt on stderr, an auth-error result), not by the corpus |

Events by origin across the recordings (derived counts): `cli_stream` 149, `hook` 76, `aibus_scheduler` 30, `derived` 2. Child ends that the CLI does not report (children die with the process) are `inferred` and say so; a killed stream is a `gap_detected` at the `unavailable` level. Stream types counted but given no event of their own: `hook_started`, `hook_response`, `task_progress`, `task_updated`, `rate_limit_event`.

## CLI version constraints

| Constraint | Value | Source |
|---|---|---|
| Tested | 2.1.311 only (recordings, live run) | #281, this PR |
| Minimum accepted by the probe | 2.1.0 (refuses the stale 1.0.34 npm build the spike found) | `cli::MIN_VERSION` |
| Other versions | Run, with a `trace_warning` | `cli::TESTED_VERSION` |
| `--forward-subagent-text` | needs 2.1.211 | #281, vendor docs |
| `--agents` file form | needs 2.1.281 (inline JSON used) | #281 |
| `--max-turns` | documented, missing from `--help` on 2.1.311 | #281 |
| `--bare` becoming the default for `-p` | would silently break subscription sign-in; the probe refuses it | #281, `ProbeError::BareDefault` |
| Required flags | `--print`, `--output-format`, `--verbose`, `--permission-prompts`, `--setting-sources` | `cli::probe` |

One version tested means nothing is known about drift. A scheduled live run against new releases is Phase 3's regression work (#295).

## Per-run latency

Not a benchmark. Host: the owner's MacBook (macOS, arm64). One sample per run, so **no median and no p95**.

| Run | Wall, derived (ms) | CLI-reported duration (ms) | Ended |
|---|---|---|---|
| 06-basic (one prompt) | 2244 | 679 | completed |
| 07-deleg (background subagents) | 5459 | 702 | completed |
| 08-control | 3685 | 540 | completed |
| 09-denied | 2445 | 1649 | completed |
| 10-killed (killed on purpose) | 3015 | 11 | interrupted |
| 11-foreground (the native-delegation reference) | 3876 | 2908 | completed |
| 12-teams-probe | 1435 | 596 | completed |
| 13-teams-control | 1428 | 624 | completed |
| 14-teams-attempt | 6326 | 5515 | completed |
| 15-killed-midrun | 2206 | none (no `result` before the kill) | interrupted |

Wall is the spike harness clock from spawn to exit or kill (2026-11-14, #281). The reported duration is `result.duration_ms` of the last result line (the scorecard's `cli_reported_duration_ms`), which for runs with several results (07, 08) is the last turn only, so it understates the run. One live run in this PR (2026-11-15, Claude Code 2.1.311, the same two-reader delegation as 11-foreground) is committed as `fixtures/claude-code/reference-285/live-report-2026-11-15.json` (host name removed): 10,716 ms wall (derived), 29 events, CLI-reported `duration_ms` 10,055 (reported), cost 0.0019 USD (estimated, list price). It passed the native-delegation checks with two observed edges and no hook cross-check (stream only). I ran the live mode two more times while building it and saw other wall times (about 9.7 to 10.2 s); **those reports were not kept, so treat them as unverified**. The spike's 11-foreground took 3.9 s, and I did not look into the difference. n=1 committed sample, so no claim either way.

Supervisor overhead, the cost of a replay and anything else that would be a timing benchmark: **unknown, not measured.** Per the benchmark rule that would go through `bench-queue.sh` on Mercury, and the issue asks for no required timing gate.

## Resource use

| Measure | Value | Quality |
|---|---|---|
| Tokens (input, output, cache read and creation), turns | Per attempt in the scorecard (`resource_use.reported_by_the_cli`) | reported, cumulative per `result` |
| Cost in USD | Per attempt, e.g. 11-foreground about 0.00104 | estimated (`costBasis: list`), never a bill |
| Billed cost | none | unknown |
| `claude` processes per attempt | 1 (lead and both children in one process) | derived, from the spike's `ps` samples |
| CPU and memory of the CLI | not sampled by the host | unknown |
| Trace size | events per case in the scorecard (for example 27 events for 11-foreground) | derived |

Cost over the whole spike was about 0.015 USD of estimated list price (#281); the committed live run was 0.0019 on the same basis.
