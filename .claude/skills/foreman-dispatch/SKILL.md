---
name: foreman-dispatch
description: Use when running inside foreman (the FOREMAN env var is 1) and you launch or manage visible terminals — dispatch/spawn a worker, open a pane with a prompt or command, check on a worker, read or drive another pane, hand a pane to the human, close one. Kanban cards and plans dispatch through foreman-kanban (`kanban dispatch`) instead.
---

# Launch and manage visible terminals in foreman

Everything needed to launch and manage panes is below, quoting safety
included. Foreman's source and docs are for changing foreman; read them only
to diagnose behavior this skill does not explain. Researching your task's
subject matter is separate and fine.
Precondition: `$env:FOREMAN` is `1`; if not, tell the user this needs to run
inside foreman.

**Kanban card or plan work goes through foreman-kanban: `kanban dispatch <id>
--agent …`.** `open` is a bare terminal: no card claim, no worktree, no
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

## The two commands (PowerShell)

    # Watchable (default): interactive — streams live, human can steer or
    # answer permission prompts in the worker's pane. Stays open when done.
    & $env:FOREMAN_EXE open --title "agent · <short-label>" -- claude "<task prompt>"

    # Fire-and-forget: exits on completion. Pane shows a dim
    # "── dispatched: … ──" banner then NOTHING until the final answer
    # (claude -p buffers; --verbose doesn't change that — verified).
    & $env:FOREMAN_EXE open --title "agent · <short-label>" -- claude -p "<task prompt>"

bash: `"$FOREMAN_EXE" open --title "agent · x" -- claude "<prompt>"`, and the
same `"$FOREMAN_EXE" <verb>` shape for every command below.
Anything works after `--` (codex, build scripts) — nothing is Claude-specific.

For a multi-line or complex prompt, build it as a single-quoted here-string
and pass the variable — this is the entire quoting story:

    $prompt = @'
    <full task prompt — backticks, $, &, | are all literal in here>
    '@
    & $env:FOREMAN_EXE open --title "agent · review" -- claude $prompt

## Quoting facts (verified in Session::spawn_argv — do not re-verify)

- Foreman passes argv **per-argument**. With a natively-installed `claude`
  (an `.exe`) the prompt arrives intact — newlines, spaces, `&`, `|`, `$`,
  `"`, backticks, everything.
- npm-installed `claude` is a `.cmd` shim routed through cmd.exe, which
  cannot carry newlines or `"` inside arguments — foreman refuses that
  dispatch loudly (`{"ok":false,…}` naming the cmd-shim) rather than
  truncate it. On that error: flatten the prompt to one `"`-free line, or
  have the user install claude natively.
- PowerShell itself is the bigger hazard: in a double-quoted string,
  backticks escape and `$` expands. The here-string pattern above sidesteps
  all of it; build every long prompt that way.
- **Variadic-flag trap:** `--allowedTools` etc. swallow every following word.
  Prompt IMMEDIATELY after `claude`/`claude -p`, flags after the prompt.

## Launch facts

- The worker opens in YOUR project (`FOREMAN_PROJECT_ID` from your env).
  `--project pN` targets another project; `--cwd <dir>` sets its directory.
- The reply JSON (`{"ok":true,"terminal":"t3","project":"p1"}`) gives the new
  terminal's id — **ids are assigned by foreman; record them, you cannot
  pick or predict them.** It is NOT the worker's result: fire-and-watch. Tell
  the user the agent is running and where.
- A `-p` worker's scrolled-away transcript is recoverable at
  `~\.claude\projects\<munged-cwd>\<session>.jsonl`.

## Your panes after launch

Every pane is reachable by its `tN` wherever it sits: behind another tab, in
a minimized window, or in a project tab the human isn't looking at. Read and
message panes where they are.

    & $env:FOREMAN_EXE status --json    # one JSON object per line, per project and per terminal

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
  after its turn; `chat --now` is for a steer that cannot wait. foreman-chat
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
  pane and the question (foreman-chat), then focus it. One pane at a time;
  the human answers the prompt, never you.
- **Close:** `close tN` — a bare id, no `--terminal` — closes that pane and
  kills its process tree. Bare `close` closes your own pane, ending you.

`& $env:FOREMAN_EXE <verb> --help` has every flag.
