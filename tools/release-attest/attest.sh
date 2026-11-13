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
# attestation without --replace, a key that cannot sign without a prompt (checked first, by signing a
# scratch file, so the long run is not wasted on it: load the key with ssh-add) and a red run. It then
# signs attestation.json with `ssh-keygen -Y sign`, checks the signature against the .pub and only then
# moves both files into release-attest/, so a failed run leaves nothing there.
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
command -v perl >/dev/null || refuse "perl is not installed (the sign preflight needs it)"
[ ! -e "$out" ] || [ -d "$out" ] || refuse "$out exists and is not a directory"
[ -f "$key" ] || refuse "no signing key at $key"
[ -f "$key.pub" ] || refuse "no public key next to the signing key ($key.pub)"
ssh-keygen -l -f "$key.pub" >/dev/null 2>&1 || refuse "$key.pub is not a public key ssh-keygen can read"
for f in "$ATTEST_FILE" "$ATTEST_FILE.sig"; do
  if [ -e "$out/$f" ] && [ $replace = 0 ]; then refuse "$out/$f already exists; pass --replace"; fi
done

commit=$(git -C "$repo" rev-parse HEAD)
work=$(mktemp -d "${TMPDIR:-/tmp}/attest.XXXXXX") || { echo "refused: could not make a temp directory; nothing ran" >&2; exit 2; }
[ -n "$work" ] && [ -d "$work" ] || { echo "refused: the temp directory is unusable; nothing ran" >&2; exit 2; }
job=attest-aibus-${commit:0:8}-$$
pushed=0
cleanup() { [ $pushed = 0 ] || "$DH" clean "$host" "$job" >/dev/null 2>&1 || true; rm -rf "$work"; }
trap cleanup EXIT

# detach: run a command in a NEW SESSION, so it has no controlling terminal (ssh-keygen opens /dev/tty itself and would
# prompt there). The parent forks first, because setsid() fails (EPERM, returned as -1, which is true in Perl) for a
# process group leader, which is what the process is when job control is on. It forwards INT/TERM/HUP to the child and
# returns the child's exit status (128+n after a signal). The wrapper's own failure (fork, setsid, exec) exits 125.
DETACH_PL='use POSIX; my $p = fork; defined $p or do { warn "fork: $!\n"; exit 125 };
if ($p) { $SIG{INT} = $SIG{TERM} = $SIG{HUP} = sub { kill "TERM", $p }; 1 while waitpid($p, 0) < 0 && $! == EINTR; exit($? & 127 ? 128 + ($? & 127) : $? >> 8) }
POSIX::setsid() > 0 or do { warn "setsid: $!\n"; exit 125 }; exec @ARGV or do { warn "exec: $!\n"; exit 125 }'
detach() { SSH_ASKPASS_REQUIRE=never perl -e "$DETACH_PL" -- "$@" </dev/null; }

# Sign preflight: the same key, namespace and no-prompt settings as the real signature, on a scratch file.
# A passphrase key that is not in ssh-agent fails here, not after the container run.
mkdir "$work/pf" && echo preflight >"$work/pf/probe" || refuse "could not make the preflight scratch file; nothing ran"
detach ssh-keygen -Y sign -f "$key" -n "$NAMESPACE" "$work/pf/probe" >/dev/null 2>"$work/pf/err" && [ -s "$work/pf/probe.sig" ] || {
  skrc=$?; [ $skrc != 125 ] || refuse "cannot detach the terminal for the key check ($(tr '\n' ' ' <"$work/pf/err")); nothing ran"
  echo "   ssh-keygen exited $skrc" >&2
  refuse "the key cannot sign without a prompt: load it with ssh-add $key first"; }

