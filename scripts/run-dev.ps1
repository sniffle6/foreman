<#
.SYNOPSIS
Build and launch a foreman dev build alongside the one you already have running.

.DESCRIPTION
Running a second foreman by hand has three ways to hurt you, all of which have
already bitten in this repo:

  1. Building into `target\` fails to link ("Access is denied (os error 5)")
     while any target-built foreman holds the exe. This script always builds
     into `target\agent\`, which the installed/daily instance never locks.

  2. A dev instance reads and WRITES `%APPDATA%\foreman\workspace.json` —
     the same file your real instance owns. Left alone for a minute it will
     happily overwrite your project layout. This script points `APPDATA` at a
     throwaway sandbox under `target\agent\appdata`, so the dev instance gets
     its own settings/workspace/themes and cannot touch yours.

  3. Killing "foreman" BY NAME also kills the user's installed foreman
     (incident 2026-07-15, looked like a crash) and, from inside a foreman
     terminal, kills your own host and session (incident 2026-07-09).
     -Kill here matches on the dev exe path only, and refuses to kill any
     process that is an ancestor of this one.

Known limitation: the control plane binds a fixed global pipe (\\.\pipe\foreman).
Your existing instance owns it, so `foreman open` / `chat` / `send` / `snapshot`
from a terminal still talk to THAT instance, not the dev one. GUI behaviour
(rendering, scrolling, input, layout) tests fine; dispatch does not.

.PARAMETER Path
Source dir to build — a git worktree, say. Defaults to the repo this script
lives in. Output lands under the REPO's target\agent\build\<source-key>, one
target dir per source dir. Sharing a single target dir across worktrees would
save a dep build but lets cargo hand you the wrong worktree's binary, which is
very hard to notice: you read a test result that came from other source.

.PARAMETER Debug
Build the debug profile. Default is release: debug Rust is slow enough to
mislead you about anything performance-related (scrolling, paint, latency).

.PARAMETER Fresh
Wipe the sandbox first — a virgin config, as a new user would see it.

.PARAMETER SeedWorkspace
Copy your real workspace.json into the sandbox so the dev instance opens your
actual projects. Off by default: it spawns a shell per terminal, which is slow
and usually not what you want for a quick check. Still writes only to the
sandbox copy.

.PARAMETER NoSeed
Don't copy settings.json / keybindings.json / themes either. Fully stock.

.PARAMETER Kill
Kill running dev instances and exit. No build, no launch.

.PARAMETER List
List every running foreman with its exe path, and exit.

.EXAMPLE
.\scripts\run-dev.ps1
Release build of the current repo, launched in a sandbox.

.EXAMPLE
.\scripts\run-dev.ps1 -Path C:\tmp\wt-galley -Fresh
Build some worktree, from a virgin config.

.EXAMPLE
.\scripts\run-dev.ps1 -Kill
Clean up the dev instances, leaving your real foreman alone.
#>
# No [CmdletBinding()] on purpose: it would add a common -Debug parameter that
# collides with the -Debug switch below. $BuildProfile / $cargoArgs likewise
# dodge PowerShell's automatic $Profile and $args.
param(
    [string] $Path,
    [switch] $Debug,
    [switch] $Fresh,
    [switch] $SeedWorkspace,
    [switch] $NoSeed,
    [switch] $Kill,
    [switch] $List
)

$ErrorActionPreference = 'Stop'

$RepoRoot     = Split-Path $PSScriptRoot
$SrcDir       = if ($Path) { (Resolve-Path $Path).Path } else { $RepoRoot }
$BuildProfile = if ($Debug) { 'debug' } else { 'release' }
$DevRoot      = Join-Path $RepoRoot 'target\agent'

# One target dir PER SOURCE DIR. Pointing several worktrees at a single shared
# target dir looks like a free dep cache, but cargo can then hand you the OTHER
# worktree's binary: build worktree A, run `cargo test` in worktree B against the
# same --target-dir, and B's run can execute A's tests. Observed here - a test
# that exists in no B source file reported a pass. Keyed by leaf name plus a hash
# of the full path, so same-named dirs in different places stay distinct.
#
# MD5 of the path, NOT String.GetHashCode(): .NET randomizes string hashing per
# process, so GetHashCode would hand out a different directory on every run —
# a cold dependency build each time and a new leaked target dir each time.
$md5          = [System.Security.Cryptography.MD5]::Create()
$SrcHash      = [BitConverter]::ToString(
                    $md5.ComputeHash([Text.Encoding]::UTF8.GetBytes($SrcDir.ToLowerInvariant()))
                ).Replace('-', '').Substring(0, 8).ToLowerInvariant()
$md5.Dispose()
$SrcKey       = "{0}-{1}" -f (Split-Path $SrcDir -Leaf), $SrcHash
$TargetDir    = Join-Path $DevRoot "build\$SrcKey"
$Exe          = Join-Path $TargetDir "$BuildProfile\foreman.exe"

# Sandbox + pid file are deliberately shared across source dirs: one dev config
# to carry settings between builds, and -Kill/-List match on $DevRoot so they
# still see every dev instance regardless of which source built it.
$Sandbox      = Join-Path $DevRoot 'appdata'
$ConfigDir    = Join-Path $Sandbox 'foreman'
$PidFile      = Join-Path $Sandbox 'dev-pids.txt'

function Get-AncestorIds {
    # Walk up from this process so -Kill can never take out its own host.
    $ids = @()
    $cur = $PID
    for ($i = 0; $i -lt 32 -and $cur; $i++) {
        $ids += $cur
        $p = Get-CimInstance Win32_Process -Filter "ProcessId = $cur" -ErrorAction SilentlyContinue
        if (-not $p) { break }
        $cur = $p.ParentProcessId
    }
    $ids
}

function Get-DevInstances {
    # Match on exe path, NEVER on name.
    Get-Process foreman -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path -like "$DevRoot\*" }
}

function Stop-DevInstances {
    $ancestors = Get-AncestorIds
    $found = @(Get-DevInstances)
    if (-not $found) { Write-Host "no dev instances running"; return }
    foreach ($p in $found) {
        if ($ancestors -contains $p.Id) {
            Write-Warning "PID $($p.Id) is an ancestor of this shell (you are running inside it) - refusing to kill"
            continue
        }
        Write-Host "killing dev instance PID $($p.Id)"
        Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
    }
    Start-Sleep -Milliseconds 500
}

if ($List) {
    Get-Process foreman -ErrorAction SilentlyContinue |
        Select-Object Id, @{n = 'Kind'; e = {
            if ($_.Path -like "$DevRoot\*") { 'dev (this script)' }
            elseif ($_.Path -like "$RepoRoot\target\*") { 'repo target build' }
            else { 'installed / other' } } }, Path |
        Format-Table -AutoSize
    return
}

if ($Kill) { Stop-DevInstances; return }

# --- build ------------------------------------------------------------------
Write-Host "building $BuildProfile from $SrcDir" -ForegroundColor Cyan
Push-Location $SrcDir
try {
    $cargoArgs = @('build', '--target-dir', $TargetDir)
    if (-not $Debug) { $cargoArgs += '--release' }
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
}
finally { Pop-Location }

if (-not (Test-Path $Exe)) { throw "expected exe not found: $Exe" }

# --- sandbox ----------------------------------------------------------------
if ($Fresh -and (Test-Path $Sandbox)) {
    Write-Host "wiping sandbox"
    Remove-Item $Sandbox -Recurse -Force
}
New-Item -ItemType Directory -Force $ConfigDir | Out-Null

$RealConfig = Join-Path $env:APPDATA 'foreman'
if (-not $NoSeed -and (Test-Path $RealConfig)) {
    foreach ($f in @('settings.json', 'keybindings.json')) {
        $s = Join-Path $RealConfig $f
        if (Test-Path $s) { Copy-Item $s $ConfigDir -Force }
    }
    $themes = Join-Path $RealConfig 'themes'
    if (Test-Path $themes) { Copy-Item $themes $ConfigDir -Recurse -Force }
}
if ($SeedWorkspace) {
    $ws = Join-Path $RealConfig 'workspace.json'
    if (Test-Path $ws) { Copy-Item $ws $ConfigDir -Force }
}

# --- launch -----------------------------------------------------------------
# APPDATA is what config_dir() reads (src/config.rs), so overriding it here is
# the whole isolation mechanism. Set on THIS shell only; Start-Process inherits.
$env:APPDATA = $Sandbox

$p = Start-Process -FilePath $Exe -PassThru
Add-Content $PidFile $p.Id

Write-Host ""
Write-Host "launched PID $($p.Id)" -ForegroundColor Green
Write-Host "  exe      $Exe"
Write-Host "  config   $ConfigDir  (isolated - your real %APPDATA%\foreman is untouched)"
Write-Host "  stop it  .\scripts\run-dev.ps1 -Kill"
Write-Host ""
Write-Host "note: your existing foreman owns \\.\pipe\foreman, so the CLI verbs" -ForegroundColor DarkYellow
Write-Host "      (open/chat/send/snapshot) still address THAT instance." -ForegroundColor DarkYellow
