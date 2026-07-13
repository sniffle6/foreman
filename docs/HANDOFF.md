# Foreman — Session Handoff

Read this top-to-bottom before doing anything. It is self-contained: the vision,
the current state, the build/verify loop, the gotchas that already cost hours, and
the next phases. Paths are relative to the repo root (the `foreman` directory,
wherever you cloned it).

Companion: `docs/foreman.md` (user-facing narrative notes) — treat this file as
authoritative on any conflict.

---

## 1. The vision

A **fast, native desktop for running many AI-agent terminal sessions** — "a modern
tmux manager built for AI." The user runs many AI coding agents (Claude Code,
Codex CLI) in terminals across several projects and needs to watch/operate them
all. Hard requirement: **it must be fast** — instant open, zero input lag, no
stutter under heavy output. "Lag in a program like this makes it DOA." That's why
it's native Rust, not Electron/Tauri.

**Product shape (decided with the user):**
- A **window-manager desktop**. Each **project** is a top-level window (drag /
  snap / resize / minimize / maximize / close).
- A project window is a **sandbox** — its content is *another* window manager.
  Inside it you open **sub-windows**: a terminal now, a browser pane later. Same
  interactions at both levels.
- **Sub-windows are confined to their project** (the user chose the confined
  model over tear-out-between-projects). It's a **recursive compositor**: one
  `WindowManager` engine used at the desktop level and nested inside each project.

**Design language:** warm-graphite, matte (no neon glow). NOT dark-navy/violet
"AI SaaS", NOT green-CRT, NOT cream editorial. One loud accent = amber `#e7a93f`
(focus borders, cursor, snap overlay). Cool teal `#74b0a4` = running/active.
Project chrome is a cooler/deeper tint than terminal chrome so the two levels read
as distinct.

---

## 2. Current state — ALL verified (build + screenshot)

- **Toolchain** working (GNU + w64devkit — see gotchas).
- **Native terminal**: spawns real shells (**PowerShell / cmd / bash via WSL**)
  through `portable-pty` (ConPTY); `alacritty_terminal` emulation with **ANSI
  colors, cursor, inverse/dim**; resizes the shell to the window.
- **Window manager** (shared engine, used at desktop + project levels):
  - drag by titlebar, click-to-focus + raise (z-order), minimize→task-manager panel→restore,
    maximize/restore, resize (corner), close. Confined to the area.
  - **Two window states — tiled + floating** (replaced the old 9-zone snap):
    each manager owns a `LayoutTree` (`src/layout.rs`) of recursive H/V splits
    with ratios; windows whose ids are tree leaves are tiled, everything else
    floats. Drag a header to tear a tile out; while dragging, leaf edges show
    split hints, leaf centers tab-merge, area edge bands split the root.
    Leader `WASD` moves within the tree, `Alt+WASD` splits, `F`/`Ctrl+F`
    toggles float. New windows tile by default (chat viewer stays floating).
    Full doc: `docs/tiling-tree.md`.
  - **Zoom (tmux-style)**: `Z` / titlebar max renders the window full-area on
    top; the tree underneath is untouched (`WindowManager.zoomed`).
  - **Per-frame re-fit**: every window re-fits each frame — tiled rects come
    from `tree.layout()`, floating windows clamp into the area — so everything
    reflows when the OS window resizes.
- **Nesting**: desktop hosts **project** windows; each project is a nested
  `WindowManager` (`Content::Project(Box<WindowManager>)`) of terminals. Focus
  cascades so exactly ONE terminal (the focused one in the focused project) reads
  the keyboard. Per-project egui `Id` namespacing (`base.with(("proj", id))`).
- **Text selection + clipboard** (terminal-standard): drag to select (amber
  highlight), **Ctrl+C** copies if there's a selection else sends interrupt
  (SIGINT), **Ctrl+Shift+C** copies, **Ctrl+V / Ctrl+Shift+V / Shift+Insert /
  right-click** paste. Control codes (Ctrl+L/R/D/A/E…) forward to the shell.

