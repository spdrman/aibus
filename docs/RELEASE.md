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
* [ ] Cards (with more than three runs): the wheel over the cards scrolls, Shift plus the wheel pans
      the row sideways, and a drag from empty space pans it. The cards canvas pans with eight runs.
      The centre column is drawn by rataflow in every build.
* [ ] Resize while a run streams, once with eight runs so the cards canvas is wider than the column:
      the layout reflows with no tearing, and the 80x24 collapsed rail comes and goes at the width
      the spec says.
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

A merge into the `release` branch is the whole release. `.github/workflows/release.yml` runs on a
push to `release`, and by hand (`workflow_dispatch`) as a dry run. The tag-triggered version is
gone, so pushing a tag does nothing.

On a push to `release` it runs these jobs, each one only if the ones before it succeeded:

1. `prepare` reads the version from `[workspace.package]` in `Cargo.toml`, makes the tag
   `v<version>`, and stops if the tag already exists, if `CHANGELOG.md` has no entry for the
   version, if the release commit is not a descendant of the tip of `main`, if `install.sh`,
   `install.ps1` or `packaging/homebrew/render-formula.sh` is missing, or if the release commit has
   no valid signed attestation (see "The release attestation" below).
2. `build` runs `cargo build --release --locked -p aibus` on six runners (aarch64 and x86_64 macOS,
   x86_64 and aarch64 Linux glibc, x86_64 Linux musl, x86_64 Windows) with `AIBUS_TAG` and
   `AIBUS_COMMIT` set, runs the binary once (`aibus version` must show the commit, and the tag on a
   release), and packs `aibus-<target>.tar.gz` (`.zip` on Windows) holding `aibus-<target>/` with
   the binary, `LICENSE` and `README.md`.
3. `package` (read-only) writes the CycloneDX SBOM `aibus.cdx.json`, copies `install.sh` and
   `install.ps1` from the repo root, and writes `SHA256SUMS` over all of it.
4. `publish` (with `finalize`, the only job with `contents: write`, and the only one with
   `id-token: write`) signs every file with `cosign sign-blob` keyless (a `*.sigstore.json` bundle
   each), then creates the tag at the release commit and the GitHub release with the CHANGELOG
   section as notes (`gh release create --target`).
5. `verify` downloads the published release and checks that the tag points at the release commit,
   that exactly the expected assets are there, that `SHA256SUMS` covers and matches them, that every
   bundle verifies against this workflow's identity
   (`https://github.com/spdrman/aibus/.github/workflows/release.yml@refs/heads/release`), and that
   `install.sh` from the release installs a working `aibus` on a Linux runner whose `aibus version`
   prints the tag and commit.
6. `finalize` runs only if all of the above succeeded. It renders `Formula/aibus.rb` from the
   published `SHA256SUMS` (`packaging/homebrew/render-formula.sh <version> <SHA256SUMS>`), commits it
   to `release`, and merges `release` into `main` with a merge commit. It refuses if `release` or
   `main` moved during the run. If anything earlier failed it does not run, `main` is untouched,
   and the `summary` job writes each job's result to the run summary.

The dry run (`gh workflow run release.yml --ref <branch>`) runs `prepare` (problems become
warnings), `build` and `package`. `publish`, `verify`, `finalize` and `summary` carry the
push-to-`release` condition on the job itself, so a dispatch skips them whatever the ref is: it
never tags, signs, publishes or merges. A dry run reports a missing or invalid attestation as a
warning too, so a branch without one still builds.

Pushes made with `GITHUB_TOKEN` (the formula commit, the merge into main) do not start new runs, so
there is no loop. If that ever changed, the second run would stop in `prepare` because the tag
exists. Every action is pinned to a full commit SHA, and `persist-credentials` is on only in
`finalize`. `main` has no branch protection, which `finalize` relies on for its push; if protection
is added, `finalize` fails closed at the push and the merge is done by hand.

Check a download with `cosign verify-blob --bundle aibus-<target>.tar.gz.sigstore.json
--certificate-identity https://github.com/spdrman/aibus/.github/workflows/release.yml@refs/heads/release
--certificate-oidc-issuer https://token.actions.githubusercontent.com aibus-<target>.tar.gz` and
`sha256sum -c SHA256SUMS`.

Not proven by a dry run: signing, publishing, `verify` and `finalize` only run on the real push, so
the first release is their first run. The keyless bundle format, the `install.sh` run and the
formula render are the parts most likely to need a fix on that run.

### The release attestation

