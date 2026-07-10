# PreToolUse(Bash) hook: if the command is a cargo build/run/test, kill any
# running foreman.exe first so the link doesn't fail with "Access is denied
# (os error 5)". Always exits 0 (never blocks the command).
#
# EXCEPT when this session runs inside foreman itself (FOREMAN=1 is injected
# into every foreman terminal): killing foreman would kill our own host, every
# other terminal, and this very session (incident: 2026-07-09).
if ($env:FOREMAN -eq '1') { exit 0 }
$raw = [Console]::In.ReadToEnd()
try { $j = $raw | ConvertFrom-Json } catch { exit 0 }
$cmd = $j.tool_input.command
if ($cmd -match 'cargo\s+(build|run|test)') {
    Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
}
exit 0
