#!/usr/bin/env bash
# Tests for the local release gate (.githooks/, tools/release-gate/). Throwaway repos, a throwaway key,
# a fake e2e command, a fake attest runner and a fake `gh` on PATH. No container, no cargo, no real e2e.
# Needs bash, git, jq, perl and ssh-keygen.
set -u
here=$(cd "$(dirname "$0")" && pwd)
src=$(cd "$here/../.." && pwd)
t=$(mktemp -d "${TMPDIR:-/tmp}/gate-test.XXXXXX")
trap 'rm -rf "$t"' EXIT
fails=0 n=0
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
refuse "a skip reason under 12 characters is refused (push)" env RELEASE_E2E_SKIP=short git push -q origin release:refs/heads/release
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
git switch -q -c topic2 HEAD~1; echo u >u.txt; git add u.txt; git commit -qm topic2; git switch -q release; git merge -q -s ours --no-ff -m m2 topic2 >/dev/null 2>&1
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
echo n >n; git add n; git commit -qm n; dec=$t/decoy5; git clone -q --bare "$r2" "$dec"
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
cd "$r"
git reset -q --hard HEAD~1
git switch -q feature
expect "pre-commit passes on a clean shell file" bash -c 'echo "#!/bin/sh" >a.sh; echo "true" >>a.sh; git add a.sh; git commit -q -m a'
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

[ $fails = 0 ] || tail -8 "$t/green.out" >&2
echo "$n tests, $fails failed"
[ $fails = 0 ]
