# Spec: prefer pwsh.exe for PowerShell Sessions

Date: 2026-07-07. Status: approved by user, not yet built.

## Problem

A PowerShell Session in foreman has no inline ghost-text predictions (the gray
suggested command you accept with `→`). Windows Terminal on the same machine
has them. Users read this as a missing foreman feature.

It is not an emulator gap. Predictions are **PSReadLine Predictive
IntelliSense**, a feature of PowerShell 7's PSReadLine (2.1+). Foreman spawns
`powershell.exe` — Windows PowerShell 5.1, which ships PSReadLine 2.0 and has
no predictions at all. Everything else already works through the existing
render/input path:

- Tab file completion works today (`src/input.rs:333` forwards Tab as `\t`;
  plain Tab is only a chord under the Leader, so the shell receives it).
- Codex/Claude CLIs draw their own ghost text and render fine in a Session.

So the fix is: spawn the right shell.

## Design

`Shell::PowerShell` resolves to `pwsh.exe` when PowerShell 7 is installed,
falling back to `powershell.exe` when it is not.

### Module and interface

The module is **shell program resolution**, and its interface is the existing
`Shell::program(self) -> &'static str` in `src/terminal.rs`. The interface does
not change — no new variant, no new parameter, no new caller obligation. The
pwsh preference is added depth behind a surface callers already know. The one
caller (`Session::spawn`, which feeds `CommandBuilder::new`) is untouched.

### Internal seam

The decision "which PowerShell?" is a pure function, private to the module,
so tests never touch the real filesystem or environment:

```rust
/// Pure: pick the PowerShell binary given the PATH value and an existence probe.
fn preferred_powershell(
    path: Option<&OsStr>,
    exists: &dyn Fn(&Path) -> bool,
) -> &'static str
```

- Scans `env::split_paths(path)` for a directory where `exists(dir/pwsh.exe)`.
- Found → `"pwsh.exe"`. Not found (or no PATH) → `"powershell.exe"`.

`Shell::program` wraps it with the real adapters (`env::var_os("PATH")`,
`Path::is_file`) and caches the answer in a `OnceLock<&'static str>` — one
filesystem probe per app run, not per spawn.

Returning the bare name `"pwsh.exe"` (not the full found path) is deliberate:
`CreateProcess` resolves it through the same PATH we scanned, and the return
type stays `&'static str` so the interface is provably unchanged.

### What does not change

- `Shell::Cmd`, `Shell::Bash`: untouched.
- `Shell::label()` stays `"powershell"` — cosmetic only; nothing keys off it.
- Dispatched Sessions (`foreman open -- <argv>`): spawn their own argv, not
  `Shell::program`.
- Agent detection (`src/proc.rs`): walks PIDs from the Session's shell PID,
  never exe names, so pwsh vs powershell is invisible to it.
- The DSR/Ready handshake: pwsh emits the same startup DSR as 5.1; the
  existing Listener path handles it. **Never `VoidListener`** still holds.
- No PSReadLine configuration is injected. Predictions default on in
  PSReadLine 2.4.x interactive sessions and the user's profile stays in
  charge — same contract as Windows Terminal.

## Decision history (settled with the user)

- **A. Prefer pwsh.exe when installed** — **accepted.** Smallest change that
  yields the wanted behavior; honors user shell config.
- **B. A + force `Set-PSReadLineOption -PredictionSource HistoryAndPlugin` at
  startup** — **rejected.** Hijacks the user's shell profile and adds a startup
  command flash; pwsh defaults predictions on anyway.
- **C. Foreman-native suggestion overlay (Warp-style)** — **rejected.** A large
  subsystem (own history store, OSC 133 shell integration, overlay paint,
  injection on accept) that duplicates PSReadLine and fights the line editors
  of Codex/Claude CLIs. Nothing today varies across that seam; it would be a
  hypothetical seam with one adapter.
- **New `Shell::Pwsh` variant** — **rejected.** Widens the interface (every
  match on `Shell` grows an arm) for a distinction no caller needs to make.
- **Accepted tradeoff:** the PATH probe is cached once per run, so installing
  pwsh while foreman is running takes a restart to notice. Fine.
- **Accepted tradeoff:** machines without pwsh 7 stay on 5.1 with no
  predictions — correct, since the feature does not exist there.

## Testing

Unit tests on the pure seam (`preferred_powershell` with a fake PATH string
and a closure probe — the test is the second adapter):

1. pwsh present in a PATH dir → `"pwsh.exe"`.
2. pwsh absent everywhere → `"powershell.exe"`.
3. No PATH at all → `"powershell.exe"`.
4. pwsh present only in a later PATH dir → still `"pwsh.exe"` (the whole
   PATH is scanned, not just the first entry).

Evidence loop for the feature itself (per the build/verify loop): build, run,
open a PowerShell Session, type a prefix of a command in history, screenshot
the ghost text; confirm `→` accepts it and Tab still menu-completes.

## Key files

- `src/terminal.rs` — `Shell::program` (the interface), new
  `preferred_powershell` (the internal seam), `Session::spawn` (sole caller).
- `src/input.rs` — Tab encoding (context only; unchanged).
- `src/proc.rs` — PID-based agent detection (context only; unchanged).
