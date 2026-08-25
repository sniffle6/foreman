# PostToolUse(Edit|Write) hook: catch the two documentation-rot classes that a
# machine can see, in the .md files agents actually load into context.
#
#   1. Line-number citations  — `src/wm.rs:2106`. An audit on 2026-08-25 found
#      65 of 65 such cites wrong in one skill, 32 of 33 in another. Cites into
#      wm.rs/main.rs were off by 150-560 lines. They fail QUIETLY: they point at
#      real code that is not the code you wanted, so the reader draws a firm
#      wrong conclusion and never learns otherwise.
#   2. Phantom symbols — a backticked identifier cited next to a src/*.rs path
#      that no longer exists in the tree (DEFAULT_SETTLE_MS, CaretGate,
#      FramePlan, compose_zone). These fail loudly once you grep, which is
#      exactly why naming the symbol beats naming the line.
#
# The rule this enforces: cite by line ONLY into something a machine pins
# (a crates.io version held by Cargo.lock). Otherwise cite `file.rs` + the
# symbol, or cite the command that derives it.
#
# Deliberately NOT a hardcoded list of dead symbols — that would be a census,
# and a census is the thing this hook exists to prevent. Symbols are derived
# from the edited file and checked against the live tree, so the NEXT deleted
# symbol is caught without anyone updating this script.
#
# Exits 2 with findings on stderr so Claude Code feeds them back and the agent
# fixes them in the same turn. Any internal error exits 0 — this must never
# block real work.
#
# Manual use:  pwsh -File .claude/hooks/cite-guard.ps1 -Path docs/HANDOFF.md
#              pwsh -File .claude/hooks/cite-guard.ps1 -All

param(
    [string]$Path,
    [switch]$All
)

$ErrorActionPreference = 'Stop'

function Get-RepoRoot {
    if ($env:CLAUDE_PROJECT_DIR) { return $env:CLAUDE_PROJECT_DIR }
    return (Split-Path (Split-Path $PSScriptRoot))
}

# Files whose citations an agent is expected to trust and act on.
# docs/superpowers/ is excluded: archived plans are correct for their date.
function Test-InScope {
    param([string]$Rel)
    if ($Rel -notmatch '\.md$') { return $false }
    $r = $Rel -replace '\\', '/'
    # Archived planning records. Correct for their date; not written to be acted on.
    if ($r -like 'docs/superpowers/*') { return $false }
    if ($r -like 'docs/epics/*') { return $false }
    if ($r -like 'docs/plans/*') { return $false }
    # Date-named docs carry their own expiry in the filename.
    if ($r -match '(^|/)\d{4}-\d{2}-\d{2}-') { return $false }
    return (
        $r -like '.claude/skills/*' -or
        $r -like '.codex/skills/*'  -or
        $r -like '.claude/agents/*' -or
        $r -like 'docs/*'           -or
        $r -eq   'CLAUDE.md'        -or
        $r -eq   'AGENTS.md'        -or
        $r -eq   'CONTEXT.md'
    )
}

