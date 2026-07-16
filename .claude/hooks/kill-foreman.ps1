# PreToolUse(Bash) hook: if the command is a cargo build/run/test, kill any
# foreman.exe running FROM THIS REPO'S target\ dir so the link doesn't fail
# with "Access is denied (os error 5)". Kill by exe path, NEVER by name: only
# a target-built instance can hold the lock on the build output — an installed
# foreman (%LOCALAPPDATA%\Programs\foreman) holds no such lock and must
# survive (incident: 2026-07-15, this hook killed the user's installed
# instance by name; it looked like a crash). Always exits 0 (never blocks).
#
# EXCEPT when this session runs inside foreman itself (FOREMAN=1 is injected
# into every foreman terminal): the host may be a target build, and killing it
# would kill every other terminal and this very session (incident: 2026-07-09).
if ($env:FOREMAN -eq '1') { exit 0 }
$raw = [Console]::In.ReadToEnd()
try { $j = $raw | ConvertFrom-Json } catch { exit 0 }
$cmd = $j.tool_input.command
if ($cmd -match 'cargo\s+(build|run|test)') {
    $target = Join-Path (Split-Path (Split-Path $PSScriptRoot)) 'target'
    Get-Process -Name foreman -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -like "$target\*" } |
        Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
}
exit 0
