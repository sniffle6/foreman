# Tab targeting

Terminals hide in three ways: behind another tab in their window, in a
minimized window, or inside a Project that is itself a background tab. This
doc covers how the control plane reaches hidden terminals, how it tells you
where each one sits, and how it brings one to the front.

## What it does

- **Every hidden terminal is still a target.** `send`, `snapshot`, `chat` and
  `close` address a terminal by its `tN`, and the `tN` follows the Session, not
  the tab or window. The lookup (`terminal_location`) scans every tab of every
  window in the Project, minimized windows included. A `pN` resolves the same
  way: a Project tab merged behind another Project tab is still found.
- **Hidden terminals keep running.** Each frame, `show` pumps the tabs it
  isn't drawing (`keepalive_inactive`, and `keepalive` for minimized windows).
  A hidden shell therefore answers its startup DSR, reads input, and drains
  output just like a visible one. Without that pump, a `send` would be written
  to a PTY whose shell never wakes up.
- **`status` shows where each tab sits.** Each line has a placement column:

  ```
  p1  focused  myrepo  C:\src\myrepo
    t3  running  chat  focused  agent · parser
    t5  running  chat  background  agent · tests
    t6  exited(0)  -  minimized  cmd
  p2  background  other  C:\src\other
  ```

  | placement | means |
  |---|---|
  | `focused` | holds the keyboard. At most one project line and one terminal line say it. |
  | `front` | the tab its window is showing |
  | `background` | behind another tab in its window |
  | `minimized` | its window is minimized (this wins over tab order) |

- **`status --json` says the same thing to programs**, plus what the text
  can't hold. It prints one object per line: project lines carry `project`,
  `name`, `cwd`, `placement`; terminal lines carry `project`, `terminal`,
  `title`, `agent` (`claude` | `codex` | `grok`), `state`
  (`running` | `exited`), `exit_code`, `chat`, `placement`, `ready`, and
  `agent_state` (`working` | `needs_you` | `idle`). A missing field means
  unknown, never a default. `agent_state` only appears while Settings →
  Agents → agent state is on, because that toggle is what installs the hooks
  and what chat delivery obeys (`docs/agent-state.md`).

  ```
  {"project":"p1","name":"myrepo","cwd":"C:\\src\\myrepo","placement":"focused"}
  {"project":"p1","terminal":"t5","title":"agent · tests","agent":"claude","state":"running","chat":true,"placement":"background","ready":true,"agent_state":"needs_you"}
  ```

- **`foreman focus` brings a tab forward**, the same way clicking its
  Sessions-panel row does. It restores the window if it's minimized, switches
  the window (and the Project's own window) to that tab, drops any zoom that
  would cover it, and gives it the keyboard.

## How to use

```powershell
& $env:FOREMAN_EXE status                                  # find it: tN + placement
& $env:FOREMAN_EXE status --json                           # same, plus ready / agent_state, per line
& $env:FOREMAN_EXE send --terminal t5 --text "go`r"        # works in any placement
& $env:FOREMAN_EXE chat --to t6 "rebase first"             # ditto
& $env:FOREMAN_EXE focus --terminal t5                     # bring it to the front
& $env:FOREMAN_EXE focus --project p2                      # a Project tab itself
& $env:FOREMAN_EXE focus                                   # your own pane
```

`focus` picks the project the way `send` does: `--project`, then
`FOREMAN_PROJECT_ID`, then the focused project. On success it replies
`{"ok":true,"project":"pN","terminal":"tN"}` (`terminal` is left out when you
focused a project).

## Why it exists

Agents and operators already used send/chat/snapshot on terminals the user
couldn't see. They worked, but nothing showed they would, and there was no
way to bring a terminal back except clicking it. The panel already had the
focus code (`surface_target`), so `focus` is just control-plane access to
it. No second focus path exists.

## What agents are taught

The embedded skills (Claude and Codex copies) carry the "when", not the
flags:

- foreman-dispatch, "Your panes after launch": every pane is reachable
  where it sits; `status --json` to check on workers; chat for agents and
  `send` only for programs that aren't agents; `focus` when the human asks
  to see a pane or a launched worker is `needs_you` (post `@you` first, one
  pane at a time); `close tN`.
- foreman-chat: delivery reaches hidden members; a stalled post is read
  through `ready` / `agent_state`, not slept on.
- foreman-kanban `orchestrating-plans.md`: on a `wait` timeout, read
  `status --json` and hand `needs_you` workers to the human.

## Gotchas

- **`focus` takes the human's keyboard.** Whatever they were typing goes to
  the new terminal from the next key on. If that terminal is showing a
  permission prompt, a stray `y` answers it. Agents are taught to use it only
  for a deliberate hand-off: the human asked to see the pane, or a worker is
  `needs_you` and an `@you` post went out first.
- **`agent_state` lags in known ways** (`docs/agent-state.md`): Claude
  interrupted with Esc stays `working`, Grok never reports `needs_you`, and
  Codex with automatic approval review never asks. That's why the skills
  fall back to a snapshot when `working` persists.
- **`status --json` against an older GUI** returns plain text lines (serde
  ignores the unknown `json` field), so a caller's JSON parse fails loudly
  rather than silently misreading.
- **It doesn't raise the foreman window over other apps.** Only what's inside
  foreman changes.
- **`front` doesn't mean visible.** A zoomed sibling can cover a `front` tab,
  and so can a Project that is `background` or `minimized`. Read the project
  line too.
- **Replies come before the action**, like `close`. A request that waited past
  `REPLY_TIMEOUT`, or whose client hung up, never changes focus later.
- **Old GUI, new CLI:** an older foreman answers `unknown cmd: focus` (exit 1).
  An older CLI never sends the verb, so nothing else changes on the wire.
- **The `status` format changed.** The placement column was added after `pN`
  on project lines and after `chat|-` on terminal lines. Anything that split
  those lines by position must shift by one. Nothing in this repo or the
  shipped skills parses them.

## Key files

- `src/control.rs`: `FocusRequest`, `CtrlMsg::Focus`, `parse_focus_args`,
  `focus_main`, `HELP_FOCUS`, `HELP_STATUS`, `StatusRequest::json`,
  `StatusProjectLine`, `StatusTerminalLine`
- `.claude/skills/foreman-{dispatch,chat,kanban}` and their `.codex/skills`
  twins: what agents are taught (embedded at compile time,
  `src/skills_install.rs`)
- `src/wm.rs`: `focus_dispatch`, `project_location`, the `CtrlMsg::Focus` arm
  of `handle_ctrl`, `surface_target` (the shared write seam), `placement`,
  `status_dispatch`, `terminal_location`, `keepalive_inactive` / `keepalive`
- Tests in `src/wm.rs`: `hidden_tabs_fixture` and the tests after it
  (`status_reports_where_each_tab_sits`, `focus_*`,
  `send_reaches_background_tab_and_minimized_terminal`,
  `chat_reaches_background_and_minimized_members`)
