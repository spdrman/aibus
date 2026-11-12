# shellcheck shell=bash
# Shared by the hooks, e2e.sh and release.sh. Source it from the repo root.
# The suite list, image and tree hash come from tools/release-attest/config.sh, so the e2e stamp and
# the signed attestation describe a tree the same way.
gate_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
. "$gate_dir/../release-attest/config.sh"

# Where stamps live: inside .git, never committed.
stamp_dir() { echo "$(cd "$(git rev-parse --git-common-dir)" && pwd)/release-e2e"; }
# The key of a tree-ish: its hash without release-attest/, so committing the attestation keeps the stamp valid.
tree_key() { tree_hash . "$1"; }
stamp_file() { echo "$(stamp_dir)/$1"; }

# RELEASE_E2E_SKIP=<reason> skips the gate. Loud, and logged next to the stamps.
gate_skipped() { # where
  [ -n "${RELEASE_E2E_SKIP:-}" ] || return 1
  mkdir -p "$(stamp_dir)"
  printf '%s skip at %s by %s: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$1" "$(git config user.email || echo unknown)" "$RELEASE_E2E_SKIP" >>"$(stamp_dir)/skips.log"
  echo "!!! RELEASE E2E GATE SKIPPED at $1: $RELEASE_E2E_SKIP (logged in $(stamp_dir)/skips.log)" >&2
}
