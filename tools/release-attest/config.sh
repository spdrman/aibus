# Shared by attest.sh and verify.sh: the few facts that name this attestation.
ATTEST_DIR=release-attest
ATTEST_FILE=attestation.json
NAMESPACE=aibus-release-attest
IDENTITY=aibus-release-attest
KIND=aibus-release-attestation
TREE_ALGO=ls-tree-r-z-sans-release-attest-v1
KEY_HINT=$HOME/.ssh/aibus-release-attest
# The committed public key the verifier checks against. Until the owner provisions the real one it
# holds a placeholder, which verify.sh refuses (docs/RELEASE.md).
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

# sha256 over `git ls-tree -r -z <commit>` with every release-attest/ entry dropped.
tree_hash() { # repo commit
  git -C "$1" ls-tree -r -z "$2" | tr '\0' '\n' | grep -v "$(printf '\t')$ATTEST_DIR/" | sha256
}

# The workspace version, read the way release.yml reads it.
declared_version() { # repo commit
  git -C "$1" show "$2:Cargo.toml" | awk '
    /^\[/ { in_ws = ($0 == "[workspace.package]") }
    in_ws && /^version[ ]*=/ { gsub(/^[^"]*"|".*$/, ""); print; exit }'
}
