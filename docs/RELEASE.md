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
tools/release-attest/attest.sh --key <private key>     # on the release candidate, tree clean
git add release-attest && git commit -m "Attest the suites for <version>"
tools/release-attest/verify.sh --expect-version <version>            # the same check `prepare` makes
```

The hash is the id of the commit's top-level git tree with the one entry named exactly
`release-attest` removed, rebuilt with `git mktree -z` (algorithm `mktree-top-level-sans-release-attest-v2`).
Nothing is split on newlines or tabs, so no file name can fake an entry, and only that top-level
entry is left out: committing the two files does not invalidate them, and any other change after the
run (a nested `release-attest` directory included) does ("the code changed after the run").
`verify.sh` checks the signature against `packaging/release-attest.pub`, that the tree hash is this
commit's, that the version is the one `Cargo.toml` declares, and that every suite is present, exited
0 and passed cells. It prints ok only if the signature check actually ran, and a `--pubkey` that is
not a readable file is a failure. `prepare` runs it on the release commit with the key read from
`main`, so the push cannot bring its own. It proves the key holder signed a green run of this tree;
it cannot stop someone with write access editing the script or the workflow in the same push.

The signing key is `ssh-ed25519` (public half committed in `packaging/release-attest.pub`,
fingerprint `SHA256:VqX4OVIeEX8cDiNWiwMIJdcBqZtpGyiUNiOpztMzHT8`); the private half is never
committed. `attest.sh` looks for the private key at `~/.ssh/aibus-release-attest` with its `.pub`
beside it. If it lives elsewhere, pass `--key <private file>` (the `.pub` must sit next to it) or set
`AIBUS_ATTEST_KEY`.

**The key has to sign without a prompt.** `attest.sh` signs with `ssh-keygen -Y sign -f <private key path>`,
and nobody is there to type a passphrase during a 15 minute run, so a passphrase-protected key has to be
loaded in `ssh-agent`: run `ssh-add ~/.ssh/aibus-release-attest` once before the release. A key in the
agent signs silently even though the script passes the private key path (checked with a throwaway key:
with the key in the agent `ssh-keygen -Y sign -f <private path>` signs without asking, without it the same
command prompts). `release.sh` and `attest.sh` both sign a small scratch file first (same key and namespace,
`SSH_ASKPASS_REQUIRE=never`, stdin from `/dev/null`) and stop with "the key cannot sign without a prompt:
load it with ssh-add <key> first" before the container starts. `ssh-keygen` opens `/dev/tty` by itself, so
the call runs through a small `perl` wrapper that forks and calls `setsid` in the child (a new session has no
controlling terminal, so it cannot prompt, even when the shell has job control on). The wrapper runs in the
background and the script waits for it, so INT, TERM or HUP sent to the script alone, or Ctrl-C to the whole
group, reach it: it sends TERM to the child's session, kills it after 2 seconds, and the script exits 130,
143 or 129 without leaving an orphan. The wrapper returns the child's exit status, and its own failure shows as "cannot
detach the terminal for the key check" instead. perl is a prerequisite of both scripts. The real signature
at the end of `attest.sh` goes through the same wrapper, so a key that dropped out of the agent mid-run
(`ssh-add -t`, a locked agent) makes it stop with exit 1 and a message to load the key, not prompt. On 0.2.0
the failure was found out at the very end of a 7 minute attestation.

The signed files are made in a temp directory, copied into a stage directory inside `release-attest/`,
verified there, the existing files are backed up (to a temp name, renamed only when the copy succeeded),
and then the two files are renamed into place. Undo acts only on a file whose rename was started: it is
restored from its backup, or removed when this run created it. A failure or INT/TERM/HUP before the first
rename (during the copies or the backups) therefore touches nothing in `release-attest/`, and a `--replace`
keeps the committed attestation byte for byte, mode included. A failed attestation leaves no untracked
`attestation.json` behind (it used to, and the next run stopped with "the tree is dirty"). If a restore
itself fails the stage directory is kept (it holds the old copy) and the message says where. SIGKILL cannot
be caught, so a kill -9 between the two renames can leave a stage directory or a half pair; remove
`release-attest/.attest-stage.*` then, and delete an untracked `release-attest/attestation.json` by hand
(`git checkout release-attest` restores tracked files only, it does not remove an untracked one). Ctrl-C to the
whole group stops `release.sh` with status 130 at every step, and `attest.sh` dies by the signal too, so
`release.sh` reports "interrupted" and not "did not sign a green run". A TERM sent to `release.sh`'s own PID (not
the group) during step 4 or 5 leaves `attest.sh` or the e2e running; an untracked `release-attest/` left after it
makes the next run refuse with "the tree is dirty", as it did before this change: look at `git status` and remove
it. `release-attest`
being a regular file is refused up front. A red run keeps its logs in a `mktemp -d` directory and says where.

Tests for the
tooling (throwaway keys, a fake runner, no container) are `bash tests/release-attest/run.sh`.

## The local release gate

Actions are off for this repo on purpose, so the gate on `release` is local: committed git hooks
plus one runner. Install them once per clone with `tools/release-gate/install.sh` (it sets
`core.hooksPath` to `.githooks`; there were no hooks before, so nothing else is replaced).

**Flow.** A feature branch (`feature/...`, `fix/...`, `lane/...`) merges onto `main` as before. A
release is cut on a branch named `release-prep/v<version>`, and that branch is merged into `release`.
That merge is what `release.yml` publishes from.

**On a feature branch** (`pre-commit`): fast checks on the staged files only, a `rustfmt --check` ratchet on
`.rs` (below) and `bash -n` on shell. Clippy, the tests and the e2e are not run here (they compile, and
compiles run in a container, see `docs/BUILD.md`), and a PR runs its own specific tests by hand. The hook
does nothing while merging and on `release` itself.

**The rustfmt ratchet.** The repo is not rustfmt-clean today (313 of 376 tracked `.rs` files fail
`rustfmt --check --edition 2021`, and no `rustfmt.toml` fixes that), so the hook does not demand a clean
file, it only stops the count from growing. Per staged `.rs` file, using the staged blob (not the working
copy) on stdin with the same `rustfmt --check --edition 2021`: a new file must pass; a file that is clean at
`HEAD` (at its old path, for a rename or copy) must still be clean; a file that was not clean at `HEAD` is
skipped with one line, `<file> was not rustfmt-clean before this commit, format check skipped`. Deleted
files are not looked at, and the hook never formats or writes anything. That is how new code stays clean
without a mass reformat. If rustfmt itself fails on either side (a non-zero exit with no `Diff in` line, such as a missing component
or a syntax error) the hook refuses and says so, so a broken toolchain never reads as "not clean" and skips
a file; with no `rustfmt` on `PATH` at all it passes, as the old hook did. Paths are read NUL-separated, so
names with spaces or newlines work. Edge cases I know about and kept: rustfmt reads the blob on stdin
because a temp copy would break on `mod` declarations, so a new file with `#![rustfmt::skip]` and a new
symlink named `*.rs` are refused (stdin prints the source back), and a parent file is not judged on an
unstaged child, since stdin does not follow `mod`. Tests: `bash tests/pre-commit/run.sh` (a fake `rustfmt` on
`PATH`, throwaway repos).

