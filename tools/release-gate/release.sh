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
#   2. branch release-prep/v<version>, cut from origin/main (or the existing one, which must contain it)
#   3. ONE commit "Release v<version>": [workspace.package] version in Cargo.toml and the path crates in
#      Cargo.lock (what lane/version-0.2.0 did). Skipped when the manifest already has the version.
#      Formula/aibus.rb stays: the release renders it from the published checksums.
#   4. full e2e through tools/release-gate/e2e.sh (fenced docker); reuses a stamp for the same tree unless --rerun
#   5. signed attestation (tools/release-attest/attest.sh) for the same tree
#   6. commit it, verify.sh --expect-version (the check `prepare` in release.yml makes), ask once (--yes skips),
#      push the branch and open the PR "Release v<version>" against `release`. The owner merges it.
# The script never creates or pushes a tag: publish in release.yml makes v<version> on the push to `release`.
# --dry-run prints every step and writes, pushes and opens nothing.
# The signing key is tried first on a scratch file (no prompt, in a new session so it has no terminal, even with job control on): a key that is not loaded in ssh-agent stops the run at once.
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
interrupted() { echo "interrupted ($1 ended with status $2, a signal)" >&2; exit "$2"; }
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
clash=$({ git for-each-ref --format='%(refname:short)' 'refs/heads/release/*'; git ls-remote --heads origin 'refs/heads/release/*' | sed 's|.*refs/heads/||'; } | head -1)
[ -z "$clash" ] || refuse "a branch named '$clash' exists (locally or on origin); git cannot hold it next to the branch release. Rename it to release-prep/${clash#release/} and delete the old one"
main_sha=$(git ls-remote origin refs/heads/main | cut -f1)
[ -n "$main_sha" ] || refuse "could not read origin/main"
[ $dry = 1 ] || git fetch -q origin main   # the objects for $main_sha, before anything reads them
current=$(git show "$main_sha:Cargo.toml" 2>/dev/null | awk '/^\[/{w=($0=="[workspace.package]")} w&&/^version[ ]*=/{gsub(/^[^"]*"|".*$/,"");print;exit}' || true)
[ -n "$current" ] || current=$(declared_version . HEAD)
if [ $down = 0 ]; then
  top=$( { git tag -l 'v[0-9]*'; git ls-remote --tags origin 'refs/tags/v[0-9]*' | sed 's|.*refs/tags/||;s|\^{}$||'; } | sed 's/^v//' | sort -uV | tail -1)
  if [ -n "$top" ]; then semver_gt "$ver" "$top" || refuse "$ver is not greater than the latest tag v$top (--allow-downgrade overrides)"; fi
  [ "$ver" = "$current" ] || semver_gt "$ver" "$current" || refuse "$ver is lower than the workspace version $current (--allow-downgrade overrides)"
fi
[ -f "$key" ] || refuse "no signing key at $key (provision it; this script never creates one)"
[ -f "$key.pub" ] || refuse "no $key.pub next to the signing key"
committed=$(git show "$main_sha:$PUBKEY_PATH" 2>/dev/null | awk '{print $1, $2}' || true)
[ -n "$committed" ] && [ "$committed" = "$(awk '{print $1, $2}' "$key.pub")" ] \
  || refuse "$key.pub is not the key committed at $PUBKEY_PATH"
command -v gh >/dev/null || refuse "gh is not installed"
# detach: run a command in a NEW SESSION, so it has no controlling terminal (ssh-keygen opens /dev/tty itself and would
# prompt there). A perl wrapper forks first, because setsid() fails for a process group leader (job control on) and
# returns -1, which is true in Perl, so the child must check for > 0. The wrapper runs in the background and the shell
# waits for it, so a signal sent to the script alone reaches it too (detach_sig). On INT/TERM/HUP the wrapper sends TERM
# to the child's session, KILLs it after 2 s, reaps it, and re-raises the signal on itself so the shell sees a death by
# signal. Otherwise it returns the child's exit status (128+n after a signal). Its own failure (fork, setsid, exec) is 125.
DETACH_PL='use POSIX; my $p = fork; defined $p or do { warn "fork: $!\n"; exit 125 };
if ($p) {
  my ($sig, $due);
  my $fwd = sub { $sig = $_[0] unless $sig; $due = time + 2 unless $due; kill "TERM", -$p; kill "TERM", $p };
  $SIG{INT} = $SIG{TERM} = $SIG{HUP} = $fwd; my $st;
  while (1) {
    my $r = waitpid($p, WNOHANG);
    if ($r == $p) { $st = $?; last }
    if ($r < 0 && $! != EINTR) { $st = 255 << 8; last }
    if (defined $due && time >= $due) { kill "KILL", -$p; kill "KILL", $p }
    select(undef, undef, undef, 0.1);
  }
  if ($sig) { $SIG{$sig} = "DEFAULT"; kill $sig, $$; sleep 1 }
  exit($st & 127 ? 128 + ($st & 127) : $st >> 8);
}
POSIX::setsid() > 0 or do { warn "setsid: $!\n"; exit 125 }; exec @ARGV or do { warn "exec: $!\n"; exit 125 }'
dpid=
# die_by SIG: clean up, then die BY the signal so a parent shell sees an interrupted child (exit only if it is ignored).
die_by() {
  trap - INT TERM HUP EXIT; pf_clean
  kill -s "$1" $$ 2>/dev/null || true
  case $1 in INT) exit 130 ;; TERM) exit 143 ;; *) exit 129 ;; esac
}
detach_sig() { [ -z "$dpid" ] || { kill -TERM "$dpid" 2>/dev/null || true; wait "$dpid" 2>/dev/null || true; }; die_by "$1"; }
detach() {
  local rc
  trap 'detach_sig INT' INT; trap 'detach_sig TERM' TERM; trap 'detach_sig HUP' HUP
  SSH_ASKPASS_REQUIRE=never perl -e "$DETACH_PL" -- "$@" </dev/null & dpid=$!
  wait "$dpid" && rc=0 || rc=$?
  trap - INT TERM HUP; dpid=
  return $rc
}

