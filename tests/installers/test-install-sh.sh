#!/bin/sh
# End to end tests for install.sh against a fake release served from a throwaway local HTTP
# server. Run inside a container that has python3 and curl or wget (see tests/installers/run.sh).
# Nothing here talks to the real GitHub release URLs.
set -u

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
t=$(mktemp -d)
port=${PORT:-8099}
fails=0
n=0

cleanup() {
  [ -z "${server:-}" ] || kill "$server" 2>/dev/null
  rm -rf "$t"
}
trap cleanup EXIT

ok() { n=$((n + 1)); echo "ok $n - $1"; }
bad() { n=$((n + 1)); fails=$((fails + 1)); echo "not ok $n - $1"; }
# expect <name> <command...>: passes when the command succeeds
expect() { name=$1; shift; if "$@"; then ok "$name"; else bad "$name"; fi; }
# refuse <name> <command...>: passes when the command fails
refuse() { name=$1; shift; if "$@"; then bad "$name"; else ok "$name"; fi; }
has() { grep -q -- "$1" "$2"; }

mkdir -p "$t/www" "$t/shim"
sh "$here/make-fake-release.sh" "$t/www/ok" 0.1.0
sh "$here/make-fake-release.sh" "$t/www/bad" 0.1.0 badsum
sh "$here/make-fake-release.sh" "$t/www/nosum" 0.1.0 nosum
python3 -m http.server "$port" --bind 127.0.0.1 --directory "$t/www" >"$t/server.log" 2>&1 &
server=$!
i=0
until python3 -c "import socket,sys; socket.create_connection(('127.0.0.1',$port),1)" 2>/dev/null; do
  i=$((i + 1)); [ $i -lt 50 ] || { echo "server did not start"; exit 1; }
  sleep 0.1
done

# uname shim, so one container can pose as macOS, aarch64 Linux or an unsupported system.
cat >"$t/shim/uname" <<'EOF'
#!/bin/sh
case "$1" in
  -s) echo "${FAKE_S:-Linux}" ;;
  -m) echo "${FAKE_M:-x86_64}" ;;
  *) echo "${FAKE_S:-Linux}" ;;
esac
EOF
chmod +x "$t/shim/uname"

# Fresh HOME per scenario. run <home-name> <env assignments...> -- <args>: runs install.sh.
base=http://127.0.0.1:$port
run() {
  h=$t/$1; shift
  mkdir -p "$h"
  # shellcheck disable=SC2086 # $ARGS is zero or one option word
  env HOME="$h" PATH="$t/shim:$PATH" AIBUS_RELEASE_BASE="$base/ok" "$@" sh "$repo/install.sh" $ARGS >"$h.out" 2>&1
}
ARGS=
out() { cat "$t/$1.out"; }

if command -v curl >/dev/null 2>&1; then echo "# downloader: curl"; else echo "# downloader: wget"; fi