try {
    $root = Get-RepoRoot

    # ---- decide what to scan -------------------------------------------------
    $targets = @()
    if ($All) {
        $targets = Get-ChildItem -Path $root -Recurse -Filter *.md -File |
            ForEach-Object { $_.FullName } |
            Where-Object { $_ -notmatch '\\target\\' -and $_ -notmatch '\\\.git\\' }
    }
    elseif ($Path) {
        $targets = @((Resolve-Path -LiteralPath $Path).Path)
    }
    else {
        # Hook mode: the edited path arrives as JSON on stdin.
        $raw = [Console]::In.ReadToEnd()
        try { $j = $raw | ConvertFrom-Json } catch { exit 0 }
        $fp = $j.tool_input.file_path
        if (-not $fp) { exit 0 }
        if (-not (Test-Path -LiteralPath $fp)) { exit 0 }
        $targets = @((Resolve-Path -LiteralPath $fp).Path)
    }

    # ---- ground truth: the live module set and the whole tree, read once -----
    $srcDir = Join-Path $root 'src'
    if (-not (Test-Path $srcDir)) { exit 0 }

    $modules = @{}
    $srcText = New-Object System.Text.StringBuilder
    foreach ($f in Get-ChildItem -Path $srcDir -Filter *.rs -File) {
        $modules[$f.Name] = $true
        [void]$srcText.Append([IO.File]::ReadAllText($f.FullName))
        [void]$srcText.Append("`n")
    }
    $srcAll = $srcText.ToString()

    $findings = @()

    foreach ($t in $targets) {
        $rel = $t.Substring($root.Length).TrimStart('\', '/') -replace '\\', '/'
        if (-not (Test-InScope $rel)) { continue }

        $lines = [IO.File]::ReadAllLines($t)
        $whole = $lines -join "`n"
        # File-level opt-out, for a doc that is deliberately a historical record
        # or a design doc naming work that does not exist yet.
        if ($whole -match '<!--\s*cite-guard:\s*off\s*-->') { continue }

        # A doc that declares itself non-current in its header — superseded, or a
        # build plan for work that does not exist — names absent symbols on
        # purpose; that is its content, not a defect. Such a header suppresses the
        # phantom-symbol check ONLY. A line-number cite is still wrong in a
        # historical doc, because the reader may still try to follow it.
        $head = ($lines | Select-Object -First 15) -join "`n"
        $historical = $head -match '(?i)(SUPERSEDED|ARCHIVED|HISTORICAL|designed,? not (built|started)|not built|deferred)'

        for ($i = 0; $i -lt $lines.Count; $i++) {
            $line = $lines[$i]
            $n = $i + 1
            # Line-level opt-out, for the doctrine's own counter-examples.
            if ($line -match '<!--\s*cite-guard:\s*ok\s*-->') { continue }

            # -- check 1: line-number citation into a REAL foreman module ------
            foreach ($m in [regex]::Matches($line, 'src/([a-z0-9_]+\.rs):(\d+)')) {
                if ($modules.ContainsKey($m.Groups[1].Value)) {
                    $findings += [pscustomobject]@{
                        File = $rel; Line = $n; Kind = 'line-cite'
                        What = $m.Value
                        Why  = "cite the symbol instead: ``src/$($m.Groups[1].Value)`` ``fn name``"
                    }
                }
            }

            # -- check 2: phantom symbol cited beside a src/*.rs path ----------
            if ($historical) { continue }
            if ($line -notmatch 'src/[a-z0-9_]+\.rs') { continue }
            # Asserting a symbol's ABSENCE is the one case where naming a
            # nonexistent symbol is correct — negative probes, removal records,
            # and "not built yet" design notes all do it deliberately.
            if ($line -match '(?i)any hit means|expect nothing|expect no |zero hits|no longer|was deleted|were deleted|removed by|removed in|never existed|does not exist|used to be|unbuilt|not built|not yet') { continue }
            foreach ($b in [regex]::Matches($line, '`([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)`')) {
                $tok = $b.Groups[1].Value
                # A git SHA in backticks is a commit, not a symbol.
                if ($tok -match '^[0-9a-f]{7,40}$') { continue }
                # Only judge tokens that look like code, not English in backticks.
                $segs = @($tok -split '::' | Where-Object { $_.Length -ge 4 })
                if ($segs.Count -eq 0) { continue }
                $hit = $false
                foreach ($s in $segs) { if ($srcAll.Contains($s)) { $hit = $true; break } }
                if (-not $hit) {
                    $findings += [pscustomobject]@{
                        File = $rel; Line = $n; Kind = 'phantom-symbol'
                        What = "``$tok``"
                        Why  = 'zero hits in src/ — deleted, renamed, or never existed'
                    }
                }
            }
        }
    }

    if ($findings.Count -eq 0) {
        if ($All -or $Path) { Write-Host "cite-guard: clean" }
        exit 0
    }

    $msg = @()
    $msg += "cite-guard: $($findings.Count) citation problem(s) — docs must not restate facts the code already holds."
    foreach ($f in $findings) {
        $msg += "  $($f.File):$($f.Line)  [$($f.Kind)]  $($f.What)"
        $msg += "      $($f.Why)"
    }
    $msg += "  Suppress a deliberate case with an inline <!-- cite-guard: ok --> on that line."
    $text = $msg -join "`n"

    if ($All -or $Path) { Write-Host $text; exit 1 }
    [Console]::Error.WriteLine($text)
    exit 2
}
catch {
    # Never block real work on a hook bug.
    exit 0
}
