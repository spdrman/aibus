# shellcheck shell=bash
# Shared by the hooks, e2e.sh and release.sh. Source it from the repo root.
# The suite list, image and tree hash come from tools/release-attest/config.sh, so the e2e stamp and
# the signed attestation describe a tree the same way.
gate_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
. "$gate_dir/../release-attest/config.sh"

# Where stamps live: inside .git, never committed.
stamp_dir() { echo "$(cd "$(git rev-parse --git-common-dir)" && pwd -P)/release-e2e"; }
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

# Fake stamps (RELEASE_E2E_CMD) count only inside tests/release-gate/run.sh, which sets the flag to the
# stamp directory of the repo under test; a leftover `1` or any other value never matches.
test_stamps_allowed() { [ "${RELEASE_E2E_TEST_STAMPS:-}" = "$(stamp_dir)" ]; }
# A stamp is believed only if it is non-empty, has exactly the keys e2e.sh writes, names the tree it is
# filed under, and does not come from a fake host (unless the test script allows it).
stamp_ok() { # key
  local f host
  f=$(stamp_file "$1")
  [ -s "$f" ] || return 1
  [ "$(cut -d= -f1 "$f" | tr '\n' ' ')" = "tree command host started finished " ] || return 1
  [ "$(sed -n 's/^tree=//p' "$f")" = "$1" ] || return 1
  host=$(sed -n 's/^host=//p' "$f" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//' | tr 'A-Z' 'a-z')
  case $host in
    mars|nas|mercury|local) ;;   # the hosts dh.sh knows
    fake-local) test_stamps_allowed || return 1 ;;
    *) return 1 ;;
  esac
}
