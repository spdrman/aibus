# Deferred work

Scope I cut on purpose, with where it goes. I add a section per piece of work and I do not open issues for these.

## B1.3 typed bridge (#306)

I landed the Rust half in `crates/aibus-bridge`: versioned DTOs over the `aibus-trace` projections, the run cursor, gap detection and resubscribe, a bounded channel feed, and the generated TypeScript bindings with a contract test. What I left for the desktop shell (#307):

- The Tauri commands and the `Channel` that carry a `Request` to `aibus_bridge::handle_json` and a batch to the UI. `ts/bridge.ts` takes an injected `invoke`, so wiring it is one function there.
- Live tailing. `stream` and `handle` re-read the trace file per call, which is right for a recorded run and for catch-up, but a run still being written wants a watcher that pushes new batches. The cursor protocol does not change.
- A second `RunRow` style summary for a run that is still open (the fleet row is built from the whole file today).
- Running the generated TS through `tsc` and a React consumer in CI: the contract test checks the bindings against the Rust and that `bridge.ts` imports only generated types, and I type-checked `bridge.ts` by hand, but there is no TS toolchain in the workspace yet.
- The "TUI and desktop share state" check from the issue: the bridge reads the same `aibus_trace::project` and `view::fleet` the TUI uses, so there is no second projection, but nothing yet drives both from one running session (that needs the control socket of #274 and the shell).
