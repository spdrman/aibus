# A1.5 Phase 1 gate: go/no-go for Phase 2 (#285)

> Ratified by the owner on 2026-11-15 as GO-WITH-CONDITIONS. Decisions: the supervisor drop (#324) is fixed first (#326); Claude Code subscription sign-in is allowed for local, opt-in use of the official CLI only (aibus never reads or stores the token, and commercial, bundled and hosted use stay blocked); the reference extension id is a reverse-DNS under the owner's domain, `org.devshell.aibus.reference-cli`, replacing the provisional `org.aibus.reference-cli`.

The text below is the recommendation as I wrote it before that ratification; where it says "owner decision" or "change needed", the decisions above settle it.

> I wrote this as a recommendation, and the owner has now ratified it as above. The amendments to the extension and checkpoint contracts below stay proposals for #286 and #289 to apply.

Written 2026-11-15 on `a1-5-reference-pilot-285`. Evidence: the reference suite (`crates/aibus/tests/reference_285.rs`), the committed scorecard (`fixtures/claude-code/reference-285/scorecard.json`, version 1.0.0), the spike record (#281, `docs/spikes/A1.1-claude-code/`), and the merged Phase 1 code (#282, #283, #284). Details live next door: [capability-note.md](capability-note.md) and [benchmark.md](benchmark.md).

## Recommendation: GO-WITH-CONDITIONS

The Claude spike did not fail, so the clause "failing the Claude spike blocks the remaining rollout" is not triggered. One unmodified `claude` process delegated to two native subagents, every edge has native ids behind it, and the pipeline (supervisor, store, projection, replay) held up on all ten cases. I'd start Phase 2, but not before the conditions below, and the policy gate stays partly closed.

### Phase 1 gate (spec section 13)

| Gate item | Result | Evidence |
|---|---|---|
| Claude CLI native-subagent spike and compliance decision | Met, with an open owner decision | #281; one live run in this PR saw it again on 2.1.311 (two observed edges, 29 events; report committed as `live-report-2026-11-15.json`) |
| Valid manifest | Met | #283: `workflow plan --dry-run` is byte-stable; case 09 reads it |
| Trustworthy event and graph replay | Met for stream evidence; hook delivery in a live run is not shown (see amendment E4) | cases 01 to 10, 12 of 12 edges observed and hook cross-checked on the recordings |
| Baseline benchmark | Met as a capability comparison, **not** as a timing benchmark | [benchmark.md](benchmark.md); latency is n=1 per run, no median or p95 |

### Policy gate

| Use | Recommendation |
|---|---|
| Open source, local, user's own unmodified `claude`, API key or Console sign-in | **Go.** This is the documented developer path |
| Same, claude.ai subscription sign-in | **Owner decision first** (unchanged from #281): ask Anthropic, accept the ambiguity, or tell users to use an API key. Until then aibus shows the CLI's own `authMethod` and says so plainly |
| Extension or package with "Claude Code" in its name | **Decided.** `org.aibus.claude-code` and `aibus-claude-code` go. The reference extension id is `org.devshell.aibus.reference-cli` |
| Bundled, preinstalled, hosted, commercial | **Blocked**, as #280 decided. Nothing in this PR clears it |
| Terms | Re-check before any release (#295). The 2026-11-14 gate in `terms-gate.md` is the last review; I did not re-fetch the terms for this issue |

### Conditions on the go

1. The owner answers the two #281 questions (subscription sign-in, extension name) before #286 fixes the extension id.
2. #324 (below) is fixed before #286 builds the extension host on the supervisor.
3. The contract amendments below are accepted or replaced, since A2.1 starts from them.
4. Phase 2 does not advertise anything the [capability note](capability-note.md) lists as unsupported.

### Defects the corpus found

| Issue | What | Blocks? |
|---|---|---|
| #324 | The supervisor drops output still queued two seconds after the CLI exits, with no gap event. Red test `defect_a_backlog_after_exit_is_not_dropped_silently` (ignored until fixed) | **Does not block the go.** Real Claude streams are small (73 lines at most in the spike) and the cases pass, but it is a silent loss in the evidence pipeline, so it blocks #286 (condition 2) |

No other defect turned up in the ten cases.

## Ratifying the Phase 2 contracts

### Extension contract (spec 6.1 to 6.3, #286, #287)

**Ratified as the direction, amended in six places.** Each amendment comes from something the suite or the code shows.

| # | Amendment | Why |
|---|---|---|
| E1 | The reference extension cannot be declarative-only. Claude's normaliser is stateful: it keeps tool-use to agent maps, dedupes across the stream and hook channels, and has a `finish` step at process exit. Spec 6.3's `normalize(event) -> Vec<TraceEvent>` has no place for that state, and "declarative manifests by default" cannot express it. Proposal: the normaliser is a per-attempt stateful object with `start`, `stream_line`, `hook_line`, `stderr_line`, `gap` and `finish` (what `aibus_supervisor::Normalizer` has now). The host owns the engine for the built-in Claude parser; third-party extensions get a WASI component with the same shape, and a declarative mode only for CLIs whose events are one-line-one-event | Cases 01, 05, 06 and 07 depend on that state |
| E2 | The prompt goes on stdin, never in argv. The spec's `argv = ["-p", "{prompt}", ...]` is wrong for the manifest. The supervisor already refuses argv prompts (`the_prompt_goes_to_stdin_and_never_to_argv`) | Argv shows in `ps` and in logs |
| E3 | The manifest declares the environment allowlist (`CLAUDE_CONFIG_DIR` for Claude) and the host strips `CLAUDECODE` and `CLAUDE_CODE_*`. Credential-shaped names are refused | #281 condition 5, `cli::bounded_env` |
| E4 | `capability_probes` must be able to say "needs hook delivery", and the host needs a defined way to receive hook payloads. In the live run the supervisor saw the stream only (`cross_checked: false`); the hooks in the recordings came from a project settings file the spike wrote. How a live run gets hooks without writing into the user's `.claude/` is untested (the inline `--settings` route is still untried) | Live mode result, #281 |
| E5 | `RuntimeCapabilities` needs more fields than spec 6.3 lists: `hook_channel`, `usage_quality` (reported or estimated), and `session_resume` must be false for native children. Probes read flag minimum versions (`--forward-subagent-text` 2.1.211, `--agents` file form 2.1.281) and refuse a CLI whose help says bare mode is the `-p` default | #281, `cli::probe` |
| E6 | Extension id and package name must not contain "Claude Code" | terms gate |

### Checkpoint contract (spec 5.3, 7.4, #289)

**Ratified for aibus-owned state only, with five amendments.** What exists today: an append-only JSONL store, one fsync per append, torn-tail repair on reopen, unique uuid v7 event ids that ascend in file order across a restart (#319), a new attempt per retry with the old one left inspectable, and `requires_reconciliation` when a tool never reported back (case 05).

| # | Amendment | Why |
|---|---|---|
| C1 | State plainly: a checkpoint holds aibus-owned scheduler state, approvals and artifact references. It never holds vendor-session state. `session_resume` stays unsupported for Claude: native children live inside the lead process and die with it (#281, case 05). A retry is a new attempt | Spec 5.3, acceptance criteria |
| C2 | The Causl durability check (spec 7.4) was not done in Phase 1: #281 didn't scope it and #282 stored events in its own file format. It becomes an **entry condition of #289**, not an assumption | Spec 7.4 asks for it before assuming crash recovery |
| C3 | `requires_reconciliation` is per attempt today (any open tool sets it). #289 needs it per tool, with an irreversibility class, or every killed run will ask for reconciliation | `AttemptView.requires_reconciliation` |
| C4 | Orphaned CLI process detection is not built or tested. The spec says "where safe"; #289 has to say what safe means (the pid is in `attempt_started`, a pid check can hit a recycled pid) | Case 05 only covers the store |
| C5 | Durability cost has to be decided with the scheduler: every append fsyncs, which also bounds how fast the host can read (#324). Batching trades a small crash window for throughput, and the contract has to say which | #324 |

Approval persistence (persist the whole pending decision before surfacing it) is not exercised by Phase 1: no `approval_requested` or `approval_resolved` event exists in any trace. That is #288's to build, and the contract stands as written.

## What this PR does not claim

- No deterministic re-execution: `aibus runs rerun` is refused, and replay is the recording.
- No vendor-state resume.
- No timing benchmark: latency is one sample per run, from the spike (derived) plus one live observation, and I did not queue anything on Mercury.
- No review of a real pull request: the recorded streams are the spike's two-subagent read tasks, which are repo-review shaped (a lead delegating lookups) but are not a PR review. The `repo-review` workflow compiles and plans, but it is plan-only (`runnable_now: false`) until the scheduler lands (#288), so no workflow ran end to end.
- No interactive approval through aibus (`--permission-prompts host` is untested).
- No other CLI: codex 0.167.0 and kimi 2.5.2 are installed and logged in here, gemini is not on PATH; live mode records versions only because their extensions are #287.

## Where things are

- Suite: `cargo test --locked -p aibus --test reference_285` (offline, ten cases plus the scorecard and example-trace checks, about 6 seconds). `AIBUS_BLESS=1` rewrites the scorecard and trace.
- Live (optional, never in CI): `AIBUS_REFERENCE_LIVE=1 AIBUS_REFERENCE_OUT=<dir> AIBUS_REFERENCE_LIVE_CLIS=claude,codex,kimi,gemini cargo test --locked -p aibus --test reference_285 -- --ignored live_reference`
- Corpus manifest: `fixtures/claude-code/reference-285/corpus.json`, stand-ins in `stand-ins/`
- Scorecard: `fixtures/claude-code/reference-285/scorecard.json`; redacted example trace: `fixtures/claude-code/reference-285/example-trace/` (`aibus runs inspect example-trace --root fixtures/claude-code/reference-285`)
