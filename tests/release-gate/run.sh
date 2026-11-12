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
  echo "=== SUITE $name ==="; echo "test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured"; echo "=== EXIT $name 0 ==="
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
export GH_LOG=$t/gh.log
export PATH="$t/bin:$PATH"

# ---------- hooks and runner
r=$t/repo; mkrepo "$r"; cd "$r" || exit 1
git switch -q -c release; git switch -q -c feature
echo two >f.txt; git commit -q -am feature
git switch -q release; echo r >r.txt; git add r.txt; git commit -q -m onrelease
refuse "merge into release with a red e2e is blocked" env RELEASE_E2E_CMD=false git merge --no-ff -m m feature
expect "a blocked merge leaves HEAD where it was" test "$(git rev-parse HEAD)" = "$(git rev-parse release)"
expect "red e2e writes no stamp" test -z "$(ls "$(git rev-parse --git-common-dir)/release-e2e" 2>/dev/null)"
git merge --abort 2>/dev/null; git reset -q --hard
expect "merge into release with a green e2e is allowed" env RELEASE_E2E_CMD=true git merge --no-ff -m m feature
expect "green e2e writes a stamp for the merged tree" test -f "$(git rev-parse --git-common-dir)/release-e2e/$(tree_hash . HEAD)"
expect "the stamp records tree, command, host and time" grep -q '^command=true' "$(git rev-parse --git-common-dir)/release-e2e/$(tree_hash . HEAD)"
expect "push to release with a stamp passes" git push -q origin release
echo three >f.txt; git commit -q -am more
refuse "push to release with a stamp for another tree is refused" git push -q origin release
expect "push to a feature branch passes without a stamp" git push -q origin release:refs/heads/feature2
git reset -q --hard HEAD~1 2>/dev/null; git switch -q feature; echo x >>f.txt; git commit -q -am x
git switch -q release; git reset -q --hard feature
refuse "push to release without any stamp is refused" git push -q origin release
refuse "a squash/ff style push (no merge hook) is still refused without a stamp" git push -q origin release:refs/heads/release
expect "RELEASE_E2E_SKIP lets the push through" env RELEASE_E2E_SKIP="testing the skip" git push -q -f origin release
expect "the skip is logged with its reason" grep -q 'testing the skip' "$(git rev-parse --git-common-dir)/release-e2e/skips.log"
echo dirty >>f.txt
refuse "e2e.sh refuses a dirty tree" env RELEASE_E2E_CMD=true tools/release-gate/e2e.sh
git checkout -q f.txt
git switch -q feature
expect "pre-commit passes on a clean shell file" bash -c 'echo "#!/bin/sh" >a.sh; echo "true" >>a.sh; git add a.sh; git commit -q -m a'
refuse "pre-commit blocks a shell syntax error" bash -c 'printf "#!/bin/sh\nif then\n" >b.sh; git add b.sh; git commit -q -m b'
git rm -q -f --cached b.sh; rm -f b.sh

# ---------- release.sh
keyargs=(--key "$t/key")
rel() { RELEASE_E2E_CMD=${E2E:-true} RELEASE_ATTEST_SH=${ATT:-$t/attest-ok.sh} tools/release-gate/release.sh "$@" "${keyargs[@]}"; }
r=$t/rel; mkrepo "$r"; cd "$r" || exit 1
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
expect "--dry-run makes no branch, no stamp, no attestation, no gh call" bash -c "! git show-ref -q refs/heads/release/v0.2.0 && [ ! -d .git/release-e2e ] && [ ! -e release-attest ] && [ ! -s '$GH_LOG' ] && [ -z \"\$(git ls-remote origin 'refs/heads/release/*')\" ]"

E2E=false refuse "a red e2e stops the release" rel 0.2.0 --yes
expect "red e2e: bump commit made, nothing pushed, no gh call" bash -c "[ -z \"\$(git ls-remote origin 'refs/heads/release/*')\" ] && [ ! -s '$GH_LOG' ] && [ ! -e release-attest ]"
git switch -q main; git branch -q -D release/v0.2.0
ATT=$t/attest-badsig.sh refuse "a failing verify stops the release" rel 0.2.0 --yes
expect "failed verify: no push, no gh call" bash -c "[ -z \"\$(git ls-remote origin 'refs/heads/release/*')\" ] && [ ! -s '$GH_LOG' ]"
git switch -q -f main; git reset -q --hard; git branch -q -D release/v0.2.0; rm -rf release-attest "$(git rev-parse --git-common-dir)/release-e2e"
expect "a green run succeeds" rel 0.2.0 --yes --closes 7
expect "the branch is release/v0.2.0 and was pushed" test -n "$(git ls-remote origin refs/heads/release/v0.2.0)"
expect "one bump commit named Release v0.2.0, then the attestation commit" test "$(git log --format=%s -2 | tr '\n' ,)" = "Attest the suites for 0.2.0,Release v0.2.0,"
expect "gh pr create ran once, against release, titled Release v0.2.0" bash -c "[ \$(wc -l <'$GH_LOG') = 1 ] && grep -q -- '--base release' '$GH_LOG' && grep -q -- '--title Release v0.2.0' '$GH_LOG'"
expect "the attestation records the version" jq -e '.version.declared == "0.2.0"' release-attest/attestation.json
expect "verify.sh accepts the committed attestation" tools/release-attest/verify.sh --expect-version 0.2.0
refuse "verify.sh refuses the same attestation for another version" tools/release-attest/verify.sh --expect-version 0.2.1
expect "Cargo.toml and the path crates in Cargo.lock moved" bash -c "grep -q '^version = \"0.2.0\"' Cargo.toml && [ \$(grep -c '^version = \"0.2.0\"' Cargo.lock) = 2 ]"
expect "no old workspace version is left (the registry crate's 0.1.0 stays on purpose)" bash -c "! grep -n '0\.1\.0' Cargo.toml && [ \$(grep -c '0\.1\.0' Cargo.lock) = 1 ]"
expect "the script made no tag, locally or on origin" test -z "$(git tag -l)$(git ls-remote --tags origin)"

echo "$n tests, $fails failed"
[ $fails = 0 ]
