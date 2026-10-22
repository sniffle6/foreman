# Seed the integration-queue screenshot fixture (card w4ufci).
#
# Builds nothing. Creates:
#   target/agent/queuefix       - a throwaway repo whose queue runs one card
#                                 at the check stage, with two queued behind it
#                                 and one handed back
#   target/agent/queuefix-held  - a throwaway repo whose queue is held
#   target/agent/appdata-queue  - a sandbox APPDATA whose workspace shows both
#                                 boards, stacked (your real settings.json and
#                                 themes/, so the shot matches your theme)
#
# Then it HOLDS both repositories' integration turns (turn.lock) for
# -HoldSeconds. A running Foreman would otherwise take the turn, find the
# fake worktrees missing, and hand every request back within one poll; with
# the turn held elsewhere its coordinator only reads and paints the queue.
# Run the screenshot while this waits:
#   pwsh -NoProfile -File ".claude/skills/build-screenshot/screenshot.ps1" `
#        -Exe ".\target\agent\debug\foreman.exe" -AppData ".\target\agent\appdata-queue"
param([int] $HoldSeconds = 900)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$sandbox = Join-Path $root "target\agent\appdata-queue"
$cfg = Join-Path $sandbox "foreman"
Remove-Item -Recurse -Force $cfg -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $cfg | Out-Null
$real = Join-Path $env:APPDATA "foreman"
if (Test-Path (Join-Path $real "settings.json")) {
    Copy-Item (Join-Path $real "settings.json") (Join-Path $cfg "settings.json") -Force
}
if (Test-Path (Join-Path $real "themes")) {
    Copy-Item (Join-Path $real "themes") (Join-Path $cfg "themes") -Recurse -Force
}

function Stamp([double] $minutesAgo) {
    [datetime]::UtcNow.AddMinutes(-$minutesAgo).ToString("yyyy-MM-ddTHH:mm:ssZ")
}

# A fresh repo with one commit; returns its path and HEAD.
function New-Repo([string] $name) {
    $proj = Join-Path $root "target\agent\$name"
    Remove-Item -Recurse -Force $proj -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force -Path (Join-Path $proj ".foreman\tasks") | Out-Null
    git -C $proj init -q -b main
    Set-Content -Path (Join-Path $proj "README.md") -Value "queue fixture"
    git -C $proj add README.md
    git -C $proj -c user.name=fixture -c user.email=fixture@localhost -c commit.gpgsign=false commit -q -m init
    $head = (git -C $proj rev-parse HEAD).Trim()
    New-Item -ItemType Directory -Force -Path (Join-Path $proj ".git\foreman\integration") | Out-Null
    @{ proj = $proj; head = $head }
}

# One In Progress worktree card and its request file. $req carries the
# request's phase-specific fields (attempt, outcome, hold).
function Add-Card($repo, [string] $id, [string] $title, [int] $seq, [string] $phase, [hashtable] $req) {
    $wt = Join-Path $repo.proj ".foreman\worktrees\$id"
    $stamp = Stamp 30
    $card = [ordered]@{
        v        = 1
        id       = $id
        title    = $title
        state    = "in_progress"
        worktree = [ordered]@{ path = $wt; branch = "card/$id"; base = "main" }
        created  = $stamp
        updated  = $stamp
    }
    $card | ConvertTo-Json -Depth 5 |
        Set-Content -Path (Join-Path $repo.proj ".foreman\tasks\$id.json") -Encoding utf8
    $r = [ordered]@{
        v         = 1
        seq       = $seq
        card      = $id
        repo      = (Join-Path $repo.proj ".git")
        target    = "main"
        dest      = $repo.proj
        worktree  = $wt
        branch    = "card/$id"
        commit    = $repo.head
        submitted = (Stamp 20)
        updated   = (Stamp 4)
        phase     = $phase
    }
    foreach ($k in $req.Keys) { $r[$k] = $req[$k] }
    $r | ConvertTo-Json -Depth 6 |
        Set-Content -Path (Join-Path $repo.proj ".git\foreman\integration\$id.json") -Encoding utf8
}

