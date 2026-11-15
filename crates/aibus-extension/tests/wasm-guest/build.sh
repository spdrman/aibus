#!/bin/sh
# Rebuilds parser.wasm and parser.wasm.sha256 from src/, in a container, so no wasm target has to be
# installed on the machine and the build is the same everywhere.
#
#   tests/wasm-guest/build.sh [docker-host]     (default: local; or mars, nas, mercury)
#
# It pushes this directory to the host with dh.sh (DH=/path/to/dh.sh to override), builds with the
# pinned toolchain and a locked Cargo.lock for wasm32-unknown-unknown, and writes the result here.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
host=${1:-local}
dh=${DH:-/Users/rom/workspace/dh.sh}
job=aibus-wasm-guest-333
"$dh" push "$host" "$job" "$here" --exclude=parser.wasm --exclude=parser.wasm.sha256 >&2
# The compiler's output depends on the architecture it runs on (an arm64 build differs from an x86_64
# one), so the build is always the x86_64 image, emulated on an arm64 host.
DH_PLATFORM=linux/amd64 "$dh" run "$host" "$job" rust:1.89.0-slim-bookworm sh -c '
  set -eu
  rustup target add wasm32-unknown-unknown >&2
  export CARGO_TARGET_DIR=/tmp/t RUSTFLAGS="--remap-path-prefix=/work=. --remap-path-prefix=/usr/local/cargo=cargo --remap-path-prefix=/usr/local/rustup=rustup"
  cargo build --release --locked --target wasm32-unknown-unknown >&2
  base64 /tmp/t/wasm32-unknown-unknown/release/aibus_wasm_guest.wasm
  cp /work/Cargo.lock /tmp/Cargo.lock.out 2>/dev/null || true
' | base64 -d > "$here/parser.wasm.new"
mv "$here/parser.wasm.new" "$here/parser.wasm"
shasum -a 256 "$here/parser.wasm" | cut -d" " -f1 > "$here/parser.wasm.sha256"
"$dh" clean "$host" "$job" >&2
echo "built $here/parser.wasm ($(wc -c < "$here/parser.wasm") bytes)" >&2
