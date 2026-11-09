#!/bin/sh
# Container entry for the install.ps1 tests: image mcr.microsoft.com/powershell (Ubuntu based).
# Installs python3 (for the fake release and a throwaway HTTP server), then PSScriptAnalyzer, parse
# check, and the pwsh tests.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
port=${PORT:-8097}

apt-get update -qq >/dev/null
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq python3 >/dev/null

t=$(mktemp -d)
trap 'kill ${server:-} 2>/dev/null; rm -rf "$t"' EXIT
mkdir -p "$t/www" "$t/scratch"
sh "$here/make-fake-release.sh" "$t/www/ok" 0.1.0
sh "$here/make-fake-release.sh" "$t/www/bad" 0.1.0 badsum
sh "$here/make-fake-release.sh" "$t/www/nosum" 0.1.0 nosum
sh "$here/make-fake-release.sh" "$t/www/ok" 0.1.1
python3 -m http.server "$port" --bind 127.0.0.1 --directory "$t/www" >"$t/server.log" 2>&1 &
server=$!
sleep 1

echo "# parse check"
pwsh -NoProfile -Command "
  \$errs = \$null; \$tokens = \$null
  [void][System.Management.Automation.Language.Parser]::ParseFile('$repo/install.ps1', [ref]\$tokens, [ref]\$errs)
  if (\$errs.Count) { \$errs | ForEach-Object { Write-Error \$_.Message }; exit 1 } else { 'install.ps1 parses' }"

echo "# PSScriptAnalyzer"
pwsh -NoProfile -Command "
  Set-PSRepository PSGallery -InstallationPolicy Trusted
  Install-Module PSScriptAnalyzer -Scope CurrentUser -Force
  \$r = Invoke-ScriptAnalyzer -Path '$repo/install.ps1' -Severity Warning,Error
  \$r | Format-Table -AutoSize | Out-String | Write-Output
  if (\$r) { exit 1 } else { 'PSScriptAnalyzer: no findings' }"

echo "# install.ps1 tests"
pwsh -NoProfile -File "$here/test-install-ps1.ps1" -Repo "$repo" -Base "http://127.0.0.1:$port" -Scratch "$t/scratch"
