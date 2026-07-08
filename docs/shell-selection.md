# Shell selection

## What it does

A PowerShell Session spawns `pwsh.exe` (PowerShell 7) when it is on PATH,
falling back to `powershell.exe` (Windows PowerShell 5.1) when it is not.
`cmd` and `bash` (WSL) Sessions are unaffected.

## Why

PSReadLine inline predictions (the gray ghost text you accept with the right
arrow) exist only in PowerShell 7's PSReadLine 2.1+. Spawning 5.1 silently
dropped that feature; users read it as a foreman bug. Foreman itself draws
nothing here — the shell owns completion and predictions; foreman just
forwards keys (Tab is `\t`, src/input.rs) and renders the output.

## How it works

`preferred_powershell` (src/terminal.rs) is a pure function: PATH value +
existence probe in, exe name out. `Shell::program` wraps it with the real
PATH/filesystem and caches the answer in a `OnceLock` — one probe per app run.

## Gotchas

- Installing pwsh while foreman is running: restart foreman to pick it up.
- Foreman never sets PSReadLine options; the user's profile is in charge.
  No ghost text usually means the machine has no pwsh 7 or the profile set
  `-PredictionSource None`.
- The bare name `"pwsh.exe"` is returned (not a full path): CreateProcess
  resolves it through the same PATH the probe scanned.

## Key files

- `src/terminal.rs` — `Shell::program` (interface), `preferred_powershell`
  (the pure seam), `Session::spawn` (sole caller).
- `docs/superpowers/specs/2026-07-07-pwsh-preference-design.md` — decision
  history (rejected: forcing PSReadLine config, native suggestion overlay).
