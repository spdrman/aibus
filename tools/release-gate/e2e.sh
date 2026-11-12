#!/usr/bin/env bash
# The full e2e for a merge into `release`, in a fenced throw-away container; writes the stamp on green.
#
#   tools/release-gate/e2e.sh [--merge-index] [--host mars|nas|mercury|local]
#
# Runs the suites of tools/release-attest/config.sh (the "every push" rows of docs/TESTING.md) in one
# container through /Users/rom/workspace/dh.sh: memory limit, swap equal to it, CPU quota (DH_MEMORY,
# DH_CPUS override). Host: --host, RELEASE_E2E_HOST, else `dh.sh pick`. Refuses a dirty tree. With
# --merge-index (the pre-merge-commit hook) the tree under test is the index, i.e. the merge result.
# The stamp ($(git rev-parse --git-common-dir)/release-e2e/<tree key>) is written only on green.
# Exit: 0 green (or skipped), 1 red, 2 refused.
# RELEASE_E2E_CMD='<cmd>' replaces the container run with a local command in the exported tree. For the
# tests of this gate only: its stamps go to release-e2e-fake/, which pre-push never reads, unless
# RELEASE_E2E_TEST_STAMPS equals this repo's stamp directory (set only by tests/release-gate/run.sh). RELEASE_E2E_SKIP=<reason> skips the gate (docs/RELEASE.md says when).
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
DH=${DH_SH:-/Users/rom/workspace/dh.sh}
unset DH_UNFENCED
refuse() { echo "refused: $1" >&2; exit 2; }

merge=0 host=${RELEASE_E2E_HOST:-}
while [ $# -gt 0 ]; do
  case $1 in
    --merge-index) merge=1; shift ;;
    --host) host=$2; shift 2 ;;
    *) echo "usage: e2e.sh [--merge-index] [--host h]" >&2; exit 2 ;;
  esac
done
gate_skipped "e2e.sh" && exit 0

if [ $merge = 1 ]; then
  git diff --quiet || refuse "unstaged changes in the working tree"
  [ -z "$(git ls-files --others --exclude-standard)" ] || refuse "untracked files in the working tree"
  tree=$(git write-tree)
else
  [ -z "$(git status --porcelain)" ] || refuse "the tree is dirty; commit or stash first"
  tree=$(git rev-parse 'HEAD^{tree}')
fi
key=$(tree_key "$tree")

sdir=$(stamp_dir)
work=$(mktemp -d "${TMPDIR:-/tmp}/release-e2e.XXXXXX")
job=aibus-reh-${key:0:8}-$$
pushed=0
cleanup() { [ $pushed = 0 ] || "$DH" clean "$host" "$job" >/dev/null 2>&1 || true; rm -rf "$work"; }
trap cleanup EXIT
trap 'exit 130' INT TERM
mkdir "$work/tree"
export_tree . "$tree" "$work/tree" || refuse "could not copy the tree out of git; nothing ran"
started=$(date -u +%Y-%m-%dT%H:%M:%SZ)

if [ -n "${RELEASE_E2E_CMD:-}" ]; then
  host=fake-local; cmd=$RELEASE_E2E_CMD
  echo "!!! RELEASE_E2E_CMD is set: TEST ONLY, nothing real runs. Unset it; the stamp goes to release-e2e-fake/ and does not open pre-push." >&2
  test_stamps_allowed || sdir=$(stamp_dir)-fake
  if (cd "$work/tree" && sh -c "$cmd"); then green=1; else green=0; fi
else
  [ -n "$host" ] || host=$("$DH" pick any)
  [ -n "$host" ] || refuse "dh.sh found no docker host up"
  cmd="suites of tools/release-attest/config.sh in $IMAGE"
  host=$(printf '%s' "$host" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//' | tr 'A-Z' 'a-z')
  host_ok "$host" || refuse "host '$host' is not one dh.sh knows (mars, nas, mercury, local); pre-push would refuse its stamp"
  nonce=$(new_nonce); [ -n "$nonce" ] || refuse "could not make a nonce (od on /dev/urandom gave nothing); nothing ran"
  lane_script "$nonce" >"$work/lane.sh"
  echo "e2e: $host, job $job (fenced: DH_MEMORY=${DH_MEMORY:-default} DH_CPUS=${DH_CPUS:-default})" >&2
  pushed=1   # before the push, so a push that dies half way still gets its volume cleaned
  "$DH" push "$host" "$job" "$work/tree" >/dev/null || refuse "pushing the tree to $host failed (is the docker host up?)"
  set +e
  "$DH" run "$host" "$job" "$IMAGE" sh -c "$(cat "$work/lane.sh")" 2>&1 | tee "$work/lane.log" | grep -E '^(=== |test result: FAILED|error)' >&2
  rc=${PIPESTATUS[0]}
  set -e
  # green: the container exited 0 and every suite has exactly one genuine (nonce) EXIT marker, 0
  green=1
  [ "$rc" = 0 ] || { green=0; echo "the container run exited $rc (137 is an OOM kill)" >&2; }
  while IFS='|' read -r name _; do
    [ "$(suite_exit "$work/lane.log" "$name" "$nonce" || true)" = 0 ] || green=0
  done <<<"$SUITES"
  grep -q '^test result: FAILED' "$work/lane.log" && green=0
  if [ $green = 0 ]; then
    keep=${TMPDIR:-/tmp}/release-e2e-log-$$; cp "$work/lane.log" "$keep"; echo "log kept in $keep (a container killed for memory shows up as a missing EXIT line)" >&2
  fi
fi

if [ $green != 1 ]; then echo "e2e RED: no stamp written" >&2; exit 1; fi
mkdir -p "$sdir"
tmp=$(mktemp "$sdir/.tmp.XXXXXX")
printf 'tree=%s\ncommand=%s\nhost=%s\nstarted=%s\nfinished=%s\ngate_sha256=%s\n' "$key" "$cmd" "$host" "$started" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$(gate_hash)" >"$tmp"
mv "$tmp" "$sdir/$key"
echo "e2e green: stamp for tree $key on $host" >&2
