#!/usr/bin/env bash
# Tests for the local release gate (.githooks/, tools/release-gate/). Throwaway repos, a throwaway key,
# a fake e2e command, a fake attest runner and a fake `gh` on PATH. No container, no cargo, no real e2e.
# Needs bash, git, jq, perl and ssh-keygen.
set -u
# Start from a known environment: whatever the calling shell exported for the gate must not change a result.
unset RELEASE_E2E_HOST RELEASE_E2E_SKIP RELEASE_E2E_CMD RELEASE_E2E_TEST_STAMPS RELEASE_GATE_TEST RELEASE_ATTEST_SH DH_HOST DH_CPUS DH_MEMORY DH_UNFENCED
here=$(cd "$(dirname "$0")" && pwd)
src=$(cd "$here/../.." && pwd)
t=$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/gate-test.XXXXXX") || exit 1
[ -n "$t" ] && [ -d "$t" ] || exit 1   # no -e here: an empty $t would aim every rm -rf "$t/..." at a path under /
trap 'rm -rf "$t"' EXIT
# Everything the runs create (log files, kept log directories) goes under $t, which the trap above removes.
mkdir "$t/tmp" && export TMPDIR=$t/tmp || exit 1
# No test may reach the real dh.sh (or docker, or ssh): DH_SH points at a stub that logs the call and fails.
# Tests that want a fake docker host set their own DH_SH on the command; this default catches the rest.
printf '#!/bin/sh\necho "$*" >>"$DH_REAL_LOG"\nexit 1\n' >"$t/dh-real-stub.sh" && chmod +x "$t/dh-real-stub.sh" || exit 1
export DH_SH=$t/dh-real-stub.sh DH_REAL_LOG=$t/dh-real.log
fails=0 n=0
. "$here/../lib/pty.sh"
ok() { n=$((n + 1)); echo "ok $n - $1"; }
bad() { n=$((n + 1)); fails=$((fails + 1)); echo "not ok $n - $1"; }
expect() { name=$1; shift; if "$@" >/dev/null 2>&1; then ok "$name"; else bad "$name"; fi; }
refuse() { name=$1; shift; if "$@" >/dev/null 2>&1; then bad "$name"; else ok "$name"; fi; }

ssh-keygen -q -t ed25519 -N '' -C throwaway -f "$t/key"
mkdir "$t/bin"
cat >"$t/bin/gh" <<'GH'
#!/bin/sh
echo "$*" >>"$GH_LOG"
echo https://example.invalid/pr/1
GH
chmod +x "$t/bin/gh"
# fake attest runner: green output for every suite
cat >"$t/fake-lane.sh" <<'FAKE'
echo "$SUITES_LIST" | while IFS='|' read -r name _; do
  echo "=== SUITE $name $LANE_NONCE ==="; echo "test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured"; echo "=== EXIT $name 0 $LANE_NONCE ==="
done
FAKE
. "$src/tools/release-attest/config.sh"
cat >"$t/attest-ok.sh" <<WRAP
#!/bin/sh
SUITES_LIST='$SUITES' exec "$src/tools/release-attest/attest.sh" "\$@" --runner-cmd "sh $t/fake-lane.sh"
WRAP
cat >"$t/attest-badsig.sh" <<WRAP
#!/bin/sh
SUITES_LIST='$SUITES' "$src/tools/release-attest/attest.sh" "\$@" --runner-cmd "sh $t/fake-lane.sh" || exit \$?
echo junk >release-attest/attestation.json.sig
WRAP
chmod +x "$t"/attest-*.sh

mkrepo() { # dir : a work repo with an origin, the gate files, version 0.1.0
  d=$1; rm -rf "$d" "$d.origin"; mkdir -p "$d/tools" "$d/packaging"
  git init -q --bare -b main "$d.origin"
  git -C "$d" init -q -b main
  git -C "$d" config user.email t@example.com; git -C "$d" config user.name t
  cp -R "$src/.githooks" "$d/"; cp -R "$src/tools/release-gate" "$src/tools/release-attest" "$d/tools/"
  printf '[workspace.package]\nversion = "0.1.0"\nedition = "2021"\n\n[workspace.dependencies]\nserde = "1"\n' >"$d/Cargo.toml"
  printf '[[package]]\nname = "aibus"\nversion = "0.1.0"\ndependencies = ["serde"]\n\n[[package]]\nname = "aibus-core"\nversion = "0.1.0"\n\n[[package]]\nname = "serde"\nversion = "0.1.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\n' >"$d/Cargo.lock"
  printf '# Changelog\n\n## [0.2.0] - unreleased\n\n## [0.1.0] - 2026-01-01\n' >"$d/CHANGELOG.md"
  cp "$t/key.pub" "$d/$PUBKEY_PATH"
  echo one >"$d/f.txt"
  git -C "$d" add -A; git -C "$d" commit -q -m base
  git -C "$d" remote add origin "$d.origin"; git -C "$d" push -q origin main
  git -C "$d" config core.hooksPath .githooks
}
# The fake command may write real stamps only inside this script: the flag has to equal the stamp directory.
usegate() { cd "$1" || exit 1; RELEASE_E2E_TEST_STAMPS=$(cd "$(git rev-parse --git-common-dir)" && pwd -P)/release-e2e; export RELEASE_E2E_TEST_STAMPS; }
export GH_LOG=$t/gh.log
export PATH="$t/bin:$PATH"
# issue #251: every gate script calls /usr/bin/mktemp by absolute path. This spy sits first on PATH, logs any
# call that reaches it and answers with an existing directory; the check at the end of the file fails if a
# gate script (its temp dir, kept logs, stamp, PR body, hooks, tree export) still calls a bare mktemp.
mkdir "$t/spybin" "$t/spyvictim" && printf '#!/bin/sh\necho "$*" >>"$SPY_LOG"\necho "$SPY_VICTIM"\n' >"$t/spybin/mktemp" && chmod +x "$t/spybin/mktemp" || exit 1
export SPY_LOG=$t/spy.log SPY_VICTIM=$t/spyvictim PATH="$t/spybin:$PATH"


# ---------- hooks and runner
r=$t/repo; mkrepo "$r"; usegate "$r"
git switch -q -c release; git switch -q -c feature
echo two >f.txt; git commit -q -am feature
git switch -q release; echo r >r.txt; git add r.txt; git commit -q -m "on release"
refuse "a refspec with spaces does not slip past the release check (no remote release yet)" git push -q origin 'HEAD^{/on release}:refs/heads/release'
refuse "the first push of release (the remote has none) is refused" git push -q origin release
expect "the bootstrap push with the logged skip passes" env RELEASE_E2E_SKIP="bootstrap release" git push -q origin release
expect "the bootstrap skip is logged" grep -q 'bootstrap release' "$(git rev-parse --git-common-dir)/release-e2e/skips.log"
refuse "merge into release with a red e2e is blocked" env RELEASE_E2E_CMD=false git merge --no-ff -m m feature
expect "a blocked merge leaves HEAD where it was" test "$(git rev-parse HEAD)" = "$(git rev-parse release)"
expect "red e2e writes no stamp" test -z "$(ls "$(git rev-parse --git-common-dir)/release-e2e" 2>/dev/null | grep -v skips.log)"
git merge --abort 2>/dev/null; git reset -q --hard
expect "merge into release with a green e2e is allowed" env RELEASE_E2E_CMD=true git merge --no-ff -m m feature
expect "green e2e writes a stamp for the merged tree" test -f "$(git rev-parse --git-common-dir)/release-e2e/$(tree_hash . HEAD)"
expect "the stamp records tree, command, host and time" grep -q '^command=true' "$(git rev-parse --git-common-dir)/release-e2e/$(tree_hash . HEAD)"
expect "push to release with a stamp passes" git push -q origin release
echo three >f.txt; git commit -q -am more
refuse "push to release with a stamp for another tree is refused" git push -q origin release
expect "push to a feature branch passes without a stamp" git push -q origin release:refs/heads/feature2
git reset -q --hard HEAD~1 2>/dev/null; git switch -q feature; echo x >>f.txt; git commit -q -am "an x commit"
git switch -q release; git reset -q --hard feature
refuse "push to release without any stamp is refused" git push -q origin release
refuse "a refspec with spaces is still judged (no stamp)" git push -q origin 'HEAD^{/an x commit}:refs/heads/release'
refuse "a squash/ff style push (no merge hook) is still refused without a stamp" git push -q origin release:refs/heads/release
expect "RELEASE_E2E_SKIP lets the push through" env RELEASE_E2E_SKIP="testing the skip" git push -q -f origin release
expect "the skip is logged with its reason" grep -q 'testing the skip' "$(git rev-parse --git-common-dir)/release-e2e/skips.log"
echo dirty >>f.txt
refuse "e2e.sh refuses a dirty tree" env RELEASE_E2E_CMD=true tools/release-gate/e2e.sh
git checkout -q f.txt
# a RELEASE_E2E_CMD left in the shell must not open the gate
git switch -q release; echo leak >leak.txt; git add leak.txt; git commit -q -m leak
env -u RELEASE_E2E_TEST_STAMPS RELEASE_E2E_CMD=true tools/release-gate/e2e.sh >"$t/leak.out" 2>&1
expect "e2e.sh with RELEASE_E2E_CMD set warns loudly" grep -q "TEST ONLY" "$t/leak.out"
refuse "a stamp from a leftover RELEASE_E2E_CMD does not open pre-push" git push -q -f origin release

