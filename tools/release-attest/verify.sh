#!/usr/bin/env bash
# Verify the signed attestation a release commit carries.
#
#   tools/release-attest/verify.sh [--repo .] [--commit <rev>] [--expect-version x.y.z] [--pubkey <file>]
#
# The public key is committed at packaging/release-attest.pub and read from the commit being
# checked, unless --pubkey names another file. Exit 0 valid, 1 invalid (every reason is printed),
# 2 usage. It proves a holder of the signing key signed a green run of a tree whose hash equals this
# commit's top-level tree minus the release-attest entry (see tree_hash in config.sh), at the version the tree declares. It cannot stop a writer
# editing this script or the workflow in the same push; the writers are the owner and the release bot.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
. "$here/config.sh"

repo=. commit=HEAD expect= pubkey= file= sigonly=0
while [ $# -gt 0 ]; do
  case $1 in
    --repo) repo=$2; shift 2 ;;
    --commit) commit=$2; shift 2 ;;
    --expect-version) expect=$2; shift 2 ;;
    --pubkey) pubkey=$2; shift 2 ;;
    --file) file=$2; shift 2 ;;        # with --signature-only: check a file on disk (attest.sh uses it)
    --signature-only) sigonly=1; shift ;;
    *) echo "usage: verify.sh [--repo d] [--commit rev] [--expect-version x.y.z] [--pubkey file]" >&2; exit 2 ;;
  esac
done

work=$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/attest-verify.XXXXXX") || { echo "cannot make a temp directory; nothing verified" >&2; exit 2; }
[ -n "$work" ] && [ -d "$work" ] || { echo "the temp directory is unusable; nothing verified" >&2; exit 2; }
trap 'rm -rf "$work"' EXIT

check_sig() { # json sig pubkeyfile
  awk -v id="$IDENTITY" 'NR == 1 { print id, $1, $2 }' "$3" >"$work/allowed_signers"
  ssh-keygen -Y verify -f "$work/allowed_signers" -I "$IDENTITY" -n "$NAMESPACE" -s "$2" <"$1" 2>&1
}

if [ $sigonly = 1 ]; then
  check_sig "$file" "$file.sig" "$pubkey" >/dev/null
  exit $?
fi

bad=()
show() { git_objects -C "$repo" show "$commit:$1" 2>/dev/null; }
if ! show "$ATTEST_DIR/$ATTEST_FILE" >"$work/a.json"; then
  echo "not valid: $commit has no $ATTEST_DIR/$ATTEST_FILE. Run tools/release-attest/attest.sh on this tree, commit the two files it writes, and push."
  exit 1
fi
show "$ATTEST_DIR/$ATTEST_FILE.sig" >"$work/a.sig" || bad+=("$commit has no $ATTEST_DIR/$ATTEST_FILE.sig")
if [ -n "$pubkey" ]; then
  [ -f "$pubkey" ] && [ -r "$pubkey" ] || { bad+=("--pubkey $pubkey is not a readable file"); pubkey=; }
else
  if show "$PUBKEY_PATH" >"$work/pub"; then pubkey=$work/pub
  else bad+=("$commit has no $PUBKEY_PATH: the owner has to commit the release public key"); fi
fi
if [ -n "$pubkey" ] && [ -f "$pubkey" ] && ! awk 'NR == 1 { exit ($1 != "ssh-ed25519") }' "$pubkey"; then
  bad+=("the release public key is not an ssh-ed25519 key (a placeholder until the owner provisions it, see docs/RELEASE.md)"); pubkey=
fi
jq -e . "$work/a.json" >/dev/null 2>&1 || { echo "not valid: $ATTEST_FILE is not valid JSON"; exit 1; }

tree=$(tree_hash "$repo" "$commit")
version=$(declared_version "$repo" "$commit")
j() { jq -r "$1" "$work/a.json"; }
[ -z "$expect" ] || [ "$expect" = "$version" ] || bad+=("the tree declares $version, not the expected $expect")
[ "$(j .schema)" = 1 ] || bad+=("schema is $(j .schema), want 1")
[ "$(j .kind)" = "$KIND" ] || bad+=("kind is $(j .kind), want $KIND")
[ "$(j '.subset // false')" = false ] || bad+=("this is a subset run, which attests nothing about a release")
[ "$(j .tree.algo)" = "$TREE_ALGO" ] || bad+=("tree.algo is $(j .tree.algo), want $TREE_ALGO")
[ "$(j .tree.hash)" = "$tree" ] || bad+=("tree.hash $(j '.tree.hash[0:12]') is not this commit's tree ${tree:0:12}: the code changed after the run, so it needs a new run")
[ "$(j .version.declared)" = "$version" ] || bad+=("version.declared $(j .version.declared) is not the version this tree declares ($version)")
[ "$(j .result.verdict)" = green ] || bad+=("result.verdict is $(j .result.verdict), want green")
jq -e '(.result.failed | type) == "number" and .result.failed == 0' "$work/a.json" >/dev/null || bad+=("result.failed is $(j .result.failed), want 0")
jq -e '(.result.passed | type) == "number" and .result.passed > 0' "$work/a.json" >/dev/null || bad+=("result.passed is 0: a run of nothing proves nothing")
jq -e '[.result.total, .result.passed, .result.failed, .result.ignored] | all(type == "number") and .[0] == .[1] + .[2] + .[3]' "$work/a.json" >/dev/null || bad+=("result.total is not passed + failed + ignored")
while IFS='|' read -r name _; do
  s=$(jq -c --arg n "$name" '[.suites[]? | select(.name == $n)] | first // empty' "$work/a.json")
  if [ -z "$s" ]; then bad+=("suite $name is missing from the attestation"); continue; fi
  jq -e '(.exit | type) == "number" and .exit == 0' <<<"$s" >/dev/null || bad+=("suite $name exited $(jq -r .exit <<<"$s")")
  jq -e '(.failed | type) == "number" and .failed == 0' <<<"$s" >/dev/null || bad+=("suite $name has $(jq -r .failed <<<"$s") failed cells")
  if ! jq -e '(.passed | type) == "number" and .passed > 0' <<<"$s" >/dev/null; then bad+=("suite $name passed no cells"); fi
done <<<"$SUITES"
checked=0
if [ -f "$work/a.sig" ] && [ -n "$pubkey" ] && [ -f "$pubkey" ]; then
  checked=1
  out=$(check_sig "$work/a.json" "$work/a.sig" "$pubkey") || bad+=("the signature does not verify: $out")
fi
[ $checked = 1 ] || bad+=("the signature was not checked")

if [ ${#bad[@]} -eq 0 ]; then
  echo "attestation ok: $(j .result.passed) passed, $(j .result.ignored) ignored, version $version, tree ${tree:0:12}"
  exit 0
fi
for p in "${bad[@]}"; do echo "not valid: $p"; done
exit 1
