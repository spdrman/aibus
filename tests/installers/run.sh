#!/usr/bin/env bash
# Run the whole installer test suite in containers on the docker host (MARS by default) through
# libviprs/tools/nas.sh. Nothing executes on this machine except ssh and the nas.sh wrapper.
#
#   tests/installers/run.sh [scratch-name]      default scratch name: aibus-r2
set -euo pipefail

nas=${NAS_SH:-$HOME/workspace/libviprs/tools/nas.sh}
name=${1:-aibus-r2}
repo=$(cd "$(dirname "$0")/../.." && pwd)
t=tests/installers
status=0

step() {
  echo "=== $1"
  shift
  "$nas" run "$name" "$@" || { echo "FAILED: $1"; status=1; }
}

"$nas" push "$repo" "$name"
trap '"$nas" clean "$name"' EXIT

step "shellcheck + sh -n (install.sh, test scripts)" koalaman/shellcheck-alpine:v0.10.0 \
  "sh -n install.sh && shellcheck -s sh install.sh && shellcheck -s sh packaging/homebrew/render-formula.sh $t/make-fake-release.sh $t/test-install-sh.sh $t/test-render-formula.sh"
step "install.sh end to end, ubuntu (curl)" ubuntu:24.04 \
  "apt-get update -qq >/dev/null && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq curl python3 >/dev/null && sh $t/test-install-sh.sh"
step "install.sh end to end, alpine (busybox wget, ash)" alpine:3.20 \
  "apk add -q python3 && sh $t/test-install-sh.sh"
step "render-formula.sh" ruby:3.3-alpine "sh $t/test-render-formula.sh"
step "install.ps1 (parse, PSScriptAnalyzer, tests)" mcr.microsoft.com/powershell:ubuntu-22.04 "sh $t/test-install-ps1.sh"
step "brew audit --strict" homebrew/brew:latest "sh $t/test-brew-audit.sh"
exit $status
