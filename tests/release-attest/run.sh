#!/usr/bin/env bash
# Tests for tools/release-attest. A throwaway key and a throwaway git repo in a temp dir; the
# runner is a fake, so no container starts and no cargo runs. Needs bash, git, jq and ssh-keygen.
set -u
# Start from a known environment: whatever the calling shell exported for the gate must not change a result.
unset RELEASE_E2E_HOST RELEASE_E2E_SKIP RELEASE_E2E_CMD RELEASE_E2E_TEST_STAMPS RELEASE_GATE_TEST RELEASE_ATTEST_SH DH_HOST DH_CPUS DH_MEMORY DH_UNFENCED
here=$(cd "$(dirname "$0")" && pwd)
tools=$(cd "$here/../../tools/release-attest" && pwd)
. "$tools/config.sh"
t=$(mktemp -d "${TMPDIR:-/tmp}/attest-test.XXXXXX") || exit 1
[ -n "$t" ] && [ -d "$t" ] || exit 1   # no -e here: an empty $t would aim every rm -rf "$t/..." at a path under /
trap 'rm -rf "$t"' EXIT
# Everything the runs create (log files, kept log directories) goes under $t, which the trap above removes.
mkdir "$t/tmp" && export TMPDIR=$t/tmp || exit 1
# No test may reach the real dh.sh (or docker, or ssh): DH_SH points at a stub that logs the call and fails.
# Tests that want a fake docker host set their own DH_SH on the command; this default catches the rest.
printf '#!/bin/sh\necho "$*" >>"$DH_REAL_LOG"\nexit 1\n' >"$t/dh-real-stub.sh" && chmod +x "$t/dh-real-stub.sh" || exit 1
export DH_SH=$t/dh-real-stub.sh DH_REAL_LOG=$t/dh-real.log
fails=0 n=0
ok() { n=$((n + 1)); echo "ok $n - $1"; }
bad() { n=$((n + 1)); fails=$((fails + 1)); echo "not ok $n - $1"; }
expect() { name=$1; shift; if "$@" >/dev/null 2>&1; then ok "$name"; else bad "$name"; fi; }
refuse() { name=$1; shift; if "$@" >/dev/null 2>&1; then bad "$name"; else ok "$name"; fi; }

ssh-keygen -q -t ed25519 -N '' -C throwaway -f "$t/key"
ssh-keygen -q -t ed25519 -N '' -C other -f "$t/other"

# A fake runner that prints green output for every suite, or red for $RED.
cat >"$t/fake.sh" <<'FAKE'
echo "$SUITES_LIST" | while IFS='|' read -r name _; do
  echo "=== SUITE $name $LANE_NONCE ==="
  [ "${DECOY:-}" = "$name" ] && echo "=== EXIT $name 0 ==="            # forged, no nonce
  echo "test result: ok. 12 passed; ${FAIL_COUNT:-0} failed; 1 ignored; 0 measured"
  if [ "${RED:-}" = "$name" ]; then code=101; else code=0; fi
  [ "${DECOY_LATER:-}" = "$name" ] && echo "=== EXIT ${RED:-} 0 ==="   # a later suite forges an earlier one
  echo "=== EXIT $name $code $LANE_NONCE ==="
done
exit ${RC:-0}
FAKE

mkrepo() { # dir version
  rm -rf "$1"; mkdir -p "$1/packaging"
  git -C "$1" init -q -b main
  git -C "$1" config user.email t@example.com; git -C "$1" config user.name t
  printf '[workspace.package]\nversion = "%s"\n' "$2" >"$1/Cargo.toml"
  echo hi >"$1/src.txt"
  cp "$t/key.pub" "$1/$PUBKEY_PATH"
  git -C "$1" add -A; git -C "$1" commit -q -m base
}
attest() { # repo [extra args]
  r=$1; shift
  SUITES_LIST="$SUITES" "$tools/attest.sh" --repo "$r" --key "$t/key" --runner-cmd "sh $t/fake.sh" "$@"
}
red_attest() { RED=graph attest "$@"; }
decoy_attest() { RED=unit DECOY=unit attest "$@"; }
later_attest() { RED=unit DECOY_LATER=graph attest "$@"; }
rc_attest() { RC=137 attest "$@"; }
fail_attest() { FAIL_COUNT=1 attest "$@"; }
commit_attest() { git -C "$1" add -A; git -C "$1" commit -q -m attest; }
verify() { "$tools/verify.sh" --repo "$1" "${@:2}"; }

