# Foreman — Claude Code Project Memory

Fast, native desktop for running many AI-agent terminal sessions ("tmux built
for AI"). Rust + egui, real PTYs (`portable-pty`/ConPTY), full terminal emulation
(`alacritty_terminal`). **Hard requirement: it must be fast** — native, not
Electron/Tauri.

**Authoritative deep doc:** `docs/HANDOFF.md` — read it top-to-bottom before
substantial work (vision, full architecture, next phases, working agreement).
This file is the quick-load summary; HANDOFF.md wins on any conflict.

## Build / verify loop (Windows, PowerShell, GNU toolchain — no MSVC)

Kill the running app first or the link fails with `Access is denied (os error 5)`:

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 20
cargo run            # debug
cargo run --release  # the "is it fast" build
cargo test           # unit tests (layout tree, wm, chat — no GUI needed)
```

The GUI can't be seen from the terminal — to verify visually, run the exe and
screenshot the window, then `Read` the PNG. Full screenshot script is in
`docs/HANDOFF.md` § 3.

 Executables are built to `target/debug/foreman.exe` (debug) or `target/release/foreman.exe` (release).
Unit tests cover layout tree, window manager, and chat model (integration tests need the GUI). Run
`cargo test` or test individual modules with `cargo test --lib layout` / `::wm` / `::chat`.

## Gotchas (these already cost hours — do not rediscover)

- **GNU toolchain, not MSVC:** `rustup default stable-gnu`.
- **Linker = w64devkit** at `C:\w64devkit\bin` (on PATH; provides `dlltool`/`gcc`/`ld`).
- **`cannot find -lgcc_eh`:** w64devkit GCC 16 folded EH into `libgcc`. Fix is an
  empty stub: `ar crs C:\w64devkit\lib\gcc\x86_64-w64-mingw32\16.1.0\libgcc_eh.a`.
  Recreate if you reinstall w64devkit.
- **Black pane / shell never prompts = the DSR trap.** Shells send `ESC [ 6 n` on
  startup and hang until the terminal replies. `Session`'s `Listener` captures
  `Event::PtyWrite` and `pump()` writes it back. **Never use `VoidListener`.**
- **`Access is denied (os error 5)`** on build = app still running; kill it first.
- **egui 0.34:** `App::ui(&mut Ui, ...)` (not `update`); go through the painter
  (`ui.painter().layout_no_wrap`) since `ui.fonts(|f|…)` needs `&mut`. Ctrl+C/V may
  arrive as `Event::Copy`/`Paste` — handle those AND key events.
- **Resize reflow still diverges; cursor sync is bundled.** ConPTY 1.25 now asks
  Foreman for the post-resize cursor before later screen-buffer queries (#19535),
  but it cannot reconstruct dropped rows or clear stale PSReadLine text. This is
  NOT a double reflow in `Session::resize`; the four "ConPTY owns redraw" variants
  still fail. `Ctrl+L` heals residuals. Evidence: `docs/conpty-resize-reflow.md`.

## Architecture

A **recursive compositor**: one `WindowManager` engine runs at the desktop level
and nested inside each project. Each project window's content is another
`WindowManager` (`Content::Project(Box<WindowManager>)`) of terminals. Sub-windows
are confined to their project. Focus cascades so exactly one terminal reads the
keyboard. Window rects are **local** (relative to each manager's `area`).

**Two window states**: every window is either **tiled** (a leaf in the manager's
`LayoutTree` of recursive H/V splits — `src/layout.rs`) or **floating**. Drag a
header to tear a tile out; drop hints (leaf edge = split, leaf center = tab,
area edge = root split) re-insert it. Leader `WASD` moves in the tree,
`Alt+WASD` splits a new terminal in, `F`/`Ctrl+F` toggles float, `Z` zooms
(overlay — the tree is untouched). New windows tile by default. Full doc:
`docs/tiling-tree.md`.

**Tabs** are a generic `Win` property restricted by *level*: any window can tab
onto any other in the **same** `WindowManager` (so projects tab with projects,
terminals with terminals). A one-tab stack is a normal window; dragging a tab
out untabs it. A multi-tab tree leaf is a tabbed container in the layout.

- `src/main.rs` — eframe `App`; hosts the desktop `WindowManager` full-bleed.
  Closing the last project quits the app (`WindowManager::deserted`), like a
  terminal emulator exiting with its last tab; an open picker/settings modal
  holds it alive.
- `src/wm.rs` — the reusable window engine: drag/focus/z-order/min/max/resize/
  close, `Win` (tab stack), `Content`, tree integration, per-frame re-fit.
  Headers at both levels are always-on quiet chrome — surface-colored
  (`terminal::BG`) on a reserved band, no fill — except a lone tiled pane
  (bare); projects add hover-opened `+`/`⋯` menus (`docs/window-chrome.md`).
  Minimize restores via the task-manager panel (chip taskbars removed).
- `src/panel.rs` — task-manager panel model + shallow view (`Content::TaskManager`);
  desktop right-edge list of projects/tabs. See `docs/task-manager-panel.md`.
- `src/layout.rs` — the tiling tree (pure, unit-tested): insert/remove/layout/
  drop targets/divider resize.
- `src/terminal.rs` — `Session` (PTY + alacritty + reader thread), color resolver,
  selection/clipboard, key routing, grid render.
- `src/control.rs` — the `foreman` CLI + IPC control plane: `foreman
  open/chat/status/close` run from inside any foreman terminal talk to the GUI.
  Terminals get `FOREMAN=1`, `FOREMAN_EXE`, `FOREMAN_PROJECT_ID`,
  `FOREMAN_TERMINAL_ID` injected so agents can dispatch and self-target.
- `src/chat.rs` — per-project chat room model (append-only log, pure data).
  Posts are injected into every member terminal's PTY as typed input (push,
  not poll); dispatched terminals auto-join, others join on first post.
  Wiring lives in control.rs/wm.rs; `Content::Chat` is a read-only viewer.
- `src/dirpicker.rs` — keyboard-driven project directory picker.
- `src/keymap.rs` — data-driven leader-key bindings. Defaults live in
  `Keymap::default` (in code); a user file at `%APPDATA%\foreman\keybindings.json`
  is merged *over* them, so new commands always get a default chord. Leader is
  `Ctrl+B` by default (tmux-style).
- `src/settings.rs` — in-app keybindings editor; a desktop-level modal overlay
  (mirrors the `dirpicker.rs` pattern), edits the live `Keymap` and signals the
  wm when to persist.
- `src/theme.rs` — every color token (surfaces, border/focus ladder, selection,
  app chrome, chat, ANSI palette) as consts, glob-imported by consumers. Static
  by design — no runtime theme system until a second theme exists.
- `src/skills_install.rs` — on startup, embeds (`include_str!`) and installs the
  `foreman-dispatch`/`foreman-chat` skills into Claude and Codex global skill
  dirs so agents in any project (incl. external repos) can discover them. Source
  skill copies live in `.claude/skills/` and `.codex/skills/`; keep them in sync
  when behavior changes, then rebuild to propagate. Best-effort — failures are
  logged, never block launch.

Subsystem docs: `docs/tiling-tree.md` (two-state windows + layout tree),
`docs/project-directories.md`,
`docs/epics/agent-dispatch-epic.md` (control CLI + chat room; known gaps in
`docs/chat-missing-features.md`),
`docs/epics/keyboard-control-epic.md` (leader/keymap/settings),
`docs/epics/window-tabbing-split-epic.md` (tab-stacks; zone parts superseded).
(`docs/foreman.md` is older narrative notes — prefer HANDOFF.md on any conflict.)

## Agent skills

### Issue tracker

GitHub Issues on `sniffle6/foreman` via `gh`; external PRs are **not** a triage surface. See `docs/agents/issue-tracker.md`.

### Triage labels

Default vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: root `CONTEXT.md` + `docs/adr/`. See `docs/agents/domain.md`.

## Working agreement

- Quality- and speed-obsessed user; no flattery, push back on bad ideas.
- Verify by building + screenshotting — don't claim it works without evidence.
- Don't needlessly hijack the user's mouse/keyboard to test.
- Commit only when asked.

## Session Context

**Memory system:** This project uses persistent memory at `C:\Users\sniff\.claude\projects\H--claude-code-foreman\memory\`.
Session knowledge persists across conversations — check MEMORY.md in that directory to see what's been recorded.

**Active work:** Currently working on `feat/browser-style-tabs` branch. See `docs/epics/window-tabbing-split-epic.md`
for the full epic context. This branch targets full tab-stack integration across projects and terminals.
