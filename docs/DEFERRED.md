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
