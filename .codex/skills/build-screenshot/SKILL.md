---
name: build-screenshot
description: Build Foreman in debug, launch it, and capture its native GUI window to win.png for visual verification. Use when asked to screenshot the app, verify a UI change, or confirm the GUI renders. This spawns a real window.
---

# Build Screenshot

Foreman is a native GUI; terminal output is not enough for visual checks. This
skill builds it, launches it, captures the window to `win.png`, and then you
inspect the image.

## Steps

0. **Check `$env:FOREMAN`.** If it is `1` you are running inside the foreman
   app. Do NOT run the kill line in step 1 — it takes down your own host and
   this session (incident: 2026-07-09). Use the inside-foreman variant of each
   step instead; the script itself is safe either way because it only ever
   stops the instance it started, by pid.

1. **Kill + build.** Outside foreman, from the repo root (kill by exe path,
   never by name — a by-name kill also takes down the user's *installed*
   foreman, which holds no lock on the build output; incident: 2026-07-15):
   ```powershell
   Get-Process foreman -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$PWD\target\*" } | Stop-Process -Force; Start-Sleep -Milliseconds 500
   cargo build 2>&1 | Select-Object -Last 20
   ```
   Inside foreman, skip the kill and build to a target dir the host does not lock:
   ```powershell
   cargo build --target-dir target/agent 2>&1 | Select-Object -Last 20
   ```
   Stop here and report if the build fails.

2. **Launch + capture** — run the bundled script from the repo root:
   ```powershell
   pwsh -NoProfile -File ".codex/skills/build-screenshot/scripts/screenshot.ps1"
   ```
   It starts `target\debug\foreman.exe`, waits ~6s, writes `win.png` to the repo
   root, and stops that instance. Flags:
   - `-Exe <path>` — which exe to launch. Inside foreman pass
     `.\target\agent\debug\foreman.exe`.
   - `-AppData <dir>` — use that directory as `APPDATA`, so the instance reads
     and writes its own `settings.json`, `themes\` and `workspace.json` and never
     touches yours. `config_dir()` in `src/config.rs` reads that variable; this is
     the same isolation `scripts\run-dev.ps1` uses (its sandbox is
     `.\target\agent\appdata`). Seed the dir with whatever the shot needs — a
     theme file plus a `settings.json` naming it, a hand-written
     `workspace.json` for a specific layout — and leave it out to capture your
     real config.
   - `-Out <png>`, `-WaitSeconds N`, `-Keep` (leave the instance running).

3. **Inspect** `win.png` with Codex's local image viewer and compare what is
   visible against the intended change.

4. **Clean up.** The script already stopped the instance it launched unless you
   passed `-Keep`. To stop a kept instance outside foreman (path-filtered, same
   reason as step 1):
   ```powershell
   Get-Process foreman -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$PWD\target\*" } | Stop-Process -Force
   ```
   Inside foreman, stop it by the pid the script printed, never by name.

## Gotchas

- The capture is `PrintWindow` on the window's own surface, not a screen grab:
  it does not pull foreman to the foreground, and a window sitting in front of
  it (a game, a browser) does not end up in the PNG. Earlier versions used
  `CopyFromScreen` and produced a screenshot of whatever was on top.
- `PrintWindow` captures the window's current frame. Hover fills, the leader
  pill, and modal scrims only appear under interaction, which the script does
  not drive — seed a `workspace.json` for layout, not for pointer state.
- A hand-written `workspace.json` must be valid JSON: a project whose `cwd`
  is not a directory is skipped, and a file that fails to parse is sidelined
  as `workspace.json.corrupt-*` and the landing screen shows instead. Use
  forward slashes in paths to dodge escape trouble.
- To verify multi-window/nested layouts without driving the mouse, prefer a
  seeded `workspace.json` under `-AppData`; the older trick of temporarily
  spawning projects/terminals at startup in `main.rs` (the `if !self.started`
  block) still works but must be **reverted**. See `docs/HANDOFF.md` §3.
- Release build (`cargo build --release`, `target\release\foreman.exe`) is the
  "is it fast" build; debug is fine for visual checks.
