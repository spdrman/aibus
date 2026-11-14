#!/usr/bin/env bash
# Tests for the rustfmt ratchet in .githooks/pre-commit (issue #272). Throwaway repos and a fake rustfmt on
# PATH (a file or stdin "fails the check" when it has a line that says UNFORMATTED). No cargo, no container.
# Needs bash and git.
set -u
here=$(cd "$(dirname "$0")" && pwd)
src=$(cd "$here/../.." && pwd)
t=$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/precommit-test.XXXXXX") || exit 1
[ -n "$t" ] && [ -d "$t" ] || exit 1
trap 'rm -rf "$t"' EXIT
fails=0 n=0
ok() { n=$((n + 1)); echo "ok $n - $1"; }
bad() { n=$((n + 1)); fails=$((fails + 1)); echo "not ok $n - $1"; }
expect() { name=$1; shift; if "$@" >/dev/null 2>&1; then ok "$name"; else bad "$name"; fi; }
refuse() { name=$1; shift; if "$@" >/dev/null 2>&1; then bad "$name"; else ok "$name"; fi; }

mkdir "$t/bin"
cat >"$t/bin/rustfmt" <<'FAKE'
#!/bin/sh
echo "$*" >>"$FAKE_RUSTFMT_LOG"
case "$*" in "--check --edition 2021") ;; *) echo "unexpected rustfmt arguments: $*" >&2; exit 3 ;; esac
# real rustfmt exits 0 on stdin even with --check and says so only in its output
if grep -q '^UNFORMATTED' ; then echo "Diff in <stdin>:1:" ; fi
exit 0
FAKE
chmod +x "$t/bin/rustfmt"
export FAKE_RUSTFMT_LOG=$t/rustfmt.log PATH="$t/bin:$PATH"

mkrepo() { # dir : a repo with the hook and a base commit holding clean.rs, dirty.rs and "with space.rs"
  d=$1; rm -rf "$d"; mkdir -p "$d"
  git -C "$d" init -q -b main
  git -C "$d" config user.email t@example.com; git -C "$d" config user.name t
  cp -R "$src/.githooks" "$d/"
  for i in 1 2 3 4 5 6 7 8; do echo "fn clean$i() {}"; done >"$d/clean.rs"
  { echo UNFORMATTED; for i in 1 2 3 4 5 6 7 8; do echo "fn dirty$i() {}"; done; } >"$d/dirty.rs"
  printf 'fn spaced() {}\n' >"$d/with space.rs"
  git -C "$d" add -A; git -C "$d" commit -q --no-verify -m base
}
hook() { (cd "$1" && ./.githooks/pre-commit); }
r=$t/r

# a file that was not clean at HEAD: edited, the hook skips it and says so
mkrepo "$r"; printf 'UNFORMATTED\nfn dirty() { 1; }\n' >"$r/dirty.rs"; git -C "$r" add dirty.rs
expect "unformatted at HEAD, edited: passes" hook "$r"
out=$(hook "$r" 2>&1); expect "unformatted at HEAD, edited: prints the skip line" bash -c 'printf "%s\n" "$1" | grep -qx "dirty.rs was not rustfmt-clean before this commit, format check skipped"' _ "$out"

# a clean file must not become unformatted
mkrepo "$r"; printf 'UNFORMATTED\nfn clean() {}\n' >"$r/clean.rs"; git -C "$r" add clean.rs
refuse "clean at HEAD, made unformatted: refused" hook "$r"
out=$(hook "$r" 2>&1); expect "clean at HEAD, made unformatted: the message names the file" bash -c 'printf "%s\n" "$1" | grep -q "clean.rs"' _ "$out"
mkrepo "$r"; printf 'fn clean() { 2; }\n' >"$r/clean.rs"; git -C "$r" add clean.rs
expect "clean at HEAD, edited and still clean: passes" hook "$r"

# new files must pass
mkrepo "$r"; printf 'UNFORMATTED\nfn new() {}\n' >"$r/new.rs"; git -C "$r" add new.rs
refuse "new unformatted file: refused" hook "$r"
mkrepo "$r"; printf 'fn new() {}\n' >"$r/new.rs"; git -C "$r" add new.rs
expect "new formatted file: passes" hook "$r"