# stamp content is checked, not just its existence
push_nf() { env -u RELEASE_E2E_TEST_STAMPS git push -q -f origin release; }
sd=$(git rev-parse --git-common-dir)/release-e2e
curgh() { bash -c '. tools/release-gate/lib.sh; gate_hash'; }
mk_stamp() { # key tree host [gate_sha256]
  mkdir -p "$sd"; printf 'tree=%s\ncommand=x\nhost=%s\nstarted=a\nfinished=b\ngate_sha256=%s\n' "$2" "$3" "${4:-$(curgh)}" >"$sd/$1"
}
sok() { bash -c '. tools/release-gate/lib.sh; stamp_ok "$1"' _ "$1"; }
env -u RELEASE_E2E_TEST_STAMPS true; key=$(tree_hash . HEAD); rm -f "$sd/$key"
mk_stamp "$key" "$key" fake-local;       refuse "a hand-made stamp with host=fake-local is refused" push_nf
mk_stamp "$key" "$key" " test ";         refuse "host=test (with whitespace) is refused" push_nf
mk_stamp "$key" "$key" "";               refuse "an empty host is refused" push_nf
mk_stamp "$key" 0000000000000000000000000000000000000000 mars; refuse "a stamp whose tree= is another tree is refused" push_nf
mkdir -p "$sd"; : >"$sd/$key";           refuse "an empty stamp file is refused" push_nf
printf 'tree=%s\n' "$key" >"$sd/$key";   refuse "a stamp without the expected keys is refused" push_nf
env -u RELEASE_E2E_TEST_STAMPS RELEASE_E2E_CMD=true tools/release-gate/e2e.sh >/dev/null 2>&1
RELEASE_E2E_TEST_STAMPS=1 RELEASE_E2E_CMD=true tools/release-gate/e2e.sh >/dev/null 2>&1
refuse "a fake run with RELEASE_E2E_TEST_STAMPS=1 left exported does not open pre-push" env RELEASE_E2E_TEST_STAMPS=1 git push -q -f origin release
printf 'tree=%s\ncommand=x\nhost=mars\nstarted=a\nfinished=b\n' "$key" >"$sd/$key"; refuse "a stamp without gate_sha256 is refused" sok "$key"
mk_stamp "$key" "$key" mars 0000000000000000000000000000000000000000000000000000000000000000; refuse "a stamp whose gate_sha256 is not the trusted gate's is refused" sok "$key"
for h in nas mercury local mars; do mk_stamp "$key" "$key" "$h"; expect "host $h is accepted by stamp_ok" sok "$key"; done
mk_stamp "$key" "$key" jupiter; refuse "an unknown host (jupiter) is refused by stamp_ok" sok "$key"
mk_stamp "$key" "$key" mars;             expect "a well-formed stamp for the tree is accepted" push_nf
rm -f "$sd/$key"

# the container's output cannot forge a marker, and its exit status counts
cat >"$t/dh-fake.sh" <<'DHF'
#!/bin/sh
echo "$1" >>"${DH_LOG:-/dev/null}"
case $1 in pick) echo "${PICK:-mars}";; clean|push) ;; run)
  script=$7
  nonce=$(printf '%s\n' "$script" | sed -n "s/.*' '\([0-9a-f]*\)'; (.*/\1/p" | head -1)
  printf '%s\n' "$SUITES_LIST" | while IFS='|' read -r n _; do
    echo "=== SUITE $n $nonce ==="
    [ "$MODE" = decoy-inside ] && [ "$n" = graph ] && echo "=== EXIT graph 0 ==="
    [ "$MODE" = decoy-later ] && [ "$n" = graph ] && echo "=== EXIT unit 0 ==="
    code=0; { [ "$MODE" = decoy-inside ] && [ "$n" = graph ]; } && code=1; { [ "$MODE" = decoy-later ] && [ "$n" = unit ]; } && code=101
    echo "=== EXIT $n $code $nonce ==="
  done
  exit "${RC:-0}";;
esac
DHF
chmod +x "$t/dh-fake.sh"
fakedh() { env -u RELEASE_E2E_CMD -u RELEASE_E2E_TEST_STAMPS SUITES_LIST="$SUITES" DH_SH="$t/dh-fake.sh" "$@" tools/release-gate/e2e.sh; }
refuse "a forged EXIT 0 inside the failing suite is red" fakedh MODE=decoy-inside
refuse "a forged EXIT 0 from a later suite does not hide a failing one" fakedh MODE=decoy-later
refuse "a container that exits 137 is red even with every marker 0" fakedh MODE=green RC=137
expect "no stamp for any of those" test ! -e "$sd/$(tree_hash . HEAD)"
expect "a clean run with real markers is green and stamps" fakedh MODE=green
expect "the stamp records the picked host" grep -q '^host=mars$' "$sd/$(tree_hash . HEAD)"
expect "the stamp carries gate_sha256" grep -Eq '^gate_sha256=[0-9a-f]{64}$' "$sd/$(tree_hash . HEAD)"
rm -f "$sd/$(tree_hash . HEAD)"
: >"$t/dh.log"
PICK=jupiter DH_LOG=$t/dh.log fakedh MODE=green >"$t/host.out" 2>&1; rc=$?
expect "e2e.sh refuses an unknown picked host with exit 2" test $rc = 2
expect "...before pushing or running anything" bash -c "! grep -qE '^(push|run)$' '$t/dh.log'"
: >"$t/dh.log"
env -u RELEASE_E2E_CMD -u RELEASE_E2E_TEST_STAMPS DH_LOG=$t/dh.log SUITES_LIST="$SUITES" DH_SH="$t/dh-fake.sh" tools/release-gate/e2e.sh --host jupiter >/dev/null 2>&1; rc=$?
expect "e2e.sh --host jupiter is refused with exit 2, nothing pushed" bash -c "[ $rc = 2 ] && ! grep -qE '^(push|run)$' '$t/dh.log'"
mkdir "$t/noodbin"; printf '#!/bin/sh\nexit 0\n' >"$t/noodbin/od"; chmod +x "$t/noodbin/od"
env -u RELEASE_E2E_CMD -u RELEASE_E2E_TEST_STAMPS PATH="$t/noodbin:$PATH" SUITES_LIST="$SUITES" DH_SH="$t/dh-fake.sh" tools/release-gate/e2e.sh >"$t/nonce.out" 2>&1; rc=$?
expect "an empty nonce is refused (exit 2)" test $rc = 2
expect "...and says so" grep -qi 'nonce' "$t/nonce.out"

