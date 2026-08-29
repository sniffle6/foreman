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

## 2. Current state — build green; terminal-completeness remainder not signed off

- **Toolchain** working (GNU + w64devkit — see gotchas).
- **Native terminal**: spawns real shells (**PowerShell / cmd / bash via WSL**)
  through `portable-pty` (ConPTY); `alacritty_terminal` emulation with **ANSI
  colors, cursor, inverse/dim, real bold/italic faces** (Hack four-face set);
  **mouse reporting** (click/drag/motion + prior wheel); **Ctrl+F scrollback
  search** (bounded, one shared budget/frame). Automated tests cover the
  correctness seams; human acid + screenshot sign-off for the remainder epic
  is still open — see `docs/epics/terminal-completeness-epic.md`. Docs:
  `docs/terminal-font-styles.md`, `docs/terminal-mouse-reporting.md`,
  `docs/terminal-scrollback-search.md`.
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
- **Automatic agent Session naming** (opt-in): guarded Claude/Codex/Grok
  `UserPromptSubmit` hooks feed one instance-specific local pipe; one bounded
  worker invokes the selected installed CLI and applies a validated title only
  while the exact Project/Member/vendor-session generation still owns it. Manual
  titles win. Project and Member ids remain attached to their tabs across
  merge/restore rather than inheriting the containing window's id. Cost,
  privacy, failure behavior, and module seams:
  `docs/agent-session-naming.md`.

`bash` works via `wsl.exe`, but the user's machine needs WSL2 enabled ("Virtual
Machine Platform" + BIOS virtualization). Not an app bug; cmd/powershell are fine.

### Architecture / files
- `src/main.rs` — eframe `App`; hosts the desktop `WindowManager` full-bleed.
  Closing the last project quits (`WindowManager::deserted`); an open
  picker/settings modal holds the app alive. `App::logic` services channels and
  recursively pumps live Sessions while the native viewport is hidden; visible
  frames do that work once through `App::ui`.
- `src/wm.rs` — the reusable window engine. `WindowManager { windows, tree,
  zoomed, z, focused, next, … }`, `Win { id, tabs, active, rect (LOCAL coords),
  z, minimized, prev }`, `Content::{Terminal, Project, Chat, TaskManager, Image,
  Settings}`.
  `show(ui, area, active, base)` is the whole thing. Headers at both levels are
  always-on quiet chrome (`docs/window-chrome.md`). `WindowManager::term_env`
  owns the environment injected into every Session.
- `src/layout.rs` — the tiling tree (pure data + math, unit-tested): insert /
  remove / rect layout / drop targets / divider resize. See
  `docs/tiling-tree.md`.
- `src/panel.rs` — task-manager panel model + shallow view; desktop right-edge
  list of projects/tabs. See `docs/task-manager-panel.md`.
- `src/terminal.rs` — `Session` (PTY + alacritty + reader thread + writer +
  `resp` reply buffer), color resolver, selection, mouse capture, search
  adapter, `read_input` (keys + clipboard), `show(ui, rect, active, resp)`
  renders the grid + overlays. `read_clipboard` uses `arboard`; copy uses
  `ctx.copy_text`.
- `src/terminal_font.rs` — four Hack faces + system fallbacks; `font_id`.
- `src/input.rs` — pure key/paste/wheel/mouse encoding + Ctrl+F open-search.
- `src/search.rs` — bounded scrollback-search model.
- `src/control.rs` — the `foreman` CLI + IPC control plane over the named pipe
  `\.\pipe\foreman`. The environment injected by `wm.rs` gives its CLI
  `FOREMAN`, `FOREMAN_EXE`, `FOREMAN_PROJECT_ID`, and `FOREMAN_TERMINAL_ID` for
  dispatch/self-targeting, plus the instance-specific `FOREMAN_TITLE_PIPE` for
  passive title events.
- `src/title_notify.rs` — early `foreman title-event` CLI path plus the bounded,
  one-way instance title pipe. Hook helpers normalize vendor payloads, reject
  subagent traffic, write once, and never wait for a reply.
- `src/agent_hooks.rs` — opt-in semantic installation of guarded global
  Claude/Codex/Grok `UserPromptSubmit` hooks. Preserves unrelated configuration,
  backs up once, replaces atomically, and reports install status to the GUI.
- `src/terminal_titles.rs` — Title lane domain state, transcript-prefix context,
  provider command adapters, one bounded worker, process deadlines, and
  untrusted-output validation. It knows nothing about window layout.
- `src/board.rs` — `Content::Board`: the per-project kanban board view (four
  fixed columns, quick-add, dispatch picker). Read seam is a per-frame store
  snapshot; writes drain as `BoardAct` intents via `drain_board_acts` in wm.rs.
  See `docs/kanban-board.md`.
- `src/chat.rs` — per-project chat room model (append-only log, pure data).
  Posts are injected into member terminals' PTYs as typed input (push, not
  poll). Wiring lives in control.rs/wm.rs; `Content::Chat` is a read-only viewer.
