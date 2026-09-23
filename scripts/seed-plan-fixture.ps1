# Seed the plan-view screenshot fixture (card 978oj5).
#
# Builds nothing. Creates:
#   target/agent/planfix          - a throwaway project with seeded cards
#   target/agent/appdata-plan     - a sandbox APPDATA (your real settings.json
#                                   and themes/, so the shot matches your theme)
#
# Then run:
#   pwsh -NoProfile -File ".claude/skills/build-screenshot/screenshot.ps1" `
#        -Exe ".\target\agent\debug\foreman.exe" -AppData ".\target\agent\appdata-plan"

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$proj = Join-Path $root "target\agent\planfix"
$tasks = Join-Path $proj ".foreman\tasks"
$sandbox = Join-Path $root "target\agent\appdata-plan"
$cfg = Join-Path $sandbox "foreman"

Remove-Item -Recurse -Force $tasks, $cfg -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $tasks, $cfg | Out-Null

# Your real look, so the screenshot is the app you actually run.
$real = Join-Path $env:APPDATA "foreman"
foreach ($n in @("settings.json")) {
    if (Test-Path (Join-Path $real $n)) { Copy-Item (Join-Path $real $n) (Join-Path $cfg $n) -Force }
}
if (Test-Path (Join-Path $real "themes")) {
    Copy-Item (Join-Path $real "themes") (Join-Path $cfg "themes") -Recurse -Force
}

# --- cards -----------------------------------------------------------------
# id, title, state, plan, wave, created-offset (minutes)
$cards = @(
    # Plan A: a finished wave above a partly-done current wave, then a future one.
    @{ id = "aa0001"; t = "split the PTY reader off the frame path"; s = "done";        p = "Terminal work"; w = 1; m = 0 },
    @{ id = "aa0002"; t = "clamp the grid walk to real bounds";      s = "done";        p = "Terminal work"; w = 1; m = 3 },
    @{ id = "aa0003"; t = "reflow the viewport on resize";           s = "in_progress"; p = "Terminal work"; w = 2; m = 6 },
    @{ id = "aa0004"; t = "answer DSR before the first paint";       s = "blocked";     p = "Terminal work"; w = 2; m = 9 },
    @{ id = "aa0005"; t = "wire the caret gate to the new metrics";  s = "backlog";     p = "Terminal work"; w = 2; m = 12 },
    @{ id = "aa0006"; t = "retire the old reflow shim";              s = "backlog";     p = "Terminal work"; w = 3; m = 15 },

    # Plan B: a long name that must elide, and a complete plan.
    @{ id = "bb0001"; t = "audit every control-plane reply shape";   s = "done";        p = "Control plane hardening and wire-compatibility sweep"; w = 1; m = 20 },
    @{ id = "bb0002"; t = "add the missing compat tests";            s = "done";        p = "Control plane hardening and wire-compatibility sweep"; w = 2; m = 23 },

    # Plan C: the typo case - one card, drawn dimmed and flat.
    @{ id = "cc0001"; t = "make the strip honour the theme";         s = "backlog";     p = "terminal-work"; w = 1; m = 26 },

    # Unplanned: must not appear in the plan view at all.
    @{ id = "dd0001"; t = "not in any plan";                          s = "backlog";     p = $null;           w = 0; m = 30 }
)

$base = [datetime]::UtcNow.AddHours(-6)
foreach ($c in $cards) {
    $stamp = $base.AddMinutes($c.m).ToString("yyyy-MM-ddTHH:mm:ssZ")
    $card = [ordered]@{
        v       = 1
        id      = $c.id
        title   = $c.t
        state   = $c.s
        created = $stamp
        updated = $stamp
    }
    if ($c.s -eq "blocked") { $card["blocked_reason"] = "needs a design decision" }
    if ($null -ne $c.p) { $card["planned"] = [ordered]@{ name = $c.p; wave = $c.w } }
    # Key order in the file does not matter; the app re-serializes on write.
    $card | ConvertTo-Json -Depth 5 | Set-Content -Path (Join-Path $tasks "$($c.id).json") -Encoding utf8
}

# --- workspace -------------------------------------------------------------
# The plan window must be FLOATING for its rect to be respected: a tiled window
# takes its geometry from the layout tree, so the child manager's tree is null.
$ws = [ordered]@{
    version = 1
    desktop = [ordered]@{
        focused = 1
        windows = @(
            [ordered]@{
                id     = 1
                active = 0
                tabs   = @(
                    [ordered]@{
                        title   = "planfix"
                        content = [ordered]@{
                            kind  = "Project"
                            child = [ordered]@{
                                cwd     = $proj
                                focused = 2
                                windows = @(
                                    [ordered]@{
                                        id     = 2
                                        active = 0
                                        tabs   = @(
                                            [ordered]@{ title = "plan"; content = [ordered]@{ kind = "Plan" } }
                                        )
                                        rect   = [ordered]@{ x = 12; y = 12; w = 620; h = 460 }
                                    }
                                )
                                tree    = $null
                            }
                        }
                    }
                )
                rect   = [ordered]@{ x = 20; y = 20; w = 1000; h = 640 }
            }
        )
        tree    = [ordered]@{ kind = "Leaf"; id = 1 }
    }
}
$ws | ConvertTo-Json -Depth 20 | Set-Content -Path (Join-Path $cfg "workspace.json") -Encoding utf8

Write-Host "seeded:"
Write-Host "  project : $proj"
Write-Host "  appdata : $sandbox"
Write-Host ""
Write-Host "now run:"
Write-Host "  pwsh -NoProfile -File `".claude/skills/build-screenshot/screenshot.ps1`" -Exe `".\target\agent\debug\foreman.exe`" -AppData `".\target\agent\appdata-plan`""
