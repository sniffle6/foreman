# Terminal Inspection

Lets an agent (or script) drive input into a terminal and read back its screen
without touching the GUI. Phase 1 (`src/inspect.rs` pure functions) and Phase 2
(the `foreman send` and `foreman snapshot` verbs) are done.

## What it does

- `foreman send` — write raw UTF-8 text and/or named key presses into a terminal's PTY.
- `foreman snapshot` — read the terminal's grid as plain text rows. Default is
  the currently displayed viewport; `--tail N` is the last N lines of the
  buffer (scrollback + live screen).

Together they close the feedback loop: an agent can `send` a command and then
`snapshot` to see the result — including output that has already scrolled off
the pane.

## How to use

```sh
# From inside a foreman terminal (self-target via env):
foreman send --text "echo hello\r"
foreman snapshot

# Targeting another terminal explicitly:
foreman send --project p1 --terminal t3 --text "ls\r"
foreman snapshot --project p1 --terminal t3

# Last N buffer lines (scrollback), not just the visible pane:
foreman snapshot --project p1 --terminal t3 --tail 80

# Named key presses (same encoding as the GUI keyboard):
foreman send --project p1 --terminal t3 --keys "F5"
foreman send --project p1 --terminal t3 --text "ls" --keys "Tab Enter"

# Combined text + keys:
foreman send --terminal t3 --text "vim file.txt" --keys "Enter"
```

## Key names for `--keys`

`F1`..`F12`, `Up Down Left Right`, `Home End PageUp PageDown Insert Delete`,
`Enter Tab Esc Backspace Space`, single uppercase letters; `Ctrl+`/`Alt+`/`Shift+`
prefixes (combinable). A bare lowercase letter has no key sequence — use `--text`
for literal characters. Unknown name → exit 2.

`--keys` splits its value on whitespace, and is repeatable (appends):
```sh
foreman send --keys "Escape F1" --keys "Enter"
# equivalent to: Escape, F1, Enter
```

## `--tail N`

Default snapshot is the **displayed viewport** — if the pane is 30 rows, you
get 30 rows, even when thousands of lines sit in scrollback. A long
`cargo test` failure that scrolled off the top is invisible.

`--tail N` walks the last N lines of the buffer (history + live screen) and
ignores the current scroll position. N larger than the buffer returns
everything. N must be a positive integer (exit 2 otherwise). `--attrs` uses
the same row span.

On the alternate screen (vim, lazygit, agent TUIs) there is no scrollback;
`--tail` then returns at most one screen of that buffer.

## `--settle-ms`

`--settle-ms N` is honored: after `send` writes, the reply waits until the
Session has produced no new output for N ms (default 120, cap 4000) so a
following snapshot is settled, not mid-update. `--settle-ms 0` replies
immediately.

## Self-target

Both verbs default to your own terminal when `--terminal` is omitted, using
`FOREMAN_TERMINAL_ID` and `FOREMAN_PROJECT_ID` from the environment (injected
into every foreman-spawned terminal). This means a zero-flag one-liner works
from inside any foreman terminal:
```sh
foreman send --text "pwd\r" && foreman snapshot
```

An explicit `--project` without `--terminal` is an error (same rule as bare
`close`): terminal ids are only unique within a project, so filling the
terminal from your env would silently target another project's pane.

## Key files

- `src/inspect.rs` — pure grid-walk: `snapshot_text`, `snapshot_tail`, `parse_keys`, `cursor_info`, `grid_contains`
- `src/terminal.rs` — `Session::feed`, `Session::term_mode`, `Session::snapshot_text`
- `src/control.rs` — `SendRequest`, `SnapshotRequest`, `CtrlMsg::Send/Snapshot`, `parse_send_args`, `parse_snapshot_args`, `send_main`, `snapshot_main`
- `src/wm.rs` — `resolve_terminal`, `session_mut`, `send_dispatch`, `snapshot_dispatch`, `handle_ctrl` arms