# shellcheck disable=SC2016
{
  # 1. good install, musl preferred on x86_64 Linux
  run a AIBUS_VERSION=0.1.0
  expect "good install exits 0 and installs" test -x "$t/a/.local/bin/aibus"
  expect "linux x86_64 gets the musl build" has "aibus 0.1.0 x86_64-unknown-linux-musl" "$t/a.out"
  expect "sha256 reported" has "sha256 ok" "$t/a.out"
  expect "skipped cosign is said" has "skipping the signature check" "$t/a.out"
  expect "PATH hint printed" has "export PATH=" "$t/a.out"
  refuse "no .profile written by default" test -e "$t/a/.profile"
  expect "only the binary lands in HOME" test "$(cd "$t/a" && find . -type f | sort)" = "./.local/bin/aibus"

  # 2. latest release when no version is given
  run b
  expect "latest resolves" has "aibus 0.1.0 x86_64-unknown-linux-musl" "$t/b.out"

  # 3. glibc only when asked, and aarch64 / macOS targets
  run c AIBUS_VERSION=0.1.0 AIBUS_LIBC=gnu
  expect "AIBUS_LIBC=gnu picks glibc" has "aibus 0.1.0 x86_64-unknown-linux-gnu" "$t/c.out"
  ARGS=--gnu run c2 AIBUS_VERSION=0.1.0
  expect "--gnu picks glibc" has "aibus 0.1.0 x86_64-unknown-linux-gnu" "$t/c2.out"
  run d AIBUS_VERSION=0.1.0 FAKE_M=aarch64
  expect "linux aarch64 gets glibc build" has "aibus 0.1.0 aarch64-unknown-linux-gnu" "$t/d.out"
  run e AIBUS_VERSION=0.1.0 FAKE_S=Darwin FAKE_M=arm64
  expect "macOS arm64 target" has "aibus 0.1.0 aarch64-apple-darwin" "$t/e.out"
  run e2 AIBUS_VERSION=0.1.0 FAKE_S=Darwin FAKE_M=x86_64
  expect "macOS x86_64 target" has "aibus 0.1.0 x86_64-apple-darwin" "$t/e2.out"
  run e3 AIBUS_VERSION=v0.1.0
  expect "leading v in AIBUS_VERSION is accepted" test -x "$t/e3/.local/bin/aibus"

  # 4. wrong checksum is refused and nothing is installed
  refuse "wrong checksum fails" run f AIBUS_VERSION=0.1.0 AIBUS_RELEASE_BASE="$base/bad"
  expect "mismatch is reported" has "checksum mismatch" "$t/f.out"
  refuse "nothing installed after mismatch" test -e "$t/f/.local/bin/aibus"
  refuse "missing SHA256SUMS entry fails" run f2 AIBUS_VERSION=0.1.0 AIBUS_RELEASE_BASE="$base/nosum"
  expect "missing entry is reported" has "no entry" "$t/f2.out"
  refuse "nothing installed without an entry" test -e "$t/f2/.local/bin/aibus"
  refuse "unknown version fails" run f3 AIBUS_VERSION=9.9.9
  refuse "nothing installed for unknown version" test -e "$t/f3/.local/bin/aibus"

  # 5. unsupported platforms
  refuse "FreeBSD refused" run g FAKE_S=FreeBSD FAKE_M=amd64
  expect "refusal explains itself" has "unsupported platform FreeBSD/amd64" "$t/g.out"
  refuse "linux riscv64 refused" run g2 FAKE_M=riscv64
  refuse "nothing installed on unsupported platform" test -e "$t/g/.local/bin/aibus"

  # 6. upgrade: re-run with a newer release
  sh "$here/make-fake-release.sh" "$t/www/ok" 0.1.1
  run a AIBUS_VERSION=0.1.1
  expect "upgrade replaces the binary" test "$("$t/a/.local/bin/aibus")" = "aibus 0.1.1 x86_64-unknown-linux-musl"
  run a2 AIBUS_INSTALL_DIR="$t/a/.local/bin"
  expect "latest after publishing 0.1.1 is 0.1.1" test "$("$t/a/.local/bin/aibus")" = "aibus 0.1.1 x86_64-unknown-linux-musl"
  refuse "no staging file left behind" test "$(ls -A "$t/a/.local/bin")" != "aibus"

  # 7. uninstall (and uninstalling twice)
  ARGS=--uninstall run a AIBUS_INSTALL_DIR="$t/a/.local/bin"
  refuse "uninstall removes the binary" test -e "$t/a/.local/bin/aibus"
  ARGS=--uninstall run a AIBUS_INSTALL_DIR="$t/a/.local/bin"
  expect "second uninstall is a no-op" has "nothing to remove" "$t/a.out"

  # 8. AIBUS_INSTALL_DIR, creating directories
  run h AIBUS_VERSION=0.1.0 AIBUS_INSTALL_DIR="$t/h/deep/er/bin"
  expect "custom dir is created and used" test -x "$t/h/deep/er/bin/aibus"

  # 9. PATH handling
  ARGS=--modify-path run i AIBUS_VERSION=0.1.0
  expect "--modify-path writes ~/.profile" has "export PATH=\"$t/i/.local/bin:" "$t/i/.profile"
  ARGS=--modify-path run i2 AIBUS_VERSION=0.1.0 AIBUS_NO_MODIFY_PATH=1
  refuse "AIBUS_NO_MODIFY_PATH=1 wins over --modify-path" test -e "$t/i2/.profile"
  run i3 AIBUS_VERSION=0.1.0 PATH="$t/shim:$t/i3/.local/bin:$PATH"
  refuse "no PATH hint when the dir is already on PATH" has "export PATH=" "$t/i3.out"
  refuse "unknown option rejected" env HOME="$t/j" sh "$repo/install.sh" --bogus

  # 10. cosign: absent (above), passing, failing
  cat >"$t/shim/cosign" <<'EOF'
#!/bin/sh
[ -n "${COSIGN_FAIL:-}" ] && exit 1
case " $* " in *" verify-blob --bundle "*) exit 0 ;; esac
exit 2
EOF
  chmod +x "$t/shim/cosign"
  run k AIBUS_VERSION=0.1.0
  expect "cosign on PATH is used" has "cosign signature ok" "$t/k.out"
  refuse "failing cosign aborts" run k2 AIBUS_VERSION=0.1.0 COSIGN_FAIL=1
  expect "cosign failure is reported" has "cosign signature check failed" "$t/k2.out"
  refuse "nothing installed after cosign failure" test -e "$t/k2/.local/bin/aibus"
  rm "$t/shim/cosign"

  # 11. a truncated script runs nothing
  lines=$(wc -l <"$repo/install.sh")
  head -n $((lines - 1)) "$repo/install.sh" >"$t/trunc.sh"
  env HOME="$t/l" PATH="$t/shim:$PATH" AIBUS_RELEASE_BASE="$base/ok" sh "$t/trunc.sh" >"$t/l.out" 2>&1
  expect "script without its last line does nothing" test ! -s "$t/l.out"
  refuse "script without its last line installs nothing" test -e "$t/l/.local/bin"
  size=$(wc -c <"$repo/install.sh")
  cut=100
  stray=0
  while [ "$cut" -lt $((size - 12)) ]; do
    head -c "$cut" "$repo/install.sh" | env HOME="$t/m" PATH="$t/shim:$PATH" AIBUS_RELEASE_BASE="$base/ok" sh >/dev/null 2>&1
    [ ! -e "$t/m/.local/bin" ] || stray=$((stray + 1))
    cut=$((cut + 97))
  done
  expect "no truncation point piped to sh installs anything" test "$stray" -eq 0

  # 12. nothing credential-shaped in the installers
  refuse "installer text has no credential handling" sh -c "grep -v '^ *#' '$repo/install.sh' '$repo/install.ps1' | grep -v 'token.actions.githubusercontent.com' | grep -v 'This script never reads' | grep -Ei 'password|secret|credential|api[_-]?key|token|netrc|keychain|ssh'"
}

echo "1..$n"
if [ "$fails" -ne 0 ]; then echo "# $fails failed"; exit 1; fi
echo "# all passed"
