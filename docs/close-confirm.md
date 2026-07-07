# Close-confirm for running subprocesses

## What it does

When you close a terminal or project (or quit the whole app) that still has a
**running child process**, foreman pops a modal first: it lists the top-level
processes you launched (an agent, a build, a server) as `Name │ Pid`, each with
a `(+n)` rollup of the helpers it spawned, and makes you confirm. Close a
terminal that's just sitting at an idle prompt and nothing pops — it closes
silently, like before.

Three flavors, same modal:

- **close this terminal?** — one pane with a running child (an agent, a build, a
  REPL, a dev server).
- **close this project?** — a project window; lists every busy terminal inside
  it, grouped by terminal name (indented), so you see the whole blast radius.
- **quit foreman?** — the window's title-bar X or Alt+F4 while anything is
  running anywhere; grouped by project.

Keys: `Enter` confirms (the "close/quit anyway" button is default), `Esc`
cancels.

## Why it exists

Every terminal's shell lives in a kill-on-close Windows Job (see
`docs/session-process-cleanup.md`). Dropping the pane takes its whole child tree
with it — correct, but silent. Before this, closing a pane running `claude`
mid-task, or a long build, just killed it with no warning. This is the
speed-bump in front of that.

## How it works

1. **What counts as "running":** a shell's `root_pid` has at least one live
   descendant in the OS process table that isn't console-host plumbing
   (`OpenConsole.exe` / `conhost.exe` are filtered out). If nothing is running,
   the close happens immediately — no modal.
2. **What's shown:** only the shell's *direct* children — the process you
   actually launched — each with a `(+n)` count of its own subtree. So an agent
   that spawned a dozen MCP servers is one row (`codex.exe (+16)`), not thirteen.
   Closing still kills the whole tree; the rollup just keeps the list readable.
3. **The gate:** every *interactive* close routes through
   `WindowManager::request_close_active_tab` / `request_close_tab` instead of the
   raw `close*` methods. Empty process list → close now. Otherwise it stashes a
   `PendingClose` and the modal opens. The raw `close`/`close_tab`/
   `close_active_tab` still exist and are still used directly by the
   programmatic `foreman close` path — **that path is never gated** (it's
   headless; a modal would break automation).
4. **Rendering:** the modal is drawn in `show_modals`, which already runs at both
   window-manager levels — so a terminal-close modal renders over its project's
   rect and a project/quit modal over the desktop, with no cross-level plumbing.
   Both the dim and the panel center on the owning manager's `area` (the panel
   uses `pivot(CENTER_CENTER).fixed_pos(area.center())`, not a viewport anchor),
   so a terminal-close over an off-center project pops over *that* project.
5. **The decision is split from the render:** `resolve_pending(outcome)` is a
   pure method (confirm → close, cancel → drop, pending → keep) so the state
   machine is unit-tested without an egui context. `show()` just draws and
   reports the outcome.
6. **The quit path** (title-bar X / Alt+F4) bypasses the window manager, so
   `main.rs` intercepts `close_requested()`: if anything is running it cancels
   the OS close (`ViewportCommand::CancelClose`) and opens the quit confirm;
   accepting sets `force_quit` and re-issues the close.

## Gotchas

- **The list is a snapshot** taken when the modal opens, not live. If a listed
  process exits while you're staring at the dialog, no big deal — confirming
  just closes, cancelling just keeps the pane.
- **Detection can lag up to ~1.5s.** `top_children` reads the shared, throttled
  `proc::SCANNER` (refreshes at most every 1500 ms). A child spawned in the last
  fraction of a second before you hit close may not be in the table yet, so a
  genuinely-busy pane could close with no warning. Narrow window, and the
  kill-on-close Job still cleans it up — but it's the one hole left in the gate.
  A forced refresh at request time would close it (follow-up).
- **Exited terminals are skipped.** A shell that has died lingers as a tab (its
  title stamped `· exited`) until you close it. Its `root_pid` is stale and the
  OS may recycle it, so the scan gates on `Session::has_exited()` — a dead
  terminal never contributes a group, even if some unrelated process now owns its
  old PID. (`has_exited` is a cheap read of the exit latch the wm polls each
  frame.)