# ---------- gate integrity: judged with the gate of the remote release tip
pushmsg() { git push -q "$@" 2>"$t/p.err"; }
git switch -q release; git reset -q --hard HEAD 2>/dev/null
RELEASE_E2E_SKIP="sync with the remote" git push -q -f origin release 2>/dev/null
base=$(git rev-parse HEAD)
git switch -q -c topic3; printf '%s\n' 'stamp_ok() { return 0; }' >>tools/release-gate/lib.sh; git commit -q -am "topic edits lib.sh"
git switch -q release; git merge -q --ff-only topic3
refuse "a topic that makes stamp_ok true and is fast-forwarded into release is refused" pushmsg origin release
expect "...because it changes the gate" grep -q 'release gate files' "$t/p.err"
git reset -q --hard "$base"
git switch -q -c topic4; echo '# note' >>tools/release-attest/config.sh; git commit -q -am "topic edits the conf"
git switch -q release; git merge -q --ff-only topic4
fakedh MODE=green >/dev/null 2>&1
refuse "a stamp made under a conf the topic changed does not open the push" pushmsg origin release
git reset -q --hard "$base"; rm -f "$sd"/*
echo '# benign' >>tools/release-gate/e2e.sh; git commit -q -am "gate change"
refuse "a push that changes a gate file is refused" pushmsg origin release
expect "...with the logged skip it passes" env RELEASE_E2E_SKIP="gate change asked for" git push -q origin release
# release-attest/ beyond the attestation and its signature
key=$(tree_hash . HEAD); mk_stamp "$key" "$key" mars
mkdir -p release-attest; echo '{}' >release-attest/attestation.json; echo sig >release-attest/attestation.json.sig
git add release-attest; git commit -q -m "attestation"
expect "a push adding only attestation.json and its signature passes with the stamp" pushmsg origin release
echo x >release-attest/note; git add release-attest; git commit -q -m "note"
refuse "a push touching release-attest/ beyond the attestation is refused even with a stamp" pushmsg origin release
expect "...and names the file" grep -q 'release-attest/note' "$t/p.err"
git reset -q --hard HEAD~1
# a stale clone: the remote tip is not here
git clone -q "$r.origin" "$t/clone2" 2>/dev/null
(cd "$t/clone2" && git config user.email t@example.com && git config user.name t && git config core.hooksPath .githooks && git switch -q release && echo c2 >c2.txt && git add c2.txt && git commit -q -m c2 && RELEASE_E2E_SKIP="another clone pushed" git push -q origin release 2>/dev/null)
echo mine >mine.txt; git add mine.txt; git commit -q -m mine
key=$(tree_hash . HEAD); mk_stamp "$key" "$key" mars
refuse "a push judged against a remote tip that is not in this clone is refused" pushmsg origin release
expect "...and says to fetch first" grep -q 'fetch first' "$t/p.err"
git fetch -q origin; git reset -q --hard origin/release
refuse "deleting release is refused" pushmsg origin :refs/heads/release
expect "deleting release with the logged skip passes" env RELEASE_E2E_SKIP="owner asked for the delete" git push -q origin :refs/heads/release
RELEASE_E2E_SKIP="bootstrap again after delete" git push -q origin release 2>/dev/null; true
# a merge that changes a gate file
git switch -q -c topicg; echo '# hook note' >>.githooks/pre-commit; git commit -q -am "hook change"
git switch -q release; echo m >m.txt; git add m.txt; git commit -q -m m2
refuse "a merge into release that changes a gate file is refused" env RELEASE_E2E_CMD=true git merge --no-ff -m m topicg
git merge --abort 2>/dev/null; git reset -q --hard
expect "...with the logged skip the merge goes through" env RELEASE_E2E_SKIP="hook change asked for" git merge --no-ff -m m topicg
git reset -q --hard HEAD~2 2>/dev/null
# ---------- review round: the skip logs itself, tree copies ignore .gitattributes, odd stdin
pushmsg_env() { env RELEASE_E2E_SKIP="$1" git push -q origin release; }
base2() { r2=$t/$1; mkrepo "$r2"; usegate "$r2"; git switch -q -c release; RELEASE_E2E_SKIP="bootstrap for the test" git push -q origin release 2>/dev/null; }
sdir() { echo "$(cd "$(git rev-parse --git-common-dir)" && pwd -P)/release-e2e"; }
loglines() { wc -l <"$(sdir)/skips.log" 2>/dev/null | tr -d ' '; }
base2 s0
expect "the bootstrap skip record lists what it waived (bootstrap)" grep -q 'waived=.*bootstrap' "$(sdir)/skips.log"
expect "...and carries the pushed sha, old tip and remote ref" grep -Eq 'pushed_sha=[0-9a-f]{40} old_tip=0{40} remote_ref=refs/heads/release' "$(sdir)/skips.log"
# S1: a lib that silences gate_skipped, fast-forwarded into release
base2 s1
git switch -q -c topic; perl -0pi -e 's/gate_skipped\(\) \{ # where\n/gate_skipped() { # where\n  return 0\n/' tools/release-gate/lib.sh
git commit -qam "quiet skip log"; git switch -q release; git merge -q --ff-only topic; before=$(loglines)
RELEASE_E2E_SKIP="gate change asked for" git push -q origin release 2>/dev/null
expect "S1: a skip push still logs when the pushed lib silences gate_skipped" test "$(loglines)" -gt "$before"
expect "S1: the record says gate-change was waived" grep -q 'waived=.*gate-change' "$(sdir)/skips.log"
echo more >more1.txt; git add more1.txt; git commit -qm more1
refuse "a skip reason under 12 characters is refused (push)" env RELEASE_E2E_SKIP=short git push -q origin release
# S1b: a lib that exits at source time
base2 s1b
git switch -q -c topic; printf 'exit 0\n' | cat - tools/release-gate/lib.sh >"$t/l" && cp "$t/l" tools/release-gate/lib.sh
git commit -qam "exit early"; git switch -q release; git merge -q --ff-only topic; before=$(loglines)
RELEASE_E2E_SKIP="gate change asked for" git push -q origin release 2>/dev/null
expect "S1b: a skip push still logs when the pushed lib exits on source" test "$(loglines)" -gt "$before"
# S2: release made from a commit with no gate, merged topic carries a silenced lib
r2=$t/s2; rm -rf "$r2"; mkdir -p "$r2"; cd "$r2"; git init -q -b main; git config user.email t@example.com; git config user.name t
echo a >a; git add a; git commit -qm old; git switch -q -c release; git switch -q -c topic main
cp -R "$src/.githooks" .; mkdir -p tools; cp -R "$src/tools/release-gate" "$src/tools/release-attest" tools/
perl -0pi -e 's/gate_skipped\(\) \{ # where\n/gate_skipped() { # where\n  return 0\n/' tools/release-gate/lib.sh
git add -A; git commit -qm gate; git config core.hooksPath .githooks; git switch -q release
refuse "a short skip reason is refused (merge)" env RELEASE_E2E_SKIP=short git merge --no-ff -q -m m topic
git merge --abort 2>/dev/null; git reset -q --hard
RELEASE_E2E_SKIP="create release branch" git merge --no-ff -q -m m topic 2>/dev/null
expect "S2: creating release (HEAD has no gate) with a skip logs a record" grep -q 'create release branch' "$(sdir)/skips.log"
expect "S2: ...listing remote-gate among what was waived" grep -q 'waived=.*remote-gate' "$(sdir)/skips.log"
# no temp dirs left behind by the merge hook's skip path
base2 s8; git switch -q -c topic; echo t >t.txt; git add t.txt; git commit -qm topic; git switch -q release; echo r >r2.txt; git add r2.txt; git commit -qm r2
mkdir "$t/tmpL"; TMPDIR=$t/tmpL RELEASE_E2E_SKIP="leak check for the merge" git merge --no-ff -q -m m topic 2>/dev/null
expect "pre-merge-commit with a skip leaves no temp dir" test -z "$(ls "$t/tmpL")"
git switch -q -c topic2 HEAD~1; echo u >u.txt; git add u.txt; git commit -qm topic2; git switch -q release; RELEASE_E2E_CMD=true git merge -q -s ours --no-ff -m m2 topic2 >/dev/null 2>&1
rm -rf "$t/tmpL"; mkdir "$t/tmpL"; TMPDIR=$t/tmpL RELEASE_E2E_CMD=true git merge --no-ff -q -m m3 topic2 >/dev/null 2>&1
expect "pre-merge-commit on a normal merge leaves no temp dir" test -z "$(ls "$t/tmpL")"
echo p >p.txt; git add p.txt; git commit -qm p; mkdir "$t/tmpP"; TMPDIR=$t/tmpP RELEASE_E2E_SKIP="leak check for the push" git push -q origin release 2>/dev/null
expect "pre-push with a skip leaves no temp dir" test -z "$(ls "$t/tmpP")"
# S3: a newline in a refspec plants a fake ref line ahead of the real one
base2 s3
echo good >g; git add g; git commit -qm good; key=$(tree_hash . HEAD); mk_stamp "$key" "$key" mars; good=$(git rev-parse HEAD)
git push -q origin release 2>/dev/null; tip=$(git rev-parse release)
echo bad >g; git commit -qam bad; git commit -q --allow-empty -m "z $good refs/heads/release $tip"
spec=$(printf 'HEAD^{/z %s refs/heads/release %s\n}' "$good" "$tip")
refuse "S3: a refspec with a newline cannot plant a fake release line" git push -q origin "$spec:refs/heads/release"
# S5: a decoy GIT_COMMON_DIR holding a stamp
base2 s5
echo n >n; git add n; git commit -qm n; dec=$t/decoy5; git clone -q --bare "$r2" "$dec"; git --git-dir="$dec" remote set-url origin "$r2.origin"; git --git-dir="$dec" config core.hooksPath "$r2/.githooks"
key=$(tree_hash . HEAD); mkdir -p "$dec/release-e2e"; printf 'tree=%s\ncommand=x\nhost=mars\nstarted=s\nfinished=f\ngate_sha256=%s\n' "$key" "$(curgh)" >"$dec/release-e2e/$key"
refuse "S5: GIT_COMMON_DIR pointing at a dir with a stamp does not open pre-push" env GIT_COMMON_DIR="$dec" git push -q origin release
# the real first push: the remote release exists but has no gate
r2=$t/s7; rm -rf "$r2" "$r2.origin"; mkdir -p "$r2"; git init -q --bare -b main "$r2.origin"; cd "$r2"; git init -q -b main; git config user.email t@example.com; git config user.name t
echo a >a; git add a; git commit -qm old; git remote add origin "$r2.origin"; git push -q origin main; git push -q origin main:refs/heads/release
cp -R "$src/.githooks" .; mkdir -p tools; cp -R "$src/tools/release-gate" "$src/tools/release-attest" tools/; git add -A; git commit -qm gate; git config core.hooksPath .githooks
git fetch -q origin; git switch -q -c release; git branch -q -u origin/release 2>/dev/null
git push origin release 2>"$t/p.err" >/dev/null
expect "a remote release without a gate is refused and the message names the skip" grep -q 'RELEASE_E2E_SKIP' "$t/p.err"
RELEASE_E2E_SKIP="first gated push of release" git push -q origin release 2>/dev/null
expect "...the logged skip lets the first gated push through, listing remote-gate" grep -q 'waived=.*remote-gate' "$(sdir)/skips.log"
# the trusted gate is read with git show, so export-ignore in the tip cannot drop it
base2 s9
printf 'tools/release-gate export-ignore\ntools/release-attest export-ignore\n' >.gitattributes; git add .gitattributes; git commit -qm attrs
RELEASE_E2E_SKIP="move the tip for the test" git push -q origin release 2>/dev/null
echo more >more.txt; git add more.txt; git commit -qm more; key=$(tree_hash . HEAD); mk_stamp "$key" "$key" mars
expect "a tip whose .gitattributes export-ignores the gate still supplies the gate" git push -q origin release
# S4: git archive honours the tree's own .gitattributes; the copy must not
base2 s4
mkdir tests; echo 'exit 1' >tests/b.sh; echo 'exit 0' >tests/a.sh; git add -A; git commit -qm t
refuse "S4 control: the e2e is red while the failing test is in the tree" env RELEASE_E2E_CMD='for f in tests/*.sh; do sh $f || exit 1; done' tools/release-gate/e2e.sh
echo 'tests/b.sh export-ignore' >.gitattributes; git add -A; git commit -qm hide
refuse "S4: an export-ignored failing test is still in the copy, so the e2e stays red" env RELEASE_E2E_CMD='for f in tests/*.sh; do sh $f || exit 1; done' tools/release-gate/e2e.sh
expect "S4: no stamp for it" test -z "$(ls "$(sdir)" 2>/dev/null | grep -v -e skips.log -e '^\.')"
printf 'tests/sub.txt export-subst\n' >.gitattributes; printf 'id $Format:%%H$\n' >tests/sub.txt; printf '#!/bin/sh\nexit 0\n' >tests/x.sh; chmod +x tests/x.sh; ln -s tests/a.sh tests/link; rm tests/b.sh
git add -A; git commit -qm modes
expect "the copy keeps modes, symlinks and export-subst text byte for byte" env RELEASE_E2E_CMD='test -x tests/x.sh && test -L tests/link && [ "$(readlink tests/link)" = tests/a.sh ] && grep -q "Format:%H" tests/sub.txt' tools/release-gate/e2e.sh
# a newline inside the source ref plants a fake line; line count and field count are both checked
base2 s3b
echo f >f2; git add f2; git commit -qm F; F=$(git rev-parse HEAD); key=$(tree_hash . HEAD); mk_stamp "$key" "$key" mars
git push -q origin release 2>/dev/null; tipb=$(git rev-parse release)
echo e >e2; git add e2; git commit -q -am E 2>/dev/null || { git add e2; git commit -qm E; }
git commit -q --allow-empty -m "$(printf 'evil\nlocal %s refs/heads/release %s' "$F" "$F")"
spec=$(printf 'HEAD^{/evil\nlocal %s refs/heads/release %s}' "$F" "$F")
refuse "a newline in the source cannot make the hook judge a planted line (fake line before the real one)" git push -q origin "$spec:refs/heads/release"
# a reason with newlines cannot forge record lines
base2 s10
echo q >q; git add q; git commit -qm q; before=$(loglines)
RELEASE_E2E_SKIP="$(printf 'forged reason here\nwaived=none\nuser=root')" git push -q origin release 2>/dev/null
expect "a skip reason with newlines is flattened into one record line" test "$(loglines)" = $((before + 1))
refuse "...so no record line starts with a forged key" grep -Eq '^(waived|user)=' "$(sdir)/skips.log"
# reasons: whitespace does not count, bad UTF-8 and line separators are handled
base2 s11
echo w >w; git add w; git commit -qm w
refuse "a skip reason of 12 spaces is refused (push)" pushmsg_env "            "
RELEASE_E2E_SKIP="$(printf '\377\376 not utf eight')" git push -q origin release 2>"$t/p.err"
expect "an invalid UTF-8 reason gets a clear refusal" grep -qi 'utf-8' "$t/p.err"
expect "...not a tr error" bash -c "! grep -q 'Illegal byte' '$t/p.err'"
before=$(loglines)
RELEASE_E2E_SKIP="$(printf 'line sep \342\200\250 and para \342\200\251 here')" git push -q origin release 2>/dev/null
expect "U+2028 and U+2029 in a reason leave one record line" test "$(loglines)" = $((before + 1))
refuse "...with no separator bytes in the log" grep -q "$(printf '\342\200[\250\251]')" "$(sdir)/skips.log"
echo n1 >n1; git add n1; git commit -qm n1
refuse "a skip reason of 12 non-breaking spaces is refused (push)" pushmsg_env "$(printf '\302\240\302\240\302\240\302\240\302\240\302\240\302\240\302\240\302\240\302\240\302\240\302\240')"
refuse "a skip reason of ideographic spaces is refused (push)" pushmsg_env "$(printf '\343\200\200\343\200\200\343\200\200\343\200\200\343\200\200\343\200\200\343\200\200\343\200\200\343\200\200\343\200\200\343\200\200\343\200\200')"
refuse "e2e.sh refuses a skip reason of 12 tabs" env RELEASE_E2E_SKIP="$(printf '\t\t\t\t\t\t\t\t\t\t\t\t')" tools/release-gate/e2e.sh
git switch -q -c topicw; echo y >y; git add y; git commit -qm y; git switch -q release; echo z >z2; git add z2; git commit -qm z2
refuse "a skip reason of spaces is refused (merge)" env RELEASE_E2E_SKIP="              " git merge --no-ff -q -m m topicw
git merge --abort 2>/dev/null; git reset -q --hard
cd "$r"
git reset -q --hard HEAD~1
git switch -q feature
expect "pre-commit passes on a clean shell file" bash -c 'echo "#!/bin/sh" >a.sh; echo "true" >>a.sh; git add a.sh; git commit -q -m a'
expect "pre-commit passes on a .sh file whose first line is not a shebang" bash -c 'printf "exit 0\n" >c.sh; git add c.sh; git commit -q -m c'
refuse "pre-commit blocks a shell syntax error" bash -c 'printf "#!/bin/sh\nif then\n" >b.sh; git add b.sh; git commit -q -m b'
git rm -q -f --cached b.sh; rm -f b.sh

# ---------- release.sh
keyargs=(--key "$t/key")
rel() { RELEASE_E2E_CMD=${E2E:-true} RELEASE_ATTEST_SH=${ATT:-$t/attest-ok.sh} tools/release-gate/release.sh "$@" "${keyargs[@]}"; }
r=$t/rel; mkrepo "$r"; usegate "$r"
for v in 1 1.2 v1.2.3 01.2.3 1.2.3.4 1.2.3-; do refuse "bad version '$v' is refused" rel "$v" --dry-run; done
refuse "no version is refused" rel --dry-run
git tag v0.5.0 && git push -q origin v0.5.0
refuse "a version whose tag exists is refused" rel 0.5.0 --dry-run
refuse "a version not greater than the latest tag is refused" rel 0.3.0 --dry-run
git tag -d v0.5.0 >/dev/null; git push -q origin :refs/tags/v0.5.0
refuse "a version lower than the workspace version is refused" bash -c "sed -i.bak 's/0.1.0\"/0.9.0\"/' CHANGELOG.md; rm -f CHANGELOG.md.bak; cd '$r'; git checkout -q CHANGELOG.md; rel 0.0.9 --dry-run"
refuse "a version with no CHANGELOG entry is refused" rel 0.2.1 --dry-run
refuse "a missing key stops it" tools/release-gate/release.sh 0.2.0 --key "$t/nokey" --dry-run
: >"$GH_LOG"
before=$(git for-each-ref | md5; git status --porcelain)
RELEASE_E2E_CMD=true RELEASE_ATTEST_SH=$t/attest-ok.sh tools/release-gate/release.sh 0.2.0 --key "$t/key" --dry-run >"$t/dry.out" 2>&1
expect "--dry-run succeeds and prints the steps" grep -q 'dry run done' "$t/dry.out"
after=$(git for-each-ref | md5; git status --porcelain)
expect "--dry-run changes no ref and no file" test "$before" = "$after"
expect "--dry-run makes no branch, no stamp, no attestation, no gh call" bash -c "! git show-ref -q refs/heads/release-prep/v0.2.0 && [ ! -d .git/release-e2e ] && [ ! -e release-attest ] && [ ! -s '$GH_LOG' ] && [ -z \"\$(git ls-remote origin 'refs/heads/release-prep/*')\" ]"

git branch release/old
refuse "a local release/... branch makes release.sh refuse" rel 0.2.0 --dry-run
rel 0.2.0 --dry-run >"$t/clash.err" 2>&1
git branch -q -D release/old
expect "the clash message points at the rename" grep -q "Rename it to release-prep/old" "$t/clash.err"
git push -q origin main:refs/heads/release/remote
rel 0.2.0 --dry-run >"$t/clash2.err" 2>&1 && bad "a remote release/... branch makes release.sh refuse" || grep -q "Rename it to release-prep/remote" "$t/clash2.err" && ok "a remote release/... branch makes release.sh refuse, with the rename message" || bad "remote clash message"
git push -q origin :refs/heads/release/remote

E2E=false refuse "a red e2e stops the release" rel 0.2.0 --yes
expect "red e2e: bump commit made, nothing pushed, no gh call" bash -c "[ -z \"\$(git ls-remote origin 'refs/heads/release-prep/*')\" ] && [ ! -s '$GH_LOG' ] && [ ! -e release-attest ]"
git switch -q main; git branch -q -D release-prep/v0.2.0
ATT=$t/attest-badsig.sh refuse "a failing verify stops the release" rel 0.2.0 --yes
expect "failed verify: no push, no gh call" bash -c "[ -z \"\$(git ls-remote origin 'refs/heads/release-prep/*')\" ] && [ ! -s '$GH_LOG' ]"
git switch -q -f main; git reset -q --hard; git branch -q -D release-prep/v0.2.0; rm -rf release-attest "$(git rev-parse --git-common-dir)/release-e2e"
rel 0.2.0 --yes --closes 7 >"$t/green.out" 2>&1; expect "a green run succeeds" test $? = 0
expect "the branch is release-prep/v0.2.0 and was pushed" test -n "$(git ls-remote origin refs/heads/release-prep/v0.2.0)"
expect "one bump commit named Release v0.2.0, then the attestation commit" test "$(git log --format=%s -2 | tr '\n' ,)" = "Attest the suites for 0.2.0,Release v0.2.0,"
expect "gh pr create ran once, against release, titled Release v0.2.0" bash -c "[ \$(wc -l <'$GH_LOG') = 1 ] && grep -q -- '--base release' '$GH_LOG' && grep -q -- '--title Release v0.2.0' '$GH_LOG'"
expect "the attestation records the version" jq -e '.version.declared == "0.2.0"' release-attest/attestation.json
expect "verify.sh accepts the committed attestation" tools/release-attest/verify.sh --expect-version 0.2.0
refuse "verify.sh refuses the same attestation for another version" tools/release-attest/verify.sh --expect-version 0.2.1
expect "Cargo.toml and the path crates in Cargo.lock moved" bash -c "grep -q '^version = \"0.2.0\"' Cargo.toml && [ \$(grep -c '^version = \"0.2.0\"' Cargo.lock) = 2 ]"
expect "no old workspace version is left (the registry crate's 0.1.0 stays on purpose)" bash -c "! grep -n '0\.1\.0' Cargo.toml && [ \$(grep -c '0\.1\.0' Cargo.lock) = 1 ]"
expect "the script made no tag, locally or on origin" test -z "$(git tag -l)$(git ls-remote --tags origin)"

# ---------- issue 236: sign preflight, no-bump wording
realsk=$(command -v ssh-keygen); mkdir "$t/skbin"
cat >"$t/skbin/ssh-keygen" <<SKS
#!/bin/sh
case " \$* " in *" -Y sign "*)
  echo "askpass=\${SSH_ASKPASS_REQUIRE:-unset} args=\$*" >>"\$SK_LOG"
  [ "\${SK_MODE:-ok}" = nosign ] && { echo "incorrect passphrase supplied to decrypt private key" >&2; exit 255; } ;;
esac
exec "$realsk" "\$@"
SKS
chmod +x "$t/skbin/ssh-keygen"; export SK_LOG=$t/sk.log
cat >"$t/dh-pf.sh" <<'DHP'
#!/bin/sh
echo "$*" >>"$DH_PF_LOG"
exit 1
DHP
chmod +x "$t/dh-pf.sh"
r=$t/rel236; mkrepo "$r"; usegate "$r"; : >"$t/dh-pf.log"; : >"$SK_LOG"; rm -f "$t/e2e.marker"; : >"$GH_LOG"
PATH="$t/skbin:$PATH" SK_MODE=nosign DH_SH="$t/dh-pf.sh" DH_PF_LOG="$t/dh-pf.log" RELEASE_E2E_CMD="touch $t/e2e.marker" RELEASE_ATTEST_SH=$t/attest-ok.sh \
  tools/release-gate/release.sh 0.2.0 --key "$t/key" --yes >"$t/pf.out" 2>&1; rc=$?
expect "a key that cannot sign without a prompt stops release.sh with exit 2" test $rc = 2
expect "...with the ssh-add hint" grep -q 'the key cannot sign without a prompt: load it with ssh-add .*key first' "$t/pf.out"
expect "...before the e2e, any dh.sh call, any branch or gh call" bash -c "[ ! -e '$t/e2e.marker' ] && [ ! -s '$t/dh-pf.log' ] && [ ! -s '$GH_LOG' ] && ! git show-ref -q refs/heads/release-prep/v0.2.0"
expect "...and the tree is clean" test -z "$(git status --porcelain)"
expect "the preflight signs with SSH_ASKPASS_REQUIRE=never" grep -q 'askpass=never' "$t/sk.log"
: >"$SK_LOG"
PATH="$t/skbin:$PATH" SK_MODE=nosign RELEASE_E2E_CMD=true RELEASE_ATTEST_SH=$t/attest-ok.sh tools/release-gate/release.sh 0.2.0 --key "$t/key" --dry-run >"$t/pf2.out" 2>&1; rc=$?
expect "a dry run reports the unusable key too" test $rc = 2
# wording: a dry run and the PR body say whether a bump commit is needed
rel 0.2.0 --dry-run >"$t/dry-bump.out" 2>&1
expect "a dry run on a lower manifest says it would set the version" grep -q 'would: set \[workspace.package\] version = "0.2.0"' "$t/dry-bump.out"
mkdir "$t/bin-body"
cat >"$t/bin-body/gh" <<'GHB'
#!/bin/sh
echo "$*" >>"$GH_LOG"
while [ $# -gt 0 ]; do [ "$1" = --body-file ] && cp "$2" "$GH_BODY"; shift; done
echo https://example.invalid/pr/1
GHB
chmod +x "$t/bin-body/gh"; export GH_BODY=$t/pr-body.txt
r=$t/rel236b; mkrepo "$r"; usegate "$r"
sed -i.bak 's/^version = "0.1.0"/version = "0.2.0"/' Cargo.toml; rm Cargo.toml.bak
perl -0pi -e 's/version = "0.1.0"\n(?!source)/version = "0.2.0"\n/g' Cargo.lock
git commit -qam "manifest already at 0.2.0"; git push -q origin main
rel 0.2.0 --dry-run >"$t/dry-nobump.out" 2>&1
expect "a dry run with the manifest already at the version says no bump commit is needed" grep -q 'no bump commit needed, the manifest is already 0.2.0' "$t/dry-nobump.out"
expect "...and does not say it would bump or set the version" bash -c "! grep -Eqi 'would: set|would bump' '$t/dry-nobump.out'"
PATH="$t/bin-body:$PATH" rel 0.2.0 --yes --closes 9 >"$t/nobump.out" 2>&1; expect "a release with no bump needed succeeds" test $? = 0
expect "...makes no bump commit" test "$(git log --format=%s -2 | tr '\n' ,)" = "Attest the suites for 0.2.0,manifest already at 0.2.0,"
expect "...and the PR body does not claim a bump" bash -c "! grep -qi 'I bumped' '$GH_BODY'"
expect "...but says the manifest already had the version" grep -q 'no bump commit, the manifest already said 0.2.0' "$GH_BODY"
rm -f "$GH_BODY"
r=$t/rel236c; mkrepo "$r"; usegate "$r"
PATH="$t/bin-body:$PATH" rel 0.2.0 --yes >"$t/bump.out" 2>&1; expect "a release that needs the bump succeeds" test $? = 0
expect "...and its PR body says I bumped the version" grep -q 'I bumped the version' "$GH_BODY"

# ---------- review of #242: release.sh's preflight has no controlling terminal, and leaves nothing behind when interrupted
ssh-keygen -q -t ed25519 -N 'pw236' -C pass -f "$t/pkey"   # a passphrase key; no agent is started or touched here
if pty_available; then
  r=$t/rel236p; mkrepo "$r"; usegate "$r"; cp "$t/pkey.pub" "$PUBKEY_PATH"; git commit -qam pub; git push -q origin main
  ( unset SSH_AUTH_SOCK SSH_AGENT_PID; export RELEASE_E2E_CMD=true RELEASE_ATTEST_SH=$t/attest-ok.sh
    pty_run 6 tools/release-gate/release.sh 0.2.0 --key "$t/pkey" --dry-run ) >"$t/pty-rel.out" 2>&1; rc=$?
  expect "from a terminal, release.sh refuses a passphrase key that is not loaded at once (exit 2, no hang)" test $rc = 2
  expect "...with the ssh-add message and no passphrase prompt" bash -c "grep -q 'the key cannot sign without a prompt: load it with ssh-add' '$t/pty-rel.out' && ! grep -qi 'enter passphrase' '$t/pty-rel.out'"
  printf 'set -m\n' >"$t/monitor.env"
  ( unset SSH_AUTH_SOCK SSH_AGENT_PID; export BASH_ENV=$t/monitor.env RELEASE_E2E_CMD=true RELEASE_ATTEST_SH=$t/attest-ok.sh
    pty_run 8 tools/release-gate/release.sh 0.2.0 --key "$t/pkey" --dry-run ) >"$t/pty-rel2.out" 2>&1; rc=$?
  expect "with job control on, release.sh still refuses a passphrase key at once (exit 2), no prompt" bash -c "test $rc = 2 && ! grep -qi 'enter passphrase' '$t/pty-rel2.out'"
else echo "SKIP: script(1) or timeout is missing, the pseudo-terminal test does not run" >&2; fi
# an interrupt during the preflight leaves no release-signcheck.* directory
r=$t/rel236t; mkrepo "$r"; usegate "$r"; mkdir "$t/tmp-int"
cat >"$t/skbin/ssh-keygen" <<SKI
#!/bin/sh
case " \$* " in *" -Y sign "*) p=\$PPID; while [ -n "\$p" ] && ! ps -o command= -p \$p | grep -q 'release-gate/release.sh'; do p=\$(ps -o ppid= -p \$p | tr -d ' '); done; [ -n "\$p" ] && kill -TERM \$p; sleep 1; exit 1 ;; esac
exec $realsk "\$@"
SKI
chmod +x "$t/skbin/ssh-keygen"
PATH="$t/skbin:$PATH" TMPDIR=$t/tmp-int RELEASE_E2E_CMD=true RELEASE_ATTEST_SH=$t/attest-ok.sh tools/release-gate/release.sh 0.2.0 --key "$t/key" --dry-run >/dev/null 2>&1; rc=$?
expect "an interrupted preflight (TERM) exits 143" test $rc = 143
expect "an interrupted preflight leaves no release-signcheck directory" test -z "$(ls "$t/tmp-int")"

# ---------- issue 267: the push prompt takes y, Y, yes, YES, Yes and nothing else
if pty_available; then
  # in: what is typed at the prompt; want: push (confirmed) or stay (refused)
  prompt267() { # name input want
    r=$t/rel267; mkrepo "$r"; usegate "$r"; : >"$GH_LOG"
    ( export RELEASE_E2E_CMD=true RELEASE_ATTEST_SH=$t/attest-ok.sh; unset SSH_AUTH_SOCK SSH_AGENT_PID
      PTY_INPUT=$2 pty_run 12 tools/release-gate/release.sh 0.2.0 --key "$t/key" ) >"$t/p267.out" 2>&1
    if [ "$3" = push ]; then
      expect "#267 $1 confirms: the branch is pushed and the PR opened" bash -c "[ -n \"\$(git ls-remote origin refs/heads/release-prep/v0.2.0)\" ] && [ \$(wc -l <'$GH_LOG') = 1 ]"
    else
      expect "#267 $1 does not confirm: nothing pushed, no PR" bash -c "[ -z \"\$(git ls-remote origin refs/heads/release-prep/v0.2.0)\" ] && [ ! -s '$GH_LOG' ]"
      expect "#267 $1 prints the refusal naming y and yes" grep -q 'not confirmed: only y or yes pushes (the commits stay local)' "$t/p267.out"
    fi
  }
  prompt267 "y" $'y\n' push
  prompt267 "Y" $'Y\n' push
  prompt267 "yes" $'yes\n' push
  prompt267 "YES" $'YES\n' push
  prompt267 "Yes" $'Yes\n' push
  prompt267 "n" $'n\n' stay
  prompt267 "N" $'N\n' stay
  prompt267 "an empty answer" $'\n' stay
  prompt267 "end of file" $'\004' stay
  prompt267 "yy" $'yy\n' stay
  prompt267 "no" $'no\n' stay
  prompt267 "a leading space then y" $' y\n' stay
  prompt267 "y then a trailing space" $'y \n' stay
  prompt267 "yEs" $'yEs\n' stay
  prompt267 "yeS" $'yeS\n' stay
  prompt267 "YEs" $'YEs\n' stay
else echo "SKIP: script(1) or timeout is missing, the push prompt tests do not run" >&2; fi

# ---------- third review of #242: a hung ssh-keygen and signals (release.sh)
mkdir "$t/hangbin"
cat >"$t/hangbin/ssh-keygen" <<HGS
#!/bin/sh
case " \$* " in *" -Y sign "*) echo \$\$ >"$t/hang.pid"; echo \$PPID >"$t/hang.ppid"; exec sleep 30 ;; esac
exec $realsk "\$@"
HGS
chmod +x "$t/hangbin/ssh-keygen"
hang_run() { # mode(group|pid) sig out cmd... ; sets hrc
  local mode=$1 sig=$2 out=$3; shift 3; rm -f "$t/hang.pid" "$t/hang.ppid"
  perl -MPOSIX -e 'setpgid(0,0); $SIG{INT} = "DEFAULT"; exec @ARGV' "$@" >"$out" 2>&1 </dev/null &
  local hp=$! i
  for i in $(seq 1 60); do [ -s "$t/hang.ppid" ] && break; sleep 0.25; done
  if [ $mode = group ]; then kill -$sig -- -$hp 2>/dev/null; else kill -$sig $hp 2>/dev/null; fi
  for i in $(seq 1 32); do kill -0 $hp 2>/dev/null || break; sleep 0.25; done
  if kill -0 $hp 2>/dev/null; then kill -9 $hp 2>/dev/null; hrc=still-running; else wait $hp; hrc=$?; fi
  sleep 0.5
}
gone() { [ -s "$t/hang.pid" ] && ! kill -0 "$(cat "$t/hang.pid")" 2>/dev/null && ! kill -0 "$(cat "$t/hang.ppid")" 2>/dev/null; }
r=$t/rel236h; mkrepo "$r"; usegate "$r"; mkdir "$t/tmp-h"
for spec in "group INT 130" "pid TERM 143" "pid HUP 129"; do
  set -- $spec
  hang_run $1 $2 "$t/h-$1-$2.out" env PATH="$t/hangbin:$PATH" TMPDIR=$t/tmp-h RELEASE_E2E_CMD=true RELEASE_ATTEST_SH=$t/attest-ok.sh tools/release-gate/release.sh 0.2.0 --key "$t/key" --dry-run
  expect "release.sh, $2 to the $1 during a hung preflight: exit $3" test "$hrc" = "$3"
  expect "...no wrong message" bash -c "! grep -q 'the key cannot sign\|ssh-keygen exited' '$t/h-$1-$2.out'"
  expect "...the hung ssh-keygen and its wrapper are gone, and no release-signcheck directory is left" bash -c "$(declare -f gone); t='$t'; gone && [ -z \"\$(ls '$t/tmp-h')\" ]"
done

# #252: a ssh-keygen that ignores the signal is KILLed after the grace period, same exit codes
mkdir "$t/ignbin"
cat >"$t/ignbin/ssh-keygen" <<IGS
#!/bin/sh
case " \$* " in *" -Y sign "*) trap '' INT TERM HUP; echo \$\$ >"$t/hang.pid"; echo \$PPID >"$t/hang.ppid"; sleep 30 ;; esac
exec $realsk "\$@"
IGS
chmod +x "$t/ignbin/ssh-keygen"; mkdir "$t/tmp-i"
for spec in "group INT 130" "pid TERM 143" "pid HUP 129"; do
  set -- $spec
  hang_run $1 $2 "$t/i-$1-$2.out" env PATH="$t/ignbin:$PATH" TMPDIR=$t/tmp-i RELEASE_E2E_CMD=true RELEASE_ATTEST_SH=$t/attest-ok.sh tools/release-gate/release.sh 0.2.0 --key "$t/key" --dry-run
  expect "#252 release.sh, $2 to the $1 with a child that ignores it: exit $3 within 8 s" test "$hrc" = "$3"
  expect "...the child that ignored the signal and its wrapper are gone" gone
done

# ---------- fourth review of #242: Ctrl-C during step 5 stops release.sh with 130, not 'did not sign a green run'
mkdir "$t/third"
cat >"$t/third/ssh-keygen" <<TRD
#!/bin/sh
case " \$* " in *" -Y sign "*)
  echo x >>"$t/sign.count"; n=\$(wc -l <"$t/sign.count" | tr -d ' ')
  if [ "\$n" = 3 ]; then echo \$\$ >"$t/hang.pid"; echo \$PPID >"$t/hang.ppid"; exec sleep 30; fi ;;
esac
exec $realsk "\$@"
TRD
chmod +x "$t/third/ssh-keygen"
r=$t/rel236s; mkrepo "$r"; usegate "$r"; rm -f "$t/sign.count"; : >"$GH_LOG"
hang_run group INT "$t/h-step5.out" env PATH="$t/third:$PATH" RELEASE_E2E_CMD=true RELEASE_ATTEST_SH=$t/attest-ok.sh tools/release-gate/release.sh 0.2.0 --key "$t/key" --yes
expect "release.sh: INT to the group while attest.sh signs (3rd sign call) exits 130" test "$hrc" = 130
expect "...and does not say attest.sh did not sign a green run" bash -c "! grep -q 'did not sign' '$t/h-step5.out'"
expect "...nothing was pushed or opened" bash -c "[ -z \"\$(git ls-remote origin 'refs/heads/release-prep/*')\" ] && [ ! -s '$GH_LOG' ]"

# ---------- follow-up: checked stdin save, temp paths, characters in a reason, invisible characters
base2 s20
echo fu >fu; git add fu; git commit -qm fu
fu_l=$(git rev-parse HEAD); fu_t=$(git rev-parse origin/release)
printf 'refs/heads/x %s refs/heads/release %s\n' "$fu_l" "$fu_t" >"$t/fu-line.txt"
expect "control, no size limit: a skipped release push passes the hook" bash -c "RELEASE_E2E_SKIP='a long enough reason' bash .githooks/pre-push origin url <'$t/fu-line.txt'"
out=$( (trap '' XFSZ; ulimit -f 0; RELEASE_E2E_SKIP='a long enough reason' bash .githooks/pre-push origin url <"$t/fu-line.txt") 2>&1 ); rc=$?
expect "a push-line save that cannot be written is refused, even with the skip set" test $rc != 0
expect "...and the refusal says the lines could not be saved" bash -c "printf '%s' \"\$1\" | grep -q 'cannot save the push lines'" _ "$out"
out=$(TMPDIR=$t/no-such-tmp bash .githooks/pre-push origin url <"$t/fu-line.txt" 2>&1); rc=$?
expect "an unusable TMPDIR (mktemp fails) refuses the push with a message of its own" bash -c "test $rc != 0 && printf '%s' \"\$1\" | grep -q 'cannot make a temp'" _ "$out"
# the test runners must not carry on with an empty temp dir: rm -rf "$t/..." would aim at paths under /
runner_guarded=(0 0); ri=0
for runner in tests/release-gate/run.sh tests/release-attest/run.sh; do
  c1="grep -A1 '^t=\$(/usr/bin/mktemp' '$src/$runner' | grep -q '|| exit 1'"
  c2="grep -q '\[ -n \"\$t\" \] && \[ -d \"\$t\" \]' '$src/$runner'"
  expect "$runner stops when mktemp fails" bash -c "$c1"
  expect "$runner checks the temp dir is a non-empty directory" bash -c "$c2"
  if bash -c "$c1" >/dev/null 2>&1 && bash -c "$c2" >/dev/null 2>&1; then runner_guarded[$ri]=1; fi
  ri=$((ri + 1))
done
ri=0
for runner in tests/release-gate/run.sh tests/release-attest/run.sh; do
  # run the stub only on a runner whose guard the grep above just confirmed: an unguarded runner (a mixed checkout) would carry on with t='' and aim rm -rf at /
  g=${runner_guarded[$ri]}; ri=$((ri + 1))
  [ "$g" = 1 ] || { bad "$runner is not guarded, so the failing-mktemp stub run was skipped"; continue; }
  out=$(TMPDIR="$t/no-such-dir" bash "$src/$runner" 2>/dev/null); rc=$?
  expect "$runner exits 1 at once, running nothing, when mktemp fails" bash -c "test $rc = 1 && test -z \"\$1\"" _ "$out"
done
expect "e2e.sh no longer copies the red log to a predictable name" bash -c "! grep -q 'release-e2e-log-\\\$\\\$' '$src/tools/release-gate/e2e.sh'"
mkdir -p "$t/kt"
out=$(env -u RELEASE_E2E_CMD -u RELEASE_E2E_TEST_STAMPS TMPDIR=$t/kt SUITES_LIST="$SUITES" DH_SH="$t/dh-fake.sh" MODE=decoy-inside tools/release-gate/e2e.sh 2>&1); kept=$(printf '%s' "$out" | sed -n 's/^log kept in \([^ ]*\) .*/\1/p')
expect "...the kept log has a mktemp name (random suffix, not the pid)" bash -c "[ -f '$kept' ] && ! printf '%s' '$kept' | grep -Eq 'release-e2e-log-[0-9]+\$'"
# characters, not bytes, and invisible characters do not count
rep() { local i out=; for ((i = 0; i < $2; i++)); do out=$out$1; done; printf '%s' "$out"; }
EA='\303\251'; ZW='\342\200\213'; BOM='\357\273\277'; WJ='\342\201\240'; HF='\343\205\244'; BR='\342\240\200'; MV='\341\240\216'
fu_hook() { printf 'refs/heads/x %s refs/heads/release %s\n' "$fu_l" "$fu_t" | env LC_ALL="$1" RELEASE_E2E_SKIP="$2" bash .githooks/pre-push origin url; }
for loc in C en_US.UTF-8; do
  refuse "[$loc] six e-acute (6 characters, 12 bytes) are refused (push)" fu_hook $loc "$(printf "$(rep "$EA" 6)")"
  refuse "[$loc] six e-acute are refused by e2e.sh too" env LC_ALL=$loc RELEASE_E2E_SKIP="$(printf "$(rep "$EA" 6)")" tools/release-gate/e2e.sh
  expect "[$loc] twelve e-acute (12 characters) pass (push)" fu_hook $loc "$(printf "$(rep "$EA" 12)")"
  for inv in ZW BOM WJ HF BR MV; do
    refuse "[$loc] twelve invisible characters ($inv) are refused (push)" fu_hook $loc "$(printf "$(rep "${!inv}" 12)")"
    refuse "[$loc] 'ab' plus ten invisible characters ($inv) are refused (push)" fu_hook $loc "$(printf "ab$(rep "${!inv}" 10)")"
  done
  refuse "[$loc] twelve zero-width spaces are refused by e2e.sh" env LC_ALL=$loc RELEASE_E2E_SKIP="$(printf "$(rep "$ZW" 12)")" tools/release-gate/e2e.sh
  expect "[$loc] a real reason with a zero-width space inside still passes" fu_hook $loc "$(printf "bootstrap${ZW} release now")"