r=$t/repo; mkrepo "$r" 1.2.3
expect "attest signs a green run" attest "$r"
expect "attestation.json and .sig written" test -f "$r/release-attest/attestation.json" -a -f "$r/release-attest/attestation.json.sig"
refuse "attest refuses an existing attestation without --replace" attest "$r"
commit_attest "$r"
expect "valid attestation verifies" verify "$r" --expect-version 1.2.3
expect "attestation records the version and counts" jq -e '.version.declared == "1.2.3" and .result.verdict == "green" and .result.passed > 0' "$r/release-attest/attestation.json"
expect "committing release-attest/ itself does not change the tree hash" test "$(tree_hash "$r" HEAD)" = "$(jq -r .tree.hash "$r/release-attest/attestation.json")"

# Tampered tree: change a tracked file after the run.
echo changed >"$r/src.txt"; git -C "$r" commit -q -am tamper
refuse "tampered tree is refused" verify "$r" --expect-version 1.2.3
verify "$r" 2>&1 | grep -q 'changed after the run' && ok "tampered tree says why" || bad "tampered tree says why"

# Wrong version.
r=$t/repo2; mkrepo "$r" 1.2.3; attest "$r" >/dev/null 2>&1; commit_attest "$r"
refuse "wrong expected version is refused" verify "$r" --expect-version 1.2.4
# Version bumped after the run (tree changes too).
sed -i.bak 's/1.2.3/1.2.4/' "$r/Cargo.toml"; rm "$r/Cargo.toml.bak"; git -C "$r" commit -q -am bump
refuse "version bumped after the run is refused" verify "$r" --expect-version 1.2.4

# Bad signature: signed by another key, then a flipped byte in the attestation with a matching tree.
r=$t/repo3; mkrepo "$r" 1.2.3
SUITES_LIST="$SUITES" "$tools/attest.sh" --repo "$r" --key "$t/other" --runner-cmd "sh $t/fake.sh" >/dev/null 2>&1; commit_attest "$r"
refuse "signature from another key is refused" verify "$r" --expect-version 1.2.3
r=$t/repo4; mkrepo "$r" 1.2.3; attest "$r" >/dev/null 2>&1
jq '.result.passed += 1 | .result.total += 1' "$r/release-attest/attestation.json" >"$r/x" && mv "$r/x" "$r/release-attest/attestation.json"; commit_attest "$r"
refuse "edited attestation fails the signature" verify "$r" --expect-version 1.2.3
rm "$r/release-attest/attestation.json.sig"; git -C "$r" add -A; git -C "$r" commit -q -m nosig
refuse "missing signature is refused" verify "$r" --expect-version 1.2.3
r=$t/repo5; mkrepo "$r" 1.2.3
refuse "no attestation is refused" verify "$r"