**Merging into `release`** (`pre-merge-commit`): when the current branch is `release`, a merge that
auto-commits runs the full e2e first and is aborted if it is red. The e2e is
`tools/release-gate/e2e.sh`. It runs the suites in `tools/release-attest/config.sh` (the "every
push" rows of `docs/TESTING.md`, the same list as the attestation, built by the same `lane_script`) in
one container through `/Users/rom/workspace/dh.sh`. The container is fenced: memory limit (60% of the host's RAM,
16g at most), swap equal to it so none, CPU quota (two thirds of the CPUs). `DH_MEMORY` and `DH_CPUS`
override that. The host is `--host`, `RELEASE_E2E_HOST`, or `dh.sh pick` (least loaded, the MacBook
last). The tree is archived from git, pushed into a named docker volume, and the volume is removed
on every exit path. A cold run compiles the workspace, so I expect tens of minutes and a few GB
(I have not timed it here yet). It refuses a dirty tree. There is no Playwright in this repo.

**The stamp.** A green run writes `$(git rev-parse --git-common-dir)/release-e2e/<tree key>`
(tree, command, host, start and finish time, and `gate_sha256`). It lives inside `.git` and is never committed. The key
is the tree id with the top-level `release-attest/` directory left out (the `tree_hash` function in `tools/release-attest/config.sh`), so adding
the attestation commit does not invalidate the stamp. That also means anything under `release-attest/` is outside the stamp key by design; the signed attestation covers the rest, and `pre-push` refuses a push that touches that directory beyond the two attestation files. `pre-push` refuses a push whose destination is
`refs/heads/release` unless a stamp exists for the tree being pushed. That also covers `merge
--no-commit`, squash merges and fast-forwards, which skip `pre-merge-commit`. `pre-push` reads the stamp, it does not just look for the file: it must have exactly the keys the runner writes, name the tree it is filed under, name a host `dh.sh` knows (mars, nas, mercury, local; `e2e.sh` checks the same list before it spends a run), and carry a `gate_sha256` equal to the hash of the gate it judges with. Pushes to any other
branch pass straight through.

**Which gate judges a push.** I made `pre-push` judge with the gate from the remote release tip, not from the tree being pushed. It reads `tools/release-gate` and `tools/release-attest` at the old sha git hands the hook with `git show` (not `git archive`, which drops whatever the tip's `.gitattributes` export-ignores) into a temp dir, and runs the stamp check from that copy. If the tip has no gate, it refuses. `gate_sha256` is a hash over `config.sh`, `e2e.sh`, `lib.sh`, `attest.sh` and `verify.sh`, so a stamp made by a gate the topic edited does not match. Besides a missing stamp, I refuse the push when: it changes a gate file (`tools/release-gate/*`, `tools/release-attest/*`, `.githooks/*`); it touches `release-attest/` beyond `attestation.json` and its signature; it deletes `release`; the remote has no `release` yet (zero old sha, the one-time bootstrap); the remote `release` exists but has no gate yet (the first gated push to the real remote, which predates the gate; the refusal prints the skip hint); or the remote tip is not in the clone (`fetch first`, no fallback to `refs/remotes`). The logged `RELEASE_E2E_SKIP` lifts every one of those. I read the ref lines from the end, so a local ref with spaces in it (`HEAD^{/some message}`) cannot hide the release line, and I refuse stdin with a line of fewer than 4 fields or with more than one line naming `refs/heads/release` (a newline inside a refspec is the only way to get either). `GIT_COMMON_DIR` is unset in both hooks so a decoy stamp directory is not read. `pre-merge-commit` does the same for a merge: it reads the gate from `HEAD` with `git show` and refuses a merge that changes a gate file. The copy of the tree that `e2e.sh` and `attest.sh` run the suites on is also built from the git objects, not `git archive`, so `export-ignore` and `export-subst` cannot drop or rewrite what gets tested; modes, symlinks and submodule directories are kept. Because the tree is untrusted input copied on the host, I check every path first and refuse the whole copy for an empty, `.`, `..` or `.git` component (any case, invisible characters taken out), make every directory (the parents of the symlinks too) in a first pass while the destination holds no link, then write the symlinks in a second pass that creates no directory, never write through a symlink or over an existing path, and check that every parent resolves inside the destination. Everything is judged before it is written, so a refused copy leaves nothing outside the destination (a test snapshots the surroundings). Known limits: the blob reads go through `git cat-file --batch` in a small perl helper inside `config.sh` (perl is already needed for the path filter, and bash 3.2 cannot read exact byte counts of binary data), so this repo's 1500 files copy in under a second instead of 11 s; the HFS-ignorable stripping covers the common characters only; and `dh.sh push` excludes directories named `target` and `node_modules` (`tar --exclude`) from what the container tests, while the tree hash still covers them (that is in `dh.sh`, outside this gate).

**The skip record.** Each hook writes its own record inline and sources nothing from the working tree or the pushed tree, so a topic that silences `gate_skipped` in `lib.sh` cannot make a skip silent. The reason has to be valid UTF-8 and 12 characters or more (characters, not bytes, counted by perl so `LC_ALL=C` makes no difference) after trimming whitespace (ASCII and the common Unicode spaces such as NBSP and U+3000) and after dropping invisible characters (zero-width space and joiners, the bidi marks, BOM, word joiner and the other U+2060 range characters, Hangul filler U+3164 and U+115F/1160, braille blank U+2800, Mongolian vowel separator U+180E, variation selectors, tag characters) (control characters and the Unicode line separators are flattened to spaces, so a reason cannot forge extra lines). One line is appended to `release-e2e/skips.log`: time, hook, user email, the reason, then `pushed_sha`, `old_tip`, `remote_ref`, `waived=` (some of `stamp`, `bootstrap`, `delete`, `gate-change`, `attest-extra`, `remote-gate`, `remote-tip-missing`, `e2e` for a merge), `user` and `branch`.

**Failed writes.** `pre-push` saves the push lines git sends on stdin before it judges them. If that write fails (disk full, quota) it refuses with "cannot save the push lines" rather than reading an empty file as "no release line", and every `mktemp` in the hooks, `e2e.sh`, `attest.sh`, `verify.sh`, `release.sh` and `export_tree` is checked (it must succeed and name an existing file or directory) before anything is written through it. A skip record or stamp that cannot be written refuses too, and a stamp must have all six lines before it is filed. Both test runners point `DH_SH` at a failing stub at the top, so no test can reach the real `dh.sh` or a docker host (a guard test fails if the stub was ever called; a test that wants a fake host sets its own `DH_SH`). The perl calls use `-C0`, so `PERL_UNICODE` cannot change how the reason is read, and a character count that is not a number refuses. The two test runners (`tests/release-gate/run.sh`, `tests/release-attest/run.sh`) run without `-e`, so they stop with `exit 1` when their own temp dir cannot be made. `e2e.sh` keeps a red run's log under a `mktemp` name, not a name built from the pid.

**Batched `export_tree`.** The copy reads blobs with `git cat-file --batch`, 500 shas per process in list order. Each reply header has to be `<the requested sha> blob <size>`, then exactly that many bytes and a newline, with nothing after the last one; a missing object, a short or long stream, a bad header or a non-zero git exit aborts the whole copy. A gitlink is never sent. The path judging and the directories are still bash (pass 0 and the directory pass), and the perl helper only writes: files with `O_EXCL|O_NOFOLLOW`, symlinks in the second pass, each after its parent was checked to be a real directory inside the destination.

**Replace refs and long links.** `export_tree`, `tree_hash` and `declared_version` run every git with `GIT_NO_REPLACE_OBJECTS=1` and `GIT_NO_LAZY_FETCH=1`, so a local `refs/replace` entry cannot swap the bytes that get exported (or the version that gets read) while the hash still names the genuine tree, and a missing object fails instead of reaching the network. A symlink blob over 4096 bytes is refused from its `cat-file` header, before its body is read, so a huge link target cannot fill memory on the way to `ENAMETOOLONG`. Bodies are not hashed against their sha: with the env vars set, that would only defend against a `git cat-file` that lies, and it would cost a hash per blob (#250).

**Override.** `RELEASE_E2E_SKIP='<reason>'` skips the gate in the hooks and the runner. It prints
the reason loudly and appends it to `release-e2e/skips.log` next to the stamps. It is legitimate for
a docs-only change to `release`, a change to the gate itself, creating the `release` branch, or when every docker host is down and the release is urgent and I
accept it. I do not use `--no-verify`. `RELEASE_E2E_CMD` replaces the container run with a local
command; it is for the tests of the gate only. It warns loudly, and its stamps go to `release-e2e-fake/`, which
`pre-push` never reads (only `tests/release-gate/run.sh` sets `RELEASE_E2E_TEST_STAMPS`, to the repo's stamp directory, to get real ones, and `pre-push` rejects fake-host stamps otherwise), so a
leftover variable in a shell cannot open the gate.

**What the skip reason has to be.** The reason is a speed bump, not a security boundary: the skip is logged and only I run it, so the filter is there to stop a lazy `x` or twelve spaces, not a determined person. It is the same rule as in the causl repos, and `.githooks/pre-push`, `.githooks/pre-merge-commit` and `gate_skipped` in `tools/release-gate/lib.sh` carry the same two `perl -T -C0` one-liners byte for byte (a test compares them). The reason has to be valid UTF-8. The first one drops default-ignorable code points (zero-width and joiner characters, variation selectors, tag characters, the Hangul fillers and so on), U+2800 and U+1D159, turns controls and Unicode spaces into spaces, and trims the ends. The second one removes every remaining space, separator, combining mark and other (control, format, private use, unassigned, surrogate) code point, and the number left has to be at least 12. So `a` plus ten spaces plus `b` is 2, and `a` plus eleven U+0301 is 1. What counts is code points, not graphemes: a ZWJ emoji family counts as several, and U+FFFC and U+FFFD count as characters. `PERL5OPT=-CSD` in the caller's shell does not change the result.

**Markers.** The container script wraps each suite in SUITE and EXIT markers that carry a random per-run nonce (`lane_script` in `tools/release-attest/config.sh`, shared with `attest.sh`), so a decoy a test prints or quotes (an old log, say) is not believed, and a suite with two markers is red. It does not stop suite code that reads the nonce from the container's argv, which runs with the same rights as the gate. Each EXIT marker starts on its own line. A run is green only if the container itself exited 0 and every suite has exactly one genuine EXIT marker with 0.

**Failure modes.** Docker host down: `dh.sh push` fails, the runner says so and cleans up; pick another
`--host`. OOM: a killed container leaves a suite without its `EXIT` line, so the run is red, and the
log is kept (path printed); lower `DH_CPUS` or move to a bigger host, do not retry in a loop. Dirty
tree: refused (in the merge hook it means unstaged or untracked files while the merge is pending).
Red: no stamp.

**What the hooks cannot do.**

* The hooks themselves come from the working tree (`core.hooksPath=.githooks`). A topic that edits `.githooks` and is merged into `release` runs its own hooks, so I treat that merge as ungated. I take the runner, conf and attest files from the release tip, but not the hooks.
* The logged skip also waives the e2e. After a gate change, or when I create `release`, I run `tools/release-gate/e2e.sh` by hand on the merged tree first.
* A gate change needs the skip on both the merge (`pre-merge-commit`) and the push (`pre-push`). Creating the `release` branch needs it on both too. The real remote `release` predates this gate (it has no `tools/release-gate` and no `.githooks`), so my first gated merge and push take the "HEAD or the remote tip has no gate" path: the skip, a reason of 12+ characters, and the e2e run by hand.
* A merge done in the GitHub web UI runs no local hook at all.
* The real boundary is GitHub branch protection on `release`, plus the signed attestation check in the `prepare` job of `release.yml`, which stays fail-closed. The local gate catches my mistakes, it does not stop someone with write access.

**How it relates to the attestation.** The stamp gates the merge and push into `release`; the signed
attestation gates the publish (`prepare` in `release.yml` verifies it). They run the same suites. The
stamp is local and unsigned, the attestation is signed and committed.

### The release script

`tools/release-gate/release.sh <version> [--dry-run] [--yes] [--closes N] [--host h] [--rerun]` (the issue
flag is `--closes N`, there is no `--issue`; `ssh-add` the key first, see above) does a
release up to the PR and stops. It never tags, publishes or merges: `publish` in `release.yml` makes
the tag `v<version>` when the PR lands on `release`.

1. Checks: `<version>` is `X.Y.Z` or `X.Y.Z-pre.N`; the tag `v<version>` does not exist locally or on
   origin; it is greater than the latest `v*` tag and not lower than the workspace version (a
   version equal to the workspace one means the bump was already made, like 0.2.0 now), or pass
   `--allow-downgrade`; `CHANGELOG.md` has the entry (I write it first); the tree is clean; the key
   exists at `~/.ssh/aibus-release-attest` (or `--key`) and its `.pub` is the one committed in
   `packaging/release-attest.pub`. The private key is only checked for existing here; then a scratch
   file is signed with it to prove it signs without a prompt (the sign preflight, see above).
2. Cuts `release-prep/v<version>` from `origin/main`.
3. One commit `Release v<version>`: `version` in `[workspace.package]` in `Cargo.toml` and the path
   crates in `Cargo.lock` (what `lane/version-0.2.0` did). `Formula/aibus.rb` stays, `finalize` renders
   it. This is before the e2e, so the e2e and the attestation cover the tree that gets released.
4. The full e2e (above). A stamp already there for the same tree is reused (`--rerun` forces it).
5. `tools/release-attest/attest.sh`, then it checks the attestation covers the tree the e2e ran on.
6. Commits `release-attest/`, runs `verify.sh --expect-version <version>` (the check `prepare` makes),
   asks once (`--yes` skips; `y`, `Y`, `yes`, `YES` or `Yes` confirms, anything else leaves the
   commits local), pushes the branch and runs `gh pr create --base release --title "Release
   v<version>"`. The body names the attestation, the stamp (tree, host, time) and the tag. When the
   manifest already said the version there is no bump commit: the dry run prints "no bump commit
   needed, the manifest is already X" and the PR body says so instead of "I bumped the version".

`--dry-run` prints every step and writes, pushes and opens nothing. To review in the PR: the bump
commit, the attestation (version and tree), the CHANGELOG entry, then merge it myself. The attestation
runs the same suites as the e2e, so a full release runs them twice; the e2e is the gate for the
merge, the attestation is the proof for publish.

Two things about the PR into `release`. It must be merged with a **merge commit**: squash and rebase
both fail in `prepare` of `release.yml`, because the pushed commit no longer descends from main's tip
(GitHub's test merge commit passes). And a PR into `release` does not close its issue, since GitHub
only closes on merges into the default branch, so `Closes #N` in the body does nothing there: close the
release issue by hand after the merge.

Timing, measured once on the 0.2.0 run (one run, one host, so a rough guide and not a promise): the e2e
took 7m17s in the container on mars, and the e2e plus the attestation about 15 minutes in total. Plan
the key and the terminal for that, not for a quick command. Tests: `bash tests/release-gate/run.sh` (fake e2e,
fake runner, throwaway key, fake `gh`; no container).

### Release procedure

* [ ] `main` is green locally (the automatic part above) and a dry run of the workflow is green.
* [ ] `version` in the root `Cargo.toml` is the release version, `Cargo.lock` is committed, and
      `CHANGELOG.md` has the entry for it.
* [ ] (or all of the steps below up to the PR with `tools/release-gate/release.sh <version>`, see above.)
* [ ] `tools/release-attest/attest.sh` is green on the commit that goes to `release`, the two files
      are committed, and `tools/release-attest/verify.sh --expect-version <version>` says ok. The
      merge into `release` must leave the same tree (fast-forward, or a merge that changes nothing).
* [ ] Open a pull request from `main` (or a branch of it) into `release`, and merge it with a merge
      commit (squash and rebase fail in `prepare`). It does not close its issue: close that by hand. The merge
      is the trigger. If `release` does not exist yet, create it once from `main` (the one-time bootstrap; the stamp does not exist for that tree, so the skip is logged): `RELEASE_E2E_SKIP="bootstrap release branch" git push origin main:refs/heads/release`. A branch named `release/...` cannot exist next to `release`, so per-release work uses `release-prep/v<version>`.
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
