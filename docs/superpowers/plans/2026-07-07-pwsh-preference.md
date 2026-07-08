# pwsh.exe Preference Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** PowerShell Sessions spawn `pwsh.exe` (PowerShell 7, with PSReadLine ghost-text predictions) when it is on PATH, falling back to `powershell.exe`.

**Architecture:** The existing `Shell::program(self) -> &'static str` interface in `src/terminal.rs` is unchanged. A new pure private function `preferred_powershell(path, exists)` decides which binary; `Shell::program` wraps it with the real PATH/filesystem adapters and caches the answer in a `OnceLock`. Spec: `docs/superpowers/specs/2026-07-07-pwsh-preference-design.md`.

**Tech Stack:** Rust, std only (`env::split_paths`, `OnceLock`). No new dependencies.

## Global Constraints

- Windows, GNU toolchain. Before any `cargo build`/`cargo run`, kill the app or the link fails: `Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500` (PowerShell) — see `CLAUDE.md` build loop.
- `Shell::program` keeps its exact signature `fn program(self) -> &'static str`. No new `Shell` variant. `Shell::label()` unchanged.
- Do NOT inject any PSReadLine configuration into the spawned shell.
- Commit messages: `type(scope): subject`, body says why, trailer `Co-Authored-By: Claude <model> <noreply@anthropic.com>`. Write the message with `git commit -F -` and a heredoc (never paste PowerShell `@'...'@` here-strings into Git Bash). Verify with `git log -1 --format=%B` after committing.
- Stage files by name, never `git add -A` (an unrelated untracked plan file exists in the repo).

---

### Task 1: Pure resolver `preferred_powershell` + unit tests

**Files:**
- Modify: `src/terminal.rs` (new private fn near `impl Shell`, ~line 140; new tests in the existing `#[cfg(test)] mod tests` at ~line 1340)

**Interfaces:**
- Consumes: nothing from other tasks.
- Produces: `fn preferred_powershell(path: Option<&std::ffi::OsStr>, exists: &dyn Fn(&std::path::Path) -> bool) -> &'static str` — private to `terminal.rs`; Task 2 calls it from `Shell::program`.

- [x] **Step 1: Write the failing tests**

In `src/terminal.rs`, inside the existing `#[cfg(test)] mod tests` block (it already has `use super::*;`), add:

```rust
    #[test]
    fn preferred_powershell_finds_pwsh_in_first_path_dir() {
        let path = std::env::join_paths(["C:\\one", "C:\\two"]).unwrap();
        let got = preferred_powershell(Some(path.as_os_str()), &|p| {
            p == Path::new("C:\\one\\pwsh.exe")
        });
        assert_eq!(got, "pwsh.exe");
    }

    #[test]
    fn preferred_powershell_finds_pwsh_in_later_path_dir() {
        // pwsh only in the SECOND dir: the whole PATH is scanned.
        let path = std::env::join_paths(["C:\\one", "C:\\two"]).unwrap();
        let got = preferred_powershell(Some(path.as_os_str()), &|p| {
            p == Path::new("C:\\two\\pwsh.exe")
        });
        assert_eq!(got, "pwsh.exe");
    }

    #[test]
    fn preferred_powershell_falls_back_when_pwsh_absent() {
        let path = std::env::join_paths(["C:\\one", "C:\\two"]).unwrap();
        assert_eq!(
            preferred_powershell(Some(path.as_os_str()), &|_| false),
            "powershell.exe"
        );
    }

    #[test]
    fn preferred_powershell_falls_back_without_path_var() {
        // No PATH at all: never probe, just fall back.
        assert_eq!(preferred_powershell(None, &|_| true), "powershell.exe");
    }
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test preferred_powershell 2>&1 | tail -20`
Expected: compile error — `cannot find function preferred_powershell in this scope`.

- [x] **Step 3: Write the implementation**

In `src/terminal.rs`, insert immediately BEFORE `impl Shell {` (~line 140):

```rust
/// Pure: pick the PowerShell binary given a PATH value and an existence probe.
/// PowerShell 7 (`pwsh.exe`) ships PSReadLine 2.1+ with inline predictions;
/// Windows PowerShell 5.1 (`powershell.exe`) does not, so prefer pwsh when
/// installed. Returns the bare exe name — CreateProcess resolves it through
/// the same PATH this scanned.
fn preferred_powershell(
    path: Option<&std::ffi::OsStr>,
    exists: &dyn Fn(&Path) -> bool,
) -> &'static str {
    let Some(path) = path else {
        return "powershell.exe";
    };
    if std::env::split_paths(path).any(|dir| exists(&dir.join("pwsh.exe"))) {
        "pwsh.exe"
    } else {
        "powershell.exe"
    }
}
```

