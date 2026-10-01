# Agent state in the Sessions panel

Each agent Session row in the Sessions panel can show a small label in its
right-edge slot: `working`, `needs you`, `idle`, or `done`. The label comes from
the agent CLI's own lifecycle hooks, not from reading the screen. Design:
`docs/superpowers/specs/2026-09-25-agent-state-design.md`.

## What it does

Claude Code, Codex CLI, and Grok Build fire hooks at lifecycle points. Foreman
already installs one guarded hook (`foreman title-event`) for Session naming.
This feature installs that same hook on a few more events, with the event name
baked into each handler as `--event <Name>`. The helper forwards the event name
and the Session's routing IDs over the existing private pipe; the GUI maps the
event to a state on the matching Session.

| Hook event | New state |
|---|---|
| `UserPromptSubmit` | Working (clears the done marker) |
| `PermissionRequest` | Needs you |
| `PostToolUse` | Working, only if the state was Needs you |
| `Stop`, `StopFailure` | Idle + done marker |
| `Interrupt` (Codex only) | Idle |

The **done marker** is set when a turn ends and clears when the Session gets
keyboard focus, in the same place the Bell clears. Needs you outranks the Bell
in the row's right-edge slot; the other states rank below it.

An event applies to the Session whose Foreman project ID and terminal ID match
(terminal IDs repeat across projects). An event whose provider does not match
the Session's icon is dropped, so Codex run from a Claude pane's Bash tool
cannot overwrite the Claude pane's state. A subagent's `PermissionRequest` and
`PostToolUse` count (the human answers its prompt); every other subagent event
is dropped.

A row shows no badge until Foreman has accepted an event from it, none after
the process exits, and none while the Session's detected icon is not the
event's provider (plain shell, agent exited back to the shell, hooks not yet
trusted).

## How to turn it on

Settings → Agents → **Show agent state in the Sessions panel**. Off by default.

Turning it on runs the hook installer, which adds the state events to each
provider's global hook file (the same files naming uses). Turning it off reruns
the installer, which removes the state handlers and keeps `UserPromptSubmit` if
naming is still on. With both naming and state off, every managed handler is
removed. Reinstalls never move an existing handler in a hook array: Codex keys
hook trust by position, so a reorder would force re-trust of every hook.

Codex asks you to trust new hooks (`/hooks` inside Codex). Until you do, Codex
fires nothing and its rows show no badge.

## Gotchas

- **Claude Esc is invisible.** Interrupting Claude fires no hook; the row keeps
  showing `working` until the next event.
- **Same-provider children overwrite the parent.** A `claude -p` launched inside
  a Claude Session inherits the pane's terminal ID, so its events change the
  pane's state.
- **Grok has no Needs you.** Grok has no `PermissionRequest` event.
- **Grok fires every event twice** (it also runs `~/.claude/settings.json`
  hooks). The Claude-under-Grok drop in `normalize_event` removes the copy; the
  mapping is idempotent anyway.
- **Codex sends a late `PostToolUse` after `Interrupt`.** That is why
  `PostToolUse` only ever moves Needs you to Working and never wakes an Idle
  Session.
- **Codex approvals were not captured** in the 2026-09-25 hook capture;
  `PermissionRequest` is documented but unverified for Codex.
- **Big tool output.** The helper reads up to 16 MB of hook stdin because a
  `PostToolUse` payload includes the tool's full output; the forwarded message
  stays small (event name and IDs). The pipe server's read cap stays at 64 KB.
- **Nothing is written into any Session.** The feature only reads hook events,
  and the GUI only ever `try_recv`s; a late or lost event makes a badge late or
  stale, never blocks typing.
- **State is tracked even while the setting is off.** The panel gates at paint,
  so enabling the setting shows the current state instead of waiting for a turn
  (only once hooks are installed, of course).

## Key files

- `src/agent_state.rs` — `HookEvent`, `AgentStateSlot` (pure event → state
  mapping), `AgentBadge`, `badge_label`.
- `src/title_notify.rs` — `parse_args` (`--agent`, `--event`), `normalize_event`
  (subagent and Grok-duplicate filtering, prompt only on `UserPromptSubmit`),
  `TitlePromptEvent.hook_event`.
- `src/agent_hooks.rs` — `HookWants`, `wanted_events`, `merge_event` (in-place
  handler update that never reorders).
- `src/wm.rs` — `apply_hook_event` (project + terminal + icon match) and the
  `panel_model` badge read.
- `src/panel.rs` — `paint_state_label` and the right-edge slot ranking.
- `src/terminal.rs` — `Session::agent_state`, done marker cleared on focus in
  `Session::show`.
- `src/config.rs`, `src/settings_menu.rs` — `agent_state_badges`.
