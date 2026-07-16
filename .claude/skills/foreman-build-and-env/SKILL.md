---
name: foreman-build-and-env
description: Use when setting up a fresh Windows machine to build foreman, or when a build/test fails with "cannot find -lgcc_eh", "Access is denied (os error 5)", "error: no library targets found in package `foreman`", or link.exe/MSVC errors; when touching rustup/stable-gnu, w64devkit, libgcc_eh.a, PATH, the kill-foreman.ps1 or cargo-fmt.ps1 hooks; or when asked about the cargo build/run/test loop, per-module test filters, the test census, or the expected-warning baseline.
---

# foreman-build-and-env

How to recreate the build environment from nothing, run the build/test loop
without breaking a live fleet, and tell an expected warning from a new one.
Baseline: committed HEAD `7fda1c2` (as of 2026-07-01). Repo root:
`H:\claude code\foreman` — the path contains a space; **always quote it**.

Two terms used below, defined once:

- **MSVC vs GNU**: Rust on Windows ships two target flavors. `-msvc` links with
  Microsoft's Visual Studio linker; `-gnu` links with a MinGW GCC toolchain.
  This repo is **GNU-only** — no Visual Studio install, ever.
- **w64devkit**: a portable MinGW-w64 GCC distribution (single zip, no
  installer) from `github.com/skeeto/w64devkit`. It supplies `gcc`/`ld`/
  `dlltool`, which Rust's GNU target needs to link.

## Fresh-machine checklist (ordered — do not reorder)

1. **Install rustup** from `https://rustup.rs` (run `rustup-init.exe`, accept
   defaults). Observed working toolchain: rustc/cargo **1.96.0** (as of
   2026-07-01). The crate is `edition = "2024"` (`Cargo.toml:4`), so anything
   ≥ 1.85 compiles it, but stay current.

2. **Switch the default toolchain to GNU:**
   ```powershell
   rustup default stable-gnu
   rustup show active-toolchain   # must print: stable-x86_64-pc-windows-gnu (default)
   ```
   **Trap:** this default is MACHINE-GLOBAL. The repo has **no
   `rust-toolchain.toml`** (verified absent at HEAD), so a rustup reinstall or
   a stray `rustup default stable` silently reverts you to MSVC and linking
   breaks with `link.exe not found` / Visual Studio errors. Adding a
   `rust-toolchain.toml` is a candidate improvement, not done — route it
   through **foreman-change-control**.

3. **Install w64devkit at `C:\w64devkit`.** Download the release zip from
   `https://github.com/skeeto/w64devkit/releases` and extract so that
   `C:\w64devkit\bin\gcc.exe` exists. Observed version: GCC **16.1.0** (as of
   2026-07-01).

4. **Put `C:\w64devkit\bin` on PATH** (persist it, and set it for the current
   session — the per-session line matches `docs/HANDOFF.md:107`):
   ```powershell
   [Environment]::SetEnvironmentVariable('Path', "C:\w64devkit\bin;" + [Environment]::GetEnvironmentVariable('Path','User'), 'User')
   $env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
   (Get-Command gcc).Source   # must print: C:\w64devkit\bin\gcc.exe
   ```

5. **Create the `libgcc_eh` stub.** w64devkit GCC 16 folded exception-handling
   into `libgcc`, but Rust's GNU target still passes `-lgcc_eh` at link time.
   Without a stub, every link fails with `cannot find -lgcc_eh`. The fix is an
   empty 8-byte archive (verified present and 8 bytes on the working machine):
   ```powershell
   $gccver = (Get-ChildItem C:\w64devkit\lib\gcc\x86_64-w64-mingw32 -Directory)[0].Name  # 16.1.0 as of 2026-07-01
   ar crs "C:\w64devkit\lib\gcc\x86_64-w64-mingw32\$gccver\libgcc_eh.a"
   ```
   **Recreate this after ANY w64devkit reinstall or upgrade** — and the version
   directory drifts with the GCC version, so derive it as above rather than
   hardcoding `16.1.0`.