# Sign preflight: the e2e and the attestation take about 15 minutes, so find out now that the key signs
# without a prompt (a passphrase key has to be in ssh-agent), not at the end of the attestation.
command -v perl >/dev/null || refuse "perl is not installed"
pf=$(mktemp -d "${TMPDIR:-/tmp}/release-signcheck.XXXXXX") && [ -n "$pf" ] && [ -d "$pf" ] || refuse "could not make a temp directory for the sign check; nothing ran"
pf_clean() { rm -f "$pf/probe" "$pf/probe.sig" "$pf/err"; rmdir "$pf" 2>/dev/null || true; }
trap pf_clean EXIT   # detach_sig exits, which runs this too
echo preflight >"$pf/probe"
detach ssh-keygen -Y sign -f "$key" -n "$NAMESPACE" "$pf/probe" >/dev/null 2>"$pf/err" && [ -s "$pf/probe.sig" ] || {
  skrc=$?; [ $skrc != 125 ] || { msg=$(tr '\n' ' ' <"$pf/err"); refuse "cannot detach the terminal for the key check ($msg); nothing ran"; }
  echo "   ssh-keygen exited $skrc" >&2
  refuse "the key cannot sign without a prompt: load it with ssh-add $key first"; }
pf_clean; trap - EXIT INT TERM HUP
echo "   version $ver (workspace is $current), tag $tag is made by release.yml on the push to release, not here" >&2

# the CHANGELOG the release branch will have: its own tip if it exists, else origin/main's
branch=release-prep/$tag
if git show-ref -q --verify "refs/heads/$branch"; then cl=$(git show "refs/heads/$branch:CHANGELOG.md")
elif git cat-file -e "$main_sha:CHANGELOG.md" 2>/dev/null; then cl=$(git show "$main_sha:CHANGELOG.md")
else cl=$(cat CHANGELOG.md); fi
grep -Eq "^## \[?${ver//./\\.}\]?([[:space:]]|$)" <<<"$cl" || refuse "CHANGELOG.md on $branch has no entry for $ver (write it on main first, release.yml needs it too)"

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
elif [ $dry = 1 ] && [ "$(if git show-ref -q --verify "refs/heads/$branch"; then declared_version . "refs/heads/$branch"; else echo "$current"; fi)" = "$ver" ]; then
  echo "   (dry run) no bump commit needed, the manifest is already $ver" >&2
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
if [ $dry = 0 ] && stamp_ok "$tkey" && [ $rerun = 0 ]; then
  echo "   a stamp for this tree exists, reusing it (--rerun to run again)" >&2
elif would "tools/release-gate/e2e.sh ${host:+--host $host}"; then :
else
  "$here/e2e.sh" ${host:+--host "$host"} && erc=0 || erc=$?
  [ "$erc" -lt 128 ] || interrupted "e2e.sh" "$erc"
  [ "$erc" = 0 ] || fail "the e2e is not green"
fi
stamp=$(stamp_file "$tkey")
if [ $dry = 0 ] && ! stamp_ok "$tkey"; then fail "no e2e stamp for tree $tkey"; fi

step "5. signed attestation"
if would "$ATTEST --key $key ${host:+--host $host}"; then :
else
  replace=; [ -e release-attest/attestation.json ] && replace=--replace
  # shellcheck disable=SC2086
  "$ATTEST" --key "$key" ${host:+--host "$host"} $replace && arc=0 || arc=$?
  [ "$arc" -lt 128 ] || interrupted "attest.sh" "$arc"
  [ "$arc" = 0 ] || fail "attest.sh did not sign a green run"
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
body=$(mktemp "${TMPDIR:-/tmp}/release-pr.XXXXXX") || refuse "could not make a temp file for the PR body; nothing was pushed"
[ -n "$body" ] && [ -f "$body" ] || refuse "the temp file for the PR body is unusable; nothing was pushed"
trap 'rm -f "$body"' EXIT
{
  if git log --format=%s "$main_sha"..HEAD | grep -qx "Release $tag"; then
    echo "Release $ver. I bumped the version, ran the full e2e, then signed the attestation, and verify.sh passes on this commit."
  else
    echo "Release $ver. There is no bump commit, the manifest already said $ver, so I ran the full e2e, then signed the attestation, and verify.sh passes on this commit."
  fi
  echo
  echo "* attestation: release-attest/attestation.json, version $ver, tree $tkey"
  echo "* e2e stamp: tree $tkey, host $(sed -n 's/^host=//p' "$stamp"), finished $(sed -n 's/^finished=//p' "$stamp")"
  echo "* tag: release.yml makes $tag when this lands on release, the script did not tag anything"
  echo
  echo "I still have to read the diff and merge this myself, the merge into release is what publishes..."
  [ -z "$closes" ] || { echo; echo "Closes #$closes"; }
} >"$body"
gh pr create --base release --head "$branch" --title "Release $tag" --body-file "$body"
