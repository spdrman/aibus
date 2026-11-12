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

# The workspace version, read the way release.yml reads it.
declared_version() { # repo commit
  git -C "$1" show "$2:Cargo.toml" | awk '
    /^\[/ { in_ws = ($0 == "[workspace.package]") }
    in_ws && /^version[ ]*=/ { gsub(/^[^"]*"|".*$/, ""); print; exit }'
}

# One script, the suites in order between markers, so one container runs them all. attest.sh and
# tools/release-gate/e2e.sh both build their container script with this, so the suite list and the
# way it is run live in one place.
lane_script() {
  echo 'set -u'
  echo 'export CARGO_TERM_COLOR=never CI=1'
  while IFS='|' read -r name cmd; do
    printf 'echo "=== SUITE %s ==="; ( %s ) 2>&1; echo "=== EXIT %s $? ==="\n' "$name" "$cmd" "$name"
  done <<<"$SUITES"
}