done
git switch -q -c topicfu; echo y >yfu; git add yfu; git commit -qm yfu; git switch -q release; echo z >zfu; git add zfu; git commit -qm zfu
refuse "twelve zero-width spaces are refused (merge)" env RELEASE_E2E_SKIP="$(printf "$(rep "$ZW" 12)")" git merge --no-ff -q -m m topicfu
refuse "six e-acute are refused under LC_ALL=C (merge)" env LC_ALL=C RELEASE_E2E_SKIP="$(printf "$(rep "$EA" 6)")" git merge --no-ff -q -m m topicfu
git merge --abort 2>/dev/null; git reset -q --hard

# a count that is not a number must refuse, never pass (perl stub: the length call prints x)
mkdir -p "$t/badperl"; realperl=$(command -v perl)
printf '#!/bin/sh\ncase "$*" in *length*) printf x ;; *) exec %s "$@" ;; esac\n' "$realperl" >"$t/badperl/perl"; chmod +x "$t/badperl/perl"
refuse "a character count that is not a number refuses the skip (e2e.sh, gate_skipped)" env PATH="$t/badperl:$PATH" RELEASE_E2E_SKIP="a perfectly long reason" tools/release-gate/e2e.sh
refuse "...and in the push hook" bash -c "printf 'refs/heads/x %s refs/heads/release %s\n' '$fu_l' '$fu_t' | env PATH='$t/badperl:$PATH' RELEASE_E2E_SKIP='a perfectly long reason' bash .githooks/pre-push origin url"
# ---------- #234: the skip reason rule (non-blank code points, same as the causl gates)
# one codepoint repeated: cp HEX COUNT. Needs no locale: perl writes the UTF-8 bytes itself.
cp() { perl -CO -e 'print chr(hex($ARGV[0])) x $ARGV[1]' "$1" "$2"; }
mkdir -p "$t/ska" && cp_topic=topicsk
git switch -q release 2>/dev/null; git reset -q --hard
git switch -q -c $cp_topic; echo sk >sk.txt; git add sk.txt; git commit -qm sk; git switch -q release; echo skr >skr.txt; git add skr.txt; git commit -qm skr
sk_head=$(git rev-parse HEAD)
sk_l=$(git rev-parse HEAD); sk_t=$(git rev-parse origin/release)
sk_push() { printf 'refs/heads/x %s refs/heads/release %s\n' "$sk_l" "$sk_t" | env LC_ALL="$1" RELEASE_E2E_SKIP="$2" bash .githooks/pre-push origin url; }
sk_merge() { local rc; env LC_ALL="$1" RELEASE_E2E_SKIP="$2" git merge --no-ff -q -m m $cp_topic; rc=$?; git merge --abort 2>/dev/null; git reset -q --hard "$sk_head"; return $rc; }
sk_gate() { env LC_ALL="$1" RELEASE_E2E_SKIP="$2" tools/release-gate/e2e.sh; }
sk_each() { # name, expect|refuse, locale, reason: the push hook, the merge hook and gate_skipped all judge it the same way
  local name=$1 verdict=$2 loc=$3 reason=$4
  $verdict "[$loc] $name (push hook)" sk_push "$loc" "$reason"
  $verdict "[$loc] $name (merge hook)" sk_merge "$loc" "$reason"
  $verdict "[$loc] $name (gate_skipped)" sk_gate "$loc" "$reason"
}
for loc in C en_US.UTF-8; do
  sk_each "a, ten spaces, b is refused" refuse $loc "a          b"
  sk_each "a plus eleven U+0301 is refused" refuse $loc "a$(cp 301 11)"
  for u in fff9 fffa fffb e000 0378 0600 13430 1d159 e0100 1d173 1bca0 200d 2800; do
    sk_each "twelve U+$u are refused" refuse $loc "$(cp $u 12)"
    sk_each "ab plus ten U+$u are refused" refuse $loc "ab$(cp $u 10)"
  done
  sk_each "bootstrap the gate passes" expect $loc "bootstrap the gate"
  sk_each "cafe with an accent passes" expect $loc "caf$(cp e9 1) bootstrap"
  sk_each "punctuation passes" expect $loc "bootstrap: gate, now (once)."
  sk_each "U+FFFC and U+FFFD count as characters" expect $loc "ab$(cp fffc 5)$(cp fffd 5)"
  sk_each "PERL5OPT=-CSD does not break a reason with an accent" expect $loc "$(PERL5OPT=-CSD; printf 'caf\303\251 gate bootstrap now')"
