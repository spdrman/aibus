#!/usr/bin/env bash
# Tests for tools/release-attest. A throwaway key and a throwaway git repo in a temp dir; the
# runner is a fake, so no container starts and no cargo runs. Needs bash, git, jq and ssh-keygen.
set -u
here=$(cd "$(dirname "$0")" && pwd)
tools=$(cd "$here/../../tools/release-attest" && pwd)
. "$tools/config.sh"
t=$(mktemp -d "${TMPDIR:-/tmp}/attest-test.XXXXXX")
trap 'rm -rf "$t"' EXIT
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

echo "$n tests, $fails failed"
[ "$fails" = 0 ]
