# verify-terminal.ps1 — one send+snapshot round-trip against a RUNNING foreman.
#
# Sends input into a foreman Session over the control plane, blocks until the
# Session's output quiets (Quiescence settle, server-side), then prints its
# Snapshot (the rendered grid as text). A thin wrapper over the foreman CLI —
# all parsing/validation lives in src/control.rs, so this script adds no
# behavior of its own.
#
# REQUIRES a running foreman GUI (it owns the \\.\pipe\foreman control pipe).
# If foreman is not running, the CLI prints "cannot reach foreman" on stderr
# and this script exits 1.
#
# Examples
#   # From inside a foreman terminal (self-targets via FOREMAN_* env):
#   pwsh -NoProfile -File verify-terminal.ps1 -Text 'echo hi' -Keys Enter
#
#   # Target another Session explicitly:
#   pwsh -NoProfile -File verify-terminal.ps1 -Project p1 -Terminal t3 -Text 'ls' -Keys Enter
#
#   # Snapshot only (no send) — a pure read:
#   pwsh -NoProfile -File verify-terminal.ps1 -Project p1 -Terminal t3
#
#   # Ground-truth cursor position (raw model cursor, one JSON line):
#   pwsh -NoProfile -File verify-terminal.ps1 -Project p1 -Terminal t3 -Cursor
#
# Exit codes (propagated from the foreman CLI): 0 ok; 1 foreman refused or is
# unreachable; 2 bad arguments.

[CmdletBinding()]
param(
    # Raw UTF-8, written to the Session verbatim — NO escape processing, so a
    # literal '\r' in the string sends backslash+r, not Enter. Embed a real CR
    # with "`r" or (better) pass -Keys Enter.
    [string]$Text,
    # Space-separated named key presses, e.g. 'Enter', 'Ctrl+C', 'Tab Enter',
    # 'F5'. Sent after -Text. Unknown names exit 2.
    [string]$Keys,
    # pN / tN. Omit both to self-target from inside a foreman terminal
    # (FOREMAN_PROJECT_ID / FOREMAN_TERMINAL_ID are read by the CLI itself).
    [string]$Project,
    [string]$Terminal,
    # Quiet window in ms the server waits after the send before replying.
    # -1 = CLI default (120 ms). 0 = fire-and-forget (no settle wait).
    # Values above 4000 are clamped server-side; 4000 ms is also the hard cap
    # on the total wait even if the Session never goes quiet.
    [int]$SettleMs = -1,
    # Include per-cell colors/style flags in the Snapshot (reply becomes one JSON line).
    [switch]$Attrs,
    # Include the raw model cursor {row, col, shape} (reply becomes one JSON line).
    [switch]$Cursor
)

# Resolve the foreman binary: FOREMAN_EXE is injected into every
# foreman-spawned terminal; otherwise fall back to the repo build outputs
# (this script lives at <repo>/.claude/skills/foreman-diagnostics-and-tooling/scripts/).
$exe = $env:FOREMAN_EXE
if (-not $exe) {
    $root = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..\..')).Path
    $exe = @(
        (Join-Path $root 'target\release\foreman.exe'),
        (Join-Path $root 'target\debug\foreman.exe')
    ) | Where-Object { Test-Path $_ } | Select-Object -First 1
}
if (-not $exe -or -not (Test-Path $exe)) {
    [Console]::Error.WriteLine('verify-terminal: foreman.exe not found — set FOREMAN_EXE or build the repo first')
    exit 1
}

$target = @()
if ($Project)  { $target += @('--project', $Project) }
if ($Terminal) { $target += @('--terminal', $Terminal) }

if ($Text -or $Keys) {
    $sendArgs = @('send') + $target
    if ($Text) { $sendArgs += @('--text', $Text) }
    if ($Keys) { $sendArgs += @('--keys', $Keys) }
    if ($SettleMs -ge 0) { $sendArgs += @('--settle-ms', "$SettleMs") }
    # Blocks until the Session produced no output for the quiet window (or the
    # 4 s cap). Success prints {"ok":true}, suppressed here; errors reach
    # stderr untouched.
    & $exe @sendArgs > $null
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

$snapArgs = @('snapshot') + $target
if ($Attrs)  { $snapArgs += '--attrs' }
if ($Cursor) { $snapArgs += '--cursor' }
& $exe @snapArgs
exit $LASTEXITCODE
