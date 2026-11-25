#!/usr/bin/env bash
# The B1.5 (#308) timing probe, from a checkout to a number. It builds the `timing_probe` test of
# crates/aibus-bridge/tests/phase1_gate_308.rs in a container (release, locked), ships the binary to
# the host that does the measuring and runs it three times while holding the bench queue's turn.
#
# Needs two helpers from the machine you run it on (set both, there are no defaults):
#   DH           path of dh.sh        (docker on a named host, trees travel as tar into named volumes)
#   BENCH_QUEUE  path of bench-queue.sh (one benchmark at a time on the measuring host)
# Optional: BUILD_HOST (default local) and RUN_HOST (default mercury) must have the same CPU
# architecture, because the binary is built on one and run on the other. JOB prefixes every
# volume and queue ticket (default s308-probe).
#
# What it prints is host facts, the binary's sha256 and one JSON line per dataset per run (see
# docs/desktop/B1.5-phase1-gate.md for what each field is). It asserts no time: timing is never a gate.
set -euo pipefail
: "${DH:?path of dh.sh}" "${BENCH_QUEUE:?path of bench-queue.sh}"
build=${BUILD_HOST:-local}; run=${RUN_HOST:-mercury}; job=${JOB:-s308-probe}
repo=$(cd "$(dirname "$0")/../.." && pwd)
tmp=$(/usr/bin/mktemp -d)
cleanup() {
  rm -rf "$tmp"
  # the four volumes this script made: two job volumes (dh.sh clean) and the two cargo caches
  "$DH" clean "$build" "$job-src" >/dev/null 2>&1 || true
  "$DH" clean "$run" "$job" >/dev/null 2>&1 || true
  "$DH" d "$build" volume rm -f "$job-target" "$job-registry" >/dev/null 2>&1 || true
}
trap cleanup EXIT

# 1. Build with the tree at /work: the test finds its fixture through CARGO_MANIFEST_DIR, which is
#    baked into the binary, so the run host needs the fixture at the same path.
"$DH" push "$build" "$job-src" "$repo" >&2
DH_CACHE_VOLUMES="$job-target:/tmp/target $job-registry:/usr/local/cargo/registry" \
  "$DH" run "$build" "$job-src" rust:1.89-bookworm sh -c \
  'CARGO_TARGET_DIR=/tmp/target cargo test --release --locked -p aibus-bridge --test phase1_gate_308 --no-run \
   && cp /tmp/target/release/deps/phase1_gate_308-???????????????? /work/probe' >&2
"$DH" d "$build" run --rm -v "j-$job-src:/w" alpine:3.20 cat /w/probe > "$tmp/probe"
chmod +x "$tmp/probe"

# 2. The run host gets the binary and the one fixture it reads, at /work.
mkdir -p "$tmp/crates/aibus-bridge" "$tmp/fixtures/claude-code/traces-284"
cp -R "$repo/fixtures/claude-code/traces-284/07-deleg" "$tmp/fixtures/claude-code/traces-284/"
"$DH" push "$run" "$job" "$tmp" >&2

# 3. Measure, holding the turn only for this.
DH_CPUS=${DH_CPUS:-4} DH_MEMORY=${DH_MEMORY:-8g} "$BENCH_QUEUE" dh "$job" debian:bookworm-slim sh -c '
  uname -srm; nproc; grep -m1 "^model name\|^Features" /proc/cpuinfo; grep MemTotal /proc/meminfo; cat /proc/loadavg
  sha256sum /work/probe
  for i in 1 2 3; do echo "RUN $i"; /work/probe --ignored --exact timing_probe --nocapture | grep "^{"; done
  cat /proc/loadavg'
