# Agent control probe harness. Dot-source, then call the functions.
# Drives a disposable foreman Session with send/snapshot/status and appends
# every snapshot to $ProbeLog with a scenario label, so the evidence survives
# the session. Findings from the 2026-09-18 run:
# docs/2026-09-18-agent-control-probes.md
$script:Fm = $env:FOREMAN_EXE
$script:Proj = $env:FOREMAN_PROJECT_ID
$script:ProbeRoot = "$PSScriptRoot"
$script:ProbeLog = if ($env:FOREMAN_PROBE_LOG) { $env:FOREMAN_PROBE_LOG } else { Join-Path $PSScriptRoot "probe-log.md" }

function Log([string]$text) { Add-Content -Path $script:ProbeLog -Value $text -Encoding utf8 }

function Probe-Open([string]$agent, [string[]]$argv, [string]$dir) {
    New-Item -ItemType Directory -Force $dir | Out-Null
    $r = & $script:Fm open --project $script:Proj --cwd $dir --title "plan-probe · $agent" -- @argv 2>&1 | Out-String
    $j = $r | ConvertFrom-Json
    Log "`n## $agent  (terminal $($j.terminal), cwd $dir)  $(Get-Date -Format o)`n"
    Log ('argv: ' + ($argv -join ' '))
    return $j.terminal
}

function Probe-Snap([string]$t, [string]$label, [int]$tail = 0) {
    if ($tail -gt 0) { $s = & $script:Fm snapshot --project $script:Proj --terminal $t --tail $tail 2>&1 | Out-String }
    else { $s = & $script:Fm snapshot --project $script:Proj --terminal $t 2>&1 | Out-String }
    # drop blank rows so the log stays readable
    $rows = ($s -split "`r?`n") | Where-Object { $_ -ne '' }
    Log "`n### $label  ($(Get-Date -Format HH:mm:ss.fff))`n"
    Log '```'
    Log ($rows -join "`n")
    Log '```'
    return ($rows -join "`n")
}

# Poll until the viewport matches $pattern or $timeoutSec passes. Returns $true on match.
function Probe-Wait([string]$t, [string]$pattern, [int]$timeoutSec = 60, [string]$label = "wait") {
    $deadline = (Get-Date).AddSeconds($timeoutSec)
    while ((Get-Date) -lt $deadline) {
        $s = & $script:Fm snapshot --project $script:Proj --terminal $t 2>&1 | Out-String
        if ($s -match $pattern) { Probe-Snap $t "$label — matched /$pattern/" | Out-Null; return $true }
        Start-Sleep -Milliseconds 700
    }
    Probe-Snap $t "$label — TIMEOUT waiting for /$pattern/" | Out-Null
    return $false
}

function Probe-Send([string]$t, [string]$text, [string]$keys, [int]$settle = 1500, [string]$label = "send") {
    $args = @('send', '--project', $script:Proj, '--terminal', $t, '--settle-ms', $settle)
    if ($text) { $args += @('--text', $text) }
    if ($keys) { $args += @('--keys', $keys) }
    $t0 = Get-Date
    $r = & $script:Fm @args 2>&1 | Out-String
    $ms = [int]((Get-Date) - $t0).TotalMilliseconds
    Log "`n> **$label**: text=$(if($text){'"'+$text+'"'}else{'-'}) keys=$(if($keys){$keys}else{'-'}) → $($r.Trim()) (round-trip $ms ms, settle $settle)"
    return $r.Trim()
}

function Probe-Status([string]$t) {
    $s = & $script:Fm status --project $script:Proj 2>&1 | Out-String
    $line = ($s -split "`r?`n") | Where-Object { $_ -match "^\s+$t\s" }
    Log "`n> status: $($line.Trim())"
    return $line.Trim()
}

function Probe-Close([string]$t) {
    $r = & $script:Fm close $t --project $script:Proj 2>&1 | Out-String
    Log "`n> close $t → $($r.Trim())"
    return $r.Trim()
}
