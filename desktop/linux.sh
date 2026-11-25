#!/bin/sh
# The B1.1 desktop spike on Linux, inside desktop/Dockerfile.linux with the repository at /src.
#
#   sh linux.sh gates   rustfmt on the host crate, clippy and check (locked), tsc, vitest
#   sh linux.sh test    the host crate's own tests (fixture shape, provenance, boundary, golden)
#   sh linux.sh build   release build of the app through the Tauri CLI, no bundle
#   sh linux.sh run     start the release build under Xvfb, sample memory and CPU for 30 s,
#                       take a screenshot after the replay, print the app log
#   sh linux.sh all     every step above, in that order
#
# Nothing here touches a provider, a key or the network beyond npm and crates.io.
set -eu
cd "$(dirname "$0")"
step=${1:-all}

npm_install() { [ -d node_modules ] || npm ci --no-audit --no-fund; }

gates() {
  ( cd src-tauri
    rustfmt --check --edition 2021 build.rs src/lib.rs src/main.rs
    # tauri-build wants ../dist to exist even for a check; an empty page is enough here.
    [ -f ../dist/index.html ] || { mkdir -p ../dist && echo '<!doctype html>' > ../dist/index.html; }
    cargo clippy --all-targets --locked -- -D warnings
    cargo check --all-targets --locked )
  npm_install
  npx tsc --noEmit
  npx vitest run --reporter verbose
}

test_host() {
  [ -f dist/index.html ] || { mkdir -p dist && echo '<!doctype html>' > dist/index.html; }
  ( cd src-tauri && cargo test --locked )
}

build() {
  npm_install
  npx tauri build --no-bundle -- --locked
  ls -l "$CARGO_TARGET_DIR/release/aibus-desktop"
}

run() {
  # bubblewrap cannot make user namespaces in a plain container, so the WebKit sandbox is off here
  # (it stays on on a real desktop), and software rendering stands in for a GPU.
  export WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1 WEBKIT_DISABLE_COMPOSITING_MODE=1 \
         LIBGL_ALWAYS_SOFTWARE=1 GDK_BACKEND=x11 NO_AT_BRIDGE=1 DISPLAY=:99
  bin=$CARGO_TARGET_DIR/release/aibus-desktop
  out=${OUT:-/src/desktop/run-out}
  mkdir -p "$out"
  echo "host: $(uname -m), $(nproc) cpus, $(dpkg -l libwebkit2gtk-4.1-0 | awk '/^ii/{print "webkit2gtk-4.1 " $3}')"
  echo "binary: $(stat -c %s "$bin") bytes (release, opt-level s, lto, stripped)"
  Xvfb :99 -screen 0 1280x800x24 >/dev/null 2>&1 &
  xvfb=$!
  sleep 2
  t0=$(date +%s%N)
  dbus-run-session -- "$bin" >"$out/app.log" 2>&1 &
  app=$!
  mapped=no
  for _ in $(seq 1 150); do
    if xwininfo -root -tree 2>/dev/null | grep -q 'aibus Desktop'; then mapped=yes; break; fi
    sleep 0.1
  done
  echo "window mapped: $mapped after $(( ($(date +%s%N) - t0) / 1000000 )) ms"
  tree() { echo "$1"; for c in $(pgrep -P "$1" 2>/dev/null); do tree "$c"; done; }
  ticks=$(getconf CLK_TCK)
  peak=0; prev=0; cpu_sum=0; n=0
  for s in $(seq 1 30); do
    sleep 1
    pids=$(tree $app)
    pss=0; cpu=0
    for p in $pids; do
      v=$(awk '/^Pss:/{print $2}' /proc/$p/smaps_rollup 2>/dev/null || true); pss=$((pss + ${v:-0}))
      c=$(awk '{print $14+$15}' /proc/$p/stat 2>/dev/null || true); cpu=$((cpu + ${c:-0}))
    done
    [ "$pss" -gt "$peak" ] && peak=$pss
    d=$((cpu - prev)); prev=$cpu
    if [ "$s" -gt 1 ]; then cpu_sum=$((cpu_sum + d)); n=$((n + 1)); fi
    if [ "$s" -eq 15 ]; then
      import -window root "$out/screenshot.png" 2>/dev/null || echo "screenshot failed"
      echo "processes at t=15s (pid rss_kb pss_kb comm):"
      for p in $pids; do
        echo "  $p $(awk '/^VmRSS:/{print $2}' /proc/$p/status 2>/dev/null) $(awk '/^Pss:/{print $2}' /proc/$p/smaps_rollup 2>/dev/null) $(cat /proc/$p/comm 2>/dev/null)"
      done
    fi
    echo "t=${s}s pss_kb=$pss cpu_ticks_delta=$d"
  done
  echo "peak PSS: ${peak} kB"
  echo "mean CPU over t=2..30s: $(awk -v t=$cpu_sum -v n=$n -v h=$ticks 'BEGIN{printf "%.1f", 100*t/h/n}')% of one core"
  kill $app 2>/dev/null || true; wait $app 2>/dev/null || true
  kill $xvfb 2>/dev/null || true
  echo "--- app log ---"; cat "$out/app.log"
}

case $step in
  gates) gates ;;
  test) test_host ;;
  build) build ;;
  run) run ;;
  all) gates; test_host; build; run ;;
  *) echo "usage: sh linux.sh gates|test|build|run|all" >&2; exit 2 ;;
esac
