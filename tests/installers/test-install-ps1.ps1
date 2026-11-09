# Tests for install.ps1 against the fake release served by test-install-ps1.sh.
# Runs under pwsh in a Linux container. The user PATH registry write is Windows only and is NOT
# exercised here: on a non-Windows platform install.ps1 skips it, so that part is unverified.
param(
    [Parameter(Mandatory)] [string]$Repo,
    [Parameter(Mandatory)] [string]$Base,
    [Parameter(Mandatory)] [string]$Scratch
)
$ErrorActionPreference = 'Stop'
$script = Join-Path $Repo 'install.ps1'
$script:n = 0
$script:fails = 0

function Check([string]$name, [bool]$cond) {
    $script:n++
    if ($cond) { Write-Output "ok $($script:n) - $name" } else { $script:fails++; Write-Output "not ok $($script:n) - $name" }
}

# Run install.ps1 in a child pwsh so $env: changes and thrown errors stay contained.
function Invoke-Installer([string]$dir, [hashtable]$envs, [string]$file = $script, [switch]$Uninstall) {
    $envs = $envs + @{ AIBUS_INSTALL_DIR = $dir }
    $assign = ($envs.GetEnumerator() | ForEach-Object { "`$env:$($_.Key) = '$($_.Value)'" }) -join '; '
    $call = if ($Uninstall) { "& '$file' -Uninstall" } else { "& '$file'" }
    $out = & pwsh -NoProfile -Command "$assign; try { $call } catch { Write-Output `"THROWN: `$(`$_.Exception.Message)`"; exit 1 }" 2>&1 | Out-String
    return @{ Out = $out; Code = $LASTEXITCODE }
}

$ok = "$Base/ok"
$exe = { param($d) Join-Path $d 'aibus.exe' }

# good install
$d = Join-Path $Scratch 'a'
$r = Invoke-Installer $d @{ AIBUS_VERSION = '0.1.0'; AIBUS_RELEASE_BASE = $ok }
Check 'good install exits 0' ($r.Code -eq 0)
Check 'binary is installed' (Test-Path (& $exe $d))
Check 'the windows build was picked' ((Get-Content (& $exe $d) -Raw) -match 'aibus 0.1.0 x86_64-pc-windows-msvc')
Check 'sha256 reported' ($r.Out -match 'sha256 ok')
Check 'only the binary lands in the install dir' ((Get-ChildItem -Force $d | Measure-Object).Count -eq 1)

# latest release
$d = Join-Path $Scratch 'b'
$r = Invoke-Installer $d @{ AIBUS_RELEASE_BASE = $ok }
Check 'latest resolves' ($r.Code -eq 0 -and (Test-Path (& $exe $d)))

# wrong checksum
$d = Join-Path $Scratch 'c'
$r = Invoke-Installer $d @{ AIBUS_VERSION = '0.1.0'; AIBUS_RELEASE_BASE = "$Base/bad" }
Check 'wrong checksum fails' ($r.Code -ne 0)
Check 'mismatch is reported' ($r.Out -match 'checksum mismatch')
Check 'nothing installed after a mismatch' (-not (Test-Path (& $exe $d)))

# SHA256SUMS without an entry
$d = Join-Path $Scratch 'd'
$r = Invoke-Installer $d @{ AIBUS_VERSION = '0.1.0'; AIBUS_RELEASE_BASE = "$Base/nosum" }
Check 'missing entry fails' ($r.Code -ne 0 -and $r.Out -match 'no entry')
Check 'nothing installed without an entry' (-not (Test-Path (& $exe $d)))

# unknown version
$d = Join-Path $Scratch 'e'
$r = Invoke-Installer $d @{ AIBUS_VERSION = '9.9.9'; AIBUS_RELEASE_BASE = $ok }
Check 'unknown version fails' ($r.Code -ne 0)
Check 'nothing installed for unknown version' (-not (Test-Path (& $exe $d)))

# upgrade
$d = Join-Path $Scratch 'a'
$r = Invoke-Installer $d @{ AIBUS_VERSION = '0.1.1'; AIBUS_RELEASE_BASE = $ok }
Check 'upgrade replaces the binary' ($r.Code -eq 0 -and (Get-Content (& $exe $d) -Raw) -match 'aibus 0.1.1 ')

# uninstall, twice
$r = Invoke-Installer $d @{ AIBUS_RELEASE_BASE = $ok } -Uninstall
Check 'uninstall removes the binary' ($r.Code -eq 0 -and -not (Test-Path (& $exe $d)))
$r = Invoke-Installer $d @{ AIBUS_RELEASE_BASE = $ok } -Uninstall
Check 'second uninstall is a no-op' ($r.Code -eq 0 -and $r.Out -match 'nothing to remove')

# piped through Invoke-Expression, the way the README runs it
$d = Join-Path $Scratch 'f'
$text = Get-Content -Raw $script
$tmpIex = Join-Path $Scratch 'iex.ps1'
Set-Content -LiteralPath $tmpIex -Value "Invoke-Expression (Get-Content -Raw '$script')"
$r = Invoke-Installer $d @{ AIBUS_VERSION = '0.1.0'; AIBUS_RELEASE_BASE = $ok } -file $tmpIex
Check 'works through Invoke-Expression' ($r.Code -eq 0 -and (Test-Path (& $exe $d)))

# a truncated script runs nothing
$d = Join-Path $Scratch 'g'
$lines = Get-Content $script
$trunc = Join-Path $Scratch 'trunc.ps1'
Set-Content -LiteralPath $trunc -Value ($lines[0..($lines.Count - 2)])
$r = Invoke-Installer $d @{ AIBUS_VERSION = '0.1.0'; AIBUS_RELEASE_BASE = $ok } -file $trunc
Check 'script without its last line installs nothing' ($r.Code -eq 0 -and -not (Test-Path $d))
$stray = 0
for ($cut = 200; $cut -lt $text.Length - 40; $cut += 211) {
    $d = Join-Path $Scratch "cut$cut"
    $part = Join-Path $Scratch "cut$cut.ps1"
    Set-Content -LiteralPath $part -Value $text.Substring(0, $cut)
    $null = Invoke-Installer $d @{ AIBUS_VERSION = '0.1.0'; AIBUS_RELEASE_BASE = $ok } -file $part
    if (Test-Path $d) { $stray++ }
}
Check 'no truncation point installs anything' ($stray -eq 0)

Write-Output "1..$($script:n)"
if ($script:fails -gt 0) { Write-Output "# $($script:fails) failed"; exit 1 }
Write-Output '# all passed'