- `src/dirpicker.rs` — keyboard-driven project directory picker.
- `src/imageview.rs` — `Content::Image`: `foreman view <path.png>` opens a
  persistent PNG viewer (fit/zoom/pan, no PTY). See `docs/image-viewer.md`.
- `src/kanban.rs` — pure card domain for the per-project board: file-per-card
  store under `.foreman/tasks/`, single-writer transitions, derived orphan
  detection (`is_orphaned`), dispatch prompt template, wait verdicts. GUI-free.
  See `docs/kanban-board.md`.
- `src/keymap.rs` — data-driven leader-key bindings. Defaults in
  `Keymap::default`; `%APPDATA%\foreman\keybindings.json` merges *over* them so
  new commands always get a default chord. Leader is `Ctrl+B`.
- `src/settings.rs` — in-app keybindings editor (desktop-level modal, mirrors
  `dirpicker.rs`); edits the live `Keymap`, signals the wm to persist.
- `src/settings_menu.rs` — the settings menu (`Ctrl+B ,`): pure model + egui
  modal view. Edits `config::Settings` live via `config::seed_live`/`live`.
  See `docs/settings-menu.md`.
- `src/theme.rs` — every color token as consts, glob-imported by consumers.
- `src/skills_install.rs` — embeds and best-effort installs the
  `foreman-dispatch`/`foreman-chat`/`foreman-icat`/`foreman-kanban` skills into Claude and Codex
  global skill dirs at GUI startup. Claude sources live in `.claude/skills/`;
  Codex sources live in `.codex/skills/`. Keep the paired copies semantically
  synced, then rebuild to propagate.

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

**Build** (kill the running app first or the link fails with `Access is denied`).
Kill by exe path, never by name — only a `target\`-built instance holds the lock
on the build output; killing by name also takes down the user's *installed*
foreman (`%LOCALAPPDATA%\Programs\foreman`), which looks like a crash
(incident: 2026-07-15). From the repo root:
```powershell
Get-Process foreman -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$PWD\target\*" } | Stop-Process -Force; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 20
```

**⚠ Unless `$env:FOREMAN` is `1`** — then you are running inside the foreman app
itself and `Stop-Process foreman` kills your own host, every terminal in it, and
you (incident: 2026-07-09, an agent reviewing this repo from a foreman terminal
took the whole app down). Ask the user to close foreman, or build to a separate
target dir that doesn't lock the running exe: `cargo build --target-dir target/agent`.

**To run a dev build next to a foreman you already have open**, use
`.\scripts\run-dev.ps1` rather than launching the exe yourself — it builds to
`target\agent`, sandboxes `APPDATA` so the dev instance can't overwrite your real
`workspace.json`, and kills by exe path with an ancestor guard. See
`docs/dev-launcher.md`.

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

**Verified against `src/` on 2026-08-24.** Most of what this section listed as
future in 2026-06 has since shipped. Re-verify before believing any roadmap,
including this one.

Shipped since the original list: the Control plane, Chat room and Dispatch
(`src/control.rs`, `src/chat.rs`); terminal inspection (`foreman send` /
`snapshot`); scrollback, wheel scrolling and scrollback search; word and line
selection via double/triple click (terminal.rs:2031-2047); tab stacks and
tab-merge by drag; the leader keymap and settings menu; workspace persistence;
the image viewer; and shell selection, which retired the old `Session.shell`
dead-code warning by giving the field a job. The `TOP_HOLD`/`GROW_LEAD`
constants the old backlog wanted tuned no longer exist.

Genuinely still open:

1. **Status lines** — project titlebar: repo + branch (+ git status).
   Per-terminal: model, token usage, state. Nothing renders these today; the
   only `branch` string in `src/` is preview text at `appearance.rs:473`.
   (Reference: the old web mockup had these; ask the user if you want the
   visual.)
2. **Agent-state detection** — the unbuilt half of "AI-agent integration".
   Running the claude/codex CLIs in terminals works, and `proc.rs::agent_for`
   already identifies which agent owns a Session (it drives the tab icons). What
   is missing is needs-input / working / done / idle state and surfacing it: a
   badge on the terminal or project titlebar, "jump to next needs-you". Design
   notes: `.claude/skills/foreman-agent-state-campaign/SKILL.md`.
3. **`Content::Browser`** — a new enum variant plus a `Content::show` arm; the
   rest of the engine is reused. `Content` today is Terminal / Project / Chat /
   Image / TaskManager (wm.rs:116).
4. **Daemon/client split** — move PTYs into a headless core so sessions survive
   UI restarts (true tmux-style). Native launch is already instant, so this is
   about live process survival, not open-speed. **Cold layout restore** (fresh
   shells at saved project cwds) already ships via `workspace.json`; see
   `docs/workspace-persistence.md`. That is not this item.

---

## 6. Working agreement
The working agreement lives in `CLAUDE.md` (Claude) and `AGENTS.md` (Codex) and
is deliberately not duplicated here.

One correction to what this section used to say: **"after a feature, update
`docs/foreman.md`" is dead practice.** That file is superseded narrative notes.
Write one doc per feature in `docs/` instead — house style and the supersession
rules are in `.claude/skills/foreman-docs-and-writing/SKILL.md`.
