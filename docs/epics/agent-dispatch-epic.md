# Agent Dispatch — Epic

Status: **built** (2026-06-10). v1 fire-and-watch is implemented and E2E-verified.

## What it does

A CLI session running inside a foreman terminal — Claude Code, codex, any
agent CLI — can open new, visible terminals in foreman. So an AI agent you're
talking to can dispatch worker agents that show up as real windows you can
watch, type into, and keep.

The v1 contract is **fire-and-watch**: the dispatcher spawns a worker terminal
and moves on; the human supervises it visually. No automatic result return
yet, but the protocol is shaped so round-trip (results flowing back to the
dispatcher) bolts on later without breaking changes.

## Why it exists

Foreman's whole point is "tmux built for AI" — running many agents visibly.
Claude Code's agent teams was researched first (2026-06) and can't be the
mechanism: its multi-terminal display is hard-coded to tmux/iTerm2 split
panes, has no pluggable spawn hook, and doesn't support split panes on
Windows at all. So foreman needs its own mechanism — and making it generic
means it works with any model or CLI, not just Claude.

## How it works

### 1. Control pipe (foreman side)

Foreman listens on the named pipe `\\.\pipe\foreman` from a background
thread. One JSON request per connection; the request is pushed over an
`mpsc` channel that the egui `App` drains each frame (the app already
repaints every 16ms, so latency is one frame).

v1 protocol — one verb:

```json
{"cmd":"open", "project":"p1"|null, "cwd":"H:\\repo", "title":"agent · fix-tests",
 "command":["claude","-p","<task>"]}
```

Reply:

```json
{"ok":true, "terminal":"t3", "project":"p1"}
{"ok":false, "error":"no such project: p9"}
```

`project: null` means "the focused project". Returning IDs now is what makes
round-trip possible later (`status` / `wait` verbs keyed by those IDs).

### 2. Client mode (same exe)

`foreman open [--project P] [--title T] [--cwd D] -- <command...>`

When argv has a subcommand, foreman runs as a thin pipe client and exits —
no GUI. Nothing extra to install; the dispatching agent just runs `foreman
open`.

### 3. Env injection

Every PTY foreman spawns gets:

- `FOREMAN=1` — "you are inside foreman"
- `FOREMAN_PROJECT_ID` — the project this terminal lives in
- `FOREMAN_TERMINAL_ID` — this terminal's id (kept from day one so the env
  contract is stable when round-trip arrives)
- `FOREMAN_EXE` — full path to the running foreman exe, so clients can invoke
  `foreman open` without any PATH setup

The client defaults `--project` from `FOREMAN_PROJECT_ID`, so a Claude
session inside foreman dispatches into **its own project** with zero
arguments.

### 4. Spawn semantics

The worker is a new terminal window in the target project running the
command directly (not wrapped in a shell). It snaps/tabs like any terminal
but deliberately does NOT take focus — a dispatch must never move the
keyboard out from under the user. Title: `agent · <label>`.

- **Exit:** when the process ends, the terminal stays open with scrollback
  and the title gains `exited (code)`. In fire-and-watch the terminal IS the
  record — closing is manual.
- **npm shims:** `claude` on Windows is a `.cmd` shim, not an exe. No PATH
  detection — try the direct spawn, and on ANY spawn error retry once wrapped
  in `cmd /c`. Consequence: `ok:true` means "a terminal opened", not "your
  command exists" — a bogus command opens a terminal showing cmd's error.

### 5. Claude as first client

A small skill/CLAUDE.md note teaches Claude: "to dispatch a visible agent,
run `foreman open --title \"agent · <label>\" -- claude -p \"<task>\"`".
Model-agnostic by construction — a codex variant is the same line with a
different command.

## Gotchas

- **Terminal IDs are per-project.** `WinId` is a per-manager counter, so
  `t3` is only unique inside its project. The protocol always scopes by
  (project, terminal) pair; never promise globally unique terminal IDs.
- **Tab-merged projects go stale.** Resolution only sees the window ids of
  active tabs — after merging project B's tab onto project A's window, B's
  old `pN` id no longer resolves and dispatchers inside B get an error until
  they pass an explicit `--project`.
- **One foreman instance per machine** (v1). First instance owns the pipe.
- **One request in flight at a time.** The pipe thread handles connections
  serially (a dispatch can hold it for up to 5s); a concurrent client may
  get a busy/connect error and should just retry.
- **The pipe is a command-execution surface.** Security boundary v1 is the
  pipe's default same-user ACL — anything running as you can open terminals.
  That's the same trust level as the shell itself.
- Workers run with the dispatcher's user/permissions; there is no sandbox.

## Out of scope (v1)

- **Round-trip**: `status`/`wait` verbs and a result-file convention are
  designed-for (IDs exist, protocol is JSON-extensible) but not built.
- **Agent-teams integration**: rejected. Watching `~/.claude/teams/*` state
  files means parsing another tool's private format that can break on any
  Claude update.

## Key files

- `src/control.rs` — pipe server thread, protocol types, arg parsing, client mode.
- `src/main.rs` — argv split (subcommand → client, else GUI); drain the
  control channel each frame and route to the desktop `WindowManager`.
- `src/wm.rs` — `handle_open` (request → project → spawn), `add_terminal_cmd`,
  project resolution, env injection, exited-title refresh.
- `src/terminal.rs` — command sessions (vs shell sessions), env injection,
  exited-state title.
