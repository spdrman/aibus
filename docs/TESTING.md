# Testing

Tests and compiles run in a container (see `docs/BUILD.md`), never on a host. This file is about
the TUI snapshots, because they are the part that needs a routine.

## Frame snapshots

A frame snapshot is the text of a `TestBackend` buffer, one row per line with the trailing blanks
trimmed, compared with a file in `crates/aibus-tui/tests/snapshots/`. A second file next to it,
`<name>.styles.txt`, holds the `fg`, `bg` and modifier runs per row, so a colour regression fails
too. The animation is driven by the tick counter the test injects, so a frame is the same on every
run and every machine.

The helpers are `assert_frame!` and `assert_styles!` in `aibus-testkit`
(`crates/aibus-testkit/src/tui.rs`), and `Rig` in `crates/aibus-tui/tests/common/mod.rs` for
frames that come out of synthetic key and mouse events.

The spec asks for `cargo insta`. I did not add it: the checker above was already in place from
C1.1 and does the same job (a named text file per frame, rewritten on request, reviewed as a diff)
without a new dependency or a second review tool. The workflow is the same shape:

```
# 1. run the tests; a changed frame fails and prints the old and the new one
cargo test -p aibus-tui

# 2. when the change is on purpose, rewrite the files
AIBUS_UPDATE_SNAPSHOTS=1 cargo test -p aibus-tui

# 3. read the diff before committing it
git diff crates/aibus-tui/tests/snapshots
```

Review the diff like code. A cursor that moved, a border that changed colour or a column that
shifted is a real change. A snapshot that is missing fails with a message that names the file; the
first run with `AIBUS_UPDATE_SNAPSHOTS=1` creates it.

## What is covered

* `c13_frames.rs`: f01, f02, f03, f04, f09 and f21 at 98x46, the style runs of f02 and f04 (cards,
  fork rows, status bar), the 80x24 collapsed rail, 120x50 with the ledger, `ascii = true`, `mono`,
  the permission modal, the transcript collapsed and expanded, the agents screen.
* `c13_interact.rs`: what keys and the mouse do, with each button tested against its key.
* `snapshots_dashboard.rs`, `snapshots_states.rs`, `shell.rs`, `dashboard_widgets.rs`: the
  dashboard and its interaction states from C1.1 and C1.2.

## Contrast

`crates/aibus-tui/tests/theme_contrast.rs` measures every text and background pair of the themes
and pins the number. A palette change shows up there as a failing ratio.
