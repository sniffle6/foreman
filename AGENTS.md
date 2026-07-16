# Foreman - Codex Project Guide

Fast, native desktop for running many AI-agent terminal sessions ("tmux built
for AI"). Rust + egui, real PTYs (`portable-pty`/ConPTY), full terminal emulation
(`alacritty_terminal`). Hard requirement: it must be fast: native, not
Electron/Tauri.

Authoritative deep doc: `docs/HANDOFF.md`. Read it top-to-bottom before
substantial work. This file is the quick-load Codex summary; HANDOFF.md wins on
any conflict.

## Build / Verify Loop

Windows, PowerShell, GNU toolchain. Kill the running app first or linking fails
with `Access is denied (os error 5)`. **Kill by exe path, never by name** —
`Stop-Process -Name foreman` also kills the user's *installed* foreman
(`%LOCALAPPDATA%\Programs\foreman`), which holds no lock on the build output
(incident: 2026-07-15). From the repo root:

```powershell
Get-Process foreman -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$PWD\target\*" } | Stop-Process -Force; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 20
cargo test
```

Run:

```powershell
cargo run
cargo run --release
```

The GUI cannot be inspected from terminal output. For visual verification, build,
launch `target\debug\foreman.exe`, capture `win.png`, then inspect it with
Codex's image viewer. The reusable workflow lives in
`.codex/skills/build-screenshot`.

## Gotchas

- GNU toolchain, not MSVC: `rustup default stable-gnu`.
- Linker is w64devkit at `C:\w64devkit\bin`.
- If Rust cannot find `-lgcc_eh`, recreate the empty stub archive at
  `C:\w64devkit\lib\gcc\x86_64-w64-mingw32\16.1.0\libgcc_eh.a`.
- Black pane or shell never prompts means the DSR trap regressed. Shells send
  `ESC [ 6 n`; `Session`'s `Listener` must capture `Event::PtyWrite`, and
  `pump()` must write it back. Never use `VoidListener`.
- egui 0.34 uses `App::ui(&mut Ui, ...)`, not `update`. Prefer painter text
  layout APIs because `ui.fonts(|f| ...)` needs `&mut`.
- Ctrl+C/V may arrive as `Event::Copy`/`Paste`; handle those and key events.

## Architecture

A recursive compositor: one `WindowManager` engine runs at the desktop level and
nested inside each project. Project content is another `WindowManager`
(`Content::Project(Box<WindowManager>)`) containing terminal sub-windows.
Sub-windows are confined to their project. Focus cascades so exactly one terminal
reads keyboard input. Window rects are local to each manager's `area`.

Key files:

- `src/main.rs` - eframe app; hosts the desktop `WindowManager`.
- `src/wm.rs` - window engine: drag/focus/z-order/min/max/resize/close/snap,
  recursive project content, tabs, and per-frame re-fit.
- `src/terminal.rs` - PTY session, alacritty emulation, clipboard/selection, key
  routing, grid rendering.
- `src/control.rs` - `foreman open` / `foreman chat` pipe client and server.
- `src/keymap.rs` and `src/settings.rs` - leader-key defaults and editor.
- `src/skills_install.rs` - startup installer for bundled Claude and Codex
  skills.

## Foreman Skills

Claude source skills live under `.claude/skills`. Codex copies live under
`.codex/skills`. Keep them semantically in sync when changing dispatch/chat
behavior, but adapt command examples to the target agent:

- Claude skills use `claude` / `claude -p`.
- Codex skills use `codex` / `codex exec`.

Foreman embeds and globally installs only the cross-repo agent-operation skills
(`foreman-dispatch`, `foreman-chat`) on startup. `build-screenshot` remains a
repo-local verification skill.

## Working Agreement

- Verify with build/tests and screenshots when GUI behavior changes. Do not claim
  visual behavior works without image evidence.
- Do not needlessly hijack the user's mouse or keyboard to test.
- Keep edits scoped; avoid unrelated refactors.
- Commit only when asked.
