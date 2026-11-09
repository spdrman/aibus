#!/bin/sh
# Smoke-runs the four fuzz targets (10 section 1), each for a bounded time. Meant to run INSIDE a
# throw-away container with the repository at the working directory (see docs/SECURITY.md), never
# on a host: it installs a nightly toolchain and cargo-fuzz.
#
#   tools/fuzz.sh [seconds-per-target]     default 60
#
# The corpus lands in fuzz/corpus/<target>/ and a crash in fuzz/artifacts/<target>/. A crash is
# copied to fixtures/fuzz/<target>/ by hand once it is understood; crates/aibus-core/tests/security/fuzz.rs
# replays everything in both places on every push.
set -eu
secs=${1:-60}
rustup toolchain install nightly --profile minimal >/dev/null 2>&1
command -v cargo-fuzz >/dev/null 2>&1 || cargo +nightly install cargo-fuzz --locked >/dev/null 2>&1
# The lockfile of the fuzz workspace starts from the root one, so the MSRV pins hold here too.
[ -f fuzz/Cargo.lock ] || cp Cargo.lock fuzz/Cargo.lock
status=0
for target in tool_pattern redactor agent_def handoff; do
  echo "=== $target (${secs}s)"
  cargo +nightly fuzz run "$target" -- "-max_total_time=$secs" -rss_limit_mb=2048 2>&1 | tail -n 25 || status=1
  # `tail` hides the exit status of the fuzzer, so look for what a crash leaves behind.
  if [ -n "$(ls fuzz/artifacts/"$target" 2>/dev/null)" ]; then
    echo "crash artifacts for $target:"; ls fuzz/artifacts/"$target"; status=1
  fi
done
exit $status
