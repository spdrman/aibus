#!/usr/bin/env bash
# Run the release suites in a fenced throw-away container and sign the result.
#
#   tools/release-attest/attest.sh [--key ~/.ssh/aibus-release-attest] [--host mercury]
#       [--out release-attest] [--replace] [--runner-cmd 'sh fake.sh']
#
# The suites in config.sh (the 'every push' rows of docs/TESTING.md) run in one container through `/Users/rom/workspace/dh.sh run <host> <job> <image> sh -c ...`,
# which fences the container: memory limit, swap equal to it (so none) and a CPU quota. Override with
# DH_MEMORY (e.g. 8g) and DH_CPUS (e.g. 4). Nothing here runs a benchmark or a fuzz target.
# It refuses a dirty tree (anything outside release-attest/), a missing key or .pub, an existing
# attestation without --replace, and a red run. It then writes attestation.json, signs it with
# `ssh-keygen -Y sign` and checks the signature against the .pub.
# Exit codes: 0 signed, 1 not green or does not add up, 2 refused before anything ran.
# --runner-cmd stands in for dh.sh in the tests: it gets the lane script in $LANE_SCRIPT.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
. "$here/config.sh"
DH=${DH_SH:-/Users/rom/workspace/dh.sh}
unset DH_UNFENCED   # the run is always fenced, whatever the calling shell exports

repo=. key=$KEY_HINT host=${DH_HOST:-mercury} out= replace=0 runner=
while [ $# -gt 0 ]; do
  case $1 in
    --repo) repo=$2; shift 2 ;;
    --key) key=$2; shift 2 ;;
    --host) host=$2; shift 2 ;;
    --out) out=$2; shift 2 ;;
    --replace) replace=1; shift ;;
    --runner-cmd) runner=$2; shift 2 ;;
    *) echo "usage: attest.sh [--repo d] [--key k] [--host h] [--out d] [--replace] [--runner-cmd c]" >&2; exit 2 ;;
  esac
done
repo=$(cd "$repo" && pwd)
: "${out:=$repo/$ATTEST_DIR}"
refuse() { echo "refused: $1" >&2; exit 2; }

[ -z "$(git -C "$repo" status --porcelain -- . ":(exclude)$ATTEST_DIR")" ] || refuse "the tree is dirty (outside $ATTEST_DIR/): a release has to describe a tree somebody can check out"
[ -f "$key" ] || refuse "no signing key at $key"
[ -f "$key.pub" ] || refuse "no public key next to the signing key ($key.pub)"
ssh-keygen -l -f "$key.pub" >/dev/null 2>&1 || refuse "$key.pub is not a public key ssh-keygen can read"
for f in "$ATTEST_FILE" "$ATTEST_FILE.sig"; do
  if [ -e "$out/$f" ] && [ $replace = 0 ]; then refuse "$out/$f already exists; pass --replace"; fi
done

commit=$(git -C "$repo" rev-parse HEAD)
work=$(mktemp -d "${TMPDIR:-/tmp}/attest.XXXXXX")
job=attest-aibus-${commit:0:8}-$$
pushed=0
cleanup() { [ $pushed = 0 ] || "$DH" clean "$host" "$job" >/dev/null 2>&1 || true; rm -rf "$work"; }
trap cleanup EXIT

lane=$work/lane.sh
nonce=$(new_nonce); [ -n "$nonce" ] || refuse "could not make a nonce (od on /dev/urandom gave nothing); nothing ran"
lane_script "$nonce" >"$lane"

started=$(date -u +%Y-%m-%dT%H:%M:%SZ)
if [ -n "$runner" ]; then
  LANE_NONCE=$nonce LANE_SCRIPT=$(cat "$lane") sh -c "$runner" >"$work/lane.log" 2>&1 && lane_rc=0 || lane_rc=$?