Nothing on Actions runs the suites, so the proof that they ran is a signed file I make before the
push. `tools/release-attest/attest.sh` archives `HEAD`, pushes it to a docker host with `dh.sh`
and runs the "every push" rows of `docs/TESTING.md` in one fenced container (memory limit, swap
equal to it so none, CPU quota; `DH_MEMORY` and `DH_CPUS` override the defaults, the host is
`--host`, default `mercury`). It refuses a dirty tree, a missing key, an existing attestation
(without `--replace`) and a red run. When it is green it writes `release-attest/attestation.json`
(commit, tree hash, version, per-suite exit and counts) and a detached `ssh-keygen -Y sign`
signature next to it. Benchmarks, fuzzing, the live provider run and the `aibus-graph-causl`
backend are not part of it.

```
tools/release-attest/attest.sh --key ~/.ssh/aibus-release-attest     # on the release candidate, tree clean
git add release-attest && git commit -m "Attest the suites for <version>"
tools/release-attest/verify.sh --expect-version <version>            # the same check `prepare` makes
```

The tree hash leaves out `release-attest/`, so committing the two files does not invalidate them,
and any other change after the run does (the verifier says "the code changed after the run").
`verify.sh` checks the signature against `packaging/release-attest.pub`, that the tree hash is this
commit's, that the version is the one `Cargo.toml` declares, and that every suite is present, exited
0 and passed cells. `prepare` runs it on the release commit with the key read from `main`, so the
push cannot bring its own. It proves the key holder signed a green run of this tree; it cannot stop
someone with write access editing the script or the workflow in the same push.

The signing key is `ssh-ed25519`, kept at `~/.ssh/aibus-release-attest` (private, never committed)
with the public half in `packaging/release-attest.pub`. Until I provision it that file is a
placeholder, and `verify.sh` refuses it, so a real push to `release` stops in `prepare`. Tests for
the tooling (throwaway keys, a fake runner, no container) are `bash tests/release-attest/run.sh`.

### Release procedure

* [ ] `main` is green locally (the automatic part above) and a dry run of the workflow is green.
* [ ] `version` in the root `Cargo.toml` is the release version, `Cargo.lock` is committed, and
      `CHANGELOG.md` has the entry for it.
* [ ] `tools/release-attest/attest.sh` is green on the commit that goes to `release`, the two files
      are committed, and `tools/release-attest/verify.sh --expect-version <version>` says ok. The
      merge into `release` must leave the same tree (fast-forward, or a merge that changes nothing).
* [ ] Open a pull request from `main` (or a branch of it) into `release`, and merge it. The merge
      is the trigger. If `release` does not exist yet, create it at the tip of `main` once.
* [ ] Watch the run (`gh run watch`). The run summary says which job stopped it, if any.
* [ ] When it is green: `main` has the merge commit and `Formula/aibus.rb`, the release has the
      assets listed above (six archives, SBOM, `SHA256SUMS`, two installers, and a bundle for
      each), and `brew install spdrman/aibus/aibus` then `aibus version` works on a clean machine.
* [ ] Verify one archive per OS with the command above, and run `aibus version` (it prints
      `vX.Y.Z` and the commit).

### If a run dies halfway

`finalize` is last, so a failed run has not touched `main`. What to remove depends on how far it
got, and it has to be done before the next try, because `prepare` refuses an existing tag:

* Died in `prepare`, `build` or `package`: nothing was created. Fix and push to `release` again.
* Died in `publish` or `verify`: delete the release and the tag it created with
  `gh release delete vX.Y.Z --cleanup-tag --yes`, and check `gh release list` and the remote tags.
  Fix, and push to `release` again.
* Died in `finalize` after the formula commit but before the merge: the release is good. Merge
  `release` into `main` by hand (a merge commit), after checking `Formula/aibus.rb` looks right.
* Died in `finalize` before the formula commit: the release is good. Render the formula by hand
  with the published `SHA256SUMS`, commit it to `release`, and merge `release` into `main`.

A release that was published and then found broken is not edited: delete it as above and cut the
next patch version.

### Changelog note rule for MSRV bumps

Raising `rust-version` (and `rust-toolchain.toml`) always gets a line in the release notes under
"Build", naming the old and new version and why. A release that bumps it says so in the tag
message too. A bump never rides in a patch release.

### Homebrew and crates.io

* Homebrew: this repository is the tap. The committed `Formula/aibus.rb` has all-zero placeholder
  checksums (template `packaging/homebrew/aibus.rb.in`); `finalize` overwrites it, rendered from the
  release's `SHA256SUMS` with `packaging/homebrew/render-formula.sh`, so it carries real checksums
  after a release. Install with `brew tap spdrman/aibus https://github.com/spdrman/aibus && brew
  install spdrman/aibus/aibus`.
  [ ] On a clean macOS or Linux machine: install as above, then `aibus version`.
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
