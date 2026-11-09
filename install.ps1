<#
.SYNOPSIS
  aibus installer for Windows (PowerShell 5.1 and 7).

.DESCRIPTION
  irm https://raw.githubusercontent.com/spdrman/aibus/main/install.ps1 | iex

  To uninstall, run the script as a file or as a script block:
    & ([scriptblock]::Create((irm https://raw.githubusercontent.com/spdrman/aibus/main/install.ps1))) -Uninstall

  Environment:
    AIBUS_VERSION         release to install, for example 0.1.0 (default: latest release)
    AIBUS_INSTALL_DIR     where aibus.exe goes (default: %LOCALAPPDATA%\Programs\aibus\bin)
    AIBUS_NO_MODIFY_PATH  set to 1 to leave the user PATH alone
    AIBUS_RELEASE_BASE    release URL prefix, only for tests (default: https://github.com/spdrman/aibus/releases)

  The work is in a function called on the last line, so a truncated download runs nothing.
  This script never reads, writes or sends a credential, token or key.
#>
[CmdletBinding()]
param([switch]$Uninstall)

function Invoke-AibusInstall {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSAvoidUsingWriteHost', '', Justification = 'Interactive installer output.')]
    [CmdletBinding()]
    param([switch]$Uninstall)

    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    # Windows PowerShell 5.1 may default to older TLS versions.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $base = if ($env:AIBUS_RELEASE_BASE) { $env:AIBUS_RELEASE_BASE } else { 'https://github.com/spdrman/aibus/releases' }
    $dir = $env:AIBUS_INSTALL_DIR
    if (-not $dir) {
        if (-not $env:LOCALAPPDATA) { throw 'aibus-install: LOCALAPPDATA is not set, set AIBUS_INSTALL_DIR' }
        $dir = Join-Path $env:LOCALAPPDATA 'Programs\aibus\bin'
    }
    $touchPath = ($env:AIBUS_NO_MODIFY_PATH -ne '1') -and ([Environment]::OSVersion.Platform -eq 'Win32NT')
    $exe = Join-Path $dir 'aibus.exe'

    if ($Uninstall) {
        if (Test-Path -LiteralPath $exe) {
            Remove-Item -LiteralPath $exe -Force
            Write-Host "aibus-install: removed $exe"
        } else {
            Write-Host "aibus-install: nothing to remove, $exe does not exist"
        }
        if ($touchPath) {
            $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
            if ($userPath) {
                $kept = @($userPath -split ';' | Where-Object { $_ -and ($_.TrimEnd('\') -ne $dir.TrimEnd('\')) })
                [Environment]::SetEnvironmentVariable('Path', ($kept -join ';'), 'User')
            }
        }
        return
    }

    $arch = [string][Runtime.InteropServices.RuntimeInformation]::OSArchitecture
    if ($arch -ne 'X64' -and $arch -ne 'Arm64') {
        throw "aibus-install: unsupported architecture $arch (only the x86_64 Windows build exists)"
    }
    if ($arch -eq 'Arm64') { Write-Host 'aibus-install: no native arm64 build, installing the x86_64 build (runs under emulation)' }
    $target = 'x86_64-pc-windows-msvc'

    $version = $env:AIBUS_VERSION
    if ($version) { $url = "$base/download/v$($version.TrimStart('v'))" } else { $url = "$base/latest/download" }

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ('aibus-install-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        $archive = "aibus-$target.zip"
        $zip = Join-Path $tmp $archive
        $sums = Join-Path $tmp 'SHA256SUMS'
        Write-Host "aibus-install: downloading $archive ($(if ($version) { $version } else { 'latest' }))"
        Invoke-WebRequest -UseBasicParsing -Uri "$url/$archive" -OutFile $zip
        Invoke-WebRequest -UseBasicParsing -Uri "$url/SHA256SUMS" -OutFile $sums

        # Checksum first: nothing is unpacked or installed before this passes.
        $want = $null
        foreach ($line in Get-Content -LiteralPath $sums) {
            $parts = $line.Trim() -split '\s+', 2
            if ($parts.Count -eq 2 -and $parts[1].TrimStart('*') -eq $archive) { $want = $parts[0].ToLowerInvariant(); break }
        }
        if (-not $want) { throw "aibus-install: SHA256SUMS has no entry for $archive" }
        $got = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($got -ne $want) { throw "aibus-install: checksum mismatch for $archive (expected $want, got $got), nothing installed" }
        Write-Host 'aibus-install: sha256 ok'

        $out = Join-Path $tmp 'unpacked'
        Expand-Archive -LiteralPath $zip -DestinationPath $out -Force
        $built = Join-Path $out "aibus-$target\aibus.exe"
        if (-not (Test-Path -LiteralPath $built)) { throw 'aibus-install: unexpected archive layout' }

        New-Item -ItemType Directory -Path $dir -Force | Out-Null
        $staged = Join-Path $dir '.aibus.new.exe'
        Copy-Item -LiteralPath $built -Destination $staged -Force
        Move-Item -LiteralPath $staged -Destination $exe -Force
        Write-Host "aibus-install: installed $exe"
    } finally {
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }

    if ($touchPath) {
        # Windows-only registry write, not exercised by the container tests.
        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $entries = @($userPath -split ';' | Where-Object { $_ })
        if (-not ($entries | Where-Object { $_.TrimEnd('\') -eq $dir.TrimEnd('\') })) {
            [Environment]::SetEnvironmentVariable('Path', (($entries + $dir) -join ';'), 'User')
            Write-Host "aibus-install: added $dir to your user PATH (open a new terminal)"
        }
        if (-not (($env:Path -split ';') -contains $dir)) { $env:Path = "$env:Path;$dir" }
    } else {
        Write-Host "aibus-install: PATH left alone, add $dir to it yourself"
    }
}

Invoke-AibusInstall -Uninstall:$Uninstall
