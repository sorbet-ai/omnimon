# Install omnimon from the GitHub release on Windows.
#
#   irm https://github.com/sorbet-ai/omnimon/releases/latest/download/install.ps1 | iex
#
# Environment:
#   OMNIMON_VERSION      release to install, e.g. 0.1.1 (default: latest)
#   OMNIMON_INSTALL_DIR  where to put omnimon.exe
#                        (default: %LOCALAPPDATA%\Programs\omnimon)
#   OMNIMON_DOWNLOAD_URL base URL to download from instead of GitHub (mirrors,
#                        testing); files are expected as <base>/<asset>
#
# Works in Windows PowerShell 5.1 and PowerShell 7.

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$repo = 'sorbet-ai/omnimon'
# x64 build; it also runs on Windows on ARM through emulation.
$asset = 'omnimon-x86_64-pc-windows-msvc.exe'

if ($env:OMNIMON_DOWNLOAD_URL) {
    $base = $env:OMNIMON_DOWNLOAD_URL.TrimEnd('/')
} elseif ($env:OMNIMON_VERSION) {
    $base = "https://github.com/$repo/releases/download/v$($env:OMNIMON_VERSION.TrimStart('v'))"
} else {
    $base = "https://github.com/$repo/releases/latest/download"
}
if ($env:OMNIMON_INSTALL_DIR) {
    $dir = $env:OMNIMON_INSTALL_DIR
} else {
    $dir = Join-Path $env:LOCALAPPDATA 'Programs\omnimon'
}

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("omnimon-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    $exe = Join-Path $tmp 'omnimon.exe'
    Write-Host "downloading $asset"
    Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile $exe

    # Verify against the release's checksums when they're available.
    $sums = $null
    try { $sums = (Invoke-WebRequest -UseBasicParsing -Uri "$base/SHA256SUMS").Content } catch { }
    if ($sums) {
        if ($sums -is [byte[]]) { $sums = [Text.Encoding]::UTF8.GetString($sums) }
        $want = $null
        foreach ($line in ($sums -split "`n")) {
            $parts = $line.Trim() -split '\s+'
            if ($parts.Count -eq 2 -and $parts[1].TrimStart('*') -eq $asset) { $want = $parts[0] }
        }
        if (-not $want) { throw "$asset is not listed in SHA256SUMS" }
        $got = (Get-FileHash -Algorithm SHA256 $exe).Hash
        if ($got -ne $want) { throw "checksum mismatch for $asset" }
    } else {
        Write-Host 'note: no SHA256SUMS found, skipping verification'
    }

    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $dest = Join-Path $dir 'omnimon.exe'
    try {
        Move-Item -Force $exe $dest
    } catch {
        throw "can't replace $dest; is omnimon still running?"
    }
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}

$version = & $dest --version
Write-Host "installed $version to $dest"

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not (($userPath -split ';') -contains $dir)) {
    $newPath = if ($userPath) { "$userPath;$dir" } else { $dir }
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    $env:Path = "$env:Path;$dir"
    Write-Host "added $dir to your PATH (open a new terminal to pick it up)"
}
