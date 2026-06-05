# Foreman (native)

A fast, native desktop for running many AI-agent terminal sessions. Rust + egui,
with real PTYs and a full terminal emulator. This is the "real" build that
replaces the HTML mockups in `foreman/`.

Lives in `foreman-native/`.

## What it does today

- A **window-manager desktop**: each terminal is a floating window you can drag,
  focus (click), raise (z-order), **minimize** (to a taskbar), **maximize**,
  **resize** (corner), and **close**. Windows are confined to the desktop area.
- Each window hosts a **real shell** via a PTY: **PowerShell / cmd / bash(WSL)**.
  Top toolbar spawns new terminals.
- Full ANSI emulation (colors, cursor, inverse/dim), resizes the shell to the
  window.

- **Snap zones / split** (Stage 1b, done): drag a window's titlebar toward an
  edge or corner of the desktop and a translucent amber overlay previews where it
  will land; release to snap. See "Snap zones" below.

- **Nesting** (Stage 2, done): the desktop hosts **project** windows; each
  project is a sandbox containing its *own* nested window manager of terminal
  sub-windows. Same drag/snap/min/max/resize at both levels. See "Nesting" below.

## Snap zones (drag-to-edge split)

Lives entirely in `wm.rs`. Ported from the web mockup `foreman/index.html`
(`detectZone` / `zoneRect` / `.snap-ov`).

- While dragging a titlebar, the pointer position is converted to a fraction of
  the manager `area`. `detect_zone(fx, fy)` maps that to a `Zone`:
  - top edge → **Max** (fill area); left/right edge → **Left/Right half** (split);
  - the four corners → **quarter** zones; middle → no snap (free drop).
- The edge band is `T = 0.085` (8.5%) and corners use a `0.22` cross-axis band.
  Because only the outer ~8.5% triggers, the whole middle is a natural dead-zone.
- `zone_rect(zone, area_size)` returns the target rect in **local** coords, inset
  by `SNAP_GAP` from the area edge with a half-gap down the centre split.
  `SNAP_GAP = 0.0` today, so snapped/maximized windows tile **edge-to-edge** and
  adjacent halves/quarters touch (bump the const to reintroduce a gap). Snapped
  windows also square their corners (`CornerRadius::ZERO`) so they sit flush to the
  area edges — rounded corners would leave triangular gaps. Floating windows keep
  the 6px radius.
- While a drag is active and over a zone, a single amber overlay
  (`SNAP_FILL` ~13% alpha + 1.5px `SNAP_STROKE`) is painted **after** the window
  loop so it sits on top of all windows.
- On `dr.drag_stopped()`, if the pointer is in a zone the window's `rect` is set
  to the zone rect, `prev` is saved (so the maximize toggle / future restore can
  return it to where it was floating), and `maximized` is set only for the Max
  zone. Everything is still `clamp`ed to the area.

Gotcha: snap is detected from `ui.ctx().pointer_latest_pos()` (screen coords)
minus `area.min`, not from the drag delta — the delta only moves the rect.

Dragging a snapped/maximized window out: on the first `dr.dragged()` frame the
window restores to its pre-snap size from `prev` (same size the double-click /
restore toggle returns), then re-anchors that smaller rect under the cursor (the
pointer keeps the same horizontal fraction across the title) so the title stays
grabbed instead of the window jumping away.

## Nesting (project windows)

Lives in `wm.rs`. The desktop `WindowManager` hosts **project** windows; a project
is just a `Win` whose `content` is `Content::Project(Box<WindowManager>)` — a whole
nested window manager. The engine is recursive: one `WindowManager` type used at
both levels.

How it works:

- **Recursion.** `Content::show` for a `Project` calls
  `wm.show(ui, content_rect, active, base.with(("proj", win_id)))`. The project
  window's content rect (everything below its titlebar) becomes the child
  manager's `area`. The child paints its own desktop bg, windows, snap overlay,
  and taskbar inside that rect. Sub-windows are **confined** to it because all of
  the child's `clamp`/snap math is relative to the passed-in `area` (local space)
  — no tear-out between projects.
