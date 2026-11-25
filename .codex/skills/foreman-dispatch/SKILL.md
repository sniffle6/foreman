---
name: foreman-dispatch
description: Use when running inside Foreman (the FOREMAN env var is 1) and Codex launches or manages visible terminals — dispatch or spawn a worker (here or on another PC with --on), open a pane with a prompt or command, check on a worker, read or drive another pane, hand a pane to the human, close one. Kanban cards and plans use foreman-kanban for continuing assignments or authorized delegation.
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

## Another PC: `--on`

`--on R` runs the command on Remote R over ssh. The pane is here, the
process is there, and it is an ordinary member of THIS project's chat.

```powershell
& $env:FOREMAN_EXE remotes                    # names --on accepts, with ssh targets
& $env:FOREMAN_EXE open --on desk --title "agent - x" -- codex "<task prompt>"
```

- **`--on` or `--host`?** Use `--on` to put a worker on another PC and run
  it from here: you chat, snapshot, send to and close it like any pane.
  `--host` talks to a separate Foreman already running on that PC, with its
  own room and panes (`foreman-chat`, "Another PC"). A PC that `remotes`
  lists takes `--on`.
- It starts in the Project's directory mapped on R. `--cwd D` names a
  directory on R instead, for this launch only. With neither, open is
  refused: pass `--cwd` or ask the human. Never guess a directory.
- Remotes are added by the human in Settings > Remotes. Do not edit
  settings.json for them.
- **The agent on R has no Foreman CLI and no skills.** Opened on a POSIX
  Remote as `codex "<brief>"` or `claude "<brief>"` (`--model <id>` allowed
  after the brief), it gets a reply note at the end of its prompt: the
  `curl` command that posts to this room, naming `$FOREMAN_URL` and
  `$FOREMAN_TOKEN`, which its tool shell has. A Claude whose brief starts
  with `/`, or one in a shape Foreman cannot place it in, gets it typed once its
  hooks report it started. Grok and Codex in another shape get it typed once
  the pane shows output. **A Claude with no hooks (a `--settings` of yours,
  a wrapper such as `npx`, anything on a Windows Remote) cannot reply until
  you post to it:** nothing is typed into it at launch, since keys then
  answer its trust dialog "No, exit", so its first chat post carries the
  note. Post to it once (for example "report here when done"). A bare agent,
  and any command that is not the agent's own program (`env X=1 codex …`,
  `bash -c "…"`, a script; `npx` or `wsl` naming the agent counts as it),
  also gets the note with its first post. It joins
  the room at its end: posts made before it opened never
  reach it. Ask it to report by chat and leave the Foreman commands out of
  its brief.
- Codex on R in a directory it has not trusted could not post: its tool
  sandbox refused the `curl` to the room ("Failed to connect"), though the
  Tunnel answered a plain shell there. Use a directory Codex already trusts.
- A Remote agent can post chat (with `to`, `re`, `now`), read chat history
  and read `status`. It cannot open, send to, close or snapshot panes, or
  use the kanban board. You drive; it reports.
- `agent_state` on a Remote Session: only for Claude on a POSIX Remote,
  started as plain `claude` (no wrapper, no `--settings` of yours). Codex,
  Grok and Windows Remotes have none, so posts to them are not held for
  Idle and `status` never says idle: snapshot to see where they are. A
  first launch in a directory Claude has not trusted shows no state while
  the trust dialog waits. Nothing is typed into a Remote Claude until its
  hooks report it past the dialog (its brief submitted, or 2 s after a bare
  one started), so posts to it wait and arrive in order once the human
  answers the dialog. Ask the human to answer it, or use a directory it trusts.
  While it waits its `status --json` line has `chat_held: true`. If no hook
  will come (hooks off or unable to reach back there, or a first argument
  that is a subcommand such as `claude agents`), it waits forever: once the
  human has answered the dialog, `deliver tN` (this PC only) sends what it
  holds. Never before: the held input would answer the dialog.
- Hookless agents on R (Codex, Grok, Claude with your `--settings`, Windows
  Remotes) are not held. A post to one is typed once its pane shows output,
  so a post sent while its trust dialog shows answers it, and so does
  Codex's typed note: Enter at Codex's "update available" prompt picks
  "Update now", which runs `npm install -g` on R and exits Codex. Open them
  in a directory they trust, or post only after the human has answered.
- In `status --json` its line carries `remote` (the Remote's name). Its ssh
  dropping shows as `state: exited` with `disconnected: true`: the pane
  waits for the human's Enter to reconnect, and Foreman never reconnects by
  itself. `reconnect tN` does what that Enter does, from this PC only; use
  it when the human asks. Until then `chat --to` it fails as exited.
- A reconnect never replays a brief. An agent launched by name or path
  resumes its last conversation (Claude, Codex) without its original prompt
  or flags, or starts fresh; one launched through `npx` or `wsl` starts
  fresh. Re-brief it by chat. A command that names no agent (`vi notes.md`)
  reruns as it was. Any other command that names one (`env X=1 codex
  "brief"`, `bash -c 'claude …'`, even `vi codex.md`) reconnects to a shell
  in its directory and runs nothing; the human reruns it if they want.
- A Windows Remote may refuse a long prompt for its command-line limit. Do
  what the error says: open without the prompt and send the brief by chat.
- `kanban dispatch` has no `--on`; card work runs on this PC.

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
