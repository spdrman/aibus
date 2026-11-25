# Deferred work

Things I cut from an issue's scope on purpose, with where they stand. One section per issue.

## #373 send guidance to a Claude Code session (phase 2 of #348)

* Fork into aibus (start a new session from a tapped one) is not built. It is a follow-up.
* The live check of the `[aibus]` message line is not done. The line format for a sender that is not a Claude session is not documented, so the tests pin what aibus controls (consent, limits, audit, refusals), not what the session does with the line. Someone with a real session needs to send one line and see how the session's own screen shows it.
* Delivery is always reported as `delivery unknown`. Marking a send delivered needs a later event that proves it, and phase 1's hook events do not name the message.
* The rate limit counts the audit rows inside the session's lock, but the count is global and the lock is per session, so two different sessions sent to at the same instant can each see 9 rows and both send. Each send still needs a person at a terminal, so I left it. A global flock around count and append would close it.
* The socket is checked before the confirmation and connected to after it. Re-checking right before `connect`, a parent directory owner and mode check, and a peer credential check after connect would close the window. Not done.
* Managed settings that cannot be read (unreadable, over 1 MiB, a symlink, invalid JSON) fall back to the user's settings, so the session's own setting decides. Failing closed for a managed file that exists but cannot be read is a follow-up.
# Deferred

Work that was in scope for an issue once and got cut to fit. Nothing here has an issue of its own yet.

## Attach phase 2 leftovers (#346)

I did `runtime = "attached"` in #353, and `step_ack`, tool-call correlation and a cooperative interrupt in #346. These three are left...

* **Channels as a push transport.** `--dangerously-load-development-channels` would let aibus push a step or an interrupt into a Claude Code session instead of waiting for it to pull. Not built, and not checked against a live client. Until it is, an interrupt reaches the agent only on its next `step_ack`, `step_tool_call` or `next_step`.
* **Codex push.** Whether a second client can attach to the thread the terminal shows, to start, steer and interrupt through the app-server, is unanswered. It needs a live Codex install and a session of the owner's, so it is an owner step.
* **A non-letter chord to accept an attach.** The typed `/attach accept <code>` stays. A chord only matters if the owner wants it instead of the code, and nobody asked for it yet.

Also unverified here: the live behaviour of `step_ack` and `step_tool_call` against Claude Code. The tests use fixtures; a PostToolUse hook that calls `step_tool_call` is not shipped.

Smaller leftovers from the #385 review, none of them a bug...

* The 10,000 reported-call cap has no test of its own (the 100-name cap does).
* Bidi and zero-width characters in a reported name rely on the shared `clean()`, which is tested elsewhere, not on a test of `tool_call`.
* A workflow cancel of a step the session already pulled queues the same interrupt as the keyboard, through the same function, but only the keyboard path has a test that pulls first.

# Deferred scope

Work cut from an issue's reduced definition of done, kept here so it is not lost. Each section names
the issue it came from and what picks it up.

## B1.5 Phase 1 gate (#308)

The gate record is `docs/desktop/B1.5-phase1-gate.md`: GO-WITH-CONDITIONS on ADR-B-001. What it leaves open, and who takes it...

- **macOS run and numeric responsiveness targets.** They block opening Phase 2 (conditions 1 and 3 there); the owner runs `docs/desktop/VERIFY-MACOS.md`.
- **Bridge wiring, deltas, the per-call re-read of the trace, and a bounded graph** go to #310. The re-read costs about 53 ms per 500-event batch at 10,000 events and about 0.6 s at 100,000.
- **Virtualised timeline** goes to #311, **command grants and the DTO credential test** to #312, **Playwright on the real webview and keyboard access to graph nodes** to #313.
- **Webview timings at medium and large** (first render, latency, memory, graph interaction) and **React Flow with memoisation** are unmeasured; nothing here runs a webview at scale.

## B1.1 Tauri desktop spike (#304)