# names with spaces
mkrepo "$r"; printf 'UNFORMATTED\n' >"$r/with space.rs"; git -C "$r" add "with space.rs"
refuse "file with a space in its name, clean at HEAD, made unformatted: refused" hook "$r"
mkrepo "$r"; printf 'fn two() {}\n' >"$r/new file.rs"; git -C "$r" add "new file.rs"
expect "new formatted file with a space in its name: passes" hook "$r"
mkrepo "$r"; printf 'UNFORMATTED\n' >"$r/new file.rs"; git -C "$r" add "new file.rs"
refuse "new unformatted file with a space in its name: refused" hook "$r"
mkrepo "$r"; printf 'fn u() {}\n' >"$r/caf\303\251.rs"; git -C "$r" add .
expect "a non-ASCII file name does not crash the hook" hook "$r"

# renames compare against the old path
mkrepo "$r"; git -C "$r" mv dirty.rs moved.rs; echo 'fn more() {}' >>"$r/moved.rs"; git -C "$r" add -A
expect "rename of a file that was not clean: passes" hook "$r"
out=$(hook "$r" 2>&1); expect "rename of a file that was not clean: skip line names the new path" bash -c 'printf "%s\n" "$1" | grep -q "moved.rs was not rustfmt-clean"' _ "$out"
mkrepo "$r"; git -C "$r" mv clean.rs moved.rs; echo UNFORMATTED >>"$r/moved.rs"; git -C "$r" add -A
refuse "rename of a clean file to an unformatted one: refused" hook "$r"
mkrepo "$r"; git -C "$r" mv clean.rs moved.rs; git -C "$r" add -A
expect "pure rename of a clean file: passes" hook "$r"

# deletions and nothing staged
mkrepo "$r"; git -C "$r" rm -q clean.rs dirty.rs
expect "deleting .rs files: passes" hook "$r"
mkrepo "$r"; : >"$t/rustfmt.log"; printf 'x\n' >"$r/notes.txt"; git -C "$r" add notes.txt
expect "no staged .rs file: passes" hook "$r"
expect "no staged .rs file: rustfmt is never run" test ! -s "$t/rustfmt.log"

# the staged blob is what counts, and the hook never touches files
mkrepo "$r"; printf 'UNFORMATTED\nfn clean() {}\n' >"$r/clean.rs"; git -C "$r" add clean.rs; printf 'fn clean() {}\n' >"$r/clean.rs"
refuse "staged version unformatted, working copy formatted: refused" hook "$r"
mkrepo "$r"; printf 'UNFORMATTED\nfn clean() {}\n' >"$r/clean.rs"; git -C "$r" add clean.rs
before=$(cd "$r" && git status --porcelain=v1 && cat clean.rs dirty.rs && git diff --cached | git hash-object --stdin)
hook "$r" >/dev/null 2>&1
after=$(cd "$r" && git status --porcelain=v1 && cat clean.rs dirty.rs && git diff --cached | git hash-object --stdin)
expect "the hook changes no file and no index entry" test "$before" = "$after"
: >"$t/rustfmt.log"; hook "$r" >/dev/null 2>&1
expect "rustfmt is called with the same flags as before (--check --edition 2021)" bash -c '[ -s "$1" ] && ! grep -qv "^--check --edition 2021$" "$1"' _ "$t/rustfmt.log"

# first commit of a repo: no HEAD, every file is new
d=$t/fresh; mkdir -p "$d"; git -C "$d" init -q -b main; cp -R "$src/.githooks" "$d/"
printf 'fn a() {}\n' >"$d/a.rs"; git -C "$d" add a.rs
expect "first commit, formatted file: passes" hook "$d"
printf 'UNFORMATTED\n' >"$d/b.rs"; git -C "$d" add b.rs
refuse "first commit, unformatted file: refused" hook "$d"

# unchanged behaviour: release branch, merge, and no rustfmt on PATH
mkrepo "$r"; printf 'UNFORMATTED\n' >"$r/clean.rs"; git -C "$r" add clean.rs; git -C "$r" switch -q -c release
expect "on release the hook does nothing" hook "$r"
mkrepo "$r"; printf 'UNFORMATTED\n' >"$r/clean.rs"; git -C "$r" add clean.rs; : >"$r/.git/MERGE_HEAD"
expect "while merging the hook does nothing" hook "$r"
mkrepo "$r"; printf 'UNFORMATTED\n' >"$r/clean.rs"; git -C "$r" add clean.rs
expect "without rustfmt on PATH the hook passes" bash -c 'cd "$1" && ! PATH=/usr/bin:/bin command -v rustfmt && PATH=/usr/bin:/bin ./.githooks/pre-commit' _ "$r"

echo "$n tests, $fails failed"
[ $fails = 0 ]
