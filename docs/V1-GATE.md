# v1 release gate for EPIC A (A3.5, #295)

Date: 2026-11-25. Tip: GATE_TIP.

**Verdict: the gate is not passed.** Nothing in this record is a release decision. I have not tagged anything and I have not ticked a box in #280; both are the owner's. "v1" here means the spec's product scope (spec 11 and 9 use it that way), not a semver 1.0: the workspace is 0.2.1 and whether the next cut is 1.0, 0.3 or something else is undecided.

Why it is not passed, in the order that matters:

1. **Most of Phase 2 and 3 exists as libraries that no command runs.** The scheduler (#288), the durable run (#289), worktree review (#290), OTLP export (#292), evaluation and the variant runner (#293) have no host. `aibus workflow run` still uses the chain runner. So the approvals, consent records, attempt review and exports that the gate asks me to attest are tested but not on the path a user's run takes.
2. **The demo the issue asks for was not run.** "Claude + one more official CLI + OpenRouter on supported machines" has not been demonstrated end to end. Claude Code and Codex were each run live in earlier phases (the ignored live tests), and OpenRouter is verified offline, but nothing ran the three together through aibus.
3. **The benchmark regression row is only partly met** (see below): the three Phase 0 benches have a Mercury baseline and passed against it, and nothing from Phase 2 or 3 has a bench.
4. **Five of the seven extensions were never run against a real CLI**, and Codex on this Mac is a newer version than its declared range.
5. **Release hygiene is open:** the CHANGELOG has no entry after 0.2.1, the Windows, Linux aarch64 and macOS x86_64 builds were never smoke-run, and the subscription sign-in and commercial-use questions in the terms gate are still the owner's.

What I did deliver is the evidence that was missing: a security review record (docs/SECURITY-REVIEW-295.md), a compatibility matrix (docs/COMPATIBILITY.md), the supply-chain results with the fixes they needed (the lock held four known advisories, now fixed), this checklist, and the cuts in docs/DEFERRED.md. Where I made a call the owner may want to change, it is listed under "Decisions" at the end.

## What I ran, and what it showed

| Check | Where | Result | Log |
|---|---|---|---|
| Scoped gate: rustfmt ratchet, clippy of the changed crates (and `aibus-extension --features wasm`), the new test by name | MARS, `aibus-gate:1.89`, slot 1 | GATE_SCOPED | `/work/nas-work/feat-295-v1-gate/295-gate.log` on MARS |
| Tip gate: `cargo clippy --workspace --all-targets --locked -D warnings`, then `cargo test` for `aibus-testkit` and `aibus-extension --features wasm` (the crates I changed, whole), and the three security suites | same container, same log | GATE_TIP_RESULT | same log |
| `cargo deny --locked check` and `cargo audit` | same container, after the tests | GATE_SUPPLY | same log |
| Red test first | MARS | `the_lock_holds_no_version_with_a_known_advisory_fix_missed` failed on `url` 2.5.8 before the fix, passed after | `295-red.log` |
| Benchmarks | Mercury, through `bench-queue.sh` | Passed against the baseline written in the same turn (table below) | see "Benchmark regression" |

## Phase 3 exit and gate

| Phase 3 exit item | Status | Evidence | What is missing |
|---|---|---|---|
| Evidence-based engineering workflow comparisons | **Partly met** | `aibus runs compare`, `runs repro`, `runs inspect --node --json` (`aibus/tests/compare_cli_294.rs`); `aibus-trace` `eval`, `experiment`, `suggest`, `compare`, `causal` (`eval_293.rs`, `compare_294.rs`); the ten-case suite `fixtures/eval-293/suite.json` | No runner launches two variants on a fixture checkout, so no number describes a real provider; the ten repositories are not in the tree; the graphical client and a planned-versus-actual diff are not built (docs/DEFERRED.md, A3.3 and A3.4) |
| Secure shareable integrations | **Partly met** | Extension contract, sandbox host, local registry with signatures and revocation, key scopes, WASM guest host (docs/spec/16-extensions.md; `aibus-extension/tests/`); the review in docs/SECURITY-REVIEW-295.md | Not an OS sandbox; five extensions are unverified against real CLIs; three need `extra_official_clis`; nothing launches an extension in a product run yet |
| Optional interoperable observability | **Partly met** | `aibus_trace::otel::to_otlp`, `aibus-otel` (off unless a destination is given; `aibus-otel/tests/export_292.rs`); accepted by `otel/opentelemetry-collector:0.116.1` on 2026-11-25 (docs/OTEL.md) | No config key or command builds a destination; Langfuse never run; no retention policy; plain `http://` to a public host is not refused (F2) |
| Release-ready documented limitations | **Partly met** | This page, docs/SECURITY-REVIEW-295.md, docs/COMPATIBILITY.md, docs/DEFERRED.md, docs/PROVIDER-FEASIBILITY.md, docs/EXTENSION-SCORECARD.md | CHANGELOG entry, version number, migration guidance beyond what is in docs/MIGRATION.md, and the owner decisions in the terms gate |

Phase 3 gate (spec 13), one line each:

| Gate item | Status | Note |
|---|---|---|
| More authorized providers | Partly | Claude Code verified (2.1.311), Codex verified (0.167.0), OpenRouter offline only, Gemini, Kimi, Copilot, Cursor and Qwen unverified, xAI and DeepSeek disabled by design |
| OTLP | Partly | See above |
| Evaluation and optimisation | Partly | Library only; four suggestion kinds, several need events nothing emits |
| Causal comparisons | Partly | Comparison and inspector exist as view models and CLI; no desktop client |
| Security | Partly | One real finding fixed, the rest listed; the boundaries are not yet on a run path |
| Documentation | Partly | Written here; the three open questions in #295 are answered by an assumption (see "Decisions") |

## Acceptance criteria of #295

| Criterion | Status | Evidence |
|---|---|---|
| Zero known credential-harvesting paths and no unapproved provider embedding paths; ambiguous integrations stay disabled | **Met for the code that exists, not demonstrated on a running product** | docs/SECURITY-REVIEW-295.md, "What the boundaries do not yet protect". xAI, DeepSeek and the unlisted CLIs are `disabled` |
| Full 15-issue acceptance suite, sample redacted trace, benchmark scorecard and reproducibility manifest published | **Partly met** | The table below is the suite's index (published in the repository, which is where I decided it goes). Sample trace: `fixtures/claude-code/reference-285/example-trace/`. Scorecard: `fixtures/claude-code/reference-285/scorecard.json` and docs/spikes/A1.5-reference-pilot/benchmark.md (documentation-based, 2026-11-15). Reproducibility report: `aibus runs repro`; there is no manifest file format yet |
| Claude + at least one more official CLI + OpenRouter on supported machines, with truthful limitations and user-owned auth | **Not met** | No end-to-end run. The pieces: Claude Code and Codex live tests (ignored, run by hand in earlier phases), OpenRouter offline |

## EPIC acceptance checklist (spec 13)

| # | Item | Status | Evidence | Gap |
|---|---|---|---|---|
| 1 | One official Claude Code invocation delegates to two genuine native subagents with an evidenced trace | **Met** | docs/spikes/A1.1-claude-code/README.md (runs `07-deleg`, `11-foreground`), fixtures `fixtures/claude-code/spike-281/`, the Phase 1 gate (#285) | One version (2.1.311) on one Mac |
| 2 | No non-OpenRouter extension asks for, intercepts, extracts, stores or reuses a vendor subscription token | **Met for aibus's code** | `manifest.rs::auth_stays_with_the_vendor_cli`, `env_allow_refuses_credentials_...`; `sandbox.rs::the_cli_gets_no_ambient_credential_vault_key_loader_variable_or_parent_path`; `aibus-supervisor/tests/credentials.rs` | A vendor CLI keeps its own token and aibus cannot police that. No product run launches one yet |
| 3 | Each public CLI adapter is separately installable, disableable and versioned, with a policy and provenance record | **Met** | `aibus-extension/tests/store.rs`, `registry.rs`, `key_scope.rs`; seven extension directories under `examples/extensions/` | Five are unverified against a real CLI (docs/COMPATIBILITY.md) |
| 4 | Workflow source and validation produce a deterministic plan and manifest; the user can preview generated CLI config changes | **Partly met** | `workflow plan --dry-run` with a stable plan hash (D-283; `aibus/tests/workflow_plan.rs`) | I found no command that prints the argv or config a vendor step would be launched with; `sandbox::prepare` builds it in library code |
| 5 | Scheduler and Jev deterministic routing and approvals execute without model calls for policy decisions | **Partly met** | `aibus-core/tests/sched_288.rs`, Jev fixtures `fixtures/jev/` | No host runs the scheduler; nothing prompts for an approval (docs/DEFERRED.md, #288) |
| 6 | The execution DAG and the causal evidence graph do not invent hidden native agents or reasoning | **Met** | Evidence levels on every node and edge; `aibus-trace/tests/` goldens under `fixtures/claude-code/views-284/`; `compare_294.rs::runs_are_never_called_comparable_while_something_about_them_is_unknown` | Library and CLI only |
| 7 | A local append-only trace and offline recorded replay work without provider access | **Met** | `aibus-trace/tests/store.rs`, `aibus/tests/reference_285.rs` (ten cases, offline, byte-for-byte scorecard) | |
| 8 | Crash recovery and retries are limited to aibus-owned authorised state; vendor resume is capability-gated | **Partly met** | `aibus-core/tests/durable_289.rs` | No host uses the durable run; a vendor step is never resumed (it starts again after a yes) |
| 9 | Write attempts are worktree-isolated, reviewed and merged only by an approved action | **Partly met** | `aibus-core/src/worktree.rs` unit tests | No `aibus attempts` command, TUI screen or MCP tool calls `review`, `approve` or `reject` (docs/DEFERRED.md, #290) |
| 10 | Two workflow variants can be evaluated against identical fixtures and compared on quality, time and available usage and cost | **Partly met** | `eval_293.rs::ten_cases_two_variants_report_what_was_observed`, `results_that_are_not_the_same_cases_for_every_variant_are_refused` | Synthetic traces; no runner; `unknown` stays `unknown` |
| 11 | Optional OTLP/Langfuse export is off by default and redacts sensitive content | **Partly met** | `export_292.rs::no_destination_means_no_connection_even_with_otel_env_set`; `otel_292.rs::secrets_are_redacted_before_export_and_names_stay_low_cardinality` | Off because nothing calls it. Langfuse not run |
| 12 | Provider compliance and release gates, regression fixtures and version compatibility status are documented | **Partly met** | docs/spikes/A1.1-claude-code/terms-gate.md (2026-11-14), docs/PROVIDER-FEASIBILITY.md, docs/COMPATIBILITY.md | The terms gate was due for re-verification "before any release (#295)"; I did not re-read the Anthropic pages for this PR. No dated terms record exists for Codex, Gemini or OpenRouter |

## The 15 issues of EPIC A

All fifteen are closed except this one. A closed issue here means its reduced scope landed, not the spec text in full: each cut is in docs/DEFERRED.md under the issue number.

| Issue | Closed with | Acceptance evidence | Cut to DEFERRED |
|---|---|---|---|
| #281 A1.1 spike | GO-WITH-CONDITIONS | docs/spikes/A1.1-claude-code/ | Owner decisions on subscription sign-in and naming still open |
| #282 A1.2 supervisor, normalised events, trace store | supervisor, evidence trace | `aibus-supervisor/tests/`, `aibus-trace/tests/store.rs`, docs/spec/14-evidence-trace.md | |
| #283 A1.3 workflow compiler, prompt versioning, dry run | compiler and plan hash | `aibus-core/tests/workflow_compile.rs`, `aibus/tests/workflow_plan.rs`, D-283 | |
| #284 A1.4 graph, timeline, inspector, replay | views and replay | `aibus-trace/tests/view.rs`, `fixtures/claude-code/views-284/`, D-284 | |
| #285 A1.5 reference pilot, scorecard, Phase 1 gate | GO | `aibus/tests/reference_285.rs`, docs/spikes/A1.5-reference-pilot/ | |
| #286 A2.1 extension SDK, sandbox host, registry | SDK, local registry, WASM host (#333) | `aibus-extension/tests/`, D-286 | Network registry, WASI |
| #287 A2.2 Codex, Gemini, OpenRouter | four sub-issues (#357 to #360) | `codex.rs`, `gemini.rs`, `openrouter_359.rs`, `handoff_360.rs`, D-357 to D-360 | Gemini unverified; consent is an environment variable in the live test |
| #288 A2.3 Jev, scheduler, approvals | pure state machine | `sched_288.rs`, D-288 | No host, no prompt (DEFERRED) |
| #289 A2.4 durable state | journal and reconciliation | `durable_289.rs`, D-289 | No host; crash-safety edges (DEFERRED) |
| #290 A2.5 worktree attempts and review | library | `worktree.rs` tests | No command; several edges (DEFERRED) |
| #291 A3.1 more extensions, feasibility register | five extensions, register | `extensions_a31.rs`, docs/PROVIDER-FEASIBILITY.md | All five unverified (DEFERRED) |
| #292 A3.2 OTLP | exporter | `otel_292.rs`, `export_292.rs`, docs/OTEL.md | No config, no Langfuse (DEFERRED) |
| #293 A3.3 evaluation | library | `eval_293.rs`, `fixtures/eval-293/` | No runner, synthetic (DEFERRED) |
| #294 A3.4 comparison and causal inspector | library, CLI, TUI | `compare_294.rs`, `compare_cli_294.rs` | No graphical client (DEFERRED) |
| #295 A3.5 this gate | this PR | this page | See below |

## Published pieces the issue asks for

| Piece | Where | Status |
|---|---|---|
| Provider policy matrix | docs/CAPABILITIES.md (generated, capabilities), docs/PROVIDER-FEASIBILITY.md (whether a CLI may be launched), docs/spikes/A1.1-claude-code/terms-gate.md (Anthropic) | Anthropic dated 2026-11-14; no dated terms record for the others |
| Supported capability and CLI versions | docs/COMPATIBILITY.md | New |
| Source-controlled benchmark corpus | `fixtures/claude-code/reference-285/corpus.json` (ten repo-review cases), `fixtures/eval-293/suite.json` (ten cases, synthetic) | Exists; the second has no repositories behind it |
| Extension authoring docs | docs/spec/16-extensions.md, `examples/extensions/template/` | Exists; I did not have a third party follow it |
| Migration guidance from v2 | docs/MIGRATION.md, "From 0.2.x to the EPIC A model" | New, short, and says what it assumes |

## Benchmark regression

**Status: partly met.** The three Phase 0 benches (`commit_stream`, `hydrate_snapshot`, `render_dashboard`) have a recorded baseline on Mercury now and a gate pass against it. Nothing newer than Phase 0 is benchmarked: the trace store, `to_otlp`, `compare` and the WASM host have no bench at all, so a regression in any of them would not be seen (docs/DEFERRED.md).

What I ran, on 2026-11-25, as job `295-bench-mercury` through `/Users/rom/workspace/bench-queue.sh dh`:

* **Build** (no exclusivity needed, so not on Mercury and not in the queue): the three bench executables, built with `cargo bench --locked --no-run` in `rust:1.89-bookworm` (linux/arm64) on the MacBook's Docker, from this PR's tree. The same bench binaries then ran in `debian:bookworm-slim` (same glibc) on Mercury. The tree sat at `/work` for both, so the baseline directory the binaries compiled in resolves.
* **Host:** MERCURY, a colima Linux VM (aarch64, 6 CPUs, kernel 6.8.0-117), load 0.00 when the turn started, queue empty before and after. The turn lasted well under a minute.
* **Pass 1** `AIBUS_RUNNER_CLASS=mercury AIBUS_BENCH_SAVE=1` wrote `benches/baselines/mercury/*.txt` (ten files, in this PR). **Pass 2** `AIBUS_BENCH_GATE=1` ran straight after against them: every bench was inside its absolute budget and inside baseline times 1.20 plus the 25 us noise floor, exit 0 for all three.

| Bench | Baseline p99 (us) | Pass 2 p99 (us) | Budget p99 (us) |
|---|---|---|---|
| commit_stream-mini-open | 51.0 | 47 | 2000 |
| commit_stream-mini-any | 46.7 | 45 | 4000 |
| hydrate_snapshot-mini-hydrate | 81.0 | 82 | 100000 |
| hydrate_snapshot-mini-parse-and-hydrate | 133.2 | 139 | 100000 |
| render_dashboard-replay-98x46-render | 278.8 | 277 | 3000 |
| render_dashboard-replay-98x46-resize | 366.7 | 367 | 10000 |
| render_dashboard-heavy-98x46-render | 308.2 | 310 | 3000 |
| render_dashboard-heavy-98x46-resize | 374.5 | 375 | 10000 |
| render_dashboard-heavy-80x24-render | 190.8 | 199 | 3000 |
| render_dashboard-heavy-80x24-resize | 177.3 | 178 | 10000 |

What these numbers are not: pass 2 followed pass 1 within one turn on the same VM, so agreement shows the bench is repeatable over a minute and nothing more. It does not show that the code did not regress against an earlier commit, because there is no earlier Mercury run to compare with. The older `benches/baselines/mars/` files are a different runner class (calibration 1586 us there against about 2630 us here) and are not comparable; I left them untouched. Calibration moves the allowed time when the machine is slower, but a colima VM's noise is not the same as a bare-metal runner's, so I would not trust a 20 % verdict from this baseline on a loaded day. The first real use of it is the next change that touches `state/` or `tui/`. To repeat: build the same three executables for linux/arm64, push them with `dh.sh push mercury <job> <dir>`, and run them with `bench-queue.sh dh <job> debian:bookworm-slim sh run.sh`; the `run.sh` I used is the loop in the "Pass" description above, `./bin/<bench> --bench` with the two environment variables.

## Decisions

Recorded as a comment on #295.

* **What "v2" means.** The issue does not say. I read it as aibus 0.2.x users moving to the new workflow, extension and provider model, and wrote a short section in docs/MIGRATION.md that says so in its first line. If the owner meant the v2.0 EPIC A spec, that section is the wrong document and a spec-delta note is needed instead.
* **Where the suite is published.** In the repository: the table above, linked from docs/TESTING.md.
* **What "v1" means.** The spec's product scope. No version number is chosen here.
* **chrono.** `cargo deny` fails on RUSTSEC-2026-0543 (unmaintained, no fix, through `rmcp`). I added one reviewed ignore with a review-by date of 2027-02-25 and kept `unmaintained = "all"`. The date is only a comment and a line in docs/RELEASE.md; nothing enforces it.

## Cut to docs/DEFERRED.md

See the A3.5 section there: the unmaintained notices, F2 to F4 of the security review, the causl crate's lock, benchmarks for the new code, the CHANGELOG entry, the network re-read of the terms, and the end-to-end three-provider demo.