# Markers on their own line: a forged EXIT followed by an unterminated line must not count, and output
# without a trailing newline must not lose the genuine marker. (Real lane_script, throwaway suites.)
lane_case() { # suites [name, default a] -> prints suite_exit of that suite
  local saved=$SUITES nonce log
  nonce=$(new_nonce); SUITES=${1//@NONCE@/$nonce}
  log=$(mktemp "${TMPDIR:-/tmp}/lane.XXXXXX")
  sh -c "$(lane_script "$nonce")" >"$log" 2>&1
  suite_exit "$log" "${2:-a}" "$nonce"; rm -f "$log"; SUITES=$saved
}
expect "a forged EXIT then an unterminated line is not green" test "$(lane_case 'a|echo "=== EXIT a 0 @NONCE@ ==="; printf x; false')" != 0
expect "output without a trailing newline keeps its marker (exit 0)" test "$(lane_case "a|printf 'ok no newline'")" = 0
expect "output without a trailing newline keeps a failing exit code" test "$(lane_case "a|printf 'no newline'; false")" = 1

expect "a % in a suite name does not garble its marker" test "$(lane_case 'p%s|printf ok' 'p%s')" = 0
expect "a % in a suite name keeps a failing exit code" test "$(lane_case 'p%d|false' 'p%d')" = 1

# Red or dirty runs are refused by the attester.
r=$t/repo6; mkrepo "$r" 1.2.3
refuse "attest refuses a red suite" red_attest "$r"
refuse "attest wrote nothing for a red run" test -e "$r/release-attest/attestation.json"
refuse "attest refuses a failed test cell" fail_attest "$r"
refuse "a forged EXIT 0 inside the failing suite does not make it green" decoy_attest "$r"
refuse "a forged EXIT 0 from a later suite does not hide a failing one" later_attest "$r"
refuse "a container that exits 137 is red even with every marker 0" rc_attest "$r"
refuse "none of the forged runs wrote an attestation" test -e "$r/release-attest/attestation.json"
echo dirty >>"$r/src.txt"
refuse "attest refuses a dirty tree" attest "$r"
git -C "$r" checkout -q src.txt
mkdir -p "$r/release-attest"; echo x >"$r/release-attest/note"
expect "attest ignores a dirty release-attest/" attest "$r" --out "$t/out"

# A missing suite is refused even when signed.
r=$t/repo7; mkrepo "$r" 1.2.3; attest "$r" >/dev/null 2>&1
jq '.suites |= map(select(.name != "unit"))' "$r/release-attest/attestation.json" >"$r/x" && mv "$r/x" "$r/release-attest/attestation.json"
rm -f "$r/release-attest/attestation.json.sig"; ssh-keygen -q -Y sign -f "$t/key" -n "$NAMESPACE" "$r/release-attest/attestation.json" 2>/dev/null
commit_attest "$r"
refuse "a validly signed attestation missing a suite is refused" verify "$r" --expect-version 1.2.3

# Fail closed while the committed key is still the placeholder.
r=$t/repo8; mkrepo "$r" 1.2.3; attest "$r" >/dev/null 2>&1
echo "PLACEHOLDER-NOT-A-KEY: replace me" >"$r/$PUBKEY_PATH"; commit_attest "$r"
refuse "the placeholder public key fails closed" verify "$r" --expect-version 1.2.3
verify "$r" 2>&1 | grep -q 'not an ssh-ed25519 key' && ok "placeholder says why" || bad "placeholder says why"
rm "$r/$PUBKEY_PATH"; git -C "$r" add -A; git -C "$r" commit -q -m nokey
refuse "a missing public key fails closed" verify "$r" --expect-version 1.2.3
r=$t/repo9; mkrepo "$r" 1.2.3; git -C "$r" rm -q "$PUBKEY_PATH"; git -C "$r" commit -q -m nokey; attest "$r" >/dev/null 2>&1; commit_attest "$r"
refuse "no committed key and no --pubkey fails closed" verify "$r" --expect-version 1.2.3
expect "an explicit --pubkey verifies" verify "$r" --expect-version 1.2.3 --pubkey "$t/key.pub"
refuse "an explicit wrong --pubkey is refused" verify "$r" --expect-version 1.2.3 --pubkey "$t/other.pub"

# Tree hash attacks: the hash is a git tree id, never text split on newlines or tabs.
r=$t/repo10; mkrepo "$r" 1.2.3; echo A >"$r/a"; echo B >"$r/b"; git -C "$r" add -A; git -C "$r" commit -q -m ab
attest "$r" >/dev/null 2>&1; commit_attest "$r"
expect "baseline with a and b verifies" verify "$r" --expect-version 1.2.3
bsha=$(git -C "$r" rev-parse HEAD:b)
git -C "$r" rm -q a b
printf 'A\n' >"$r/$(printf 'a\n100644 blob %s\tb' "$bsha")"
git -C "$r" add -A; git -C "$r" commit -q -m evil
refuse "a file name faking two ls-tree entries is refused" verify "$r" --expect-version 1.2.3
r=$t/repo11; mkrepo "$r" 1.2.3; attest "$r" >/dev/null 2>&1; commit_attest "$r"
d="$r/src/x$(printf "\t")release-attest"; mkdir -p "$d"; echo evil >"$d/main.rs"
git -C "$r" add -A; git -C "$r" commit -q -m nested
refuse "a path with tab and release-attest/ inside it is not hidden from the hash" verify "$r" --expect-version 1.2.3

# --pubkey that is not a readable file.
r=$t/repo12; mkrepo "$r" 1.2.3; attest "$r" >/dev/null 2>&1; commit_attest "$r"
refuse "--pubkey on a missing file is refused" verify "$r" --expect-version 1.2.3 --pubkey "$t/does-not-exist"
refuse "--pubkey on a directory is refused" verify "$r" --expect-version 1.2.3 --pubkey "$t"
verify "$r" --expect-version 1.2.3 --pubkey "$t/does-not-exist" 2>&1 | grep -q 'attestation ok' && bad "no ok without a signature check" || ok "no ok without a signature check"

# A string-typed count is not a count.
r=$t/repo13; mkrepo "$r" 1.2.3; attest "$r" >/dev/null 2>&1
jq '.suites[0].passed |= tostring' "$r/release-attest/attestation.json" >"$r/x" && mv "$r/x" "$r/release-attest/attestation.json"
rm -f "$r/release-attest/attestation.json.sig"; ssh-keygen -q -Y sign -f "$t/key" -n "$NAMESPACE" "$r/release-attest/attestation.json" 2>/dev/null; commit_attest "$r"
refuse "a validly signed string count is refused" verify "$r" --expect-version 1.2.3

# The attester cleans up its work dir and ignores DH_UNFENCED from the shell.
r=$t/repo14; mkrepo "$r" 1.2.3; mkdir "$t/tmp14"
refuse "attest refuses a dirty tree" env TMPDIR="$t/tmp14" bash -c 'echo x >> "$1/src.txt"; SUITES_LIST="$2" "$3/attest.sh" --repo "$1" --key "$4" --runner-cmd "sh $5"' _ "$r" "$SUITES" "$tools" "$t/key" "$t/fake.sh"
expect "a refused attest leaves no work dir" test -z "$(ls "$t/tmp14")"
grep -q 'unset DH_UNFENCED' "$tools/attest.sh" && ok "attest.sh unsets DH_UNFENCED" || bad "attest.sh unsets DH_UNFENCED"

# An empty nonce (od gave nothing) is refused with a message, not a silent exit.
r=$t/repo15; mkrepo "$r" 1.2.3; mkdir "$t/noodbin"; printf '#!/bin/sh\nexit 0\n' >"$t/noodbin/od"; chmod +x "$t/noodbin/od"
PATH="$t/noodbin:$PATH" SUITES_LIST="$SUITES" "$tools/attest.sh" --repo "$r" --key "$t/key" --runner-cmd "sh $t/fake.sh" >"$t/nonce.out" 2>&1; rc=$?
expect "attest.sh with an empty nonce exits 2" test $rc = 2
expect "...and says why" grep -qi 'nonce' "$t/nonce.out"

# git archive honours the tree's .gitattributes (export-ignore drops files, export-subst rewrites them); the copy attest.sh sends must not.
r=$t/repo16; mkrepo "$r" 1.2.3; mkdir "$r/tests"; echo 'exit 1' >"$r/tests/b.sh"; printf '#!/bin/sh\n' >"$r/tests/x.sh"; chmod +x "$r/tests/x.sh"; ln -s b.sh "$r/tests/link"
printf 'id $Format:%%H$\n' >"$r/tests/sub.txt"; printf 'tests/b.sh export-ignore\ntests/sub.txt export-subst\n' >"$r/.gitattributes"
git -C "$r" add -A; git -C "$r" commit -q -m attrs
cat >"$t/dhcopy.sh" <<'DHC'
#!/bin/sh
case $1 in push) mkdir -p "$DH_COPY"; cp -a "$4/." "$DH_COPY/";; run) exit 1;; esac
DHC
chmod +x "$t/dhcopy.sh"
DH_COPY=$t/copy16 DH_SH=$t/dhcopy.sh "$tools/attest.sh" --repo "$r" --key "$t/key" --host mars >/dev/null 2>&1
expect "attest.sh copies an export-ignored file into the tree it runs" test -f "$t/copy16/tests/b.sh"
expect "attest.sh keeps export-subst text byte for byte" grep -q 'Format:%H' "$t/copy16/tests/sub.txt"
expect "attest.sh keeps the exec bit and symlinks" bash -c "test -x '$t/copy16/tests/x.sh' && test -L '$t/copy16/tests/link' && [ \"\$(readlink '$t/copy16/tests/link')\" = b.sh ]"