`bash` works via `wsl.exe`, but the user's machine needs WSL2 enabled ("Virtual
Machine Platform" + BIOS virtualization). Not an app bug; cmd/powershell are fine.

### Architecture / files
- `src/terminal.rs` — `Session` (PTY + alacritty + reader thread + writer +
  `resp` reply buffer), color resolver, selection (`sel_anchor`/`sel_head`,
  `selection_text`, `cell_at`), `read_input` (keys + clipboard), `show(ui, rect,
  active, resp)` renders the grid + cursor + selection highlight. `read_clipboard`
  uses `arboard`; copy uses `ctx.copy_text`.
- `src/wm.rs` — the reusable window engine. `WindowManager { windows, tree,
  zoomed, z, focused, next, … }`, `Win { id, tabs, active, rect (LOCAL coords),
  z, minimized, prev }`, `Content::{Terminal, Project, Chat}`.
  `show(ui, area, active, base)` is the whole thing.
- `src/layout.rs` — the tiling tree (pure data + math, unit-tested): insert /
  remove / rect layout / drop targets / divider resize. See
  `docs/tiling-tree.md`.
- `src/main.rs` — eframe `App`: toolbar (`+ project`, `+ terminal in project`) +
  the desktop `WindowManager`.
- `src/skills_install.rs` — embeds and best-effort installs the
  `foreman-dispatch`/`foreman-chat` skills into Claude and Codex global skill
  dirs at GUI startup. Claude sources live in `.claude/skills/`; Codex sources
  live in `.codex/skills/`. Keep the paired skill copies semantically synced.

### Coordinate model (matters for new work)
Each `WindowManager` works in its own `area: Rect`. Window rects are **local**
(relative to `area.min`); screen rect = `rect.translate(area.min)`. Confinement =
`clamp(rect, area.size())`. Nesting: a project's content rect becomes the child's
`area`; pass a unique `base` Id down. Snap/dwell use
`ui.ctx().pointer_latest_pos()` minus `area.min`.

---

## 3. The build / verify loop (USE THIS EXACTLY)

Windows, PowerShell, no MSVC. PATH for cargo + linker:
```powershell
$env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
Set-Location <repo root>   # the foreman/ directory, wherever you cloned it
```
(`C:\w64devkit\bin` is also persisted on the user PATH, so a plain terminal works.)

**Build** (kill the running app first or the link fails with `Access is denied`):
```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 20
```

**⚠ Unless `$env:FOREMAN` is `1`** — then you are running inside the foreman app
itself and `Stop-Process foreman` kills your own host, every terminal in it, and
you (incident: 2026-07-09, an agent reviewing this repo from a foreman terminal
took the whole app down). Ask the user to close foreman, or build to a separate
target dir that doesn't lock the running exe: `cargo build --target-dir target/agent`.

**Run + screenshot the window** (you can't see the GUI otherwise — capture it and
`Read` the PNG):
```powershell
$p = Start-Process -FilePath ".\target\debug\foreman.exe" -PassThru
Start-Sleep -Seconds 6
Add-Type @"
using System; using System.Runtime.InteropServices;
public class Cap { [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  public struct RECT { public int Left, Top, Right, Bottom; } }
"@
[Cap]::SetForegroundWindow($p.MainWindowHandle) | Out-Null; Start-Sleep -Milliseconds 400
$r = New-Object Cap+RECT; [Cap]::GetWindowRect($p.MainWindowHandle, [ref]$r) | Out-Null
Add-Type -AssemblyName System.Drawing
$b = New-Object System.Drawing.Bitmap(($r.Right-$r.Left), ($r.Bottom-$r.Top))
$g = [System.Drawing.Graphics]::FromImage($b); $g.CopyFromScreen($r.Left,$r.Top,0,0,$b.Size)
$b.Save("$(Get-Location)\win.png"); $g.Dispose(); $b.Dispose()
```
Then `Read` `win.png`.

