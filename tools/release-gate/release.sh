#!/usr/bin/env bash
# shellcheck source=tools/release-gate/lib.sh
# One release, end to end, stopping at the first failure. Never tags, publishes or merges.
#
#   tools/release-gate/release.sh <version> [--dry-run] [--yes] [--key ~/.ssh/aibus-release-attest]
#                                 [--host h] [--closes N] [--rerun] [--allow-downgrade]
#
#   <version> is X.Y.Z or X.Y.Z-pre.1. It must be greater than the highest v* tag and not lower than the
#   workspace version, and its tag v<version> must not exist locally or on origin.
#   1. preconditions: clean tree, the CHANGELOG entry, the signing key whose .pub equals the committed
#      packaging/release-attest.pub (the private key is never read, only its presence is checked), gh
#   2. branch release/v<version>, cut from origin/main (or the existing one, which must contain it)
#   3. ONE commit "Release v<version>": [workspace.package] version in Cargo.toml and the path crates in
#      Cargo.lock (what lane/version-0.2.0 did). Skipped when the manifest already has the version.
#      Formula/aibus.rb stays: the release renders it from the published checksums.
#   4. full e2e through tools/release-gate/e2e.sh (fenced docker); reuses a stamp for the same tree unless --rerun
#   5. signed attestation (tools/release-attest/attest.sh) for the same tree
#   6. commit it, verify.sh --expect-version (the check `prepare` in release.yml makes), ask once (--yes skips),
#      push the branch and open the PR "Release v<version>" against `release`. The owner merges it.
# The script never creates or pushes a tag: publish in release.yml makes v<version> on the push to `release`.
# --dry-run prints every step and writes, pushes and opens nothing.
# Test hooks: RELEASE_ATTEST_SH (attest.sh stand-in), plus RELEASE_E2E_CMD of e2e.sh. Exit 1 a step failed, 2 refused.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
here=$gate_dir
ATTEST=${RELEASE_ATTEST_SH:-$here/../release-attest/attest.sh}

ver= dry=0 yes=0 key=$KEY_HINT host=${RELEASE_E2E_HOST:-} closes= rerun=0 down=0
while [ $# -gt 0 ]; do
  case $1 in
    --dry-run) dry=1; shift ;;
    --yes) yes=1; shift ;;
    --key) key=$2; shift 2 ;;
    --host) host=$2; shift 2 ;;
    --closes) closes=$2; shift 2 ;;
    --rerun) rerun=1; shift ;;
    --allow-downgrade) down=1; shift ;;
    -*) echo "usage: release.sh <version> [--dry-run] [--yes] [--key k] [--host h] [--closes N] [--rerun] [--allow-downgrade]" >&2; exit 2 ;;
    *) [ -z "$ver" ] || { echo "one version only" >&2; exit 2; }; ver=$1; shift ;;
  esac
done
step() { echo "== $*" >&2; }
refuse() { echo "refused: $1" >&2; exit 2; }
fail() { echo "stopped: $1" >&2; exit 1; }
would() { if [ $dry = 1 ]; then echo "   (dry run) would: $*" >&2; return 0; fi; return 1; }
# a > b by semver precedence (a prerelease sorts below its release)
semver_gt() {
  [ "$1" != "$2" ] || return 1
  local ca=${1%%-*} cb=${2%%-*}
  if [ "$ca" != "$cb" ]; then [ "$(printf '%s\n%s\n' "$ca" "$cb" | sort -V | tail -1)" = "$ca" ]; return; fi
  [ "$1" = "$ca" ] && return 0   # same core, a is the release
  [ "$2" = "$cb" ] && return 1
  [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -1)" = "$1" ]
}

step "1. preconditions"
[ -n "$ver" ] || refuse "no version; usage: release.sh <version>"
[[ $ver =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z]+(\.[0-9A-Za-z]+)*)?$ ]] || refuse "'$ver' is not X.Y.Z or X.Y.Z-pre.N"
tag=v$ver
[ -z "$(git tag -l "$tag")" ] || refuse "tag $tag already exists locally"
[ -z "$(git ls-remote --tags origin "refs/tags/$tag")" ] || refuse "tag $tag already exists on origin"
[ -z "$(git status --porcelain)" ] || refuse "the tree is dirty"
main_sha=$(git ls-remote origin refs/heads/main | cut -f1)
[ -n "$main_sha" ] || refuse "could not read origin/main"
current=$(git show "$main_sha:Cargo.toml" 2>/dev/null | awk '/^\[/{w=($0=="[workspace.package]")} w&&/^version[ ]*=/{gsub(/^[^"]*"|".*$/,"");print;exit}' || true)
[ -n "$current" ] || current=$(declared_version . HEAD)
if [ $down = 0 ]; then
  top=$( { git tag -l 'v[0-9]*'; git ls-remote --tags origin 'refs/tags/v[0-9]*' | sed 's|.*refs/tags/||;s|\^{}$||'; } | sed 's/^v//' | sort -uV | tail -1)
  if [ -n "$top" ]; then semver_gt "$ver" "$top" || refuse "$ver is not greater than the latest tag v$top (--allow-downgrade overrides)"; fi
  [ "$ver" = "$current" ] || semver_gt "$ver" "$current" || refuse "$ver is lower than the workspace version $current (--allow-downgrade overrides)"