6. **Clone and build:**
   ```powershell
   git clone https://github.com/sniffle6/foreman.git
   Set-Location foreman
   cargo build 2>&1 | Select-Object -Last 20
   ```

7. **Optional — WSL for bash Sessions.** A Session set to bash spawns
   `wsl.exe` (`src/terminal.rs:164`), i.e. Windows Subsystem for Linux. On a
   fresh machine run `wsl --install` (needs the Virtual Machine Platform
   Windows feature and virtualization enabled in BIOS/UEFI), then reboot. A
   bash Session failing on a machine without WSL2 is machine setup, not an app
   bug.

## The build / verify loop

Kill the running app first or the link fails with `Access is denied (os error
5)` — Windows locks a running exe's file, and the linker cannot overwrite it.
**Kill by exe path, never by name** — only a `target\`-built instance holds
the lock; a by-name kill also takes down the user's *installed* foreman
(`%LOCALAPPDATA%\Programs\foreman`), which looks like a crash (incident:
2026-07-15). From the repo root:

```powershell
Get-Process foreman -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$PWD\target\*" } | Stop-Process -Force; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 20
cargo run              # debug
cargo run --release    # the "is it fast" build
cargo test             # unit tests — no GUI needed
```

**⚠ If `$env:FOREMAN` is `1`, you are running INSIDE the foreman app** — do
NOT `Stop-Process foreman`: you kill your own host, every other terminal, and
yourself mid-command (incident: 2026-07-09). Ask the user to close foreman, or
build without touching the running exe: `cargo build --target-dir target/agent`.

Binaries land at `target\debug\foreman.exe` and `target\release\foreman.exe`.
The GUI cannot be seen from a terminal — to verify visually, use the
**build-screenshot** skill; for headless send/Snapshot verification and
measurement, see **foreman-diagnostics-and-tooling**. What counts as evidence
is defined in **foreman-validation-and-qa**.

### Which kill is automated (and which is not)

A Claude Code PreToolUse hook (`.claude/settings.json` →
`.claude/hooks/kill-foreman.ps1`) auto-kills foreman, but **only** for
Bash-tool commands matching the regex `cargo\s+(build|run|test)`. It always
exits 0 (never blocks the command). Since 2026-07-15 it kills **by exe path**
— only instances running from this repo's `target\` dir — so an installed
foreman (`%LOCALAPPDATA%\Programs\foreman`) survives builds.

| How you run cargo                              | Kill automated?                          |
| ---------------------------------------------- | ---------------------------------------- |
| Claude Code **Bash tool**, `cargo build/run/test` | Yes — PreToolUse hook kills first     |
| Claude Code **PowerShell tool**                | No — prepend the path-filtered kill line |
| Your own terminal, scripts, CI-like automation | No — prepend the path-filtered kill line |

**Fleet warning:** the hook (and the manual line) takes down **every**
`foreman.exe` under `target\` — debug and release alike. If a live release
fleet built from this repo is running agents you care about, do NOT run cargo
through the Bash tool.

### Never kill a live release fleet just to run tests

Debug builds and tests do not collide with a running **release** exe:

- `cargo build` (debug) links `target\debug\foreman.exe` — a different file
  from the running `target\release\foreman.exe`. No lock conflict.
- `cargo test` links hashed test binaries under `target\debug\deps\`
  (`foreman-<hash>.exe`, verified present); it does not relink either normal exe.

So while a release fleet is live: run `cargo test` (or a debug `cargo build`)
via the **PowerShell tool** or your own terminal, WITHOUT the kill line. The
collision only exists when relinking the exact exe that is currently running.

### The formatting hook

A PostToolUse hook (`.claude/hooks/cargo-fmt.ps1`) runs `cargo fmt` after
every Claude Edit/Write of a `.rs` file (gated on the `\.rs$` extension;
always exits 0). Manual `cargo fmt` is therefore usually redundant inside
Claude sessions — but NOT for edits made outside them. The full configuration
inventory lives in **foreman-config-and-flags**.

## cargo test — ground truth

- All tests are in-crate `#[cfg(test)]` modules; there is **no `tests/`
  directory** (verified) and this is a **bin-only crate** — the sole cargo
  target is `bin foreman` at `src/main.rs` (no `src/lib.rs`, no `[lib]`).
