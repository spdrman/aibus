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

# Deferred work

Scope I cut on purpose, with where it goes. I add a section per piece of work and I do not open issues for these.

## B1.3 typed bridge (#306)

I landed the Rust half in `crates/aibus-bridge`: versioned DTOs over the `aibus-trace` projections, the run cursor, gap detection and resubscribe, a bounded channel feed, and the generated TypeScript bindings with a contract test. What I left for the desktop shell (#307):

- The Tauri commands and the `Channel` that carry a `Request` to `aibus_bridge::handle_json` and a batch to the UI. `ts/bridge.ts` takes an injected `invoke`, so wiring it is one function there.
- Live tailing. `stream` and `handle` re-read the trace file per call, which is right for a recorded run and for catch-up, but a run still being written wants a watcher that pushes new batches. The cursor protocol does not change.
- A second `RunRow` style summary for a run that is still open (the fleet row is built from the whole file today).
- Running the generated TS through `tsc` and a React consumer in CI: the contract test checks the bindings against the Rust and that `bridge.ts` imports only generated types, and I type-checked `bridge.ts` by hand, but there is no TS toolchain in the workspace yet.
- The "TUI and desktop share state" check from the issue: the bridge reads the same `aibus_trace::project` and `view::fleet` the TUI uses, so there is no second projection, but nothing yet drives both from one running session (that needs the control socket of #274 and the shell).
