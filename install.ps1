#Requires -Version 5.1
# Foreman installer / manual updater.
#   irm https://raw.githubusercontent.com/sniffle6/foreman/main/install.ps1 | iex
# Does exactly four things: download latest release zip, verify SHA256,
# extract to %LOCALAPPDATA%\Programs\foreman, add that dir to the USER PATH.
# No shortcuts, no registry. Re-run any time to update.
# NOTE: runs inside the user's shell via iex - never call `exit` here.

$ErrorActionPreference = 'Stop'

$repo = if ($env:FOREMAN_INSTALL_REPO) { $env:FOREMAN_INSTALL_REPO } else { 'sniffle6/foreman' }
$dest = Join-Path $env:LOCALAPPDATA 'Programs\foreman'
$zipSuffix = '-x86_64-windows.zip'

if (Get-Process -Name foreman -ErrorAction SilentlyContinue) {
    Write-Host 'foreman is running - close it first, then re-run this installer.' -ForegroundColor Yellow
    return
}

[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$headers = @{ 'User-Agent' = 'foreman-install' }
$rel = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/latest" -Headers $headers

$asset = $rel.assets | Where-Object { $_.name.EndsWith($zipSuffix) } | Select-Object -First 1
if (-not $asset) { throw "release $($rel.tag_name) has no asset ending in $zipSuffix" }
$sums = $rel.assets | Where-Object { $_.name -eq 'SHA256SUMS.txt' } | Select-Object -First 1
if (-not $sums) { throw "release $($rel.tag_name) has no SHA256SUMS.txt" }

$tmp = Join-Path ([IO.Path]::GetTempPath()) "foreman-install-$([Guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    $zipPath = Join-Path $tmp $asset.name
    Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $zipPath -Headers $headers
    $sumsPath = Join-Path $tmp 'SHA256SUMS.txt'
    Invoke-WebRequest -Uri $sums.browser_download_url -OutFile $sumsPath -Headers $headers

    $line = Get-Content $sumsPath | Where-Object { $_.EndsWith("  $($asset.name)") } | Select-Object -First 1
    if (-not $line) { throw "SHA256SUMS.txt has no entry for $($asset.name)" }
    $expected = ($line -split '\s+')[0].ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 $zipPath).Hash.ToLowerInvariant()
    if ($actual -ne $expected) { throw "hash mismatch: expected $expected, got $actual - aborting install" }

    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    Expand-Archive -Path $zipPath -DestinationPath $dest -Force

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (($userPath -split ';') -notcontains $dest) {
        [Environment]::SetEnvironmentVariable('Path', "$userPath;$dest", 'User')
        Write-Host "added $dest to your user PATH (new terminals will pick it up)"
    }
    Write-Host "foreman $($rel.tag_name) installed to $dest" -ForegroundColor Green
    Write-Host "run it: `"$dest\foreman.exe`" (or 'foreman' from a new terminal)"
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