# ---------- export_tree: containment (a tree is untrusted input copied on the host before anything is fenced)
x=$t/exp; mkdir -p "$x"; git -C "$x" init -q; xg() { git -C "$x" "$@"; }
xb=$(printf 'hi' | xg hash-object -w --stdin)
xin=$(printf '100644 blob %s\tescaped\0' "$xb" | xg mktree -z)
xroot() { printf '%s\0' "$@" | xg mktree -z; }   # records already shaped "mode type sha<TAB>name"
rec() { printf '%s %s %s\t%s' "$1" "$2" "$3" "$4"; }
mkdir -p "$t/dest1"; rm -rf "$t/dest1"
refuse "a tree entry named .. is refused" export_tree "$x" "$(xroot "$(rec 040000 tree "$xin" ..)")" "$t/dest1"
expect "...and nothing escaped next to the destination" test ! -e "$t/escaped"
refuse "a .git entry is refused" export_tree "$x" "$(xroot "$(rec 040000 tree "$xin" .git)")" "$t/dest2"
refuse "a .GIT entry is refused" export_tree "$x" "$(xroot "$(rec 040000 tree "$xin" .GIT)")" "$t/dest3"
refuse "a nested .git component is refused" export_tree "$x" "$(xroot "$(rec 040000 tree "$(xroot "$(rec 040000 tree "$xin" .git)")" sub)")" "$t/dest3b"
expect "a leading-dash directory is copied" export_tree "$x" "$(xroot "$(rec 040000 tree "$xin" -dash)")" "$t/dest4"
expect "...with its file" test -f "$t/dest4/-dash/escaped"
# the case-collision shape: symlink Z -> /abs/dir, then z/target.txt. On a case-insensitive filesystem the file goes through the link.
victim=$t/victim; mkdir -p "$victim"; echo keep >"$victim/target.txt"
xlink=$(printf '%s' "$victim" | xg hash-object -w --stdin)
xt=$(printf '100644 blob %s\ttarget.txt\0' "$xb" | xg mktree -z)
export_tree "$x" "$(xroot "$(rec 120000 blob "$xlink" Z)" "$(rec 040000 tree "$xt" z)")" "$t/dest5" >/dev/null 2>&1
expect "Z -> /abs/dir plus z/target.txt never writes into the link's target" test "$(cat "$victim/target.txt")" = keep
# an ordered list where a symlink precedes a path through it (impossible in git, so a direct unit test of the writer)
printf '%s\0%s\0' "$(rec 120000 blob "$xlink" ln)" "$(rec 100644 blob "$xb" ln/target.txt)" >"$t/list6"
refuse "a symlink followed by a path through it aborts the copy" export_entries "$x" "$t/list6" "$t/dest6"
expect "...and the link target is untouched" test "$(cat "$victim/target.txt")" = keep
printf '%s\0%s\0' "$(rec 100644 blob "$xb" ln/target.txt)" "$(rec 120000 blob "$xlink" ln)" >"$t/list7"
refuse "a symlink whose path already exists as a directory aborts the copy" export_entries "$x" "$t/list7" "$t/dest7"
expect "...and the link target is still untouched" test "$(cat "$victim/target.txt")" = keep
# nothing may exist outside the destination after a refused copy, not even an empty directory made while checking
snap() { # dir pruned-subtree: everything under dir except the destination
  find "$1" -path "$2" -prune -o -print | sort; }
