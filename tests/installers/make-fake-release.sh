#!/bin/sh
# Build a fake aibus release tree for the installer tests (needs tar, python3, sha256sum).
#
#   make-fake-release.sh <root> <version> [ok|badsum|nosum]
#
# Writes <root>/download/v<version>/ with the six archives, a *.sigstore.json stub per archive and
# SHA256SUMS, and points <root>/latest/download at that same content. The "binary" is a shell
# script that prints its version and target, so tests can tell which archive was installed.
# badsum corrupts the checksum of every archive, nosum leaves the archives out of SHA256SUMS.
set -eu

root=$1
version=$2
mode=${3:-ok}
rel=$root/download/v$version
mkdir -p "$rel"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

for target in aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-gnu \
  aarch64-unknown-linux-gnu x86_64-unknown-linux-musl x86_64-pc-windows-msvc; do
  pkg=$work/aibus-$target
  mkdir -p "$pkg"
  echo LICENSE >"$pkg/LICENSE"
  echo README >"$pkg/README.md"
  case $target in
    *windows*)
      printf '#!/bin/sh\necho "aibus %s %s"\n' "$version" "$target" >"$pkg/aibus.exe"
      (cd "$work" && python3 -m zipfile -c "$rel/aibus-$target.zip" "aibus-$target")
      file=aibus-$target.zip
      ;;
    *)
      printf '#!/bin/sh\necho "aibus %s %s"\n' "$version" "$target" >"$pkg/aibus"
      chmod 755 "$pkg/aibus"
      tar -czf "$rel/aibus-$target.tar.gz" -C "$work" "aibus-$target"
      file=aibus-$target.tar.gz
      ;;
  esac
  echo '{}' >"$rel/$file.sigstore.json"
done

(
  cd "$rel"
  : >SHA256SUMS
  case $mode in
    nosum) ;;
    *) sha256sum aibus-* >SHA256SUMS ;;
  esac
  if [ "$mode" = badsum ]; then
    sed 's/^./x/' SHA256SUMS >SHA256SUMS.bad && mv SHA256SUMS.bad SHA256SUMS
  fi
)

mkdir -p "$root/latest"
rm -rf "$root/latest/download"
cp -R "$rel" "$root/latest/download"
