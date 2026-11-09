# 13. Issue map

This section says how the spec became work. The tracker is GitHub issues in `spdrman/aibus`; this file is the index into them and the record of the decisions the panel took when it cut the scope into pieces. If an issue and this file disagree, the issue is newer and wins, and I update this file.

## 1. The scheme

Three levels, two of them issues.

- **EPIC**: one issue, titled `EPIC A: <name>`. The body names the spec sections it covers, the milestones it delivers, the EPICs it depends on, and holds one `### A1 <phase name>` section per phase, each with a GitHub task list of `- [ ] #<n>` links to the task issues. Labels `epic`, `epic:A`.
- **Phase**: `A1`, `A2`. Not an issue; a section in the EPIC body. Phases are sequential within an EPIC. At most two per EPIC.
- **Task**: one issue, titled `A1.1 <name>` (task 1 of phase 1 of EPIC A). Body: a goal paragraph, spec references with GitHub links into this branch, an acceptance checklist (tests that must exist and pass, benchmark gates with their numbers, `TestBackend` snapshot requirements for UI work, WCAG ratios where the spec states them), the crates touched, `Blocked by` and `Blocks` lines naming other task ids, and `Part of #<epic>` at the top. Labels `task`, `epic:A`, one or more `area:<crate>` (`area:aibus`, `area:aibus-core`, `area:aibus-graph`, `area:aibus-provider`, `area:aibus-provider-anthropic`, `area:aibus-provider-openai`, `area:aibus-tui`, `area:aibus-testkit`) or `area:ci`. At most five per phase. Tasks within a phase can run in parallel; where one needs another in the same phase to finish, `Blocked by` says so and the task starts against the earlier one's stubs.

Nothing is assigned at creation. An issue gets an assignee the moment someone picks it up, not before.

The constraint the whole plan fits inside: at most three EPICs, at most two phases each, at most five tasks per phase, the first EPIC a spike. That is 30 tasks and 3 EPICs, 34 open issues with the spec issue #1.

## 2. Panel decisions

The panel (agentic AI, API design, Rust, architecture, performance, CLI, UI, security) proposed, attacked and settled these. Each is short on purpose; the reasoning that matters is in the task bodies.