- **WSL work is invisible.** A `SH`/WSL terminal runs its real process *inside*
  the WSL VM, which isn't a Windows process, so `top_children` can't see it — a
  long `sleep` in a WSL pane closes with no warning. Same platform blind spot the
  agent-icon detection has (`proc.rs` header); no cheap fix.
- **`OpenConsole.exe` / `conhost.exe` are always excluded.** Whether they show up
  as children of the shell depends on the ConPTY host; the denylist means an
  idle shell never false-triggers the modal either way.
- **A confirm is globally modal — for the mouse as well as the keyboard.** While
  one is open *anywhere* in the app, no second confirm can open (even in another
  project) and the dialog owns all input. This is enforced by `app_modal`, an
  app-wide flag the desktop recomputes each frame (`any_pending_close` walks the
  whole tree) and threads down through `show`:
  - **Keyboard:** `is_focus` / `pump_commands` see the frozen flag, so no terminal
    reads keys and the leader stays dormant — only the dialog reads `Enter`/`Esc`.
    Without this the modal's `Enter` would *also* submit into the doomed process.
  - **Mouse:** `apply_acts` drops every background window act (tab switch, merge,
    minimize, zoom, float, focus, opening a picker) while a modal is up. Without
    this you could switch/merge the doomed tab out from under the dialog and
    Confirm would close the *wrong* tab, or minimize the project and hide the
    modal while the whole app stayed frozen (a phantom soft-lock).
  - **No stacking:** the close funnels (`request_close_*` / `begin_quit_confirm`)
    refuse via `overlay_blocks_close()` when a confirm, the dir picker, the
    settings editor, or an in-progress rename is up, so two overlays never fight
    over one keypress. One visible consequence: hitting the OS window-X while a
    picker/settings/rename overlay is open is swallowed (the quit is cancelled,
    no modal appears) — dismiss the overlay first, then X again. This is the safe
    trade: a quit-confirm can't render on top of the picker.
- **A pending confirm holds the app alive** — `deserted()` returns false while
  one is open, so closing the last project can't yank the modal out from under
  you before you answer.
- **The trigger/display policy is one function.** Want "warn only for agents",
  "warn for any live shell", or the full flat tree instead of direct-children +
  rollup? Change `collect_top_level` in `src/proc.rs`; nothing else moves.
- **Wrapper caveat.** The headline row is the shell's *direct* child. If you
  launch an agent through a wrapper (a `.cmd` shim, `node somescript`), the row
  shows the wrapper, not the agent — the rollup count still covers everything.
- **Colors come from `theme.rs`.** The modal renders in the current theme; the
  steel/orange look in the mockup is a separate, not-yet-landed re-theme.

## Known follow-ups

- **Grouping reads a project's *direct* terminals only.** `project_groups` and
  `groups_in_tab`'s project path don't recurse into a nested project's own
  terminals. This is fine today because projects are only ever created at the
  desktop (never inside another project), so nesting can't occur — but if that
  ever changes, both would under-warn and mis-count.
- **`force_quit` is set-once, never cleared.** If winit ever dropped the
  post-accept `ViewportCommand::Close`, the quit gate would silently disable for
  the rest of the session. Not observed; noted for the record.

## Key files

- `src/proc.rs` — `top_children(root_pid)` + the pure, tested `collect_top_level`
  / `count_descendants` (the trigger policy and the `(+n)` rollup).
- `src/confirm.rs` — `ConfirmClose` modal view (`ProcGroup`, `ConfirmOutcome`,
  grouped/flat `Name │ Pid` list).
- `src/wm.rs` — the gate (`request_close_*` + `overlay_blocks_close`), grouping
  (`terminal_shells`, `terminal_groups`, `project_groups`, `groups_in_tab`), the
  pure `resolve_pending`, `pending_close` state, the app-wide `app_modal` freeze
  (`any_pending_close` + the `show` threading + the `apply_acts` mouse gate), and
  `begin_quit_confirm` / `take_quit_confirmed`.
- `src/terminal.rs` — `Session::root_pid()` and `has_exited()` (the exited-gate
  that keeps recycled PIDs out of the scan).
- `src/main.rs` — the app-quit guard (`close_requested` interception, `force_quit`).
- `docs/superpowers/specs/2026-07-06-close-confirm-running-subprocess-design.md` —
  full design + the approved mockup (`…-close-confirm-mockup.html`).
