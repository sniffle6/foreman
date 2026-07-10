---
name: build-screenshot
description: Build Foreman in debug, launch it, and capture its native GUI window to win.png for visual verification. Use when asked to screenshot the app, verify a UI change, or confirm the GUI renders. This spawns a real foreground window.
---

# Build Screenshot

Foreman is a native GUI; terminal output is not enough for visual checks. This
skill builds it, launches it, captures the window to `win.png`, and then you
inspect the image.

## Steps

0. Bail out if `$env:FOREMAN` is `1` — you are running inside the Foreman app;
   the kill below would take down your own host and this session
   (incident: 2026-07-09). Tell the user and stop.

1. Kill any running Foreman and build:

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 20
```

Stop and report the build output if it fails.

2. Launch and capture from the repo root:

```powershell
pwsh -NoProfile -File ".codex/skills/build-screenshot/scripts/screenshot.ps1"
```

Pass `-WaitSeconds N` if the app needs longer to settle. The script writes
`win.png` in the repo root.

3. Inspect `win.png` with Codex's local image viewer and compare what is visible
against the intended change.

4. Clean up:

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue
```

## Gotchas

- The capture pulls Foreman to the foreground and screenshots screen pixels at
  the window rect. Do not fight it for focus while it runs.
- To verify multi-window or nested layouts without driving the mouse, temporarily
  spawn projects/terminals at startup in `main.rs`, screenshot, then revert.
- Release build (`cargo build --release`, `target\release\foreman.exe`) is the
  speed build; debug is fine for ordinary visual checks.
