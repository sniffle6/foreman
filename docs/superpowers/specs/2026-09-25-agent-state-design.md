# Agent state in the Sessions panel — design

Date: 2026-09-25. Decisions settled with the user in this session, then cut
down after a simplicity review.

## Goal

Show, per agent Session in the Sessions panel, whether the agent is working,
needs you, or is idle, for Claude Code, Codex CLI, and Grok Build. A new
provider needs only its hook event names mapped.

Speed rule: event handling must never block typing, scrolling, pane switching,
or prompt submission. A late or lost event only makes a badge late or stale.

## The design in one paragraph

Each provider CLI fires lifecycle hooks. Foreman already installs one guarded
hook (`foreman title-event`) for Session naming. This feature installs that same
hook on a few more events. The helper forwards the event name with the existing
naming message over the existing private pipe. The GUI maps the event name to a
state on the Session it came from. That is the same shape Warp ships in its
official `claude-code-warp` plugin: a handful of hooks and a direct mapping.

## States

| State | Meaning |
|---|---|
| **Working** | The agent is in a turn. |
| **Needs you** | The agent is blocked on you: a permission prompt or a question. |
| **Idle** | The turn ended; the agent is at its input box. |

Separate from state: a **done marker** is set when a turn ends and clears when
that Session gets keyboard focus, in the same place the Bell clears.

A Session shows no badge until Foreman has received an event from it, and none
while the Session's detected agent (`Session::icon_kind`) is not the event's
provider. The icon check covers "hooks not installed", "Codex has not trusted
the hooks yet", plain shells, and the agent exiting back to the shell, without
reading any provider's private config.

## Event mapping

| Hook event | New state |
|---|---|
| `UserPromptSubmit` | Working (clears done) |
| `PermissionRequest` | Needs you |
| `PostToolUse` | Working, **only if the state is Needs you** |
| `Stop`, `StopFailure` | Idle + done |
| `Interrupt` (Codex only) | Idle |

Nothing else is installed. `SessionStart` is left out: Claude re-fires it
mid-turn after compaction, and Codex does not fire it until the first prompt.
`SessionEnd` is left out: the icon check already hides the badge on exit.

**The guard rule:** `PostToolUse` only moves Needs you to Working. It exists to
leave Needs you after you approve or answer. It never wakes an Idle Session,
because Codex delivers a `PostToolUse` about 1.5s *after* `Interrupt`.

**Identity:** an event applies to the Session matching its Foreman project ID
**and** terminal ID (terminal IDs repeat across projects), reusing the tree walk
in `src/wm.rs` `prepare_title_request`. An event whose provider does not match
the Session's icon is dropped (`source_matches_icon`, already used by naming).
That stops an agent launched *inside* another agent's pane, such as Codex run
from a Claude Session, from overwriting the pane's state.

**Subagents:** a subagent's `PermissionRequest` and `PostToolUse` are kept,
because the human answers its prompts and its tool completing is what leaves
Needs you. Every other subagent event is dropped. Subagents are detected by
`agent_id` only.

**Duplicates:** Grok runs hooks from `~/.claude/settings.json` as well as its
own, so every Grok event arrives twice. The existing Claude-under-Grok drop in
`src/title_notify.rs` `normalize_event` removes the copy; the mapping is
idempotent anyway.

## Transport

One hook, one message, one channel.

- The hook command stays `foreman title-event --agent <agent>`.
- The helper reads hook stdin and sends the existing `TitlePromptEvent` with a
  new `hook_event` field. `prompt` becomes optional and is set only for
  `UserPromptSubmit` from a main agent. State needs nothing else: the event
  name and the IDs.
- The helper's stdin cap rises to 16 MB, because a `PostToolUse` payload
  includes the tool's full output; the current 64 KB cap would drop it and
  pin the badge at Needs you. The server's read cap stays small, because the
  forwarded message is small.
- `MAX_INFLIGHT` rises from 8 to 32 so tool-heavy turns across several panes,
  doubled by Grok, do not get a `Stop` turned away.
- The GUI drain always applies state, and starts naming only when `prompt` is
  present and naming is enabled.
- No separate channel: the channel bounds message count, naming sends one
  message per human prompt, and the GUI drains every frame.
- No wire-compatibility concern: the helper is `$FOREMAN_EXE` of the same
  instance that owns the pipe.

## Setup

A Settings toggle, **Show agent state in the Sessions panel**, off by default.
The installer picks its event list from settings: naming alone installs only
`UserPromptSubmit`; state installs the full mapping list above. Managed
handlers are removed from every known event and re-added only on the wanted
ones, so turning state off removes its hooks and naming-only users never pay
a PowerShell launch per tool call. Claude and Codex hooks run `async`. Codex
asks the user to trust new hooks (`/hooks`); until they do, Codex fires
nothing and no badge appears.

## Panel

Keep the existing title and icon. Show a small label in the row's right-edge
slot: `working`, `needs you`, `idle`, or `done`. Needs you outranks the Bell;
the other states rank below it. Painting stays read-only.

Deferred: rolling Needs you up onto a collapsed Project row, and a jump to the
next Needs you Session.

## Accepted limitations

- **Claude Esc is invisible.** Interrupting Claude fires no hook. The Session
  keeps showing Working until its next event, as in Warp.
- **Same-provider children overwrite the parent.** A `claude -p` launched
  inside a Claude Session inherits the pane's terminal ID, so its events
  change the pane's state.
- **Grok has no Needs you:** it has no `PermissionRequest` event.
- **No badge before the first prompt.**
- **Codex approvals were not captured**; `PermissionRequest` is documented but
  unverified for Codex.

## Rejected alternatives

- **Passive PTY detection** (output quiescence, cursor rest; the plan in the
  `foreman-agent-state-campaign` skill). Cannot tell Needs you from Idle: both
  are a quiet screen with a parked cursor.
- **Structured agent protocols** (T3 Code drives Claude through the Agent SDK,
  Codex through `codex app-server`, Grok through ACP). Works because T3 owns
  the conversation; Foreman hosts the real interactive TUI.
- **Separate naming and state hook sets** with a `--kind` flag, a message
  envelope, split channels, and in-place handler updates. About twice the
  code for isolation the one-channel design does not need.
- **A reducer with Unknown, stale-session tracking, turn-ID bookkeeping,
  `SessionStart`/`SessionEnd` handling, and an Esc heuristic.** Built for rare
  cases; the icon check and the guard rule cover what the capture showed.

## Evidence

Hook capture on 2026-09-25 against Claude Code 2.1.282, Codex CLI 0.157.0
(`--no-daemon`), and Grok Build 1.0.41, run inside Foreman, plus a code review
of `src/title_notify.rs` and `src/agent_hooks.rs`. Findings used above:
terminal IDs repeat across projects; Claude fires `PermissionRequest` for
questions even in auto mode and `PostToolUse` when answered; Claude Esc fires
nothing; Codex fires `Interrupt` and then a late `PostToolUse`; Codex fires
`SessionStart` lazily and skips untrusted hooks silently; Grok duplicates every
event; closing a pane fires no `SessionEnd`; Codex keys hook trust by handler
position.