nul() { printf '%s' "$1"; }
nfc=$(printf '\303\251'); nfd=$(printf 'e\314\201')
check_shape() { # name  link-target-blob-expr  link-name  deep-path
  local name=$1 target=$2 lname=$3 deep=$4 o=$t/o-$1 dest before after blob
  rm -rf "$o"; mkdir -p "$o/victim" "$o/work"; echo keep >"$o/victim/target.txt"; dest=$o/work/dest
  case $target in ABS) blob=$(printf '%s' "$o/victim" | xg hash-object -w --stdin) ;; *) blob=$(printf '%s' "$target" | xg hash-object -w --stdin) ;; esac
  : >"$o/list"; printf '%s\0%s\0' "$(rec 120000 blob "$blob" "$lname")" "$(rec 120000 blob "$xb" "$deep")" >"$o/list"
  before=$(snap "$o" "$dest")
  export_entries "$x" "$o/list" "$dest" >/dev/null 2>&1
  after=$(snap "$o" "$dest")
  [ "$before" = "$after" ]
}
expect "a link then a link under the same name (absolute target): nothing created outside the copy" check_shape sameabs ABS zz zz/newd/deeper/L
expect "...relative target ../../victim" check_shape samerel ../../victim zz zz/newd/deeper/L
expect "...a case-different name (Z / z)" check_shape caseabs ABS Z z/newd/deeper/L
expect "...an NFD link and an NFC directory" check_shape nfd ABS "$nfd" "$nfc/newd/L"
expect "...a link to .. and a path through it" check_shape up .. ll ll/sub/M
expect "...a link to ../.. and a path through it" check_shape up2 ../.. ll ll/sub/M
expect "...a link to .. and a case-different name" check_shape upcase .. L l/sub/M
# the same hostile shape, end to end through e2e.sh (a repo whose HEAD tree has Z -> victim and z/newd/deeper/L)
er=$t/o-e2e; rm -rf "$er"; mkdir -p "$er/victim" "$er/repo"; echo keep >"$er/victim/target.txt"; git -C "$er/repo" init -q -b main
git -C "$er/repo" config user.email t@example.com; git -C "$er/repo" config user.name t
eb=$(printf 'x' | git -C "$er/repo" hash-object -w --stdin); ev=$(printf '%s' "$er/victim" | git -C "$er/repo" hash-object -w --stdin)
e1=$(printf '120000 blob %s\tL\0' "$eb" | git -C "$er/repo" mktree -z)
e2=$(printf '040000 tree %s\tdeeper\0' "$e1" | git -C "$er/repo" mktree -z)
e3=$(printf '040000 tree %s\tnewd\0' "$e2" | git -C "$er/repo" mktree -z)
e4=$({ printf '120000 blob %s\tZ\0' "$ev"; printf '040000 tree %s\tz\0' "$e3"; } | git -C "$er/repo" mktree -z)
ec=$(git -C "$er/repo" commit-tree -m hostile "$e4"); git -C "$er/repo" update-ref refs/heads/main "$ec"; git -C "$er/repo" reset -q --hard 2>/dev/null; git -C "$er/repo" update-index --assume-unchanged Z z/newd/deeper/L 2>/dev/null   # a case-insensitive checkout cannot hold both
esnap=$(snap "$er" "$er/repo")
(cd "$er/repo" && RELEASE_E2E_CMD=true "$here/../../tools/release-gate/e2e.sh" >/dev/null 2>&1)
expect "e2e.sh on a tree with Z -> victim and z/newd/deeper/L creates nothing outside its copy" test "$esnap" = "$(snap "$er" "$er/repo")"
expect "no list file is left next to a destination" test -z "$(ls "$t" | grep -e 'export-tree' -e '\.ls\.')"
r=$t/repo17; mkrepo "$r" 1.2.3
expect "the attestation tree hash is still the tree id without release-attest/" test "$(tree_hash "$r" HEAD)" = "$(git -C "$r" rev-parse 'HEAD^{tree}')"