To test multi-window/nested layouts without hijacking the user's mouse,
temporarily spawn a few projects/terminals at startup in `main.rs` (call
`add_project` / `add_terminal_to_focused` in the `if !self.started` block),
screenshot, then REVERT. For interactions you must drive (drag/snap/copy), you can
use Win32 `mouse_event` / `SendKeys` from PowerShell — but it MOVES THE USER'S
MOUSE/FOCUS, so do it sparingly and tell them.

---

## 4. Gotchas that already cost hours (do not rediscover)

1. **GNU toolchain, not MSVC.** `rustup default stable-gnu` (no Visual Studio).
2. **Linker = w64devkit** at `C:\w64devkit\bin` (provides `dlltool`/`gcc`/`ld`).
3. **`-lgcc_eh` stub.** w64devkit GCC 16 folded EH into `libgcc`; Rust still links
   `-lgcc_eh`. Empty stub at
   `C:\w64devkit\lib\gcc\x86_64-w64-mingw32\16.1.0\libgcc_eh.a` (`ar crs <path>`).
   Recreate if you reinstall w64devkit.
4. **DSR trap (do not undo).** Shells send `ESC [ 6 n` on startup and hang until
   the terminal replies. `Session`'s `Listener` captures `Event::PtyWrite` and
   `pump()` writes it back. Black pane / `rx 4 bytes` = this. Never use
   `VoidListener`.
5. **egui 0.34**: `App::ui(&mut Ui, ...)` (not `update`). `ui.fonts(|f|…)` needs
   `&mut` → use `ui.painter().layout_no_wrap` / `painter.layout_job`. `rect_stroke`
   takes `StrokeKind`. Ctrl+C/X/V may arrive as `Event::Copy`/`Cut`/`Paste`
   (handle both those AND key events — see `read_input`).
6. **`Access is denied (os error 5)`** = app still running; kill it first.
7. **ConPTY resize (Windows).** Bundled OpenConsole **1.25.260512002-preview**
   implements post-resize cursor re-sync with the host (#19535); Foreman answers
   DSR/CPR per RX chunk and keeps minimized panes pumping. Height grow uses
   `resize_anchored` so typing does not land mid-scrollback. Residual content
   glitches (stale wrap paint, empty band after shrink→grow, wrap-overflow) are
   **known** — full conhost reflow parity is parked. **`Ctrl+L`** heals a pane.
   Authority: `docs/conpty-resize-reflow.md`. Do not re-try “let ConPTY own
   redraw” or blame a double reflow in `Session::resize`. The bundled pair is a
   **preview** package — swap to stable 1.25 when Microsoft publishes it
   (hashes + provenance: `assets/conpty/README.md`).

---

## 5. Next phases (pick up here)

The window-manager skeleton + terminal are done. What's left is the "built for AI"
substance:

1. **Status lines** — project titlebar: repo + branch (+ git status). Per-terminal:
   model · token usage · state. (Reference: the old web mockup had these; ask the
   user if you want the visual.)
2. **AI-agent integration** (the differentiator) — run claude/codex CLIs in the
   terminals; detect "needs input" / done / idle and surface it (badge on the
   terminal/project titlebar, a "jump to next needs-you", etc.).
3. **`Content::Browser`** — new enum variant + `Content::show` arm; the rest of the
   engine is reused.
4. **Daemon/client split** — move PTYs into a headless core so sessions persist
   across UI restarts (true tmux-style). Native launch is already instant, so this
   is about live process survival, not open-speed. **Cold layout restore**
   (fresh shells at saved project cwds) already ships via `workspace.json` —
   see `docs/workspace-persistence.md`; that is not this item.

Smaller polish backlog: scrollback + scroll-wheel, word/line select (double/triple
click), tab-merge windows (drop one window onto another), keyboard tiling
shortcuts, tune `TOP_HOLD`/`GROW_LEAD` to feel, remove the `Session.shell`
dead-code warning (or use it).

---

## 6. Working agreement
- The user is quality- and speed-obsessed; no flattery, push back on bad ideas.
- Verify by building + screenshotting; don't claim it works without evidence.
- Don't needlessly hijack the user's mouse/keyboard to test.
- After a feature, update `docs/foreman.md`. Commit only when asked.