I cut these from the spike on 2026-11-22. B1.5 (#308) reviews them with the go/no-go.

- **Live Claude Code data in the desktop app.** The spike replays the A1.1 capture through the real
  normaliser, labelled as a fixture. Following a live supervised run means tailing a `TraceStore`
  from the host and streaming new snapshots over the same channel. That needs a launch command with a
  bounded argument set (no command strings from the webview), so it belongs with the run workspace
  slice of Phase 2, not a spike.
- **macOS build and run.** Unverified; owner steps are in `docs/desktop/VERIFY-MACOS.md`.
- **Timing benchmarks.** The Linux numbers in `docs/desktop/B1.1-go-no-go.md` are observations from
  two Xvfb runs on the NAS, not benchmarks. A queued Mercury run (`bench-queue.sh`) can repeat them once
  Phase 2 has a workload worth timing.
- **Shared UI components.** Done in #307: the shell uses the shared primitives and tokens.
- **Bundling and signing.** `bundle.active` is false. AppImage/deb, `.app`/`.dmg`, notarisation and
  updater keys are release work, not spike work.

# Deferred work

Scope that was cut from an issue on its way to closing, with the reason and what picks it up. One
section per issue.

## #288 A2.3 Jev policy engine, deterministic DAG scheduler and approval gates

I landed the scheduler as a pure state machine (`workflow::sched`), the approval gates on it and the
dry run. These parts are not in it:

- **No run path uses the scheduler yet.** `aibus workflow run`, the `workflow` tool and the TUI still
  walk the chain runner (`workflow::execute`). Wiring one host onto `Scheduler::advance` / `report` is the
  next step, and the "the scheduler (#288) runs it" refusals stay until a host exists.
- **The chain runner's OpenRouter-account steps.** An `aibus` step whose account (or per-account
  fallback) is OpenRouter's runs on the chain runner without asking Jev, because the chain runner
  delegates with `account: None` and the account is picked in the launcher, which does not know the
  run is a workflow step. Threading that through `Command::Delegate` touches about 24 call sites, so it
  goes with the host wiring above. The scheduler path does check it (`set_account` re-runs Jev and
  returns `Stop`).
- **A person answering in the TUI and CLI.** `Scheduler::approve` and `deny` need a name and log it,
  but nothing prompts for them yet; the live handoff fixture's consent is still an environment
  variable. This comes with the host wiring too.