# ---------- export_tree: blobs are read with `git cat-file --batch`, 500 per call (issue #237)
# A stub git on PATH logs every call and delegates to the real git; STUB_MODE rewrites what a --batch reply looks like.
sb=$t/stubbin; mkdir -p "$sb"; REAL_GIT=$(command -v git); export REAL_GIT
cat >"$sb/git" <<'STUB'
#!/bin/bash
echo "$*" >>"$STUB_LOG"
case " $* " in *" cat-file --batch"*) ;; *) exec "$REAL_GIT" "$@" ;; esac
case ${STUB_MODE:-} in
  fail)      "$REAL_GIT" "$@"; exit 3 ;;
  truncate)  "$REAL_GIT" "$@" | perl -e 'local $/; $d = <STDIN>; print substr($d, 0, length($d) - 3)'; exit 0 ;;
  missing)   read -r first; echo "$first missing"; exit 0 ;;
  wrongsha)  "$REAL_GIT" "$@" | perl -e 'local $/; $d = <STDIN>; $d =~ s/^[0-9a-f]{40}/0000000000000000000000000000000000000000/; print $d'; exit 0 ;;
  extra)     "$REAL_GIT" "$@"; echo junk; exit 0 ;;
  shortsize) "$REAL_GIT" "$@" | perl -e 'local $/; $d = <STDIN>; $d =~ s/^([0-9a-f]+ blob )(\d+)/$1.($2+4)/e; print $d'; exit 0 ;;
  *)         exec "$REAL_GIT" "$@" ;;
