# Deferred scope

Work cut from an issue's reduced definition of done, kept here so it is not lost. Each section names
the issue it came from and what picks it up.

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
- **Shared UI components.** The spike styles its own few elements. #307 swaps them for the shared
  React primitives and tokens from #305 (PR #374) once that merges.
- **Bundling and signing.** `bundle.active` is false. AppImage/deb, `.app`/`.dmg`, notarisation and
  updater keys are release work, not spike work.

# Deferred work

Scope that was cut from an issue on its way to closing, with the reason and what picks it up. One
section per issue.

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

## B1.3 typed bridge (#306)

I landed the Rust half in `crates/aibus-bridge`: versioned DTOs over the `aibus-trace` projections, the run cursor, gap detection and resubscribe, a bounded channel feed, and the generated TypeScript bindings with a contract test. What I left for the desktop shell (#307):

- The Tauri commands and the `Channel` that carry a `Request` to `aibus_bridge::handle_json` and a batch to the UI. `ts/bridge.ts` takes an injected `invoke`, so wiring it is one function there.
- Live tailing. `stream` and `handle` re-read the trace file per call, which is right for a recorded run and for catch-up, but a run still being written wants a watcher that pushes new batches. The cursor protocol does not change.
- A second `RunRow` style summary for a run that is still open (the fleet row is built from the whole file today).
- Running the generated TS through `tsc` and a React consumer in CI: the contract test checks the bindings against the Rust and that `bridge.ts` imports only generated types, and I type-checked `bridge.ts` by hand, but there is no TS toolchain in the workspace yet.
- The "TUI and desktop share state" check from the issue: the bridge reads the same `aibus_trace::project` and `view::fleet` the TUI uses, so there is no second projection, but nothing yet drives both from one running session (that needs the control socket of #274 and the shell).
