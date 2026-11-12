#!/usr/bin/env bash
# Point git at the committed hooks: tools/release-gate/install.sh
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
git config core.hooksPath .githooks
echo "core.hooksPath = .githooks (undo with: git config --unset core.hooksPath)"
