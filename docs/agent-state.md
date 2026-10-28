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
in the row's right-edge slot; the other states rank below it. A tab stack's
"+N" chip paints the same badge on its list rows and turns the needs-you
colour while a tab it hides needs you (`docs/window-chrome.md`).

Agents read the same state through `foreman status --json` (`agent_state`:
`working`, `needs_you`, or `idle`; the done marker stays panel-only), behind
the same setting, so status never reports state that chat delivery ignores.
The orchestration skill uses it to hand needs-you workers to the human
(`docs/tab-targeting.md`).

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
- **Grok has no Needs you.** Grok has no `PermissionRequest` event, so Foreman
  also installs no `PostToolUse` hook for Grok: that event only leaves Needs
  you, and a helper launch per tool call would buy nothing.
- **Grok fires every event twice** (it also runs `~/.claude/settings.json`
  hooks). The Claude-under-Grok drop in `normalize_event` removes the copy; the
  mapping is idempotent anyway.
- **Codex sends a late `PostToolUse` after `Interrupt`.** That is why
  `PostToolUse` only ever moves Needs you to Working and never wakes an Idle
  Session.
- **Codex's shared app-server daemon carries the wrong identity.** Since
  Codex 0.157 every interactive TUI attaches to one daemon per `CODEX_HOME`, and
  the daemon runs tools and hooks with the environment of the TUI that started
  it. A second Codex Session then reported state and posted chat as the first
  one (2026-10-19, p7 t7 as t5), and its own posts were delivered unheld while
  it worked. `agent_command::codex_in_process` adds `--no-daemon` to every
  interactive Codex Foreman spawns, which keeps the server in-process. The
  flag needs Codex 0.156+; an older Codex rejects it and does not start. A
  `codex` typed by hand in a shell pane still attaches to the daemon.
- **Codex shows Needs you only when a human approves.** Verified 2026-10-15
  (Codex 0.162.1): a real approval prompt fires `PermissionRequest` and the
  row shows `needs you`; approving returns it to Working, then done. With
  `approvals_reviewer = "auto_review"` in `~/.codex/config.toml`, an
  automatic reviewer answers instead and no prompt reaches you, so the row
  never shows `needs you`. That is correct, not a missed event.
- **Big tool output.** The helper reads up to 16 MB of hook stdin because a
  `PostToolUse` payload includes the tool's full output; the forwarded message
  stays small (event name and IDs). The pipe server's read cap stays at 64 KB.
- **State detection writes nothing into any Session.** It only reads hook
  events. Chat delivery consumes the slot to hold posts while Working or Needs
  you when this setting is enabled; `--now` bypasses Working. See
  `docs/chat-delivery.md` for the delivery policy and hook limitations.
- **Flipping the setting resets every Session's state.** While badges are off
  only the naming hook runs, so each prompt would leave a Session at `working`
  with no `Stop` to follow. Enabling starts every slot clean; a row shows a badge
  again after its next hook event.
- **The icon decides which provider owns the pane.** When several agents run
  under one shell (Claude's Bash tool running `codex exec`), the process scan
  picks the one closest to the shell, so the pane keeps its Claude identity and
  Codex's events are dropped.

## Key files

- `src/agent_state.rs` — `HookEvent`, `AgentStateSlot` (pure event → state
  mapping), `AgentBadge`, `badge_label`.
- `src/title_notify.rs` — `parse_args` (`--agent`, `--event`), `normalize_event`
  (subagent and Grok-duplicate filtering, prompt only on `UserPromptSubmit`),
  `TitlePromptEvent.hook_event`.
- `src/agent_hooks.rs` — `HookWants`, `wanted_events`, `merge_event` (in-place
  handler update that never reorders).
- `src/wm.rs` — `apply_hook_event` (project + terminal + icon match), the
  `panel_model` badge read, and the `status_dispatch` read for `status --json`.
- `src/panel.rs` — `paint_state_label` and the right-edge slot ranking.
- `src/terminal.rs` — `Session::agent_state`, done marker cleared on focus in
  `Session::show`.
- `src/config.rs`, `src/settings_menu.rs` — `agent_state_badges`.

## Assignment compaction recovery

Assignment context is independent of optional naming and badges. The installer
adds a synchronous `SessionStart` matcher for `compact` to Claude and Codex
hook settings, but only for a CLI found on the session PATH (`.exe`, npm
`.cmd`, or a bare name). It never creates a configuration file for an absent
CLI. With naming and badges off and neither CLI present, it writes nothing.
A recovery handler stays installed after its CLI disappears. Existing user handlers and trust positions stay in place.
The `assignment-context` helper emits only the provider's `additionalContext`
JSON when the instance control pipe returns a matching owner's recovery brief.
It ignores subagent events, unrelated sessions, and malformed input.

Delivery is disclosed, never assumed. Every assignment result carries a
`recovery` line read from the provider's own configuration: not installed;
disabled (Claude's `disableAllHooks`); installed but runnable only once trusted
(Codex, whose trust store Foreman cannot read); or installed. It tells the
agent to run `assignment status` when no brief arrives. The installer also
warns the user about disabled hooks on every run, and about Codex trust whenever
it changes the handler. It warns when `claude --version` or `codex --version`
is older than the configured warning threshold (Claude Code 2.1.310,
Codex 0.164.0), or cannot be read. That threshold does not establish assignment
delivery acceptance. Assignment
Begin/Checkpoint from shell tools and mid-card recovery delivery were verified
with Codex 0.165.0 and `--no-daemon`; its compact hooks omit `CODEX_THREAD_ID`.
The complete two-card integration lifecycle was verified with Claude.
Older versions are reported as unverified, not refused, because the true
minimum is unknown. Only the user-level file
Foreman manages is read: hooks or `disableAllHooks` in project, local,
managed, or `--settings` configuration are not seen.

The helper caps stdin and exits quietly after a 5 s deadline; the managed hook
allows 10 s. The context request starts several git processes, which took
1.8-2 s under build load against 0.4 s idle, and compaction takes far longer. Server requests
have bounded concurrency and short read-only Git probes. Context requests have
their own capacity, so polling waits cannot starve them. A repeated invocation
only reads scope, current brief, observed Git/queue state, and the continuation
note. Disagreements are shown explicitly; the hook never claims a card, transfers
ownership, submits integration, or modifies Git. Continue/Resume remain the
execution authority when hooks are missing, failed, timed out, or untrusted.

Routing validates a fresh process tree, the native root provider's creation time,
and the caller's provider session environment (`CODEX_THREAD_ID` or
`CLAUDE_CODE_SESSION_ID`), in addition to Foreman instance/project/terminal IDs.
Only Codex's read-only `context` request may omit the environment variable:
compact hooks supply `session_id` in JSON instead. The complete owner key,
including that session ID and validated provider process identity, must match
the recorded assignment before any brief is returned. An available but
mismatched environment variable is still rejected. All other operations retain
environment proof, including Begin, Continue, and explicit Status.
A same-provider child process does not inherit ownership. Daemon routing and
unavailable identity fail closed. Trust is never enabled automatically. Hook
installation alone is not evidence of delivery; supported-version and actual
mid-card compaction evidence must be checked separately.

Provider contracts: [Codex hooks](https://learn.chatgpt.com/docs/hooks),
[Claude hooks](https://code.claude.com/docs/en/hooks), and
[Claude session environment](https://code.claude.com/docs/en/env-vars).