# --- running queue ---------------------------------------------------------
$run = New-Repo "queuefix"
Add-Card $run "qa0001" "answer DSR before the first paint" 4 "integrating" @{
    attempt = [ordered]@{
        id = "fixture"; pid = 1; run = "fixture"; started = (Stamp 4); stage = "check"
        retries = 0; target_commit = $run.head; prepared = $run.head
    }
}
Add-Card $run "qa0002" "clamp the grid walk to real bounds" 5 "queued" @{}
Add-Card $run "qa0003" "retire the old reflow shim" 6 "queued" @{}
Add-Card $run "qa0004" "split the PTY reader off the frame path" 2 "needs_resolution" @{
    outcome = [ordered]@{
        reason = "conflict"; detail = "conflicts in: src/terminal.rs"
        next   = "resolve the conflicts in the worktree, ``git add`` them, ``git rebase --continue``, then resubmit"
        at     = (Stamp 9)
    }
}

# --- held queue ------------------------------------------------------------
$held = New-Repo "queuefix-held"
Add-Card $held "qb0001" "wire the caret gate to the new metrics" 1 "queued" @{
    hold = "destination dirty: uncommitted changes in the destination checkout: src/board.rs"
}
Add-Card $held "qb0002" "make the strip honour the theme" 2 "queued" @{}

# --- workspace: the two boards, stacked ------------------------------------
function Project-Window([int] $id, [int] $boardId, [string] $proj) {
    [ordered]@{
        id     = $id
        active = 0
        tabs   = @(
            [ordered]@{
                title   = (Split-Path -Leaf $proj)
                content = [ordered]@{
                    kind  = "Project"
                    child = [ordered]@{
                        cwd     = $proj
                        focused = $boardId
                        windows = @(
                            [ordered]@{
                                id     = $boardId
                                active = 0
                                tabs   = @([ordered]@{ title = "board"; content = [ordered]@{ kind = "Board" } })
                                rect   = [ordered]@{ x = 0; y = 0; w = 900; h = 300 }
                            }
                        )
                        tree    = [ordered]@{ kind = "Leaf"; id = $boardId }
                    }
                }
            }
        )
        rect   = [ordered]@{ x = 0; y = 0; w = 900; h = 300 }
    }
}
$ws = [ordered]@{
    version = 1
    desktop = [ordered]@{
        focused = 1
        windows = @((Project-Window 1 2 $run.proj), (Project-Window 3 4 $held.proj))
        tree    = [ordered]@{
            kind     = "Split"
            dir      = "V"
            ratios   = @(0.5, 0.5)
            children = @([ordered]@{ kind = "Leaf"; id = 1 }, [ordered]@{ kind = "Leaf"; id = 3 })
        }
    }
}
$ws | ConvertTo-Json -Depth 20 | Set-Content -Path (Join-Path $cfg "workspace.json") -Encoding utf8

# --- hold both turns -------------------------------------------------------
# std's File::try_lock locks the whole file; a one-byte range lock from here
# overlaps it, so the app's try_turn sees WouldBlock ("another owner").
$locks = foreach ($repo in @($run, $held)) {
    $path = Join-Path $repo.proj ".git\foreman\integration\turn.lock"
    $fs = [System.IO.File]::Open($path, "OpenOrCreate", "ReadWrite", "ReadWrite")
    $fs.Lock(0, 1)
    $fs
}

Write-Host "seeded:"
Write-Host "  running : $($run.proj)"
Write-Host "  held    : $($held.proj)"
Write-Host "  appdata : $sandbox"
Write-Host ""
Write-Host "holding both turns for $HoldSeconds s; run now:"
Write-Host "  pwsh -NoProfile -File `".claude/skills/build-screenshot/screenshot.ps1`" -Exe `".\target\agent\debug\foreman.exe`" -AppData `".\target\agent\appdata-queue`""
Start-Sleep -Seconds $HoldSeconds
$locks | ForEach-Object { $_.Dispose() }