else
  mkdir "$work/tree"
  export_tree "$repo" HEAD "$work/tree" || refuse "could not copy the tree out of git; nothing ran"
  pushed=1   # before the push, so a push that dies half way still gets its volume cleaned
  "$DH" push "$host" "$job" "$work/tree" >/dev/null || refuse "pushing the tree to $host failed"
  "$DH" run "$host" "$job" "$IMAGE" sh -c "$(cat "$lane")" >"$work/lane.log" 2>&1 && lane_rc=0 || lane_rc=$?
fi
finished=$(date -u +%Y-%m-%dT%H:%M:%SZ)

# Per-suite exit and cell counts out of cargo's `test result: ok. N passed; M failed; K ignored;`.
suites_json='[]'
while IFS='|' read -r name _; do
  part=$work/$name.log
  awk -v n="$name" -v x="$nonce" '$0 == "=== SUITE " n " " x " ===" {on=1; next} $0 ~ "^=== EXIT " n " [0-9]+ " x " ===$" {on=0} on' "$work/lane.log" >"$part"
  exit_code=$(suite_exit "$work/lane.log" "$name" "$nonce" || true)
  : "${exit_code:=1}"
  [ "$lane_rc" = 0 ] || exit_code=1   # the container itself died (OOM kill is 137): nothing it printed counts
  read -r p f i < <(awk '/^test result:/ { for (k = 1; k <= NF; k++) { if ($k ~ /^passed;/) p += $(k-1); if ($k ~ /^failed;/) f += $(k-1); if ($k ~ /^ignored;/) i += $(k-1) } } END { print p+0, f+0, i+0 }' "$part")
  echo "$host: $name exit $exit_code, $p passed, $f failed" >&2
  suites_json=$(jq -c --arg n "$name" --argjson e "$exit_code" --argjson p "$p" --argjson f "$f" --argjson i "$i" \
    '. + [{name: $n, exit: $e, passed: $p, failed: $f, ignored: $i}]' <<<"$suites_json")
done <<<"$SUITES"

att=$(jq -n --argjson s "$suites_json" --arg commit "$commit" --arg kind "$KIND" --arg algo "$TREE_ALGO" \
  --arg tree "$(tree_hash "$repo" HEAD)" --arg ver "$(declared_version "$repo" HEAD)" --arg host "$host" \
  --arg a "$started" --arg b "$finished" '
  ([$s[].failed] | add) as $failed |
  ([$s[].passed] | add) as $passed |
  ([$s[].ignored] | add) as $ignored |
  {schema: 1, kind: $kind, commit: $commit, tree: {hash: $tree, algo: $algo}, version: {declared: $ver},
   run: {host: $host, startedAt: $a, finishedAt: $b}, suites: $s,
   result: {verdict: (if ($s | all(.exit == 0)) and $failed == 0 and $passed > 0 then "green" else "red" end),
            total: ($passed + $failed + $ignored), passed: $passed, failed: $failed, ignored: $ignored}}')
verdict=$(jq -r .result.verdict <<<"$att")
if [ "$verdict" != green ]; then
  keep=${TMPDIR:-/tmp}/attest-logs-$$; mkdir -p "$keep"; cp "$work"/*.log "$keep"/
  echo "not green: $(jq -c .result <<<"$att"); logs kept in $keep" >&2
  exit 1
fi

mkdir -p "$out"
rm -f "$out/$ATTEST_FILE.sig"
printf '%s\n' "$att" | jq . >"$out/$ATTEST_FILE"
ssh-keygen -Y sign -f "$key" -n "$NAMESPACE" "$out/$ATTEST_FILE" >/dev/null
[ -f "$out/$ATTEST_FILE.sig" ] || { echo "ssh-keygen -Y sign failed" >&2; exit 1; }
"$here/verify.sh" --signature-only --file "$out/$ATTEST_FILE" --pubkey "$key.pub" >/dev/null || { echo "the signature does not verify against $key.pub" >&2; exit 1; }
echo "signed $out/$ATTEST_FILE: $(jq -r '.result.passed' <<<"$att") passed, tree $(jq -r '.tree.hash[0:12]' <<<"$att")"