esac
STUB
chmod +x "$sb/git"; export STUB_LOG=$t/stub.log
bx=$t/bx; mkdir -p "$bx"; git -C "$bx" init -q; bg() { git -C "$bx" "$@"; }
bsha() { bg hash-object -w --stdin; }   # content on stdin
# trees of N entries over seven shared blobs (so the same sha repeats), named f1..fN
mkn() { # n -> tree id
  local n=$1 i; local -a sh=()
  for i in 0 1 2 3 4 5 6; do sh[i]=$(printf 'content %s\n' "$i" | bsha); done
  for ((i = 1; i <= n; i++)); do printf '100644 blob %s\tf%s\0' "${sh[i % 7]}" "$i"; done | bg mktree -z
}
same_as_tree() { # dir tree : every file hashes to the sha the tree lists, same names, no extra files
  local d=$1 want got
  want=$(bg ls-tree -r "$2" | sed 's/^[0-9]* blob //; s/\t/ /' | LC_ALL=C sort -k2)
  got=$(cd "$d" && find . -type f | sed 's|^\./||' | while IFS= read -r f; do printf '%s %s\n' "$(bg hash-object --no-filters -- "$d/$f")" "$f"; done | LC_ALL=C sort -k2)
  [ -n "$want" ] && [ "$want" = "$got" ]
}
rehash() { # workdir gitdir : the tree id of a directory's content
  GIT_DIR=$2 GIT_WORK_TREE=$1 GIT_INDEX_FILE=$1.idx bash -c 'git add -A -f . && git write-tree'
}
for cnt in 499 500 501 1000 1001; do
  tr=$(mkn $cnt); : >"$STUB_LOG"
  PATH=$sb:$PATH export_tree "$bx" "$tr" "$t/bn$cnt" >/dev/null 2>&1
  expect "$cnt entries with repeated shas are copied byte for byte" same_as_tree "$t/bn$cnt" "$tr"
  calls=$(grep -c 'cat-file' "$STUB_LOG")
  expect "...with at most $(((cnt + 499) / 500)) cat-file call(s) (got $calls)" test "$calls" -le $(((cnt + 499) / 500))
