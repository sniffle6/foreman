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

## Ctrl+Delete live verification

Verified on 2026-09-30 at revision `f949401`, using an isolated
`cargo build --target-dir target/agent` debug instance. Its `APPDATA` pointed
to a scratch directory, `FOREMAN_NO_LANDING=1`, and skill installation and
update checking were disabled in scratch settings. All commands below targeted
that instance's `FOREMAN_PIPE`, project `p2`; the running host was untouched.

| Client | Input and cursor setup | Snapshot after `Ctrl+Delete` | Result |
|---|---|---|---|
| PowerShell 7.6.6 / PSReadLine 2.4.5 (`pwsh -NoLogo -NoProfile`) | `alpha beta gamma`, `Home Ctrl+Right` | `PS> alpha  gamma` | Deletes `beta`; the live binding reports `Ctrl+Delete KillWord`. |
| Claude Code 2.1.286 (plan mode, no prompt submitted) | `alpha beta gamma`, `Home Ctrl+Right` | `❯ alpha beta gamma` becomes `❯ alphabeta gamma` | Deletes only the space. Ctrl+Right stops at the end of `alpha` in this client. |
| Same Claude Code Session | Fresh input, `Home` followed by six `Right` keys (start of `beta`) | `❯ alpha eta gamma` | Deletes only `b`, not the next word. |

Reproduction with the test instance's executable and pipe selected:

```powershell
& $exe send --project p2 --terminal t2 --text 'alpha beta gamma' --keys 'Home Ctrl+Right'
& $exe snapshot --project p2 --terminal t2 --cursor
& $exe send --project p2 --terminal t2 --keys 'Ctrl+Delete'
& $exe snapshot --project p2 --terminal t2 --cursor
```

For the unwrapped PowerShell snapshot, the test Session used
`function prompt { 'PS> ' }`. Before and after deletion its cursor was
`row:20, col:10`, at the start of `beta`. Claude's cursor was `row:19, col:7`
on the space after `alpha`, or `col:8` at the start of `beta`; deletion kept
the cursor in place. Repeating the Claude test with literal bytes
`--text ([string][char]27 + '[3;5~')` also produced `alpha eta gamma`.

The existing `input::encode_key` mapping remains `ESC[3;5~`
(hex `1b 5b 33 3b 35 7e`). It matches Windows Terminal's standard ANSI
encoding: its `TerminalInput::_encodeRegular` assigns Delete code 3 and
final `~`; Ctrl sets modifier bit 4, and `_formatEncodingHelper` adds 1
to produce parameter 5. See the
[Windows Terminal implementation](https://github.com/microsoft/terminal/blob/main/src/terminal/input/terminalInput.cpp)
(checked on the verification date). This comparison is against upstream
source, not a live Windows Terminal capture.

Conclusion: next-word deletion already works in PSReadLine. This Claude Code
version handles the same sequence as character deletion. Foreman needs no
encoding change to match Windows Terminal; an alternative sequence would be
a client-specific workaround. WSL/readline was not tested.

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
