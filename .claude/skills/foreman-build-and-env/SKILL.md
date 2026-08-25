---
name: foreman-build-and-env
description: Use when setting up a fresh Windows machine to build foreman, or when a build/test fails with "cannot find -lgcc_eh", "Access is denied (os error 5)", "error: no library targets found in package `foreman`", or link.exe/MSVC errors; when touching rustup/stable-gnu, w64devkit, libgcc_eh.a, PATH, the kill-foreman.ps1 or cargo-fmt.ps1 hooks; or when asked about the cargo build/run/test loop, per-module test filters, what CI does and does not gate, or the expected-warning baseline.
---

# foreman-build-and-env

How to recreate the build environment from nothing, run the build/test loop
without breaking a live fleet, and tell an expected warning from a new one.
Repo root: `H:\claude code\foreman` — the path contains a space; **always
quote it**.

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
   2026-07-01). The crate is `edition = "2024"` (`Cargo.toml`), so anything
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
   session — the per-session line matches the one in `docs/HANDOFF.md`):
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
   `wsl.exe` (`src/terminal.rs`, the `Shell::Bash` arm), i.e. Windows
   Subsystem for Linux. On a
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
- **A census is a query, not a fact — never write one down.** Test totals and
  per-module counts rot within weeks, and a stale count read as current is
  worse than no count at all. Derive it when you need it:
  ```
  rg -c '#\[test\]' src/ | sort -t: -k2 -rn
  ```
  The same rule applies to every enumeration in this repo's docs: field lists,
  "N verbs", "N modules". Write the command, or write nothing.
- Expect all-green unless a commit message declares red WIP — confirm with
  `cargo test`, and see **foreman-validation-and-qa** for the flaky-test policy.
- Per-module runs use a **name filter on the bin target**:
  ```powershell
  cargo test layout::    # src/layout.rs tests
  cargo test wm::        # src/wm.rs tests
  cargo test chat::      # src/chat.rs tests
  ```
- **`--lib` never works here** — `foreman` is bin-only, so `cargo test --lib
  layout` fails with ``error: no library targets found in package `foreman` ``
  (exit 101). Use the filter form above, or `cargo test --bin foreman <filter>`.
  CLAUDE.md carried this bad form until 2026-08-24 and seeded copies of it
  across the repo; fixed at the same time.

## Warning baseline

Derive the current set — do not trust a written list:

```powershell
cargo build 2>&1 | Select-String -Pattern 'warning'
```

These families are expected; anything else is yours to fix.

**Deprecated `Context::screen_rect`** (`src/main.rs`, in `show_os_chrome`).
egui 0.34.3 deprecates it ("split into
viewport_rect() and content_rect()"). Fix candidate: `content_rect()`; don't
drive-by fix — see **foreman-change-control**.

**`dead_code` on `pub` fns whose only callers are `#[cfg(test)]`.** In a
*binary* crate rustc does not count a test-only caller as a use, so real
component accessors used by the test suite warn. **Do not delete them** —
`docs/followups-latency-and-control.md` records that decision. Examples that
have held: `LayoutTree::is_empty` (`src/layout.rs`) and `inspect::grid_contains`
(`src/inspect.rs`).

**Do not treat that membership as a list.** It churns silently in both
directions: a fn gains a production caller and stops warning (`LayoutTree::leaves`
now has one in `src/wm.rs`; so do `Session::ready` and `chat_post`), or it gets
`#[cfg(test)]`-gated and stops warning (`ChatLog::post`, the `dirpicker`
accessors). A stale "these are false positives" list is exactly how a REAL
dead-code warning gets waved through — before calling any specific warning
expected, check that symbol's caller set with `rg` yourself.

## Build-time error → fix

Runtime symptoms (black Session, dead input, etc.) belong to
**foreman-debugging-playbook**; this table is build-environment only.

| Error text                                             | Cause                                        | Fix                                                     |
| ------------------------------------------------------ | -------------------------------------------- | ------------------------------------------------------- |
| `cannot find -lgcc_eh`                                 | libgcc_eh stub missing (fresh/reinstalled w64devkit) | Checklist step 5 (`ar crs …\libgcc_eh.a`)        |
| `Access is denied (os error 5)`                        | Linking over a running `foreman.exe`         | The `Stop-Process` kill line, then rebuild              |
| `link.exe not found` / Visual Studio component errors  | rustup reverted to MSVC default              | `rustup default stable-gnu`                             |
| `dlltool` / `gcc` not found                            | `C:\w64devkit\bin` not on PATH               | Checklist step 4                                        |
| ``error: no library targets found in package `foreman` `` | `cargo test --lib …` — bin-only crate           | `cargo test <module>::` filter                       |

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

## What CI gates (and what it does not)

CI is release-gated only — `.github/workflows/release.yml` runs `cargo test` +
`cargo build --release` on a `v*` tag push (it also installs the GNU toolchain
to match local builds, and refuses to release if the tag and the `Cargo.toml`
version disagree). It is the repo's **only** workflow, and its other two
triggers do not widen that: `pull_request` is `paths`-restricted to
`.github/workflows/release.yml` and `install.ps1`, plus manual
`workflow_dispatch`. So **ordinary commits and ordinary PRs are entirely
ungated** — nothing runs on them — and local `cargo test` remains the real
correctness gate. But a knowingly-red WIP commit on main WILL fail the next
release tag. Do not park a broken test on main and assume nothing will notice.

Re-derive the trigger set rather than trusting this paragraph:

```powershell
Get-Content .github/workflows/release.yml -TotalCount 12
```

## When NOT to use this skill

- **Runtime failures** (black Session, resize corruption, dead input) →
  **foreman-debugging-playbook**. This skill is build-environment only.
- **Test conventions and what counts as evidence** (as opposed to how to run
  the build) → **foreman-validation-and-qa**.

## Provenance and maintenance

Machine-specific claims that are both load-bearing and volatile, with the
command that settles each:

| Claim                                   | Re-verify with                                                                 |
| --------------------------------------- | ------------------------------------------------------------------------------ |
| Toolchain 1.96.0 / stable-gnu default   | `rustc --version; rustup show active-toolchain`                                |
| GCC version dir + 8-byte `libgcc_eh` stub | `Get-ChildItem C:\w64devkit\lib\gcc\x86_64-w64-mingw32; (Get-Item C:\w64devkit\lib\gcc\x86_64-w64-mingw32\*\libgcc_eh.a).Length` |
| Hook behavior (matchers, regex, exit 0) | Read `.claude/settings.json`, `.claude/hooks/kill-foreman.ps1`, `.claude/hooks/cargo-fmt.ps1` |
| Pinned dep versions                     | Cargo.lock one-liner above                                                     |
