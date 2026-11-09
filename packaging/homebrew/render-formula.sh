#!/bin/sh
# Render Formula/aibus.rb from the template and a release's SHA256SUMS.
#
#   packaging/homebrew/render-formula.sh <version> <path-to-SHA256SUMS> [output]
#
# The output defaults to Formula/aibus.rb at the repo root. If every checksum is all zeros the
# formula is written with a loud PLACEHOLDER header, which is how the committed copy is made.
set -eu

if [ $# -lt 2 ] || [ $# -gt 3 ]; then
  echo "usage: $0 <version> <SHA256SUMS> [output]" >&2
  exit 2
fi
version=${1#v}
sums=$2
here=$(cd "$(dirname "$0")" && pwd)
out=${3:-$here/../../Formula/aibus.rb}

case $version in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *) echo "render-formula: bad version '$1'" >&2; exit 2 ;;
esac
[ -r "$sums" ] || { echo "render-formula: cannot read $sums" >&2; exit 2; }

sed_args=$(mktemp)
trap 'rm -f "$sed_args"' EXIT
zeros=0
for target in aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu x86_64-unknown-linux-musl; do
  sum=$(awk -v f="aibus-$target.tar.gz" '{n=$2; sub(/^\*/, "", n)} n == f {print $1; exit}' "$sums")
  case $sum in
    *[!0-9a-f]* | "") echo "render-formula: no valid sha256 for aibus-$target.tar.gz in $sums" >&2; exit 1 ;;
  esac
  [ ${#sum} -eq 64 ] || { echo "render-formula: sha256 for $target is not 64 hex characters" >&2; exit 1; }
  case $sum in *[1-9a-f]*) ;; *) zeros=$((zeros + 1)) ;; esac
  upper=$(printf '%s' "$target" | tr 'a-z-' 'A-Z_')
  printf 's/@SHA256_%s@/%s/\n' "$upper" "$sum" >>"$sed_args"
done

notice=$(mktemp)
trap 'rm -f "$sed_args" "$notice"' EXIT
if [ "$zeros" -gt 0 ]; then
  printf '%s\n' '# PLACEHOLDER CHECKSUMS: the sha256 lines below are all zeros. The release workflow replaces' \
    '# this file with one rendered from the real SHA256SUMS (packaging/homebrew/render-formula.sh).' >"$notice"
fi

mkdir -p "$(dirname "$out")"
printf 's/@VERSION@/%s/\n' "$version" >>"$sed_args"
sed -f "$sed_args" "$here/aibus.rb.in" | sed -e '/^@NOTICE@$/{r '"$notice" -e 'd;}' >"$out"
echo "rendered $out for $version"