lane=$work/lane.sh
nonce=$(new_nonce); [ -n "$nonce" ] || refuse "could not make a nonce (od on /dev/urandom gave nothing); nothing ran"
lane_script "$nonce" >"$lane" && [ -s "$lane" ] || refuse "could not write the lane script; nothing ran"

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
  keep=$(mktemp -d "${TMPDIR:-/tmp}/attest-logs.XXXXXX") && [ -n "$keep" ] && [ -d "$keep" ] || { echo "not green: $(jq -c .result <<<"$att"); could not make a directory for the logs" >&2; exit 1; }
  cp "$work"/*.log "$keep"/
  echo "not green: $(jq -c .result <<<"$att"); logs kept in $keep" >&2
  exit 1
fi

# Sign in $work (detached too: if the key was loadable for the preflight it signs from the agent without asking; a key that
# dropped out of the agent mid-run fails here instead of prompting), then verify and install the two files.
printf '%s\n' "$att" | jq . >"$work/$ATTEST_FILE"
detach ssh-keygen -Y sign -f "$key" -n "$NAMESPACE" "$work/$ATTEST_FILE" >/dev/null 2>"$work/sign.err" && [ -f "$work/$ATTEST_FILE.sig" ] || {
  skrc=$?; echo "ssh-keygen -Y sign failed (exit $skrc): $(tr '\n' ' ' <"$work/sign.err")" >&2
  echo "the key may have left ssh-agent during the run: load it with ssh-add $key first, then run again" >&2; exit 1; }
# Both files go into $out through a stage directory inside it (same filesystem): copy, verify the copy, back up what is
# there, then two renames. A failure or a TERM/INT/HUP (SIGKILL cannot be caught) puts back what was there before (a
# --replace keeps the old pair) and removes only what this run made; the final names of an existing attestation are
# never deleted. If a restore itself fails the stage directory is kept, it holds the old copy.
made=0
if [ ! -d "$out" ]; then mkdir -p "$out" || { echo "could not make $out" >&2; exit 1; }; made=1; fi
stage=$(mktemp -d "$out/.attest-stage.XXXXXX") && [ -n "$stage" ] && [ -d "$stage" ] || { echo "could not make a stage directory in $out" >&2; if [ $made = 1 ]; then rmdir "$out" 2>/dev/null || true; fi; exit 1; }
keepstage=0
undo() {
  local i=0 f
  for f in "$ATTEST_FILE" "$ATTEST_FILE.sig"; do
    if [ -f "$stage/old.$i" ]; then
      cp -p "$stage/old.$i" "$out/$f" || { keepstage=1; echo "could not restore $out/$f; the old copy is in $stage/old.$i" >&2; }
    elif [ ! -e "$stage/$f" ]; then rm -f "$out/$f"   # no old copy and the staged file is gone: this run renamed it in
    fi
    i=$((i + 1))
  done
}
leave() {
  [ $keepstage = 0 ] || { echo "the stage directory $stage was kept" >&2; return 0; }
  rm -f "$stage/$ATTEST_FILE" "$stage/$ATTEST_FILE.sig" "$stage/old.0" "$stage/old.1"; rmdir "$stage" 2>/dev/null || true
  if [ $made = 1 ]; then rmdir "$out" 2>/dev/null || true; fi
}
failmove() { undo; leave; echo "${1:-could not move the signed attestation into $out}" >&2; exit 1; }
trap 'undo; leave; exit 143' INT TERM HUP
cp "$work/$ATTEST_FILE" "$stage/$ATTEST_FILE" && cp "$work/$ATTEST_FILE.sig" "$stage/$ATTEST_FILE.sig" || failmove
"$here/verify.sh" --signature-only --file "$stage/$ATTEST_FILE" --pubkey "$key.pub" >/dev/null || failmove "the signature does not verify against $key.pub"
i=0; for f in "$ATTEST_FILE" "$ATTEST_FILE.sig"; do
  if [ -f "$out/$f" ]; then cp -p "$out/$f" "$stage/old.$i" || failmove; fi
  i=$((i + 1))
done
mv -f "$stage/$ATTEST_FILE" "$out/$ATTEST_FILE" || failmove
mv -f "$stage/$ATTEST_FILE.sig" "$out/$ATTEST_FILE.sig" || failmove
trap - INT TERM HUP
leave
echo "signed $out/$ATTEST_FILE: $(jq -r '.result.passed' <<<"$att") passed, tree $(jq -r '.tree.hash[0:12]' <<<"$att")"
