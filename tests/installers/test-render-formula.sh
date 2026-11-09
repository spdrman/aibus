#!/bin/sh
# Tests for packaging/homebrew/render-formula.sh. Needs sh, awk, sed and ruby (for ruby -c).
set -u

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
render=$repo/packaging/homebrew/render-formula.sh
t=$(mktemp -d)
trap 'rm -rf "$t"' EXIT
fails=0
n=0
ok() { n=$((n + 1)); echo "ok $n - $1"; }
bad() { n=$((n + 1)); fails=$((fails + 1)); echo "not ok $n - $1"; }
expect() { name=$1; shift; if "$@"; then ok "$name"; else bad "$name"; fi; }
refuse() { name=$1; shift; if "$@"; then bad "$name"; else ok "$name"; fi; }

# A SHA256SUMS where every file has a different, recognisable checksum (target index repeated).
i=0
: >"$t/SHA256SUMS"
for f in aibus-aarch64-apple-darwin.tar.gz aibus-x86_64-apple-darwin.tar.gz \
  aibus-x86_64-unknown-linux-gnu.tar.gz aibus-aarch64-unknown-linux-gnu.tar.gz \
  aibus-x86_64-unknown-linux-musl.tar.gz aibus-x86_64-pc-windows-msvc.zip aibus.cdx.json; do
  i=$((i + 1))
  d=$(printf '%x' "$i")
  printf '%s  %s\n' "$(printf '%064s' '' | tr ' ' "$d")" "$f" >>"$t/SHA256SUMS"
done

sh "$render" 0.1.0 "$t/SHA256SUMS" "$t/aibus.rb" >/dev/null
f=$t/aibus.rb

# The sha256 on the line after each url must be that target's checksum.
sum_after() { awk -v u="$1" 'index($0, u) {getline; gsub(/.*sha256 "|".*/, ""); print; exit}' "$f"; }
want() { printf '%064s' '' | tr ' ' "$1"; }
expect "aarch64 macOS checksum" test "$(sum_after aibus-aarch64-apple-darwin.tar.gz)" = "$(want 1)"
expect "x86_64 macOS checksum" test "$(sum_after aibus-x86_64-apple-darwin.tar.gz)" = "$(want 2)"
expect "aarch64 linux (glibc) checksum" test "$(sum_after aibus-aarch64-unknown-linux-gnu.tar.gz)" = "$(want 4)"
expect "x86_64 linux (musl) checksum" test "$(sum_after aibus-x86_64-unknown-linux-musl.tar.gz)" = "$(want 5)"
refuse "the glibc x86_64 and windows sums are not used" grep -E "$(want 3)|$(want 6)|$(want 7)" "$f"
expect "version is set" grep -q '^  version "0.1.0"$' "$f"
refuse "no template tokens left" grep -q '@[A-Z0-9_]*@' "$f"
refuse "real sums carry no placeholder header" grep -q PLACEHOLDER "$f"
expect "livecheck block present" grep -q 'livecheck do' "$f"
expect "test block runs aibus version" grep -q 'bin}/aibus version' "$f"
expect "rendered formula passes ruby -c" ruby -c "$f"

# A leading v on the version is accepted.
sh "$render" v0.2.0 "$t/SHA256SUMS" "$t/v.rb" >/dev/null
expect "leading v is stripped" grep -q '^  version "0.2.0"$' "$t/v.rb"

# Placeholder checksums are marked loudly.
zero=$(printf '%064s' '' | tr ' ' 0)
for tg in aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu x86_64-unknown-linux-musl; do
  printf '%s  aibus-%s.tar.gz\n' "$zero" "$tg"
done >"$t/ZERO"
sh "$render" 0.1.0 "$t/ZERO" "$t/z.rb" >/dev/null
expect "all-zero sums get a PLACEHOLDER header" grep -q '^# PLACEHOLDER CHECKSUMS' "$t/z.rb"
expect "placeholder formula passes ruby -c" ruby -c "$t/z.rb"

# The committed formula is exactly what the script renders from placeholder sums.
sh "$render" 0.1.0 "$t/ZERO" "$t/committed.rb" >/dev/null
expect "committed Formula/aibus.rb is the rendered placeholder" diff -u "$repo/Formula/aibus.rb" "$t/committed.rb"

# Failure modes.
grep -v musl "$t/SHA256SUMS" >"$t/MISSING"
refuse "missing target is an error" sh "$render" 0.1.0 "$t/MISSING" "$t/m.rb"
sed 's/^1111/zzzz/' "$t/SHA256SUMS" >"$t/BADHEX"
refuse "non-hex checksum is an error" sh "$render" 0.1.0 "$t/BADHEX" "$t/h.rb"
refuse "bad version is an error" sh "$render" latest "$t/SHA256SUMS" "$t/l.rb"
refuse "unreadable sums file is an error" sh "$render" 0.1.0 "$t/nope" "$t/n.rb"

echo "1..$n"
if [ "$fails" -ne 0 ]; then echo "# $fails failed"; exit 1; fi
echo "# all passed"
