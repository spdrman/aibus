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

## Last

* [ ] The version in `Cargo.toml` and in `aibus version` agree with the tag.
* [ ] `docs/BUILD.md` is current for any new build-time dependency.
* [ ] The release notes list every box above that I did not tick, and why.