1. **Three EPICs, not thirteen.** The first cut had one EPIC per concern (foundations, graph, agents, runtime, advisor and router, providers, TUI, CLI, persistence, continuity, security, testing, release) and 76 tasks of one to three days. The owner's ceiling of three EPICs by two phases by five tasks collapsed that into 30 work packages of roughly one to two weeks each. The fine-grained bodies were merged into the acceptance lists of the packages that absorbed them, so nothing in the spec lost its checklist line... the granularity changed, the coverage did not. The fine-grained set is kept outside the repo as working material.
2. **EPIC A is a spike.** Its job is to prove the four risky claims before anyone builds on them: the §12 contract fits in under 1,500 lines (A1.2), one commit per message with a side store stays under 4 ms p99 (A1.3), a run flows from a definition through the tool boundary into NDJSON and a rendered frame on the fake provider (A2.1 to A2.5), and `causl-engine-core` can be obtained and hosted (A1.5). Spike tasks are measured and documented under `docs/spikes/`, not hardened. The code stays and becomes the skeleton of EPIC B.
3. **Milestones map across EPICs, not one to one.** M0 to M6 are cumulative and cross-cut the crates; one EPIC per milestone would have put the TUI shell in M3 and its widgets in M4 and its tests in M6. Instead each task names its milestone, and section 4 below gives the mapping. M0 is EPIC A; M1 and M2 are EPIC B; M3 to M6 are EPIC C with their engine halves in B.
4. **The causl backend is two tasks and neither blocks `mini`.** A1.5 answers "can we get the crate and host it" during the spike; C2.4 finishes the backend, runs the differential gate and records the D-02 default decision. Both carry the dependency on the private Gitea or the public mirror (`sheetbaseproject/causl-engine-core`, empty until the owner publishes) in their `Blocked by` line. The default feature set never compiles the host layer, so an unobtainable crate costs two open issues and nothing else.
5. **Tests land with their features; C2.2 consolidates.** Every task's acceptance list names the tests it must add. C2.2 then makes sure the fifteen integration cases of 10 section 4, the twenty replay fixtures and the CI matrix of 10 section 1 exist as one coherent suite, and owns `aibus-testkit`.
6. **Security is a boundary per feature plus one proof pass.** The allow-list, path rules, env scrubbing, permission modes and redaction are acceptance criteria of A2.2, A2.4, B1.2, B1.3 and B1.4. C2.1 is the pass that proves the 09 section 10 threat table row by row, adds the fuzz targets, and ships the optional sandbox off by default (D-05).
7. **The TUI is three tasks.** Shell and theme and keys (C1.1), the recorded panels (C1.2), the screens beyond the recording with the frame snapshot suite (C1.3). The spike's A2.5 frame is what makes C1.2 a widget task rather than a plumbing task. WCAG AA for the `light` theme is measured in a test with the ratios written down, not eyeballed.
8. **Providers are one task for both real crates.** Anthropic and the OpenAI dialects share the model, price and token plumbing, and the cassette discipline is identical; splitting them would have duplicated the acceptance list. Bedrock, Vertex and OAuth stay in the release task (C2.5) where M6 puts them.
9. **No GitHub Project.** The token available at creation time lacked the `project` and `read:project` scopes. The EPIC bodies carry the task lists, which is what a board would have shown. Add a Project later if someone wants the swimlanes.
10. **Open questions 1 to 25 are built as recommended.** Each task names the questions it settles. Three deferrals the spec itself makes are listed in section 6.

## 3. EPICs, phases and tasks

| Id | Issue | Title | Milestone | Blocked by |
|---|---|---|---|---|
| **EPIC A** | #33 | Spike: prove the architecture end to end | M0, M1 (fake) | |
| A1.1 | #3 | Workspace, toolchain and CI skeleton | M0 | |
| A1.2 | #4 | Graph trait, conformance suite and mini engine | M0 | A1.1 |
| A1.3 | #5 | Core types, graph layout, side store and the Elm loop | M0 | A1.1, A1.2 |
| A1.4 | #6 | Provider trait, fake provider and FakeClock | M0 | A1.1 |
| A1.5 | #7 | causl-engine-core feasibility | M2 (early) | A1.2, the crate |
| A2.1 | #8 | Agent definitions, minimal | M0 | A1.3 |
| A2.2 | #9 | Tool executor, policy boundary and the read-only tools | M1 | A1.3 |
| A2.3 | #10 | Run loop on the fake provider | M1 | A1.3, A1.4, A2.1, A2.2 |
| A2.4 | #11 | Config, paths, redactor and headless aibus run | M0, M1 | A1.3, A2.3 |
| A2.5 | #12 | Dashboard frame from view nodes | M3 (early) | A1.3 |
| **EPIC B** | #34 | Build the engine, providers and runtime | M1, M2, M4 engine, M3 persistence | EPIC A |
| B1.1 | #13 | Agent definitions, discovery, built-ins and aibus agent | M0 | A2.1 |
| B1.2 | #14 | Tools, policy, permission modes and bash | M1 | A2.2 |
| B1.3 | #15 | Run loop, command hooks, results, retries and budgets | M1 | A2.3, B1.2 |
| B1.4 | #16 | Accounts, selection chain, credentials and aibus account | M0 | A1.4, A2.4 |
| B1.5 | #17 | Anthropic, OpenAI, Kimi and compatible providers | M1, M2 | A1.4, B1.4 |
| B2.1 | #18 | Delegation, orchestrator limits, background and foreground | M2 | B1.1, B1.3 |
| B2.2 | #19 | Fork, resume, send_message, compaction and worktrees | M2, M4 | B2.1 |
| B2.3 | #20 | Advisor checkpoints and the router | M4 | B2.1, B1.5 |
| B2.4 | #21 | Persistence: session dir, snapshots, transcripts, replay, index, resume, retention | M1, M3, M6 | A2.4, B1.3 |
| B2.5 | #22 | CLI: headless run, exit codes, --json, completions, doctor, logging, trace | M1 | A2.4, B1.3, B1.4 |
| **EPIC C** | #35 | Surface, continuity, hardening and release | M3, M4 UI, M5, M6 | EPIC B phase B2 |
| C1.1 | #23 | TUI shell, theme and key map | M3 | A2.5, B2.5 |
| C1.2 | #24 | Dashboard widgets | M3, M4 | C1.1, B2.1, B2.3 |
| C1.3 | #25 | Transcript, modals, screens and the frame snapshot suite | M3 | C1.2, B2.2, B2.4 |
| C1.4 | #26 | Ledger, handoffs, memory, recall and learning extraction | M5 | B2.2, B2.4 |
| C1.5 | #27 | Workflows, trigger hints, agent import and the M5 tool extensions | M5 | B2.1, C1.4 |
| C2.1 | #28 | Security hardening: boundaries, fuzz, redaction corpus, optional sandbox | M6 | B1.2, B1.3, B1.4, C1.4, C1.5 |
| C2.2 | #29 | Test infrastructure: testkit, engine integration suite, replay fixtures | M3, M6 | B2.3, B2.4, B2.5, C1.3 |
| C2.3 | #30 | Performance gates: benches, baselines, memory budget | M3, M6 | B2.4, C1.2 |
| C2.4 | #31 | causl backend, differential test and the default gate | M2, M6 | A1.5, B2.4, C2.2, the crate |
| C2.5 | #32 | Release pipeline, distribution, OAuth and Bedrock/Vertex | M6 | B1.5, C2.1, C2.2, C2.3, C2.4 |

