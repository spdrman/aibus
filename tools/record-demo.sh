#!/usr/bin/env bash
# Rebuilds the README demo: docs/media/demo/msgs.jsonl and docs/media/aibus-ui.gif.
#
#   tools/record-demo.sh msgs    run docs/media/demo/script.ron on the fake provider with the real
#                                engine (`aibus run --record`) and keep the message stream
#   tools/record-demo.sh gif     play that stream into the dashboard (`aibus demo`) inside a
#                                pseudo-terminal, write a cast, render the GIF, check it
#   tools/record-demo.sh all     build, msgs, build again (the stream is compiled in), gif
#
# This runs inside the build container, never on a host: see docs/media/README.md for the
# `nas.sh` line. Nothing here touches a network except the one pinned download of the renderer,
# a real keychain or a real provider. The session runs under a throwaway $HOME.
set -euo pipefail

STAGE=${1:-all}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
TARGET=${CARGO_TARGET_DIR:-/tmp/target}
AIBUS=$TARGET/debug/aibus
DEMO=$ROOT/docs/media/demo
OUT=$ROOT/docs/media/aibus-ui.gif
WORK=${WORK:-/tmp/aibus-demo}

# The renderer: agg (asciinema), the release binary, checked against a pinned digest.
AGG_VERSION=v1.7.0
AGG_SHA256=593122aced7f11b367e397c06061eaadb7bd6ef7acde16832a3138d70105d26c
AGG_URL=https://github.com/asciinema/agg/releases/download/$AGG_VERSION/agg-x86_64-unknown-linux-musl
# DejaVu Sans Mono draws the box characters; the version is whatever the image's Debian ships.
FONT="DejaVu Sans Mono"
COLS=$(sed -n 's/^pub const COLS: u16 = \([0-9]*\);/\1/p' "$ROOT/crates/aibus-tui/src/demo.rs")
ROWS=$(sed -n 's/^pub const ROWS: u16 = \([0-9]*\);/\1/p' "$ROOT/crates/aibus-tui/src/demo.rs")
PROMPT=$(sed -n 's/^pub const PROMPT: &str = "\(.*\)";/\1/p' "$ROOT/crates/aibus-tui/src/demo.rs")

build() { (cd "$ROOT" && cargo build --locked -p aibus); }

msgs() {
  local home=$WORK/home
  rm -rf "$WORK/home" "$WORK/rec"
  mkdir -p "$home/proj/src" "$home/proj/.aibus/agents"
  printf '# demo\n\nA tiny project for the README recording.\n' >"$home/proj/README.md"
  printf 'fn main() {\n    println!("hello");\n}\n' >"$home/proj/src/main.rs"
  cp "$DEMO"/agents/*.md "$home/proj/.aibus/agents/"
  (
    cd "$home/proj"
    env -u XDG_CONFIG_HOME -u XDG_DATA_HOME -u XDG_STATE_HOME -u XDG_CACHE_HOME -u AIBUS_CONFIG \
      HOME="$home" "$AIBUS" run --json --record "$WORK/rec" --script "$DEMO/script.ron" \
      --advisor fake-large --router fake-small "$PROMPT" >"$WORK/out.ndjson" 2>"$WORK/err.txt" \
      || { cat "$WORK/err.txt" >&2; exit 1; }
  )
  cp "$WORK/rec/msgs.jsonl" "$DEMO/msgs.jsonl"
  echo "recorded $(wc -l <"$DEMO/msgs.jsonl") messages"
}

gif() {
  mkdir -p "$WORK"
  if ! command -v agg >/dev/null; then
    curl -fsSL -o "$WORK/agg" "$AGG_URL"
    echo "$AGG_SHA256  $WORK/agg" | sha256sum -c -
    chmod +x "$WORK/agg"
    export PATH=$WORK:$PATH
  fi
  fc-list | grep -q "$FONT" || { apt-get update -qq && apt-get install -y -qq --no-install-recommends fonts-dejavu-core >/dev/null; }
  # Twice, and the two must agree byte for byte (the cast) and frame for frame (the text).
  local home=$WORK/demo-home
  mkdir -p "$home"
  for n in 1 2; do
    env -i PATH="$PATH" HOME="$home" TERM=xterm-256color LANG=C.UTF-8 \
      python3 -I "$ROOT/tools/demo-cast.py" "$COLS" "$ROWS" "$WORK/demo$n.cast" "$AIBUS" demo
    env -i PATH="$PATH" HOME="$home" "$AIBUS" demo --text >"$WORK/demo$n.txt"
  done
  cmp "$WORK/demo1.cast" "$WORK/demo2.cast"
  cmp "$WORK/demo1.txt" "$WORK/demo2.txt"
  agg --font-family "$FONT" --font-size 15 --quiet --fps-cap 10 --last-frame-duration 0.2 \
    "$WORK/demo1.cast" "$WORK/demo.gif"
  python3 -I "$ROOT/tools/demo-gif-check.py" "$WORK/demo.gif"
  cp "$WORK/demo.gif" "$OUT"
  agg --version
  echo "wrote $OUT"
}

case $STAGE in
  msgs) build; msgs ;;
  gif) build; gif ;;
  all) build; msgs; build; gif ;;
  *) echo "usage: $0 msgs|gif|all" >&2; exit 2 ;;
esac