done
# PERL5OPT in the environment of the hook itself (the previous row only exported it for the printf)
refuse "PERL5OPT=-CSD in the hook's environment still refuses twelve zero-width joiners (push)" env PERL5OPT=-CSD bash -c "$(declare -f cp sk_push); sk_push C \"\$(cp 200d 12)\""
expect "PERL5OPT=-CSD in the hook's environment still passes an accented reason (push)" env PERL5OPT=-CSD bash -c "$(declare -f sk_push); sk_l=$sk_l; sk_t=$sk_t; sk_push C \"\$(printf 'caf\\303\\251 gate bootstrap now')\""
# the two one-liners are the same bytes in the three places
norm_of() { grep -o "perl -T -C0 -e 'use Encode qw(decode FB_CROAK);.*print \$d;'" "$1"; }
count_of() { grep -o "perl -T -C0 -e 'use Encode qw(decode); local.*print length(\$d);'" "$1"; }
for fn in norm_of count_of; do
  a=$($fn "$src/.githooks/pre-push"); b=$($fn "$src/.githooks/pre-merge-commit"); c=$($fn "$src/tools/release-gate/lib.sh")
  expect "$fn finds exactly one line in each file" bash -c '[ -n "$1" ] && [ -n "$2" ] && [ -n "$3" ] && [ "$(printf "%s\n" "$1" | wc -l)" -eq 1 ]' _ "$a" "$b" "$c"
  expect "$fn is byte-identical in pre-push, pre-merge-commit and lib.sh" bash -c '[ "$1" = "$2" ] && [ "$2" = "$3" ]' _ "$a" "$b" "$c"