## 4. Milestones to tasks

| Milestone | Exit criterion from 11 | Tasks |
|---|---|---|
| M0 skeleton and contracts | workspace, graph and conformance, provider trait and fake, definitions and config and redactor, the first subcommands, no secret in temp dirs | A1.1, A1.2, A1.3, A1.4, A2.1, A2.4, B1.1, B1.4 |
| M1 one run, headless | `aibus run --agent explore` on a real account and on the fake, replay fixture green | A2.2, A2.3, A2.4, B1.2, B1.3, B1.5, B2.4, B2.5 |
| M2 sub-agents | lead on Anthropic, three Kimi workers, one in a worktree, both graph backends replay identically | B2.1, B2.2, B1.5, A1.5, C2.4 |
| M3 the dashboard | f01 to f21 snapshots, 100k-line replay under budget, resume mid-session | A2.5, C1.1, C1.2, C1.3, B2.4, C2.3, C2.2 |
| M4 advisor and router | rail highlight and purple card in the snapshot, split row, both events in `--json` | B2.3, C1.2 |
| M5 continuity | handoff parses with the Continuous-Claude schema, next lead sees it, recall finds it, `/fix` runs with gates | C1.4, C1.5 |
| M6 hardening and release | sandbox optional, OAuth, Bedrock/Vertex, 12-hour memory test, fuzz nightly, signed binaries, the D-02 gate | C2.1, C2.3, C2.4, C2.5, B2.4 (prune) |

## 5. Coverage

Every numbered section of every spec file, every ADR and every milestone deliverable maps to at least one task. Where a section is split across tasks the list says which part went where.

