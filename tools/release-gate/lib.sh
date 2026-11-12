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

# sha256 over the files that decide what a stamp means: the conf, the runner, this library and the
# attest scripts. e2e.sh writes it into the stamp; stamp_ok compares it with the copy of the gate it
# runs from, which pre-push takes from the remote release tip. A stamp made by another gate is no stamp.
gate_hash() {
  local f
  for f in "$gate_dir/../release-attest/config.sh" "$gate_dir/e2e.sh" "$gate_dir/lib.sh" "$gate_dir/../release-attest/attest.sh" "$gate_dir/../release-attest/verify.sh"; do
    [ -f "$f" ] || return 1
    sha256 <"$f"
  done | sha256
}
# The hosts dh.sh knows. stamp_ok and e2e.sh use this one list, so a run is never spent on a stamp
# pre-push would refuse.
host_ok() { # host (already trimmed and lower-cased)
  case $1 in mars|nas|mercury|local) return 0 ;; *) return 1 ;; esac
}

# RELEASE_E2E_SKIP=<reason> skips the gate. Loud, and logged next to the stamps.
gate_skipped() { # where
  [ -n "${RELEASE_E2E_SKIP:-}" ] || return 1
  local reason
  printf '%s' "$RELEASE_E2E_SKIP" | iconv -f UTF-8 -t UTF-8 >/dev/null 2>&1 || { echo "RELEASE_E2E_SKIP is not valid UTF-8; refusing." >&2; exit 2; }
  reason=$(printf '%s' "$RELEASE_E2E_SKIP" | perl -e 'use Encode qw(decode FB_CROAK); local $/; my $d = decode("UTF-8", <STDIN>, FB_CROAK); $d =~ s/[\x{ad}\x{34f}\x{61c}\x{115f}\x{1160}\x{17b4}\x{17b5}\x{180b}-\x{180e}\x{200b}-\x{200f}\x{202a}-\x{202e}\x{2060}-\x{206f}\x{2800}\x{3164}\x{fe00}-\x{fe0f}\x{feff}\x{ffa0}\x{fff9}-\x{fffb}\x{e0000}-\x{e007f}]//g; $d =~ s/[\x00-\x1f\x7f-\x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}]/ /g; $d =~ s/^\s+|\s+$//g; binmode(STDOUT, ":utf8"); print $d;') || { echo "RELEASE_E2E_SKIP could not be normalised (perl missing?); refusing." >&2; exit 2; }
  nchars=$(printf '%s' "$reason" | perl -e 'use Encode qw(decode); local $/; print length(decode("UTF-8", <STDIN>));') || { echo "cannot count the characters of RELEASE_E2E_SKIP; refusing." >&2; exit 2; }
  if [ "$nchars" -lt 12 ]; then echo "RELEASE_E2E_SKIP needs a real reason (12 characters or more once whitespace and invisible characters are trimmed); refusing." >&2; exit 2; fi
  mkdir -p "$(stamp_dir)" || { echo "cannot make the stamp directory; refusing." >&2; exit 2; }
  printf '%s skip at %s by %s: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$1" "$(git config user.email || echo unknown)" "$reason" >>"$(stamp_dir)/skips.log" || { echo "cannot write the skip record; nothing was skipped." >&2; exit 2; }
  echo "!!! RELEASE E2E GATE SKIPPED at $1: $reason (logged in $(stamp_dir)/skips.log)" >&2
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
  [ "$(cut -d= -f1 "$f" | tr '\n' ' ')" = "tree command host started finished gate_sha256 " ] || return 1
  [ "$(sed -n 's/^tree=//p' "$f")" = "$1" ] || return 1
  host=$(sed -n 's/^host=//p' "$f" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//' | tr 'A-Z' 'a-z')
  [ "$(sed -n 's/^gate_sha256=//p' "$f")" = "$(gate_hash)" ] || return 1
  if host_ok "$host"; then return 0; fi
  [ "$host" = fake-local ] && test_stamps_allowed
}
