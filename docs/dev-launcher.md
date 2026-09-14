# Dev launcher (`scripts/run-dev.ps1`)

Builds a foreman dev build and runs it **next to** the foreman you already have
open, without the two instances fighting each other.

```powershell
.\scripts\run-dev.ps1          # release build of this repo, launched sandboxed
.\scripts\run-dev.ps1 -Kill    # stop it again
```

## Why it exists

Running a second foreman by hand has three ways to hurt you. All three have
already happened in this repo, so the script does them right by default.

**1. The build fights the running exe.** Building into `target\` fails to link
with `Access is denied (os error 5)` while any target-built foreman holds the
file. The script always builds into `target\agent\`, which your installed or
daily instance never locks.

**2. The dev instance eats your workspace.** foreman reads *and writes*
`%APPDATA%\foreman\workspace.json`. A second instance restores your real project
layout on startup and then debounce-writes back over it — so a dev build you left
open for a minute can quietly replace the layout your real instance owns.
`config_dir()` (`src/config.rs`) resolves that path from the `APPDATA`
environment variable, so the script points `APPDATA` at a throwaway sandbox
under `target\agent\appdata`. The dev instance gets its own settings, themes,
keybindings and workspace, and cannot see or touch yours.

**3. Killing "foreman" kills the wrong foreman.** Stopping the process *by name*
also kills the user's installed foreman (incident 2026-07-15 — it looked like a
crash), and from inside a foreman terminal it kills your own host, every other
terminal in it, and your own session mid-command (incident 2026-07-09).
`-Kill` matches on the **exe path** only, and additionally walks up the parent
chain from the calling shell and refuses to kill any process that turns out to
be one of its own ancestors.

**4. A shared target dir serves the wrong worktree's binary.** Point two
worktrees at one `--target-dir` and cargo can run the *other* one's build. This
is not theoretical — it happened while writing this script. Worktree A (a perf
branch) was built and tested into the shared dir; then `cargo test` in worktree
B, whose source contains no such code, reported `real_grid_scroll_…` passing and
a test count belonging to A. A green suite was read as verification of B while
describing A entirely.

That failure is near-silent: no error, no warning, just a plausible pass. So the
script keys the target dir per source dir (`target\agent\build\<leaf>-<hash>`)
and pays the extra dependency build. If you invoke cargo by hand across
worktrees, give each one its own `--target-dir`, and treat any test count that
does not match the source in front of you as a stale binary until proven
otherwise.

## Using it

| Flag | What it does |
| --- | --- |
| *(none)* | Release build of this repo, sandboxed config, launched |
| `-Path <dir>` | Build some other source dir — a git worktree, say |
| `-Debug` | Debug profile instead of release |
| `-Fresh` | Wipe the sandbox first — a virgin config, as a new user sees it |
| `-SeedWorkspace` | Copy your real `workspace.json` in, so it opens your actual projects |
| `-NoSeed` | Don't copy settings/keybindings/themes either — fully stock |
| `-List` | Show every running foreman, labelled by exe path |
| `-Kill` | Stop dev instances and exit. No build, no launch |

By default it seeds `settings.json`, `keybindings.json` and `themes\` from your
real config so the dev build *looks* like yours, but leaves `workspace.json` out
so it starts empty and light. `-SeedWorkspace` adds it — still as a sandboxed
copy that is never written back.

## Gotchas

**Release is the default, on purpose.** Debug Rust is slow enough to mislead you
about anything performance-shaped — scroll smoothness, paint cost, input latency.
Reach for `-Debug` when you want a panic backtrace, not when you want a verdict
on speed.

**The well-known pipe belongs to whichever instance bound it first.**
`src/control.rs` `PIPE` is `\\.\pipe\foreman`; the first-launched foreman owns
it and later instances fail that bind. Terminals spawned by an instance get
`FOREMAN_PIPE` pointing at *that* instance's own pipe (`instance_pipe`), so
`foreman open` / `chat` / `send` / `snapshot` from inside a dev-build terminal
reach the dev host. Callers outside any foreman terminal — or a pre-v0.4.0
CLI that does not read `FOREMAN_PIPE` — still talk to the well-known name
(usually the installed daily driver). GUI behaviour tests fine either way.

**The sandbox lives under `target\`,** so `cargo clean` deletes it. That is
usually what you want, but don't park anything you care about in there.

**Each source dir gets its own target dir,** under
`target\agent\build\<leaf>-<hash>`. Sharing one target dir across worktrees is
tempting — it would save a dependency build per worktree — but see hazard 4: it
buys you a build cache and a way to read someone else's test results.

**It does not check whether you are inside foreman.** It doesn't need to: it
never kills by name, and the ancestor guard covers the case where the host
*is* a dev build. Contrast `.claude/hooks/kill-foreman.ps1`, which bails out
entirely when `FOREMAN=1` because it kills without knowing the caller's intent.

## Key files

- `scripts/run-dev.ps1` — the script; comment-based help at the top (`Get-Help`)
- `src/config.rs` — `config_dir()`, which reads `APPDATA`; the isolation hinges on it
- `src/workspace.rs` — the `workspace.json` load/save that hazard 2 is about
- `src/main.rs` — captures the live tree and debounce-writes the workspace
- `src/control.rs` — `PIPE` (well-known) and `instance_pipe` / `FOREMAN_PIPE`
  (per-instance; what in-foreman CLIs use)
- `.claude/hooks/kill-foreman.ps1` — the kill-safety precedent, and both incidents
- `docs/HANDOFF.md` § 3 — the plain build/verify loop this script wraps
