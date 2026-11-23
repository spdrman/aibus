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

Scope I cut on purpose, with where it goes. I add a section per piece of work and I do not open issues for these.

## B1.3 typed bridge (#306)

I landed the Rust half in `crates/aibus-bridge`: versioned DTOs over the `aibus-trace` projections, the run cursor, gap detection and resubscribe, a bounded channel feed, and the generated TypeScript bindings with a contract test. What I left for the desktop shell (#307):

- The Tauri commands and the `Channel` that carry a `Request` to `aibus_bridge::handle_json` and a batch to the UI. `ts/bridge.ts` takes an injected `invoke`, so wiring it is one function there.
- Live tailing. `stream` and `handle` re-read the trace file per call, which is right for a recorded run and for catch-up, but a run still being written wants a watcher that pushes new batches. The cursor protocol does not change.
- A second `RunRow` style summary for a run that is still open (the fleet row is built from the whole file today).
- Running the generated TS through `tsc` and a React consumer in CI: the contract test checks the bindings against the Rust and that `bridge.ts` imports only generated types, and I type-checked `bridge.ts` by hand, but there is no TS toolchain in the workspace yet.
- The "TUI and desktop share state" check from the issue: the bridge reads the same `aibus_trace::project` and `view::fleet` the TUI uses, so there is no second projection, but nothing yet drives both from one running session (that needs the control socket of #274 and the shell).