done
# awkward contents in one batch: a body that looks like a reply header, empty, NUL and high bytes, the word missing, a lone newline
hdr=$(printf 'x' | bsha); fake=$(printf '%s blob 5\nhello\n%s missing\n' "$hdr" "$hdr" | bsha)
e0=$(printf '' | bsha); bin=$(printf 'a\0b\377\376\0\0\n\n' | bsha); mis=$(printf 'missing' | bsha); nl=$(printf '\n' | bsha)
big=$t/big20; head -c 20971520 /dev/urandom >"$big"; bigsha=$(bg hash-object -w "$big")
lk=$(printf 'target dir/' | bsha)
tr=$({ printf '100644 blob %s\t%s\0' "$fake" "hdr-lookalike" "$e0" "empty" "$bin" "binary" "$mis" "missing" "$nl" "newline" "$bigsha" "big" "$fake" "dup-lookalike"
       printf '100755 blob %s\t%s\0' "$bin" "exec.sh"
       printf '120000 blob %s\t%s\0' "$lk" "link"
       printf '100644 blob %s\t%s\0' "$e0" $'new\nline' "$e0" $'tab\there' "$e0" "q\"uote'x" "$e0" "-dash" "$e0" "sp ace"; } | bg mktree -z)
rm -rf "$t/bw"; PATH=$sb:$PATH export_tree "$bx" "$tr" "$t/bw" >/dev/null 2>&1
expect "special blobs and names: the copy re-hashes to the same tree id" test "$(rehash "$t/bw" "$bx/.git")" = "$tr"
expect "...20 MB blob identical" cmp -s "$big" "$t/bw/big"
expect "...NUL and high bytes identical" test "$(cksum <"$t/bw/binary")" = "$(printf 'a\0b\377\376\0\0\n\n' | cksum)"
expect "...exec bit kept" test -x "$t/bw/exec.sh" -a ! -x "$t/bw/binary"
expect "...symlink kept with its exact target" test "$(readlink "$t/bw/link")" = "target dir/"
# fail closed: a missing object, a short stream, a wrong header sha, a header with a wrong size, a non-zero exit
tr=$(mkn 600)
for m in missing truncate wrongsha shortsize extra fail; do
  rm -rf "$t/bf-$m"
  refuse "stub git ($m): the copy is refused" env STUB_MODE=$m PATH=$sb:$PATH bash -c ". '$tools/config.sh'; export_tree '$bx' '$tr' '$t/bf-$m'"
done
# a really missing object (no stub): the tree names a blob that is not in the repository
gone=$(printf 'never stored\n' | bg hash-object --stdin)
trm=$(printf '100644 blob %s\tgone\0' "$gone" | bg mktree -z --missing)
refuse "a blob missing from the repository aborts the copy" export_tree "$bx" "$trm" "$t/bmiss"
nulink=$(printf 'a\0b' | bsha)
refuse "a symlink whose target holds a NUL aborts the copy" export_tree "$bx" "$(printf '120000 blob %s\tl\0' "$nulink" | bg mktree -z)" "$t/bnul"
# a gitlink is never sent to cat-file
trg=$(printf '160000 commit %s\tsub\0' 1111111111111111111111111111111111111111 | bg mktree -z --missing); : >"$STUB_LOG"
PATH=$sb:$PATH export_tree "$bx" "$trg" "$t/bgl" >/dev/null 2>&1
expect "a gitlink becomes an empty directory and starts no cat-file" test -d "$t/bgl/sub" -a "$(grep -c 'cat-file' "$STUB_LOG")" = 0
# identity with git archive on this very repository's tree (it has no .gitattributes, so archive writes the same bytes)
repo_root=$(cd "$here/../.." && pwd)
rm -rf "$t/arc" "$t/exp-self"; mkdir -p "$t/arc"; git -C "$repo_root" archive HEAD | tar -x -C "$t/arc"
export_tree "$repo_root" HEAD "$t/exp-self" >/dev/null 2>&1
expect "this repository's tree: diff -r against git archive shows nothing" diff -r "$t/arc" "$t/exp-self"
expect "...and the copy re-hashes to the commit's tree id" test "$(rehash "$t/exp-self" "$(git -C "$repo_root" rev-parse --absolute-git-dir)")" = "$(git -C "$repo_root" rev-parse 'HEAD^{tree}')"

expect "no test reached the real dh.sh or docker (the default DH_SH stub was never called)" test ! -s "$t/dh-real.log"
[ -s "$t/dh-real.log" ] && { echo "calls that reached the dh.sh stub:" >&2; cat "$t/dh-real.log" >&2; }
echo "$n tests, $fails failed"
[ "$fails" = 0 ]