| Spec | Section | Tasks |
|---|---|---|
| 00 | What aibus is, Vocabulary, Design stance | A1.3 (types and vocabulary), A2.4 and B2.5 (engine without the terminal), C1.4 (compound, don't compact), A1.3 (everything bounded) |
| 01 | 1, 1.1, 1.2, 1.3 | A2.1, B1.1 |
| 01 | 1.4, 1.5 | B1.1 |
| 01 | 2 Roles | B2.1 (lead, worker), B2.3 (advisor, router) |
| 01 | 3 Model selection | B1.4 |
| 01 | 4, 4.1, 4.2, 4.3, 4.4 | B2.1 |
| 01 | 5.1 | A2.3 |
| 01 | 5.2 | B2.2 |
| 01 | 5.3 | A2.3, B1.3 |
| 01 | 5.4, 5.5 | B2.2 |
| 01 | 6, 6.1, 6.2, 6.3 | B1.3 (command hooks), C1.5 (prompt and http hooks) |
| 01 | 7, 7.1, 7.2 | B2.3 (engine), C1.2 (panels) |
| 01 | 8, 8.1, 8.2, 8.3 | C1.4 |
| 01 | 8.4 | C1.5 |
| 01 | 9 Tools | A2.2, B1.2, C1.5 (`web_fetch`, `mcp:`) |
| 02 | 1, 2 | A1.1 (A1.3 for the thread model) |
| 02 | 3, 3.1, 3.2, 3.3 | A1.3, A2.3, B1.3, B2.1 |
| 02 | 4 Module tree | A1.3, A2.5 (followed by every later task) |
| 02 | 5, 6 | A1.3, B1.3 |
| 02 | 7 Startup | A2.4 |
| 03 | 1, 4 | A1.2 |
| 03 | 2, 3 | A1.5, C2.4 (A1.2 for the mini half) |
| 03 | 5, 7 | A1.3 |
| 03 | 6 | A1.3, B2.4 (replay), C1.3 (time travel) |
| 03 | 8 | A1.3, C1.3 |
| 03 | 9 | A1.2, B2.4 (migrations) |
| 03 | 10 | B1.4 |
| 03 | 11 | A1.3 (measured), C2.3 (gated) |
| 04 | 1, 2, 3.1 | A1.4 |
| 04 | 3 | B1.5 |
| 04 | 4, 4.1, 4.2, 5, 6 | B1.4 (A2.4 for the config shape) |
| 04 | 7 | A1.4, B1.5 |
| 05 | 1 | A2.5, C1.1 |
| 05 | 2, 2.1 to 2.9 | C1.2 |
| 05 | 3 | C1.3 (C1.1 for Help) |
| 05 | 4 | C1.1 (keys), C1.3 (slash commands) |
| 05 | 5 | C1.3 |
| 05 | 6, 7 | A2.5, C1.1 |
| 05 | 8 | C1.2, C1.3 |
| 06 | 1 | A2.4 (parse), B1.1, B1.4, B2.2, B2.4, C1.4, C1.5 (each subcommand with its feature), B2.5 |
| 06 | 2, 3, 4, 5, 6, 7 | B2.5 (A2.4 for the minimal `run`, A1.3 for the exit-code enum) |
| 07 | 1, 3 | A2.4 (B2.4 for the session tree) |
| 07 | 2, 2.1, 2.2, 2.3 | B2.4 (C1.4 for `memory_fts` use) |
| 07 | 2.4 | B1.1, C1.4, C1.5 |
| 07 | 4 | B2.4, B2.5 (log), B1.5 (model cache), B2.2 (worktrees) |
| 07 | 5 | B2.4 |
| 08 | 1 | A2.5 (measured), C2.3 (gated) |
| 08 | 2 | A2.3, C2.3 |
| 08 | 3, 4 | A1.3, A1.2, C2.3 |
| 08 | 5 | C2.3 (B1.3 for dropping conversations) |
| 08 | 6 | B2.5 |
| 08 | 7 | B1.5, B2.2, B2.3 |
| 08 | 8 | B1.1, B1.5 |
| 09 | 1 | B1.2, B1.4, C2.1 |
| 09 | 2 | A2.2, B1.2 |
| 09 | 3 | B1.2 |
| 09 | 4 | A2.4, C2.1 |
| 09 | 5 | B1.1, A2.3, C1.5, C2.1 |
| 09 | 6 | B1.3, C1.5 |
| 09 | 7 | B1.2, B2.2, B2.4 |
| 09 | 8 | B1.5, C1.5 |
| 09 | 9 | A1.1, A1.5, C2.4, C2.5 |
| 09 | 10 | C2.1 |
| 10 | 1 | C2.2 |
| 10 | 2 | the task that owns each module (A2.1, B1.1, B1.2, B1.3, A2.3, A2.4, C1.4, B2.3, A1.3, B2.4) |
| 10 | 3 | A1.4 |
| 10 | 3.1 | B1.5 |
| 10 | 4 | C2.2 (cases land in B1.3, B2.1, B2.2, B2.3, B2.4, C1.4) |
| 10 | 5 | A2.3, B2.4, C2.2, C2.4 |
| 10 | 6 | A2.5, C1.2, C1.3 |
| 10 | 7 | B2.5 (B1.1, B1.4 for their subcommands) |
| 10 | 8 | A1.2 |
| 10 | 9 | C2.3 |
| 10 | 10 | C2.2, C2.5 |
| 11 | 1 M0 to M6 | section 4 above |
| 11 | 2 Out of scope | stays out; see section 6 |
| 11 | 3 Dependencies | A1.1 (crate choices), A1.5 (the causl risk) |
| 12 | Q1 | B1.1, C1.5 |
| 12 | Q2 | A2.1 |
| 12 | Q3, Q4, Q5, Q6 | B2.3 |
| 12 | Q7 | A2.3, B1.2 |
| 12 | Q8 | B2.1 |
| 12 | Q9 | B2.2 |
| 12 | Q10 | C1.4 |
| 12 | Q11 | C1.3, C1.4 |
| 12 | Q12 | A1.5, C2.4 |
| 12 | Q13 | A1.3 |
| 12 | Q14 | B2.4, C1.3 |
| 12 | Q15 | A1.1 |
| 12 | Q16 | B1.4 |
| 12 | Q17 | B1.2, C1.3 |
| 12 | Q18, Q19 | C1.1 |
| 12 | Q20 | B1.5 |
| 12 | Q21 | C1.1, C2.5 |
| 12 | Q22 | B2.3, C1.2 |
| 12 | Q23 | A2.3 |
| 12 | Q24 | C1.4 |
| 12 | Q25 | B1.5 |
| ADR | D-01 | A1.1 |
| ADR | D-02 | A1.2, A1.5, C2.4 |
| ADR | D-03, D-04 | A1.3 |
| ADR | D-05 | A2.2, B1.2, C2.1 |
| ADR | D-06 | A2.4, B1.4 |
| ADR | D-07 | B2.1, C1.5 |
| ADR | D-08 | B2.2, B2.3, C1.2 |
| ADR | D-09 | B2.4 |
| ADR | D-10 | A1.1 |
| ADR | D-11 | C1.4, C1.5 |

## 6. Deferred, and why

The out-of-scope list in 11 section 2 stays out; no task touches it. Beyond that, three items the spec itself parks are not in any task's acceptance list and get an issue only if someone asks:

- **Advisor delta mode** (Q5): the spec says "a config option in M4 if cost bites". B2.3 records the packet size per call so the cost can be measured; nobody builds the option until a session shows it is needed.
- **Per-session "trust this agent for `bash(cargo *)`" suggestion** (Q17): the spec calls it an M3 UX review item. C1.3 ships the queue and the `a` key; the suggestion waits for a real session's noise.
- **A launcher screen** on `aibus` with no arguments (Q19): the spec picks "start immediately" and calls the launcher a later UX item. C1.1 implements the pick.

Nothing else is deferred. If a reader finds a spec sentence with no task behind it, that is a bug in this map, and the fix is a line in the right task's acceptance list.