- **Focus cascade (one keyboard reader).** `active` ANDs down the tree. A window
  is the keyboard reader only if `focused == Some(id) && active` (`is_focus`). The
  desktop passes `active=true`; each project receives `active = is_focus` of its
  own `Win`, and passes that down to its sub-windows. So the keyboard is read by
  exactly one leaf: the focused terminal inside the focused project. Two terminals
  in two different projects never both type. Interaction now also propagates
  **upward**: `WindowManager::show` returns whether any of its windows was
  interacted with this frame, and when a `Content::Project` window's child reports
  an interaction the parent focuses that project. So clicking a sub-window inside a
  background project switches the desktop to that project (this cascades through
  arbitrary nesting depth, since each focus makes the parent's `acts` non-empty too).
- **Id namespacing.** Every nested manager gets a unique `base` Id via
  `base.with(("proj", win_id))`. Without this, egui interaction Ids (drag/resize/
  buttons keyed on `base.with((id, role))`) would collide across projects that
  reuse the same per-window ids (each manager numbers windows from 1).
- **Adding things.** `add_project(shell, ctx)` makes a project window that starts
  with one terminal sub-window. There's no global toolbar anymore — a project
  titlebar carries its own `+` button just left of the min/max/close controls,
  which queues `Act::AddProject` (applied after the render loop) to spawn a sibling
  project. Adding a terminal *into* a project is done from that same titlebar:
  each project header draws compact `PS · CMD · SH` keys after its title.
  Clicking one queues `Act::AddTerm(win_id, shell)`, which (after the render loop)
  finds that project window and calls its child manager's `add_terminal(shell)`.
  The controls live on the header (not a global bar) so the target site is the
  window you click — no "focused project" ambiguity. Terminal headers don't get
  the keys or the `+` (gated on `is_project`); the keys also drop off if a project
  window is too narrow to fit them beside the window controls.
- **Visual distinction.** Project titlebars use a subtly cooler/deeper tint
  (`PROJ_TITLE_BG` / `PROJ_TITLE_BG_FOCUS`) vs the warm terminal titlebars, so the
  two nesting levels read as distinct without leaving the warm-graphite palette.
  Window borders are a thin uniform `BORDER_W = 0.75` px; focus is shown by colour,
  not weight. The focused **project** uses a punchier `PROJ_BORDER_FOCUS`
  (`240,158,11`) while focused terminals keep the softer `BORDER_FOCUS`
  (`231,169,63`), so the selected project pops at a glance.

Snap/min/max/resize/close/taskbar all work *inside* a project for free because the
engine is shared. You can also snap/drag the project windows themselves on the
desktop.

Gotchas / TODO:

- **TODO (status line):** project titlebars currently show only `project N`. A
  repo/branch status line (and a per-terminal model/tokens/state line) is the next
  bit of polish — see the web mockup `foreman/index.html` for the target.
- **TODO (browser content):** `Content` is ready for a `Browser` variant alongside
  `Terminal` and `Project`; only `show` needs a new arm.
- **Borrow shape:** the window loop recurses via `self.windows[i].content.show(...)`.
  That's a single mutable borrow of one `Win`; the recursion only touches the child
  manager, so there's no overlapping borrow. Keep new code inside the existing
  collect-`Act`s-then-apply pattern.

## Stack (and why)

- **eframe/egui** — GPU UI, instant launch (no Chromium). Chosen over Electron
  (slow cold start) and Tauri (webview IPC bottlenecks under high terminal output).
- **portable-pty** — spawns real shells in a pseudo-terminal; ConPTY on Windows.
  This is the bash/pwsh/cmd picker.
- **alacritty_terminal** — the same VTE/grid engine Alacritty and Zed use. Parses
  the shell's bytes into a cell grid we render.

## Build / run

```
cd foreman-native
cargo run
```

PATH already has `~/.cargo/bin` and `C:\w64devkit\bin` (the linker). If a fresh
machine: install Rust (`winget install Rustlang.Rustup`), then see the toolchain
gotchas below.

## Non-obvious gotchas (these all bit us — read before touching the build)

- **No MSVC? Use the GNU toolchain.** `rustup default stable-gnu`. It links
  without Visual Studio.
- **GNU needs a real MinGW linker.** Rust's bundled one lacks `dlltool`, so we use
  **w64devkit** (`C:\w64devkit\bin` on PATH) for `dlltool`/`gcc`/`ld`.
- **`cannot find -lgcc_eh`.** w64devkit's GCC 16 merged the EH runtime into
  `libgcc`, but Rust still links `-lgcc_eh`. Fix: an **empty stub archive** at
  `C:\w64devkit\lib\gcc\x86_64-w64-mingw32\16.1.0\libgcc_eh.a`
  (`ar crs libgcc_eh.a`). If you reinstall w64devkit, recreate it.
- **Black screen / shell never prompts = the DSR trap.** Shells send a
  cursor-position query (`ESC [ 6 n`) on startup and *wait* for the terminal's
  reply before drawing the prompt. alacritty generates that reply via its
  `EventListener`; if you use `VoidListener` it's dropped and the shell hangs
  forever. We capture `Event::PtyWrite` and write it back to the PTY each
  `pump()`. **Do not** swap this back to `VoidListener`.
- **egui `ui.fonts(|f| ...)` wants `&mut`** in 0.34; go through the painter
  (`ui.painter().layout_no_wrap`, `painter.layout_job`) instead.
- **eframe 0.34 `App` trait uses `ui(&mut Ui, ...)`**, not `update(&Context, ...)`.
- **`Access is denied (os error 5)` on build** = the app is still running and
  locking the exe. Kill `foreman-native` first.

## Key files

- `foreman-native/src/terminal.rs` — `Session` (PTY + alacritty + reader thread),
  color resolver, `show(rect, active)` that renders the grid and routes keys.
- `foreman-native/src/wm.rs` — `WindowManager` + `Win` + `Content`. The reusable
  window engine (drag/focus/z/min/max/resize/close/snap), confined to a rect.
  `Content::Project(Box<WindowManager>)` nests it (recursive compositor); see
  "Nesting" above. `add_project` makes the outer level; the per-project header keys
  (`Act::AddTerm` → child `add_terminal`) make the inner one.
- `foreman-native/src/main.rs` — the eframe app: just hosts the desktop manager
  full-bleed (no toolbar; new projects come from the `+` on a project titlebar).

## Bash on Windows

`bash` runs via `wsl.exe`. If it shows "WSL2 is not supported… enable Virtual
Machine Platform", that's a Windows setup step (enable the feature + CPU
virtualization in BIOS), not an app bug.