- **Linux unattended runs** wait for the subreaper helper (#352, PR #370). The scheduler starts
  nothing itself, so it has no code that depends on it.
- **The #104 MCP caps** (`--max-runs`, `--max-concurrent-runs`, `--max-cost-usd`) are not inputs to
  `max_agent_runs` / `max_parallel` yet.
- **Cost and latency preference** (spec 5.2 rule 6): nothing has evidence-backed numbers, so there is
  no ranking rather than a made-up one.

## #289 A2.4 Durable aibus state, checkpoints, attempts and crash reconciliation

I landed durable journaled transitions, `requires_reconciliation`, a new attempt id per retry with the old trace kept read-only, and the persisted handoff spent set (`workflow::durable`, D-289). These parts are not in it:

- **No host uses `Run` yet.** Like the scheduler (#288), nothing in `aibus workflow run`, the TUI or the MCP tool drives it, and nothing chooses where `<root>` lives. That comes with the host wiring.
- **Queue position and checkpoint references.** There is no queue across runs to have a position in yet, and no step writes a checkpoint the journal could point to.
- **Recorded-trace replay across attempts, `rerun --isolated`, and the manifest and environment-difference reports.** `trace` reads any attempt's records, but there is no command, no rendering and no environment manifest to diff. The command spelling question (`aibus rerun` or `aibus runs rerun`) stays open with it.
- **Vendor-session resume capability gates.** A vendor step is never resumed: it starts over in a new session after a yes. A documented resume API per vendor, and how `/resume` (#124) maps onto `vendor_resume`, are not done.
- **Detecting live or orphaned CLI processes.** The run lock tells a live aibus from a dead one; a child process that outlived its aibus is not looked for.
- **Causl.** Spec 7.4 asks for its event persistence and snapshot durability to be validated before crash recovery depends on it; nobody has, so the journal is plain JSONL.
- **Crash-safety edges the review found.** The new run directory's entry in `<root>` is not synced after it is created, so a power cut right after `create` could lose the whole run. `retry` returns an error if making the old journal read-only fails, although by then the run already is the new attempt. `open` does not check a retry's `previous` id or carried spent set against the earlier attempt's journal. And the failed-write cut-back (the `broken` latch) and the skip of an empty attempt in `open` have no tests, since both need fault injection.
- **Upgrades.** A new Jev policy revision makes `Scheduler::new` refuse the pinned plan, so a run that was in flight across an upgrade cannot be reopened. It fails closed; carrying such a run over (or finishing it on the old revision) is not designed.
- **Tamper resistance against someone who can write the run directory.** The hash chain catches an edit that changes a decision on replay, but someone with write access can rewrite the whole journal consistently. An HMAC keyed from the keychain would close that, same as the D-360 note.

## #290 A2.5 Git worktree attempts, artifacts, code review and fleet UX

I cut this to the library layer in `aibus_core::worktree`: a worktree per write attempt with its base commit pinned under `refs/aibus/base/<id>`, one repository lock around every change to the repository, cleanup that keeps uncommitted work, and `review` / `approve` / `reject` with no automatic merge. These parts are not in it:

- **No host calls approve or reject yet.** There is no `aibus attempts` command, TUI screen or MCP tool, so an operator cannot reach `review`, `approve` or `reject` from the product. That needs a decision on where the prompt lives, and it comes with the scheduler host wiring in #288's deferred list.
- **The fleet view is `/runs` as it is (#284).** It does not list worktree attempts, and the issue's "pause" is not built: no documented CLI control capability pauses a run, so there is nothing honest to call. Stop and inspect are the ones `/runs` and `aibus runs` already have; routing the fleet's stop through #274's `session_cancel` stays open.
- **Test reports and artifact hashes beyond the patch.** `review` hashes the patch (SHA-256). Running the project's tests in the attempt, storing their report, hashing other artifacts, and writing the task-run association and git status before and after into the run manifest (spec 4.3) are not done.
- **Resuming a path is not exclusive.** `create` on a worktree that is already there hands it back, so two runs given the same run id share one tree. Distinct run ids never do. A claim on the attempt (a lock file that dies with its process) would close this.
- **A second `approve` after a conflict.** The conflict is aborted and the attempt kept on `aibus/<id>`, but calling `approve` again fails on the existing branch. Resolving the conflict, by rebasing the attempt or merging by hand, is the operator's job for now.
- **Ignored files** in a rejected or approved attempt are not captured in its patch, and `reject` removes them with the checkout.
- **Windows** is untested: paths and the lock use `fs4`, but I only ran the tests on Linux.
- **Two small things in `create`** (from review): the parent directory is made before the "inside the repository" check, so a refused path can leave an empty directory behind, and `git worktree prune` there clears every stale registration of the repository, not only this attempt's.
- **A swapped `.git` file in the attempt's checkout.** The attempt's tree is the agent's, so it can replace the `.git` file (the `gitdir:` pointer) with one pointing elsewhere before `review` or `approve` run git inside it. Checking that the pointer still names this repository's `worktrees/<id>` directory before every call is the follow-up.

## #368 Protected-path rule misses /private/var on macOS

I fixed the protected-path rule and the path-tool specifiers so they compare resolved paths. Two
gaps came up in review that were already there before this change, and I left them alone:

- **Case-insensitive filesystems.** The protected-home check (`starts_with`) and the glob match are
  case-sensitive, so on APFS (or any case-insensitive volume) a write to `~/.CONFIG/AIBUS/x` that
  creates new directories, or to `/tmp/OUT/x` against a deny on `/tmp/out/**`, isn't caught by the
  rule. Fixing it means folding case per volume (or comparing against what the filesystem reports
  for the existing part of the path), which is its own change.
- **A rule that names a symlinked file.** A rule with no glob that names a symlink to a file, say
  `write(/tmp/link)`, only resolves its directory, never its last component, so it can't meet the
  canonical subject (the link's target). Resolving the last component too would let the link's
  owner move where the rule points, so it needs the same owner check the allow side has.

Scope I cut on purpose, with where it goes. I add a section per piece of work and I do not open issues for these.

## B1.4 desktop shell and workspace layout (#307)

I built the shell, the four workspaces from a list, the shared primitives and tokens, theme, shortcuts, state notices, safe external links and the saved dock layout, all in `desktop/src`. Left for later...

- **The bridge wiring.** The Tauri commands and the `Channel` that carry a `Request` to `aibus_bridge::handle_json` are not added: `desktop/src-tauri` still only has the two spike commands. The Run and Trace workspaces keep reading the fixture replay. It needs a build of `src-tauri` with WebKitGTK, which the main gate does not have.
- **Live tailing and the open-run summary row** (both listed under B1.3) wait on that wiring.
- **An opener command for external links.** `ExternalLink` only accepts plain https URLs and hands them to an injected opener. The default is `window.open` with `noopener,noreferrer`, which a Tauri webview may block. A host command (no shell or opener plugin in the capability) is the proper fix and needs the Rust build too.
- **Native window size and position across restart.** The dock layout, workspace and theme persist in webview storage. The window frame itself needs `tauri-plugin-window-state` (or a small host command), again Rust. `tauri.conf.json` now sets a 640 by 480 minimum so small windows stay usable.
- **Playwright and GIFs.** The suite is vitest in jsdom. Nothing drives the real webview, so layout at small sizes and mixed DPI is untested. Spacing tokens, some font sizes, a 720px media query and the graph SVG size are fixed pixels, so I am not claiming the layout scales.
- **Webview parity on macOS and Linux.** Not run on either (no Tauri build ran). Steps stay in `docs/desktop/VERIFY-MACOS.md`.
- **Keyboard use of graph nodes.** The graph is an `svg` with `role="img"` and click-only nodes, so tool nodes cannot be selected from the keyboard. Making them operable means restructuring the graph for assistive tech, which I left for a follow-up.
- **Replay and Compare** is #314: one more entry in the list in `App.tsx`.

## B1.3 typed bridge (#306)

I landed the Rust half in `crates/aibus-bridge`: versioned DTOs over the `aibus-trace` projections, the run cursor, gap detection and resubscribe, a bounded channel feed, and the generated TypeScript bindings with a contract test. What I left for the desktop shell (#307):

- The Tauri commands and the `Channel` that carry a `Request` to `aibus_bridge::handle_json` and a batch to the UI. `ts/bridge.ts` takes an injected `invoke`, so wiring it is one function there.
- Live tailing. `stream` and `handle` re-read the trace file per call, which is right for a recorded run and for catch-up, but a run still being written wants a watcher that pushes new batches. The cursor protocol does not change.
- A second `RunRow` style summary for a run that is still open (the fleet row is built from the whole file today).
- Running the generated TS through `tsc` and a React consumer in CI: the contract test checks the bindings against the Rust and that `bridge.ts` imports only generated types, and I type-checked `bridge.ts` by hand, but there is no TS toolchain in the workspace yet.
- The "TUI and desktop share state" check from the issue: the bridge reads the same `aibus_trace::project` and `view::fleet` the TUI uses, so there is no second projection, but nothing yet drives both from one running session (that needs the control socket of #274 and the shell).

## #305 B1.2 Shared React components (PR #374)

What #305 closes with: six primitives (`Button`, `TextBox`, `Checkbox`, `FormRow`, `Dialog`, `Tabs`) and
the design tokens in `shared-ui/`, the npm licence gate, and the `consumers/aibus-smoke` consumer that
imports them by package name. What I left for later...

- **Desktop app consumption moves to #307 (B1.4).** `desktop/` has no React frontend yet, so the aibus
  consumer today is the smoke app. B1.4 builds the shell from these primitives and tokens: it adds
  `@sharedui/design-tokens` and `@sharedui/react-primitives` to the desktop `package.json`, imports them
  by name (no copied source), and the smoke app stops being the only consumer.
- **Licence of the token values.** Settled: the owner confirmed MIT on 2026-11-24 (see `shared-ui/LICENSING.md`).
- **Separate semver repo.** The packages live in `shared-ui/` here; `git subtree split --prefix=shared-ui`
  makes the repo once the owner picks its org and name. Publishing the `@sharedui` scope waits on that too.
- **More components.** Card, Badge, Chip, RadioGroup and the rest that `shared-ui/INVENTORY.md` classes as
  reusable. Icons wait on a licence-clean icon set.
- **Data grid package.** TanStack Table plus Virtual, when B1.4 or later has a real use for it.
- **Webapp adoption.** Moving the webapp onto the packages is a webapp change and needs paulotten's review.
  The local `scripts/webapp-consumer-check.mjs` run is the cross-consumer check until then.

## A3.1 extensions (#291), cut on 2026-11-22

- Install, login-state and launch tests against a real Kimi Code, Copilot CLI, Cursor Agent and
  Qwen Code, then replacing the synthetic fixtures with recordings and tightening `cli_versions`.
- Tool failure and error-result mappings, which need each CLI's real failure events.
- A published community fixture pack. The fixtures sit under `examples/extensions/<name>/fixtures`
  for now, and where to publish them (repo, the A2.1 catalog or a release asset) is still open.
- Building anything for xAI/Grok or DeepSeek: both have a first-party CLI (Grok Build, and DeepSeek Harness `dsh` with `--profile headless/acp/sdk`, read 2026-11-25) and both stay `disabled` until their terms and login flows are read. See `docs/PROVIDER-FEASIBILITY.md`.
- Host policy for the new extensions: `sandbox::OFFICIAL_CLIS` only lists claude, codex, gemini and kimi, so `copilot`, `cursor-agent` and `qwen` are refused with `NotOfficial` until the user adds them to `extra_official_clis`, and `flags.rs` has no per-CLI flag list for them, so once added only the denylist checks their argv. Adding them to the allowlist with flag lists is a core edit that waits until a real CLI of each has been read.

## A3.2 OpenTelemetry export (#292)

Cut from the reduced scope, still wanted:

- A retention policy (per-project retention and deletion of local traces). Export redaction is in; retention is not.
- Reading `Destination` and `Revisions` from the aibus config file and a CLI switch. Today they are plain structs a caller fills in.
- Langfuse interop. The OTLP body follows the generic OTLP/HTTP shape, but I have not run it against Langfuse, so that row is unverified. Langfuse-specific attribute mapping (`langfuse.*`) is not done.
- Wiring the completeness diagnostics into the trace inspector.
- Scheduler decisions and provider sessions as their own spans (route decisions are events on the attempt span for now).
- A bounded queue between the run and the exporter. Each export is one thread (the caller writes the `thread::spawn`), so a slow collector under many exports would pile threads up; a bounded queue with drop-on-full belongs with the config wiring.
- Model and version labels on the resource (spec 7.2). Only the provider extension is exported today.

## H1.5 per-agent track record (#392)

- **Evals result type.** PR #398 (branch `feat/293-evals`, #293 / A3.3) is open now and has `EvaluationResult` in `crates/aibus-trace/src/eval.rs`, but I wrote the `agent eval` scorecard before it and did not merge the two: the scorecard is its own minimal JSON (`scorecard_version` as a string, `agent`, `summary`, `cases[]` with `result`, `reason` and the two exit codes) and `EvaluationResult` is one row per case with `result_version` as a `u32`, a verdict (pass, fail, incomplete), checks and a run context. They should converge later: each case row becomes an `EvaluationResult` with `done_when` as a deterministic check, and the scorecard keeps only the summary, with the version types lined up. It is a different crate and a separate command, so it is a follow-up once #398 lands.
- **Cases only judge by exit codes.** A case passes when the run exits 0 and its `done_when` exits 0. Output matching, scoring and cost or turn budgets per case are not there. Cases run in the project directory, not a scratch copy, so a case whose agent writes files leaves them there.
- **Learning extraction uses the lead's model** for workers too, one call per finished worker run (at most four a session). A per-agent extraction model is not wired.
- **Roster suffix is read once at session start.** An agent reload (`reload_on_change`, TUI) rebuilds the roster without it until the next session.
- **`agent stats` reads the session index**, so runs of a session that was killed before it ended are not counted until `aibus session list --rebuild` or the next start rebuilds the index.
