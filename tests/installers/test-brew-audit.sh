#!/bin/sh
# Container entry for the Homebrew audit: image homebrew/brew (Linux Homebrew), runs as the
# linuxbrew user. Audits the committed placeholder formula and one rendered with distinct sums.
# Offline audit only (no --online), since no release exists yet.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
t=$(mktemp -d)

i=0
: >"$t/SHA256SUMS"
for tg in aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu x86_64-unknown-linux-musl; do
  i=$((i + 1))
  printf '%s  aibus-%s.tar.gz\n' "$(printf '%064s' '' | tr ' ' "$i")" "$tg" >>"$t/SHA256SUMS"
done

export HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_INSTALL_FROM_API=1 HOMEBREW_NO_ANALYTICS=1
brew tap-new --no-git spdrman/aibus >/dev/null
tap=$(brew --repository spdrman/aibus)
mkdir -p "$tap/Formula"

for which in committed rendered; do
  if [ "$which" = committed ]; then
    cp "$repo/Formula/aibus.rb" "$tap/Formula/aibus.rb"
  else
    sh "$repo/packaging/homebrew/render-formula.sh" 0.1.0 "$t/SHA256SUMS" "$tap/Formula/aibus.rb" >/dev/null
  fi
  echo "# brew audit --strict ($which)"
  brew audit --strict --formula spdrman/aibus/aibus
  echo "# brew style ($which)"
  brew style --formula "$tap/Formula/aibus.rb"
done
echo "# brew audit: clean"
