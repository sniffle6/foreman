---
name: foreman-dispatch
description: Use when running inside Foreman (the FOREMAN env var is 1) and Codex launches or manages visible terminals — dispatch or spawn a worker, open a pane with a prompt or command, check on a worker, read or drive another pane, hand a pane to the human, close one. Kanban cards and plans use foreman-kanban for continuing assignments or authorized delegation.
---

# Launch and Manage Visible Terminals in Foreman

Everything needed to launch and manage panes is below, quoting safety
included. Foreman's source and docs are for changing Foreman; read them only
to diagnose behavior this skill does not explain. Researching your task's
subject matter is separate and fine.
Precondition: `$env:FOREMAN` is `1`. If not, tell the user this needs to run
inside a Foreman terminal.

Codex sandbox: Foreman's control channel is a named pipe the sandbox blocks
("Access is denied (os error 5)"). Run every `$env:FOREMAN_EXE` command with
escalated/approved execution from the start.

**Kanban cards and plans use foreman-kanban.** Continue related approved cards
in the current Session with assignments. Delegate independent card work only
when authorized and useful, using `kanban dispatch <id> --agent …`.
`open` is a bare terminal: no card claim, no worktree, no
`Card:` trailer, no integration queue, and the board never learns the worker
exists. `kanban dispatch` is exactly what the board's "Start with" button does.

## Choosing workers

Use an interactive agent when it must receive project chat or remain open
for steering. Non-interactive `codex exec` and `claude -p` workers cannot
receive chat while running and exit on completion.

For free-form chat teams, use both providers by default: Codex for research
and review, Claude for implementation and verification. Follow an explicit
user choice, or use the available provider if one CLI is unavailable.
Kanban cards follow `foreman-kanban` instead.

## Commands

PowerShell:

```powershell
# Watchable worker: interactive Codex session. Streams live, can receive chat,
# and stays open when done.
& $env:FOREMAN_EXE open --title "agent - <short-label>" -- codex "<task prompt>"

# Fire-and-forget worker: non-interactive Codex exec. It exits on completion and
# cannot receive project chat while it runs.
& $env:FOREMAN_EXE open --title "agent - <short-label>" -- codex exec "<task prompt>"
```

bash (and the same `"$FOREMAN_EXE" <verb>` shape for every command below):

```bash
"$FOREMAN_EXE" open --title "agent - <short-label>" -- codex "<task prompt>"
```

Anything works after `--` (`codex`, `claude`, build scripts, custom commands).
The reply JSON gives the new terminal id and project id:

```json
{"ok":true,"terminal":"t3","project":"p1"}
```

The ids are assigned by Foreman; record them, you cannot pick or predict
them. The reply is NOT the worker's result: fire-and-watch. Tell the user the
agent is running and where.

## Prompt Quoting

Foreman passes argv per argument, but Windows command shims still matter. If the
tool runs through a `.cmd` or `.bat` shim, Foreman refuses prompts containing
newlines, carriage returns, or literal `"` because `cmd.exe` cannot carry them
reliably.

Practical rule for Codex on Windows:

- For the npm-shim `codex`, use one-line, quote-free prompts.
- For multi-line prompts, use a native executable if available, or flatten the
  prompt before dispatch.
- Keep long prompts simple rather than hand-escaping them inline in PowerShell.

Example one-line prompt:

```powershell
$prompt = 'Review src/wm.rs for local-vs-screen coordinate regressions and report findings only'
& $env:FOREMAN_EXE open --title "agent - review" -- codex $prompt
```

If Foreman reports a `cmd-shim` error, flatten the prompt to one quote-free line
or install/use a native executable for that tool.

**Variadic-flag trap:** flags like `--ask-for-approval` / `--full-auto` that
swallow following words must not sit between the CLI name and the prompt.
Prompt immediately after `codex` / `codex exec`; flags after the prompt.

## Targeting

- The worker opens in your current project by default (`FOREMAN_PROJECT_ID`).
- Use `--project pN` to target another Foreman project.
- Use `--cwd <dir>` to set the worker's process directory.

Examples:

```powershell
& $env:FOREMAN_EXE open --project p2 --title "agent - tests" -- codex "Run cargo test and summarize failures"
& $env:FOREMAN_EXE open --cwd "C:\code\repo" --title "cmd" -- cmd.exe
```

## Your Panes After Launch

Every pane is reachable by its `tN` wherever it sits: behind another tab, in
a minimized window, or in a project tab the human isn't looking at. Read and
message panes where they are.

```powershell
& $env:FOREMAN_EXE status --json    # one JSON object per line, per project and per terminal
```

- **Check on workers:** `status --json`. Each terminal line has `state`
  (`running` | `exited`, plus `exit_code`), `ready` (the pane takes input),
  `placement` (`focused` | `front` | `background` | `minimized`), and, when
  the agent's hooks report it, `agent_state`: `working` (leave it),
  `needs_you` (hand it to the human, below), or `idle` (its turn ended;
  snapshot to read its last message). No `agent_state` means unknown, not
  idle: snapshot instead. Check when you need an answer; a worker's result
  reaches you through chat or its final message, so a timer adds nothing.
- **Talk to an agent:** chat. A post waits until the agent can take it —
  Ready, and Idle too when agent state is on — so a Working agent gets it
  after its turn; `chat --now` is for a steer that cannot wait. `foreman-chat`
  covers message content, targeting, and team rules.
- **Read a pane:** `snapshot --terminal tN --tail 40` — the last 40 lines,
  scrollback included.
- **Drive a program that is not an agent** — a dev server, REPL, or script
  prompt you launched: `send --terminal tN --keys "Ctrl+C"` or
  `` send --terminal tN --text "y`r" `` (bash: `--text $'y\r'`), then
  snapshot to see the effect. Agents get chat instead: `send` types straight
  into the pane past the delivery gate, so it can land mid-turn or answer
  the agent's permission prompt.
- **Hand a pane to the human:** `focus --terminal tN` brings it to the front
  and moves the human's keyboard there, mid-keystroke if they are typing.
  When the human asks to see a pane, focus it. When a worker you launched is
  `needs_you`, snapshot it to learn what it asks, post `@you` naming the
  pane and the question (`foreman-chat`), then focus it. One pane at a time;
  the human answers the prompt, never you.
- **Close:** `close tN` — a bare id, no `--terminal` — closes that pane and
  kills its process tree. Bare `close` closes your own pane, ending you.

`& $env:FOREMAN_EXE <verb> --help` has every flag.
