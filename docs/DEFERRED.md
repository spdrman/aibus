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
  one Xvfb run on MARS, not benchmarks. A queued Mercury run (`bench-queue.sh`) can repeat them once
  Phase 2 has a workload worth timing.
- **Shared UI components.** The spike styles its own few elements. #307 swaps them for the shared
  React primitives and tokens from #305 (PR #374) once that merges.
- **Bundling and signing.** `bundle.active` is false. AppImage/deb, `.app`/`.dmg`, notarisation and
  updater keys are release work, not spike work.