Note: `Path` is already imported at the top of `terminal.rs` (`use std::path::Path;`); `std::ffi::OsStr` and `std::env` are referenced by full path so no import changes are needed in this task.

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test preferred_powershell 2>&1 | tail -10`
Expected: `test result: ok. 4 passed` (the four new tests). A dead-code warning for the unused function is acceptable until Task 2 wires it in — do not add `#[allow(dead_code)]`.

- [x] **Step 5: Commit**

```bash
git add src/terminal.rs
git commit -F - <<'EOF'
feat(terminal): pure preferred_powershell resolver

Pure seam (PATH value + existence probe in, exe name out) so tests never
touch the real filesystem. Wired into Shell::program in the next commit.

Co-Authored-By: Claude <model> <noreply@anthropic.com>
EOF
git log -1 --format=%B
```

(Replace `<model>` with your actual model name. Verify the printed message is intact — no stray `@`.)

---

### Task 2: Wire into `Shell::program` with a cached probe + verify in the running app

**Files:**
- Modify: `src/terminal.rs:141-147` (`Shell::program`)
- Create: `docs/shell-selection.md` (feature doc)

**Interfaces:**
- Consumes: `preferred_powershell(Option<&OsStr>, &dyn Fn(&Path) -> bool) -> &'static str` from Task 1.
- Produces: `Shell::program(self) -> &'static str` (unchanged signature) now returns `"pwsh.exe"` on machines where pwsh is on PATH. Sole caller `Session::spawn` (src/terminal.rs:496) needs no change.

- [x] **Step 1: Replace the `Shell::program` body**

Current code (src/terminal.rs:141-147):

```rust
    fn program(self) -> &'static str {
        match self {
            Shell::Cmd => "cmd.exe",
            Shell::PowerShell => "powershell.exe",
            Shell::Bash => "wsl.exe",
        }
    }
```

Replace with:

```rust
    fn program(self) -> &'static str {
        match self {
            Shell::Cmd => "cmd.exe",
            Shell::PowerShell => {
                // Probe PATH once per run; installing pwsh mid-run needs a restart.
                static PWSH: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
                *PWSH.get_or_init(|| {
                    preferred_powershell(std::env::var_os("PATH").as_deref(), &|p| p.is_file())
                })
            }
            Shell::Bash => "wsl.exe",
        }
    }
```

- [x] **Step 2: Run the full test suite**

Run: `cargo test 2>&1 | tail -5`
Expected: all tests green, zero failures (PTY-backed terminal tests included — `Session::spawn(Shell::PowerShell, ...)` now spawns pwsh where installed; the DSR/Ready handshake is identical).

- [x] **Step 3: Build and verify in the running app**

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 5
```

Expected: clean build (baseline warnings only). Then launch `target/debug/foreman.exe`, open a project with a PowerShell Session, and verify with evidence (screenshot per `docs/HANDOFF.md` § 3, and `Read` the PNG):

1. The pane shows a pwsh 7 banner/prompt (not "Windows PowerShell 5.1").
2. Type a prefix of a command in your history → gray ghost text appears.
3. Press `→` at end of line → the suggestion is accepted into the input.
4. Type a partial filename and press Tab → menu completion still works.

If pwsh predictions don't render, STOP and debug before committing — do not claim success without the screenshot evidence.

- [x] **Step 4: Write the feature doc**

Create `docs/shell-selection.md`:

```markdown
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
```

- [x] **Step 5: Commit**

```bash
git add src/terminal.rs docs/shell-selection.md
git commit -F - <<'EOF'
feat(terminal): prefer pwsh.exe for PowerShell Sessions

PSReadLine inline predictions (ghost text, right-arrow accept) only exist in
PowerShell 7; spawning powershell.exe 5.1 silently dropped them. Probe PATH
once (OnceLock) via the pure preferred_powershell seam; fall back to 5.1
where pwsh is absent. Interface Shell::program unchanged; label, cmd, wsl,
dispatch, and PID-based agent detection untouched.

Co-Authored-By: Claude <model> <noreply@anthropic.com>
EOF
git log -1 --format=%B
```

(Replace `<model>` with your actual model name. Verify the printed message is intact.)
