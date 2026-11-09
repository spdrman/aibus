#!/bin/sh
# Container entry for the Homebrew install test (issue #83): image homebrew/brew, runs as the
# linuxbrew user. Builds fake release archives with the real layout (aibus-<target>/aibus plus
# LICENSE and README.md), renders the formula against them, taps it locally and runs brew
# install, brew test, aibus version and brew audit --strict.
#
#   FORMULA=path/to/aibus.rb   skip the render and install this formula as is (it must point at
#                              a reachable release, e.g. the committed one against the real v0.1.0)
set -eu

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
t=$(mktemp -d)
version=0.1.0
export HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_INSTALL_FROM_API=1 HOMEBREW_NO_ANALYTICS=1

brew tap-new --no-git spdrman/aibus >/dev/null
tap=$(brew --repository spdrman/aibus)
mkdir -p "$tap/Formula"

if [ -n "${FORMULA:-}" ]; then
  cp "$FORMULA" "$tap/Formula/aibus.rb"
else
  rel=$t/release/v$version
  mkdir -p "$rel"
  : >"$t/SHA256SUMS"
  for target in aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu x86_64-unknown-linux-musl; do
    pkg=$t/pkg/aibus-$target
    mkdir -p "$pkg"
    echo LICENSE >"$pkg/LICENSE"
    echo README >"$pkg/README.md"
    cat >"$pkg/aibus" <<SCRIPT
#!/bin/sh
case "\$1" in
  version) echo "aibus $version $target" ;;
  completions) echo "# $target completions for \$2" ;;
  *) exit 2 ;;
esac
SCRIPT
    chmod 755 "$pkg/aibus"
    tar -czf "$rel/aibus-$target.tar.gz" -C "$t/pkg" "aibus-$target"
    (cd "$rel" && sha256sum "aibus-$target.tar.gz") >>"$t/SHA256SUMS"
  done
  sh "$repo/packaging/homebrew/render-formula.sh" "$version" "$t/SHA256SUMS" "$tap/Formula/aibus.rb" >/dev/null
  # Point the download URLs at the fake release on disk.
  sed -i "s#https://github.com/spdrman/aibus/releases/download#file://$t/release#" "$tap/Formula/aibus.rb"
  grep -q "file://$t/release" "$tap/Formula/aibus.rb" || { echo "FAIL: url rewrite did not apply"; exit 1; }
fi

echo "# brew install"
brew install spdrman/aibus/aibus
echo "# brew test"
brew test spdrman/aibus/aibus
echo "# aibus version"
out=$(aibus version)
echo "$out"
case $out in *"aibus $version"*) ;; *) echo "FAIL: unexpected aibus version output"; exit 1 ;; esac
echo "# completions installed"
ls "$(brew --prefix)/etc/bash_completion.d/aibus" "$(brew --prefix)/share/zsh/site-functions/_aibus" "$(brew --prefix)/share/fish/vendor_completions.d/aibus.fish"
echo "# brew audit --strict"
brew audit --strict --formula spdrman/aibus/aibus
echo "# brew install: ok"