done
git reset -q --hard "$sk_head"

# ---------- #235: the runners start from a clean environment and keep their files inside their own temp dir
for runner in tests/release-gate/run.sh tests/release-attest/run.sh; do
  mkdir -p "$t/hdr/a/b" "$t/hdr/outer"
  { sed -n '/^set -u$/,/^export DH_SH=/p' "$src/$runner" | grep -v -e '^here=' -e '^src=' -e '^tools=' -e '^\. "\$tools' -e '^export DH_SH='
    echo 'env | grep -E "^(RELEASE_|DH_)" | grep -v "^DH_SH=" | sed "s/^/LEAK /"'
    echo 'case $TMPDIR in "$t"/*) echo inside ;; *) echo "outside $TMPDIR" ;; esac'
    echo '/usr/bin/mktemp "${TMPDIR:-/tmp}/release-e2e-log.XXXXXX" >/dev/null; mkdir -p "${TMPDIR:-/tmp}/attest-logs-$$"'
  } >"$t/hdr/a/b/head.sh"
  out=$(env -u DH_SH -u DH_REAL_LOG PATH="${PATH#"$t/spybin:"}" RELEASE_E2E_HOST=x RELEASE_E2E_SKIP=x RELEASE_E2E_CMD=x RELEASE_E2E_TEST_STAMPS=x RELEASE_GATE_TEST=x RELEASE_ATTEST_SH=x DH_HOST=x DH_CPUS=1 DH_MEMORY=1g DH_UNFENCED=1 TMPDIR="$t/hdr/outer" bash "$t/hdr/a/b/head.sh" 2>&1)
  expect "$runner: variables exported by the caller are gone before the first test" bash -c '! printf "%s\n" "$1" | grep -q LEAK' _ "$out"
  expect "$runner: TMPDIR for the run is inside its own temp dir" bash -c 'printf "%s\n" "$1" | grep -qx inside' _ "$out"
  expect "$runner: nothing is left in the caller's TMPDIR afterwards" test -z "$(ls -A "$t/hdr/outer")"
  rm -rf "$t/hdr"
done

spy_hits=$(grep -E '/(release-e2e|release-e2e-log|\.tmp|release-signcheck|release-pr|release-gate-head|release-prepush|attest|attest-logs|\.attest-stage|attest-verify)\.XXXXXX|\.ls\.XXXXXX' "$SPY_LOG" 2>/dev/null)
expect "no gate script called the mktemp found on PATH (all of them use /usr/bin/mktemp)" test -z "$spy_hits"
[ -z "$spy_hits" ] || { echo "bare mktemp calls:" >&2; echo "$spy_hits" | sed "s#.*/##" | sort | uniq -c >&2; }
expect "no test reached the real dh.sh or docker (the default DH_SH stub was never called)" test ! -s "$t/dh-real.log"
[ -s "$t/dh-real.log" ] && { echo "calls that reached the dh.sh stub:" >&2; cat "$t/dh-real.log" >&2; }
[ $fails = 0 ] || tail -8 "$t/green.out" >&2
echo "$n tests, $fails failed"
[ $fails = 0 ]