- Census (as of 2026-07-01, HEAD `7fda1c2`): **353 `#[test]` fns across 16
  files in `src/`**. Re-derive, don't trust:
  ```powershell
  (Select-String -Path "H:\claude code\foreman\src\*.rs" -Pattern '#\[test\]').Count
  ```
- Pass state: not re-run while writing this skill (shared `target\` lock).
  Earlier the same day, pre-commit, the census was 344 green + 9 intentionally
  red `src/frame.rs` TDD tests; `7fda1c2` then landed the frame-plan
  implementation, so expect all-green at HEAD — confirm with `cargo test`.
- Per-module runs use a **name filter on the bin target**:
  ```powershell
  cargo test layout::    # src/layout.rs tests
  cargo test wm::        # src/wm.rs tests
  cargo test chat::      # src/chat.rs tests
  ```
- **DOC DRIFT (flagged):** `CLAUDE.md` says `cargo test --lib layout`. That
  fails — verified by running it: ``error: no library targets found in package
  `foreman` `` (exit 101), because no library target exists. Use the filter
  form above. Also stale: `docs/followups-latency-and-control.md:91` says
  "181 tests" — the count is 353 fns now.

## Warning baseline (as of 2026-07-01, HEAD `7fda1c2`)

Expected warnings — anything NOT in this table is yours to fix:

| Warning                          | Where                          | Why it's expected                                                                 |
| -------------------------------- | ------------------------------ | --------------------------------------------------------------------------------- |
| deprecated `Context::screen_rect` | `src/main.rs:87`              | egui 0.34.3 deprecates it ("split into viewport_rect() and content_rect()"). Fix candidate: `content_rect()`; don't drive-by fix — see foreman-change-control. |
| dead_code `cwd`/`query`/`selected` | `src/dirpicker.rs:40,44,48`  | False positive — see below                                                         |
| dead_code `grid_contains`        | `src/inspect.rs:112`           | False positive — see below                                                         |
| dead_code `is_empty` / `leaves`  | `src/layout.rs:54,69`          | False positive — see below                                                         |

**Why the dead_code ones are false positives:** they are `pub` fns in a
*binary* crate whose only callers are `#[cfg(test)]` — real component
accessors used by the test suite. `docs/followups-latency-and-control.md:79-84`
records the decision: **do not delete them.**

**DOC DRIFT (flagged):** that same doc's list names `ready`, `post`, and
`chat_post` as warners and `shell` as a write-only field. All four have since
gone live or been gated: `ready()` has a production caller (`src/wm.rs:1588`),
`ChatLog::post` is now `#[cfg(test)]` (`src/chat.rs:143`), `chat_post` has a
production caller (`src/wm.rs:1048`), and `shell` is read at
`src/terminal.rs:461`. The table above is the verified current set (derived by
caller inspection; re-verify with a build):

```powershell
cargo build 2>&1 | Select-String -Pattern 'warning'
```

## Build-time error → fix

Runtime symptoms (black Session, dead input, etc.) belong to
**foreman-debugging-playbook**; this table is build-environment only.

| Error text                                             | Cause                                        | Fix                                                     |
| ------------------------------------------------------ | -------------------------------------------- | ------------------------------------------------------- |
| `cannot find -lgcc_eh`                                 | libgcc_eh stub missing (fresh/reinstalled w64devkit) | Checklist step 5 (`ar crs …\libgcc_eh.a`)        |
| `Access is denied (os error 5)`                        | Linking over a running `foreman.exe`         | The `Stop-Process` kill line, then rebuild              |
| `link.exe not found` / Visual Studio component errors  | rustup reverted to MSVC default              | `rustup default stable-gnu`                             |
| `dlltool` / `gcc` not found                            | `C:\w64devkit\bin` not on PATH               | Checklist step 4                                        |
| ``error: no library targets found in package `foreman` `` | `cargo test --lib …` (stale CLAUDE.md form) | `cargo test <module>::` filter                       |

## Toolchain and pinned versions (as of 2026-07-01)

rustc/cargo **1.96.0**, `stable-x86_64-pc-windows-gnu` (default); w64devkit
GCC **16.1.0**; edition **2024**. From `Cargo.lock`: eframe/egui **0.34.3**,
alacritty_terminal **0.26.0**, portable-pty **0.9.0**, interprocess **2.4.2**,
arboard **3.6.1**, resvg **0.45.1**, sysinfo **0.33.1**, winit **0.30.13**.
Re-verify:

```powershell
rustc --version; rustup show active-toolchain; gcc --version | Select-Object -First 1
Select-String -Path "H:\claude code\foreman\Cargo.lock" -Pattern '^name = "(eframe|egui|alacritty_terminal|portable-pty|interprocess|arboard|resvg|sysinfo|winit)"' -Context 0,1 | ForEach-Object { $_.Line + ' ' + $_.Context.PostContext[0] }
```

## There is no CI

No `.github/workflows`, no CI config of any kind is tracked (verified at
HEAD). Local `cargo test`, the two hooks above, and review are the entire
pipeline. Nothing will catch what you don't run yourself.

## When NOT to use this skill

- **Visually verifying a change** (build + screenshot + Read the PNG) →
  **build-screenshot** skill.
- **Running the app or driving the Control plane CLI** (verbs, flags,
  transport, timeouts) → **foreman-run-and-operate**.
- **Measuring behavior** (headless send/Snapshot, latency harness) →
  **foreman-diagnostics-and-tooling**.
- **Runtime failures** (black Session, resize corruption, dead input) →
  **foreman-debugging-playbook**; terminal-domain concepts (PTY, ConPTY, VT) →
  **terminal-emulation-reference**.
- **Evidence/acceptance standards and test conventions** →
  **foreman-validation-and-qa**.
- **Config files, env vars, tunables** → **foreman-config-and-flags**.
- **Fixing baseline warnings or pinning the toolchain per-repo** — classify
  through **foreman-change-control** first.

## Provenance and maintenance

Written 2026-07-01 against HEAD `7fda1c2`; every command and path above was
run or file-verified on the working machine that day. Drift-prone claims and
their re-verification one-liners:

| Claim                                   | Re-verify with                                                                 |
| --------------------------------------- | ------------------------------------------------------------------------------ |
| Toolchain 1.96.0 / stable-gnu default   | `rustc --version; rustup show active-toolchain`                                |
| No `rust-toolchain.toml`                | `Test-Path "H:\claude code\foreman\rust-toolchain.toml"` (expect False)        |
| GCC version dir `16.1.0` + 8-byte stub  | `Get-ChildItem C:\w64devkit\lib\gcc\x86_64-w64-mingw32; (Get-Item C:\w64devkit\lib\gcc\x86_64-w64-mingw32\*\libgcc_eh.a).Length` |
| Hook behavior (matchers, regex, exit 0) | Read `.claude/settings.json`, `.claude/hooks/kill-foreman.ps1`, `.claude/hooks/cargo-fmt.ps1` |
| Test census 353 / bin-only crate        | census one-liner above; `cargo metadata --no-deps --format-version 1 \| ConvertFrom-Json \| % { $_.packages[0].targets.kind }` |
| Warning baseline (4 rows)               | `cargo build 2>&1 \| Select-String warning`                                    |
| Pinned dep versions                     | Cargo.lock one-liner above                                                     |
| `Shell::Bash` → `wsl.exe`               | `Select-String -Path "H:\claude code\foreman\src\terminal.rs" -Pattern 'wsl.exe'` |
| No CI                                   | `git -C "H:\claude code\foreman" ls-files \| Select-String 'workflows'` (expect nothing) |
