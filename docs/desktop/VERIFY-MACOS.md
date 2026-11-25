# Verifying the B1.1 desktop spike on macOS

I built and ran the spike on Linux only (docker, under Xvfb). Nobody has built or opened it on a Mac
yet, so everything macOS-specific in [B1.1-go-no-go.md](B1.1-go-no-go.md) is unverified until these
steps are done on the MacBook by the owner... It takes about ten minutes plus the first Rust build.

Nothing here needs a provider login, an API key or the network beyond npm and crates.io. The app only
replays the recorded fixture compiled into it.

## Prerequisites

- Xcode command line tools (`xcode-select --install`). WKWebView ships with macOS, so there is no
  webview package to install.
- Rust 1.90 through rustup. `desktop/src-tauri/rust-toolchain.toml` pins it, so rustup fetches it the
  first time cargo runs in that folder. The repository workspace stays on 1.89.
- Node 22 (or 20+), with npm.

This is a deliberate exception to the "no host toolchain on the Mac" habit: a macOS desktop build
cannot run in a Linux container, so it has to be native. Keep it to this folder.

## Steps

```sh
git fetch origin && git switch feat/304-tauri-spike   # or main once #304 merges
cd desktop
npm ci                                  # .npmrc sets ignore-scripts, so no install scripts run
npx tsc --noEmit && npx vitest run      # same frontend gates as Linux
(cd src-tauri && cargo test --locked)   # fixture shape, provenance, IPC boundary, golden snapshot
npx tauri build --no-bundle -- --locked
./src-tauri/target/release/aibus-desktop
```

`npx tauri dev` also works for poking at the UI with hot reload.

## What to check and write down

1. The window opens titled "aibus Desktop".
2. The yellow banner says **Fixture** with "Recorded fixture, not a live run" and the step counter
   climbs to its total in about 11 seconds.
3. The Graph panel ends with one `lead` agent, two subagents (`reader-a`, `reader-b`), a `Read` tool
   under each, and blue delegation links from the lead to both.
4. The Timeline panel lists the normalised events and scrolls as they arrive; clicking a row or a node
   highlights the same agent in the other panels and fills Details.
5. The banner's right side names the Tauri version, `macos/aarch64` and the WebKit version.
6. Switch System Settings > Appearance between Light and Dark: the panels, banner and tabs follow.
7. Dockview: drag a tab to split, resize a sash, and check nothing renders blank or flickers.
8. Measurements, while the app is idle after the replay:

   ```sh
   ps -o pid,rss,%cpu,comm -p $(pgrep -f aibus-desktop) ; \
   ps -o pid,rss,%cpu,comm -p $(pgrep -f 'com.apple.WebKit') 
   ls -l src-tauri/target/release/aibus-desktop
   ```

   Record RSS of the app plus its WebKit WebContent and Networking processes, CPU over 30 s, binary
   size and how long the first `cargo build` took.

## Recording the result

Add a "macOS (MacBook, YYYY-MM-DD)" section to [B1.1-go-no-go.md](B1.1-go-no-go.md) with the macOS
version, chip, WebKit version, the numbers from step 8, and any rendering or input difference from
the Linux run (fonts, scrolling, dockview drag, dark mode). If any step fails, open a bug with the
four headings and link it from that section. B1.5 (#308) reads that section before it confirms the
go/no-go.
