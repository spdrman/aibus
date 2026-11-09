#!/bin/sh
# aibus installer for macOS and Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/spdrman/aibus/main/install.sh | sh
#   sh install.sh --uninstall
#
# Options:  --uninstall  --gnu (glibc build on Linux)  --modify-path (append PATH to ~/.profile)
#
# Environment:
#   AIBUS_VERSION         release to install, for example 0.1.0 (default: latest release)
#   AIBUS_INSTALL_DIR     where the binary goes (default: $HOME/.local/bin)
#   AIBUS_NO_MODIFY_PATH  set to 1 to never touch shell startup files, even with --modify-path
#   AIBUS_LIBC            set to gnu to install the glibc build on Linux (same as --gnu)
#   AIBUS_RELEASE_BASE    release URL prefix, only for tests (default: https://github.com/spdrman/aibus/releases)
#
# Everything is inside main(), which runs on the last line, so a truncated download runs nothing.
# This script never reads, writes or sends a credential, token or key.

main() {
  set -eu

  base=${AIBUS_RELEASE_BASE:-https://github.com/spdrman/aibus/releases}
  dir=${AIBUS_INSTALL_DIR:-${HOME:?HOME is not set}/.local/bin}
  version=${AIBUS_VERSION:-}
  libc=${AIBUS_LIBC:-musl}
  action=install
  modify_path=0

  say() { printf '%s\n' "aibus-install: $*"; }
  fail() {
    printf '%s\n' "aibus-install: error: $*" >&2
    exit 1
  }

  for arg in "$@"; do
    case $arg in
      --uninstall) action=uninstall ;;
      --gnu) libc=gnu ;;
      --modify-path) modify_path=1 ;;
      -h | --help)
        say "usage: install.sh [--uninstall] [--gnu] [--modify-path] (see the header of the script for the environment knobs)"
        return 0
        ;;
      *) fail "unknown option: $arg" ;;
    esac
  done

  if [ "$action" = uninstall ]; then
    if [ -e "$dir/aibus" ]; then
      rm -f "$dir/aibus"
      say "removed $dir/aibus"
    else
      say "nothing to remove, $dir/aibus does not exist"
    fi
    say "shell startup files are not edited by default, so there is nothing else to undo"
    return 0
  fi

  # Platform to release target. Linux prefers musl (runs on any distro); only x86_64 has one.
  os=$(uname -s)
  arch=$(uname -m)
  case "$os/$arch" in
    Darwin/arm64 | Darwin/aarch64) target=aarch64-apple-darwin ;;
    Darwin/x86_64) target=x86_64-apple-darwin ;;
    Linux/x86_64 | Linux/amd64)
      if [ "$libc" = gnu ]; then target=x86_64-unknown-linux-gnu; else target=x86_64-unknown-linux-musl; fi
      ;;
    Linux/aarch64 | Linux/arm64)
      [ "$libc" = gnu ] || say "no musl build for aarch64, using the glibc build"
      target=aarch64-unknown-linux-gnu
      ;;
    *) fail "unsupported platform $os/$arch (supported: macOS arm64 and x86_64, Linux x86_64 and aarch64; Windows uses install.ps1)" ;;
  esac

  if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL -o "$2" "$1"; }
  elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -q -O "$2" "$1"; }
  else
    fail "need curl or wget"
  fi

  if command -v sha256sum >/dev/null 2>&1; then
    sha256() { sha256sum "$1" | awk '{print $1}'; }
  elif command -v shasum >/dev/null 2>&1; then
    sha256() { shasum -a 256 "$1" | awk '{print $1}'; }
  else
    fail "need sha256sum or shasum"
  fi

  if [ -n "$version" ]; then
    url="$base/download/v${version#v}"
  else
    url="$base/latest/download"
  fi

  tmp=$(mktemp -d "${TMPDIR:-/tmp}/aibus-install.XXXXXX")
  trap 'rm -rf "$tmp"' EXIT INT TERM

  archive="aibus-$target.tar.gz"
  say "downloading $archive (${version:-latest})"
  fetch "$url/$archive" "$tmp/$archive" || fail "could not download $url/$archive"
  fetch "$url/SHA256SUMS" "$tmp/SHA256SUMS" || fail "could not download $url/SHA256SUMS"

  # Checksum first: nothing is unpacked or installed before this passes.
  want=$(awk -v f="$archive" '{n=$2; sub(/^\*/, "", n)} n == f {print $1; exit}' "$tmp/SHA256SUMS")
  [ -n "$want" ] || fail "SHA256SUMS has no entry for $archive"
  got=$(sha256 "$tmp/$archive")
  [ "$got" = "$want" ] || fail "checksum mismatch for $archive (expected $want, got $got), nothing installed"
  say "sha256 ok"

  if command -v cosign >/dev/null 2>&1; then
    fetch "$url/$archive.sigstore.json" "$tmp/$archive.sigstore.json" || fail "could not download the cosign bundle"
    cosign verify-blob --bundle "$tmp/$archive.sigstore.json" \
      --certificate-identity-regexp 'https://github.com/spdrman/aibus/' \
      --certificate-oidc-issuer https://token.actions.githubusercontent.com \
      "$tmp/$archive" >/dev/null 2>&1 || fail "cosign signature check failed for $archive, nothing installed"
    say "cosign signature ok"
  else
    say "cosign not found on PATH, skipping the signature check (the sha256 check still ran)"
  fi

  tar -xzf "$tmp/$archive" -C "$tmp" "aibus-$target/aibus" || fail "unexpected archive layout"
  mkdir -p "$dir"
  # Copy beside the target, then rename, so an upgrade never leaves a half-written binary.
  cp "$tmp/aibus-$target/aibus" "$dir/.aibus.new.$$"
  chmod 755 "$dir/.aibus.new.$$"
  mv -f "$dir/.aibus.new.$$" "$dir/aibus"
  say "installed $dir/aibus"
  "$dir/aibus" version 2>/dev/null || true

  case ":$PATH:" in
    *":$dir:"*) ;;
    *)
      if [ "$modify_path" = 1 ] && [ "${AIBUS_NO_MODIFY_PATH:-}" != 1 ]; then
        # shellcheck disable=SC2016 # $PATH must stay literal in the profile line
        printf '\nexport PATH="%s:$PATH"\n' "$dir" >>"$HOME/.profile"
        say "added $dir to PATH in $HOME/.profile (open a new shell)"
      else
        say "$dir is not on your PATH. Add it with:"
        say "  export PATH=\"$dir:\$PATH\""
      fi
      ;;
  esac
}

main "$@"
