# PreToolUse(Bash) hook: if the command is a cargo build/run/test, kill any
# running foreman.exe first so the link doesn't fail with "Access is denied
# (os error 5)". Always exits 0 (never blocks the command).
$raw = [Console]::In.ReadToEnd()
try { $j = $raw | ConvertFrom-Json } catch { exit 0 }
$cmd = $j.tool_input.command
if ($cmd -match 'cargo\s+(build|run|test)') {
    Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
}
exit 0
