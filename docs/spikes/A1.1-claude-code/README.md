# A1.1 spike: Claude Code runtime integration and policy feasibility (#281)

This is the spike for #281, the hard gate of EPIC A (#280). I ran it on 2026-11-14 on the owner's Mac against the Claude Code CLI the owner already had installed and signed in, with no change to that install. Everything here is either something I saw in a captured run (and the fixture is linked), something Anthropic's own docs or terms say (quoted and dated), or marked as my inference. The go/no-go is ratified at #285, not here...

| File | What's in it |
|---|---|
| [evidence.md](evidence.md) | What the stream and hooks actually expose, each item labelled observed / correlated / inferred / unavailable, plus the parent/child edge proof |
| [subagents-vs-teams.md](subagents-vs-teams.md) | Normal subagents vs native agent teams, and the event/control paths that don't exist |
| [terms-gate.md](terms-gate.md) | The dated terms/distribution gate |
| [runs.md](runs.md) | Every CLI invocation with its flags, the harness, and the fixture manifest |
| [`fixtures/claude-code/spike-281/`](../../../fixtures/claude-code/spike-281/) | The captured runs (sanitized paths only, otherwise byte for byte) |

## Recommendation: GO-WITH-CONDITIONS

Technically it works, and better than the spec feared. One unmodified `claude -p --output-format stream-json --verbose` process, started from a tokio `Command` with an argv vector, a validated cwd and a cleared environment, delegated to two native subagents, each of which ran a `Read` tool, and the lead returned a final result (`11-foreground`). Every parent/child edge in that run is backed by explicit native IDs in the stream (`tool_use.id` -> `task_started.tool_use_id` -> child messages' `parent_tool_use_id` and `agent_id`), and a second independent channel (command hooks) carries the same `agent_id` and `tool_use_id`s, so the edges are both observed and cross-correlated. I didn't need the aibus-managed fallback, so there's no fallback experiment; nothing aibus-managed is labelled native anywhere.

Privacy is fine with conditions: aibus never touched a credential, `auth status --json` contains no token (it does carry email and org name, which have to be redacted), and the stream itself never carried account identity. Hook payloads and child messages do carry file contents and prompts, so payload capture has to stay opt-in.

Policy is where the conditions bite. Open-source local use where the user runs their own unmodified `claude` is the closest documented fit, but for **subscription (claude.ai OAuth) sign-in** Anthropic's documents pull in two directions (details in [terms-gate.md](terms-gate.md)). With **API key / Console sign-in** it's the documented developer path. Bundled, preinstalled or hosted use needs the Commercial Terms plus explicit conditions, and **commercial release stays blocked** as #280 already decided.

### Conditions

1. **Owner decision on subscription sign-in (needs the owner).** Before aibus documents or advertises running Claude Code on a Pro/Max/Ultra subscription, the owner either gets Anthropic's confirmation (the legal page points to sales) or accepts the ambiguity explicitly. Until then I'd have aibus show the CLI's own `authMethod` and say plainly that subscription usage through a third-party tool may be treated differently by Anthropic (the support article says it may be drawn from usage credits).
2. **Naming (needs the owner).** Anthropic's legal page says you "can't use the Claude Code or Anthropic names ... as part of your own product, feature, or company name". The spec's extension id `org.aibus.claude-code` and package `aibus-claude-code` probably fall foul of that. Plain-text "runs Claude Code" is fine. #286 needs a different id.
3. **Commercial, bundled and hosted release stay blocked** pending the review the spec asks for. Nothing in this spike is distribution clearance.
4. **Pin and probe versions.** I tested 2.1.311 only. Optional flags have minimum versions (`--forward-subagent-text` needs 2.1.211, the `--agents` file form 2.1.281) and the headless docs say `--bare` "will become the default for `-p` in a future release". Bare mode never reads OAuth or the keychain, so that change would silently break subscription users. The adapter has to probe every version and fail loudly, never fall back.
5. **Bounded environment must pass `CLAUDE_CONFIG_DIR` and drop the parent session's `CLAUDECODE` / `CLAUDE_CODE_*` variables.** The owner's real install lives in `~/.claude-b` (via `CLAUDE_CONFIG_DIR`). A naive `env_clear()` would have pointed the CLI at the wrong config dir.
6. **Payloads stay local and opt-in**, with the redaction rules from spec 10.3, because hook and stream payloads carry file contents, prompts and absolute paths.

## Technical capability, in short

| Spec ask | Result | Fixture |
|---|---|---|
| Discover the genuine executable, version, flags, `auth status` | Done. Three `claude` candidates on this Mac, PATH order picks a Developer ID signed (Anthropic PBC, Q6L2SF6YDW) native binary, 2.1.311. Discovery can't trust the first PATH hit blindly | `01`..`05` |
| One headless prompt via `tokio::process::Command`, streaming plus normal completion | Done, 11 events, exit 0, 2.2 s | `06-basic` |
| Genuine parent delegation to two native subagents, IDs and hooks | Done, both background and foreground variants | `07-deleg`, `11-foreground` |
| Normal subagents vs native agent teams | Teams don't spawn under `-p` (documented, and observed: a "team" request ran as two ordinary subagents) | `12`..`14` |
| Dated terms/distribution gate | Done, commercial marked blocked | [terms-gate.md](terms-gate.md) |
| Failure shapes for #282 | Permission denial, killed mid-run (truncated stream), multi-`result` background run | `09`, `10`, `07`/`08` |

## Things the next issues need to know

These change how #282 onwards should be built. The details are in [evidence.md](evidence.md).

- **`result` isn't terminal.** With background subagents (the default when nothing is said) the lead ends its turn early, gets woken up by each completion, and one process prints two or three `result` events (`07`, `08`). The attempt ends at process exit, and the last `result` before exit is the real one.
- **No timestamps on system and hook events.** Only `assistant`/`user` messages carry `timestamp`. `observed_at` has to come from aibus.
- **Tool name drift inside one stream.** `system/init` lists the delegation tool as `Task` while the model calls it `Agent`.
- **Children are in-process.** Sampling `ps` every 250 ms during `08` showed one `claude` process for the lead and both children. aibus can't signal a single native child; killing the process kills them all (`10`, no orphan left).
- **The host can't force foreground subagents.** `run_in_background` is a field the model fills in. Asking in the prompt worked (`11`), but that's a request, not a control.
- **Costs are estimates.** `total_cost_usd` comes with `costBasis: "list"`. For a subscription user that's a list-price estimate, not a bill, so it's `estimated` under invariant 9.

## What changes downstream

- **#282 supervisor**: the attempt ends at process exit, not at the first `result`; aibus stamps `observed_at` itself; normalise `Task`/`Agent`; keep unknown variants; key child messages on `agent_id`; a killed run is `interrupted`, never `completed`. The env allowlist needs `CLAUDE_CONFIG_DIR`. Hooks are a second evidence channel, but turning them on means config: the spike used a scratch project `.claude/settings.json`, and the inline `--settings <json>` route (no file written) is untested. Discovery has to cope with several installs. The fixtures here start its corpus.
- **#283 compiler**: inline `--agents` JSON compiles workflow roles to native subagents without writing anything into the user's `.claude/`, which is the cheapest way to satisfy "no silent overwrite". Foreground or background can only go in the prompt.
- **#284 graph and inspector**: the edge chain and the unavailable list in [evidence.md](evidence.md) are the spec for evidence labels. Cost shows as `estimated` (`costBasis: list`); per-child usage is vendor-reported.
- **#285 gate**: ratifies this go/no-go and the two owner questions; `11-foreground` is the reference case for native delegation.
- **#286 extension SDK**: the extension id and package name can't contain "Claude Code"; capability probes have to read version minimums from the docs (`--max-turns` isn't in `--help` at all), and a future `--bare` default for `-p` has to be detected.
- **#288 Jev and approvals**: `--permission-prompts none` gives a clean auto-deny with a `permission_denied` event, a safe default. Interactive approval through the host (`--permission-prompts host`) is untested.
- **#289 durable state**: native children die with their process and can't be resumed by aibus; only aibus-managed children can be retried one at a time. The Causl durability check probably lands here or in #282.
- **#290 worktrees**: under `-p` the CLI runs a folder's project hooks and `.mcp.json` without a trust prompt, so a worktree's `.claude/` is untrusted input. The CLI has its own `-w/--worktree`; aibus should own worktrees and not use it.
- **#291 provider register**: [terms-gate.md](terms-gate.md) is a template for the other providers' dated entries.
- **#292 OTel**: there are no native timestamps to export, only aibus-side ones; the CLI's own telemetry is separate and out of aibus's hands.
- **#295 release gate**: commercial, bundled and hosted stay blocked; terms get re-checked at release (the Usage Policy changed on 2026-11-12).
- **#287, #293, #294**: nothing here changes them beyond the evidence labels #294 inherits from #284.

## What I didn't do

- I didn't read any transcript. Hooks report `transcript_path` and `agent_transcript_path` under the owner's config dir; reading them would mean touching `~/.claude*`, which this spike doesn't do. Whether `--no-session-persistence` really kept them off disk, I don't know.
- I didn't test `--input-format stream-json`, `--permission-prompts host` or a permission-prompt tool, so interactive approval is untested, not unavailable.
- I didn't do the Causl durability check from spec 7.4. #281 doesn't scope it and #280 doesn't settle where it lives; I'd put it in #282 or #289. That's an owner call.
- I didn't add any Rust to the repo. The harness lived in a scratch crate; its source is in [runs.md](runs.md) so #282 can start from it.
