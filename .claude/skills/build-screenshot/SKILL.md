---
name: build-screenshot
description: Build foreman in debug, launch it, and capture its window to win.png so a change can be verified visually. Use when asked to screenshot the app, verify a UI change, or confirm the GUI renders. User-only — it spawns a real window.
disable-model-invocation: true
---

# build-screenshot

Foreman is a native GUI; you can't see it from the terminal. This skill builds it,
runs it, and grabs a PNG of its window so you can `Read` the result.

## Steps

0. **Bail out if `$env:FOREMAN` is `1`** — you are running inside the foreman
   app; the kill below would take down your own host and this session
   (incident: 2026-07-09). Tell the user and stop.

1. **Kill + build** (the PreToolUse hook also kills foreman, but be explicit).
   Kill by exe path, never by name — a by-name kill also takes down the user's
   *installed* foreman, which holds no lock on the build output (incident:
   2026-07-15). From the repo root:
   ```powershell
   Get-Process foreman -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$PWD\target\*" } | Stop-Process -Force; Start-Sleep -Milliseconds 500
   cargo build 2>&1 | Select-Object -Last 20
   ```
   Stop here and report if the build fails.

2. **Launch + capture** — run the bundled script from the repo root:
   ```powershell
   pwsh -NoProfile -File ".claude/skills/build-screenshot/screenshot.ps1"
   ```
   It starts `target\debug\foreman.exe`, waits ~6s, and writes `win.png` to the
   repo root. Pass `-WaitSeconds N` if the app needs longer to settle.

3. **Read** `win.png` and describe what you see vs. what the change intended.

4. **Clean up** — kill the app when done so the next build doesn't hit
   `Access is denied (os error 5)` (path-filtered, same reason as step 1):
   ```powershell
   Get-Process foreman -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$PWD\target\*" } | Stop-Process -Force
   ```

## Gotchas

- The capture pulls foreman to the foreground and screenshots screen pixels at the
  window rect — don't fight it for focus while it runs.
- To verify multi-window/nested layouts without driving the mouse, temporarily
  spawn projects/terminals at startup in `main.rs` (the `if !self.started` block),
  screenshot, then **revert**. See `docs/HANDOFF.md` §3.
- Release build (`cargo build --release`, `target\release\foreman.exe`) is the
  "is it fast" build; debug is fine for visual checks.
