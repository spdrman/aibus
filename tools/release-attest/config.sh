# Shared by attest.sh and verify.sh: the few facts that name this attestation.
ATTEST_DIR=release-attest
ATTEST_FILE=attestation.json
NAMESPACE=aibus-release-attest
IDENTITY=aibus-release-attest
KIND=aibus-release-attestation
TREE_ALGO=mktree-top-level-sans-release-attest-v2
KEY_HINT=${AIBUS_ATTEST_KEY:-$HOME/.ssh/aibus-release-attest}
# The committed public key the verifier checks against (docs/RELEASE.md).
PUBKEY_PATH=packaging/release-attest.pub
IMAGE=rust:1.89-bookworm

# name|command. These are the "every push" rows of docs/TESTING.md 'Layers', and every one has to be
# in the attestation or the verifier refuses it. Not here: benchmarks, fuzzing, the live provider
# run, and the aibus-graph-causl backend (its crate is private, see docs/BUILD.md).
SUITES='unit|cargo test --workspace --locked --lib --bins
graph|cargo test --locked -p aibus-graph
integration|cargo test --locked -p aibus-core --test integration
tui|cargo test --locked -p aibus-tui
cli|cargo test --locked -p aibus --tests
replay|cargo test --locked -p aibus-testkit --test replay --test session && cargo test --locked -p aibus-core --test replay
capability-matrix|cargo test --locked -p aibus-testkit --test capability_matrix
provider-contract|cargo test --locked -p aibus-provider-anthropic -p aibus-provider-openai -p aibus-provider-gemini
security|cargo test --locked -p aibus-core --test security && cargo test --locked -p aibus-tui --test security_commands && cargo test --locked -p aibus --test mcp_security'

sha256() { if command -v sha256sum >/dev/null 2>&1; then sha256sum | cut -d' ' -f1; else shasum -a 256 | cut -d' ' -f1; fi; }

# The id of the commit's top-level tree with the one entry named exactly release-attest dropped,
# rebuilt with `git mktree -z`. Records stay NUL-separated end to end (a file name can hold a newline
# or a tab), and the drop matches the fixed "mode type sha<TAB>name" shape, so only the top-level
# entry goes and nothing nested can hide from the hash.
tree_hash() { # repo commit
  git -C "$1" ls-tree -z "$2" | perl -0ne 'print unless /\A\d+ \w+ [0-9a-f]+\trelease-attest\0\z/' | git -C "$1" mktree -z
}

# Writes the exact content of a tree-ish into a directory, entry by entry from the git objects. `git
# archive` is not used on purpose: it follows the tree's own .gitattributes, so export-ignore drops
# files and export-subst rewrites them, and the copy that runs the suites would not be the tree that
# gets hashed. Modes (644/755), symlinks and submodule directories are kept; a failure returns 1.
export_tree() { # repo tree-ish dir
  local ls rec meta mode sha path tgt
  mkdir -p "$3" || return 1
  ls=$(mktemp "${TMPDIR:-/tmp}/export-tree.XXXXXX") || return 1
  git -C "$1" ls-tree -r -z "$2" >"$ls" || { rm -f "$ls"; return 1; }
  while IFS= read -r -d '' rec; do
    meta=${rec%%$'\t'*}; path=${rec#*$'\t'}
    read -r mode _ sha <<<"$meta"
    mkdir -p "$3/$(dirname "$path")" || { rm -f "$ls"; return 1; }
    case $mode in
      120000) tgt=$(git -C "$1" cat-file blob "$sha"; printf x) || { rm -f "$ls"; return 1; }
              ln -s "${tgt%x}" "$3/$path" || { rm -f "$ls"; return 1; } ;;
      160000) mkdir -p "$3/$path" ;;
      *) git -C "$1" cat-file blob "$sha" >"$3/$path" || { rm -f "$ls"; return 1; }
         if [ "$mode" = 100755 ]; then chmod +x "$3/$path"; fi ;;
    esac
  done <"$ls"
  rm -f "$ls"
}

# The workspace version, read the way release.yml reads it.
declared_version() { # repo commit
  git -C "$1" show "$2:Cargo.toml" | awk '
    /^\[/ { in_ws = ($0 == "[workspace.package]") }
    in_ws && /^version[ ]*=/ { gsub(/^[^"]*"|".*$/, ""); print; exit }'
}

# One script, the suites in order, each between a SUITE and an EXIT marker that carry a per-run random
# nonce ($1). attest.sh and tools/release-gate/e2e.sh both build their container script with this, so the
# suite list and the way it is run live in one place. The nonce stops accidental and log-embedded decoys
# (a test that prints or quotes an old log); a reader only believes a marker that carries it, and a
# suite with two markers is red. It does not stop suite code that reads the nonce out of the container's
# argv: that code runs with the same rights as the script and is out of scope. The EXIT marker starts on
# a fresh line, so output without a trailing newline cannot swallow it.
new_nonce() { od -An -N12 -tx1 /dev/urandom | tr -d ' \n'; }
lane_script() { # nonce
  echo 'set -u'
  echo 'export CARGO_TERM_COLOR=never CI=1'
  local name cmd qn qx
  qx=$(printf '%s' "$1" | sed "s/'/'\\\\''/g")
  while IFS='|' read -r name cmd; do
    # names and nonce travel as printf arguments (single-quoted), never inside a format string
    qn=$(printf '%s' "$name" | sed "s/'/'\\\\''/g")
    printf "printf '=== SUITE %%s %%s ===\\\\n' '%s' '%s'; ( %s ) 2>&1; rc=\$?; printf '\\\\n=== EXIT %%s %%s %%s ===\\\\n' '%s' \"\$rc\" '%s'\n" "$qn" "$qx" "$cmd" "$qn" "$qx"
  done <<<"$SUITES"
}
# The exit code of one suite, from the log: exactly one genuine EXIT marker or nothing.
suite_exit() { # log name nonce
  local lines
  lines=$(sed -n "s/^=== EXIT $2 \([0-9][0-9]*\) $3 ===\$/\1/p" "$1")
  [ "$(grep -c . <<<"$lines")" = 1 ] && echo "$lines"
}
