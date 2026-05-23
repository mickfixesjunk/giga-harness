# giga-harness PowerShell installer.
#
# Usage:
#   irm https://github.com/mickfixesjunk/giga-harness/releases/latest/download/install.ps1 | iex
#
# Downloads the Windows release zip, extracts giga.exe into
# %LOCALAPPDATA%\Programs\giga\, and adds that dir to the user PATH
# if it isn't already there. Re-run any time to upgrade.

$ErrorActionPreference = 'Stop'

$Repo       = 'mickfixesjunk/giga-harness'
$Archive    = 'giga-x86_64-pc-windows-msvc.zip'
$Url        = "https://github.com/$Repo/releases/latest/download/$Archive"
$InstallDir = if ($env:GIGA_INSTALL_DIR) { $env:GIGA_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\giga' }
$TempDir    = Join-Path $env:TEMP ("giga-install-" + [Guid]::NewGuid().ToString('N'))

Write-Host "target:   x86_64-pc-windows-msvc"
Write-Host "archive:  $Archive"
Write-Host "download: $Url"
Write-Host "install:  $InstallDir"

try {
    New-Item -ItemType Directory -Force -Path $TempDir | Out-Null
    $ArchivePath = Join-Path $TempDir $Archive

    # Force TLS 1.2 on older PS5 hosts; PS7+ defaults to TLS 1.3.
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    Invoke-WebRequest -Uri $Url -OutFile $ArchivePath -UseBasicParsing

    Expand-Archive -Path $ArchivePath -DestinationPath $TempDir -Force

    $BinSrc = Join-Path $TempDir 'giga.exe'
    if (-not (Test-Path $BinSrc)) {
        throw "didn't find giga.exe inside $Archive"
    }

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    Copy-Item -Path $BinSrc -Destination (Join-Path $InstallDir 'giga.exe') -Force

    Write-Host ""
    Write-Host "installed: $(Join-Path $InstallDir 'giga.exe')"

    # Persist InstallDir to user PATH if missing.
    $UserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not $UserPath) { $UserPath = '' }
    $OnPath = $UserPath.Split(';') | Where-Object { $_ -ieq $InstallDir }
    if (-not $OnPath) {
        $NewPath = if ($UserPath) { "$UserPath;$InstallDir" } else { $InstallDir }
        [Environment]::SetEnvironmentVariable('Path', $NewPath, 'User')
        $env:Path = "$env:Path;$InstallDir"
        Write-Host ""
        Write-Host "added to user PATH (durable). Existing shells need a restart"
        Write-Host "to pick it up; this shell has it for the current session."
    }

    Write-Host ""
    Write-Host "try it:   giga --help"
}
finally {
    if (Test-Path $TempDir) {
        Remove-Item -Recurse -Force -Path $TempDir
    }
}