fi
grep -Eq "^## \[?${ver//./\\.}\]?([[:space:]]|$)" CHANGELOG.md || refuse "CHANGELOG.md has no entry for $ver (write it first, release.yml needs it too)"
[ -f "$key" ] || refuse "no signing key at $key (provision it; this script never creates one)"
[ -f "$key.pub" ] || refuse "no $key.pub next to the signing key"
committed=$(git show "$main_sha:$PUBKEY_PATH" 2>/dev/null | awk '{print $1, $2}' || true)
[ -n "$committed" ] && [ "$committed" = "$(awk '{print $1, $2}' "$key.pub")" ] \
  || refuse "$key.pub is not the key committed at $PUBKEY_PATH"
command -v gh >/dev/null || refuse "gh is not installed"
echo "   version $ver (workspace is $current), tag $tag is made by release.yml on the push to release, not here" >&2

branch=release/$tag
step "2. branch $branch"
if git show-ref -q --verify "refs/heads/$branch"; then
  would "git switch $branch" || git switch -q "$branch"
elif would "git fetch origin main; git switch -c $branch origin/main"; then :
else
  git fetch -q origin main
  git switch -q -c "$branch" origin/main
fi
if [ $dry = 0 ]; then
  git merge-base --is-ancestor "$main_sha" HEAD || refuse "$branch does not contain the tip of origin/main ($main_sha)"
  [ -z "$(git status --porcelain)" ] || refuse "the tree on $branch is dirty"
fi

step "3. bump commit"
if [ $dry = 0 ] && [ "$(declared_version . HEAD)" = "$ver" ]; then
  echo "   the manifest already says $ver, no bump commit" >&2
elif would "set [workspace.package] version = \"$ver\" in Cargo.toml and the path crates in Cargo.lock; commit 'Release $tag'"; then :
else
  export V=$ver
  perl -0pi -e 's/(\[workspace\.package\](?:(?!\n\[).)*?\nversion\s*=\s*")[^"]*(")/$1$ENV{V}$2/s' Cargo.toml
  # path crates are the [[package]] blocks without a source line
  perl -0pi -e 's/(\[\[package\]\]\nname = "aibus[^"]*"\nversion = ")[^"]*("\n(?!source))/$1$ENV{V}$2/g' Cargo.lock
  [ "$(awk '/^\[/{w=($0=="[workspace.package]")} w&&/^version[ ]*=/{gsub(/^[^"]*"|".*$/,"");print;exit}' Cargo.toml)" = "$ver" ] || fail "could not set the version in Cargo.toml"
  bad=$(perl -0ne 'while (/\[\[package\]\]\nname = "(aibus[^"]*)"\nversion = "([^"]*)"\n(?!source)/g) { print "$1 $2\n" if $2 ne $ENV{V} }' Cargo.lock)
  [ -z "$bad" ] || fail "Cargo.lock still has path crates at another version: $bad"
  git add Cargo.toml Cargo.lock
  git commit -q -m "Release $tag"
fi

tkey=$(tree_key HEAD)
step "4. full e2e (tree $tkey)"
if [ $dry = 0 ] && [ -f "$(stamp_file "$tkey")" ] && [ $rerun = 0 ]; then
  echo "   a stamp for this tree exists, reusing it (--rerun to run again)" >&2
elif would "tools/release-gate/e2e.sh ${host:+--host $host}"; then :
else "$here/e2e.sh" ${host:+--host "$host"} || fail "the e2e is not green"
fi
stamp=$(stamp_file "$tkey")
if [ $dry = 0 ] && [ ! -f "$stamp" ]; then fail "no e2e stamp for tree $tkey"; fi

step "5. signed attestation"
if would "$ATTEST --key $key ${host:+--host $host}"; then :
else
  replace=; [ -e release-attest/attestation.json ] && replace=--replace
  # shellcheck disable=SC2086
  "$ATTEST" --key "$key" ${host:+--host "$host"} $replace || fail "attest.sh did not sign a green run"
  [ "$(jq -r .tree.hash release-attest/attestation.json)" = "$tkey" ] || fail "the attestation covers a different tree than the e2e ran on"
fi

step "6. commit, verify, push, release PR"
if would "git add release-attest; commit; verify.sh --expect-version $ver; push origin $branch; gh pr create --base release --title 'Release $tag'"; then
  echo "   dry run done: nothing was written, pushed or opened" >&2; exit 0
fi
git add release-attest
git commit -q -m "Attest the suites for $ver"
"$here/../release-attest/verify.sh" --expect-version "$ver" || fail "verify.sh rejects the attestation (it is committed locally, nothing pushed)"
if [ $yes = 0 ]; then
  read -r -p "push $branch and open the release PR against release? [y/N] " a </dev/tty || a=n
  [ "$a" = y ] || fail "not confirmed (the commits stay local)"
fi
git push -q -u origin "$branch"
body=$(mktemp "${TMPDIR:-/tmp}/release-pr.XXXXXX"); trap 'rm -f "$body"' EXIT
{
  echo "Release $ver. I bumped the version, ran the full e2e, then signed the attestation, and verify.sh passes on this commit."
  echo
  echo "* attestation: release-attest/attestation.json, version $ver, tree $tkey"
  echo "* e2e stamp: tree $tkey, host $(sed -n 's/^host=//p' "$stamp"), finished $(sed -n 's/^finished=//p' "$stamp")"
  echo "* tag: release.yml makes $tag when this lands on release, the script did not tag anything"
  echo
  echo "I still have to read the diff and merge this myself, the merge into release is what publishes..."
  [ -z "$closes" ] || { echo; echo "Closes #$closes"; }
} >"$body"
gh pr create --base release --head "$branch" --title "Release $tag" --body-file "$body"
