# Release checklist

Everything a test can check, a test checks. This is the rest (10 section 10): the things that
need a real keychain, a real terminal or a real OS. I go through it before a tag, on the machines
I have, and write what I could not try into the release notes instead of ticking it.

Before this list, the automatic part has to be green: `cargo build`, `cargo test`, `cargo clippy`
and `cargo deny check` as in `docs/BUILD.md`, the nightly live provider run, the benches and the
fuzz targets.

## Keychain, per OS

The credential store is `keyring`; the tests use an in-memory keychain. On each OS, with a fresh
user and no `AIBUS_*` key variables set:

* [ ] `aibus doctor` names the keychain backend it found, with no warning.
* [ ] `printf %s "$KEY" | aibus account add smoke --provider anthropic --key-stdin` succeeds, and
      `aibus account show smoke` prints the key reference and never the key.
* [ ] The key is in the OS store, found by hand under the service name `aibus`:
      macOS Keychain Access, Windows Credential Manager, Linux Secret Service
      (`secret-tool search service aibus`).
* [ ] `aibus account test smoke` reaches the provider. A wrong key reports an auth error.
* [ ] `aibus run --agent explore "hello"` works with the key coming from the store.
* [ ] Nothing under `~/.config/aibus`, `~/.local/share/aibus`, `~/.local/state/aibus` or
      `~/.cache/aibus` contains the key: `grep -r "$KEY" ~/.config/aibus ~/.local/share/aibus
      ~/.local/state/aibus ~/.cache/aibus` finds nothing.
* [ ] `aibus account remove smoke` removes the entry from the OS store.
* [ ] Linux without a running Secret Service: `aibus doctor` says so, and `--env` or `--file`
      accounts still work.
* [ ] macOS: the first access prompts once, "Always Allow" sticks across a binary update.

## Terminal emulators

For each of iTerm2, Terminal.app, kitty, WezTerm and Windows Terminal, run `aibus` in a real
window, then go through the list. Unix terminals on macOS and Linux, Windows Terminal on Windows
(10 and 11).

* [ ] The dashboard draws at 98x46 and at 80x24 without wrapping or stray cells, and the border
      glyphs are the box-drawing ones (or the ASCII set with `ascii = true`).
* [ ] The colours match the recording: the purple of `same_error_x2`, the green and red of the
      status bar. In a 256-colour terminal (`TERM=xterm-256color`) the fallback palette is readable.
* [ ] Keys: every binding in the key map (`aibus-tui/src/keys.rs`, shown in the help screen) reaches
      the app, in particular `Ctrl-C` (cancel), `Ctrl-B` (background a wait), `Esc` and `Tab`.
      Note any the terminal eats (kitty and WezTerm bind some `Ctrl-Shift` chords).
* [ ] Mouse: click selects a card, the wheel scrolls the focused pane, the right click opens the
      card menu, and a drag selects text without breaking the layout.
* [ ] Resize while a run streams: the layout reflows with no tearing, and the 80x24 collapsed rail
      comes and goes at the width the spec says.
* [ ] Paste of several lines into the prompt arrives as one paste (bracketed paste), not as
      submitted lines.
* [ ] Unicode: a streamed line with accents, CJK and an emoji keeps its alignment.
* [ ] Quitting restores the terminal: the cursor is back, the alternate screen is left, the mouse
      mode is off.
* [ ] `NO_COLOR=1` gives plain text, and `aibus run` with a piped stdout is a stream, never a TUI.
* [ ] Windows Terminal only: `aibus run` from PowerShell and cmd, and the key reading when the
      terminal is a tab in a split.

## Windows (unverified so far)

I have no Windows machine at hand, so nothing in this section has been tried. The release workflow
builds the `x86_64-pc-windows-msvc` binary, but a build is not a smoke run. On Windows 10 and 11:

* [ ] The binary starts and `aibus doctor` passes. It names the keychain backend as Windows
      Credential Manager (the `keyring` crate's `windows-native` feature).
* [ ] The `account add` / `account show` / `account remove` steps of the keychain list above work,
      and the entry shows up in Credential Manager (service `aibus`, user `account:smoke`).
* [ ] The TUI smoke-renders in Windows Terminal (not a blocker for the tag, Q21): dashboard,
      resize, quit restores the terminal.
* [ ] `aibus version` prints the tag and commit of the release.

Same for macOS arm64/x86_64 and Linux aarch64: the workflow builds them, I smoke-run the ones I
have a machine for, and the rest go in the release notes as not tried.

## Release pipeline

`.github/workflows/release.yml` runs on a pushed `v*` tag, and by hand (`workflow_dispatch`) as a
dry run that builds and checksums but signs and publishes nothing. On a tag it:

1. builds `cargo build --release --locked -p aibus` for aarch64 and x86_64 macOS, x86_64 and
   aarch64 Linux (glibc), x86_64 Linux musl and x86_64 Windows, and packs each as `.tar.gz`
   (`.zip` on Windows). `AIBUS_TAG` and `AIBUS_COMMIT` are set so `aibus version` prints both;
2. writes a CycloneDX SBOM with `cargo cyclonedx` and `SHA256SUMS` for every file;
3. signs every file and `SHA256SUMS` with `cosign sign-blob` keyless (a `.sigstore.json` bundle
   next to each), using the job's OIDC token, so no key or secret is stored;
4. creates the GitHub release with all of it attached.

Only the last job has `id-token: write` and `contents: write`. Every action is pinned to a full
commit SHA. The workflow has never run: the runner labels, the `cargo cyclonedx` flags and the
cosign bundle step are unverified until the first dry run.

Check a download with `cosign verify-blob --bundle aibus-<target>.tar.gz.sigstore.json
--certificate-identity-regexp 'https://github.com/spdrman/aibus/' --certificate-oidc-issuer
https://token.actions.githubusercontent.com aibus-<target>.tar.gz` and `sha256sum -c SHA256SUMS`.

### Tag procedure

* [ ] `main` is green locally (the automatic part above) and the dry run of the workflow is green.
* [ ] `version` in the root `Cargo.toml` is the release version, and `Cargo.lock` is committed.
* [ ] `git tag -s vX.Y.Z` on that commit and `git push origin vX.Y.Z`. The tag is the only trigger.
* [ ] When the workflow finishes: verify one archive per OS with the command above, and run
      `aibus version` (it prints `vX.Y.Z` and the commit).
* [ ] Fill in the Homebrew formula and publish to crates.io (next section).

### Changelog note rule for MSRV bumps

Raising `rust-version` (and `rust-toolchain.toml`) always gets a line in the release notes under
"Build", naming the old and new version and why. A release that bumps it says so in the tag
message too. A bump never rides in a patch release.

### Homebrew and crates.io

* Homebrew: `packaging/homebrew/aibus.rb` is the template, with placeholder checksums. The tap
  repository (`spdrman/homebrew-aibus`) is created by the owner. After the release, copy the file to
  `Formula/aibus.rb` in the tap, set `version`, and put in the `SHA256SUMS` lines.
  [ ] On a clean macOS or Linux machine: `brew install spdrman/aibus/aibus`, then `aibus version`.
* crates.io: the workspace sets `publish = false`, so `cargo install aibus` does not work yet. When
  I decide to publish, `aibus` needs its path dependencies published first (or a single-crate
  package), `publish` turned on, and a crates.io token. None of that is done.
  [ ] On a clean machine: `cargo install aibus --locked`, then `aibus version`. A crates.io build
      has no git checkout, so it prints `commit unknown`, which is expected.
* Record the result of each clean-machine run (OS, date, outcome) in the release notes.

### Advisories

`.github/workflows/audit.yml` runs `cargo audit` on the lockfile and opens an issue (or comments on
the open one) when it finds an advisory. It is `workflow_dispatch` only: no schedule, in line with
the rest of the workflows. I start it before every tag and whenever I touch `Cargo.lock`. `cargo
deny check` in the local gate covers the same advisory database.

## Last

* [ ] The version in `Cargo.toml` and in `aibus version` agree with the tag.
* [ ] `docs/BUILD.md` is current for any new build-time dependency.
* [ ] The release notes list every box above that I did not tick, and why.
