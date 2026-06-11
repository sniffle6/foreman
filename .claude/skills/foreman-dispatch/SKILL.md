---
name: foreman-dispatch
description: Dispatch a visible worker agent into a new foreman terminal. Use when the user asks to dispatch/spawn an agent (or run a task) in a new visible terminal while running inside foreman (the FOREMAN env var is set to 1).
---

# Dispatch a visible agent into foreman

Only available when running inside a foreman terminal — check `$env:FOREMAN`
is `1` first; if not, tell the user this needs to run inside foreman.

Dispatch (PowerShell) — pick the mode by whether the human wants to watch:

    # Watchable (default): interactive session — streams output live, shows
    # the working spinner, and the human can steer or answer permission
    # prompts directly in the worker's terminal. Stays open when done.
    & $env:FOREMAN_EXE open --title "agent · <short-label>" -- claude "<full task prompt>"

    # Fire-and-forget: exits on completion. The pane opens with a dim
    # "── dispatched: <command> ──" banner (foreman injects it), then shows
    # nothing further until the final answer (claude -p buffers; --verbose
    # does not change this — verified). Use when nobody needs progress
    # feedback; silence below the banner means "working".
    & $env:FOREMAN_EXE open --title "agent · <short-label>" -- claude -p "<full task prompt>"

If a `-p` worker's text scrolls away or can't be copied, its full transcript
is recoverable at `~\.claude\projects\<munged-cwd>\<session>.jsonl`.

**Variadic-flag trap (live failure, 2026-06-10):** flags like
`--allowedTools` consume every following word, so
`claude --allowedTools Bash Read "<prompt>"` swallows the prompt as a tool
name — the worker starts with NO task (interactive: an empty REPL; `-p`:
exits 1 "Input must be provided"). Put the prompt IMMEDIATELY after
`claude` / `claude -p`, before any variadic flags:
`claude -p "<full task prompt>" --allowedTools Bash Read Grep`.

- The worker appears as a new terminal in YOUR project (foreman reads
  `FOREMAN_PROJECT_ID` from your environment). Pass `--project pN` to target
  another project, `--cwd <dir>` to set its working directory.
- The reply JSON gives the new terminal's id — NOT the worker's results. This
  is fire-and-watch: the human supervises the worker's terminal. Do not poll
  for results; tell the user the agent is running and where.
- Nothing is Claude-specific: any CLI works after `--` (codex, plain
  commands, build scripts).

---

## Worker mode choice

Pick the mode based on whether the worker needs to receive coordination:

**Fire-and-forget (`claude -p "<prompt>"`):**
- Cannot *receive* chat mid-run — print mode does not read stdin.
- *Can* post (`foreman chat` is a process spawn, not a stdin read).
- The pane stays silent under the dispatch banner until the final answer.
- Pattern: end the prompt with "post your result with
  `foreman chat \"<summary>\"` before exiting." The human sees the result
  arrive in their own session's chat, not just the worker's scrollback.

**Steerable/collaborative (`claude "<prompt>"`):**
- Receives every post as typed input — the project chat broadcasts directly
  into the worker's PTY.
- Does not auto-exit; the session stays open when the task is done.
- To stop it: post an instruction addressed to it in the chat, or include
  exit-when-done language in the initial prompt.

---

## Chat usage

Post to the project room (same-project only). Invoke via `FOREMAN_EXE`
since the exe directory is not on PATH inside spawned shells
(PowerShell: `& $env:FOREMAN_EXE chat "…"`; bash: `"$FOREMAN_EXE" chat "…"`):

    & $env:FOREMAN_EXE chat "message text"                # post a message
    & $env:FOREMAN_EXE chat --history                     # read last 20 messages
    & $env:FOREMAN_EXE chat --history 50                  # read last 50 messages
    & $env:FOREMAN_EXE chat --project p2 "message"        # override env project
    & $env:FOREMAN_EXE chat -- --message-starting-with-dashes   # -- ends flag parsing

Target a message so it interrupts ONLY the named terminals (everyone still
sees it in history and the chat window — mentions filter delivery, not
visibility):

    & $env:FOREMAN_EXE chat --to t3 "rebase first, then rerun"   # flag form
    & $env:FOREMAN_EXE chat "@t3 rebase first, then rerun"       # leading-@ sugar
    & $env:FOREMAN_EXE chat --to t2 --to t3 "you two own src/wm.rs"  # multi-target
    & $env:FOREMAN_EXE chat "@you tests are red, need a decision"    # flag the human

Mention rules:

- Only a LEADING run of `@tX` / `@you` tokens targets — `@t3` mid-sentence is
  prose and never narrows delivery. `--` does not suppress a leading mention.
- Targeted frames read `[chat p1 #N] t1→t2,t3: text`. If your id is right of
  the arrow, the message was addressed to you specifically — act on it.
- Bad targets fail the WHOLE post loudly (unknown id, exited terminal,
  non-member, yourself). On a stale-id error, re-read `--history` — the
  transcript is the roster; a respawned worker has a new id.
- `@you` reaches the human through the chat window without interrupting any
  agent — the cheapest way to flag something for the fleet runner.

`foreman chat` reads `FOREMAN_PROJECT_ID` and `FOREMAN_TERMINAL_ID` from
the environment automatically. Calling it outside a foreman terminal
(env vars unset) is an immediate error (exit 2, no pipe call).

History output is one line per message, oldest first (seq gaps are normal —
join/exit events consume seqs but are excluded from history):

    #12 t1: split the work by module
    #14 t3: I'll take src/wm.rs

---

## Standing convention — inject into every dispatched prompt

**Copy this paragraph verbatim into every prompt sent to a dispatched
agent.** It tells the agent how to parse incoming chat and when to respond:

> You are in a project chat. Messages arrive as `[chat p1 #N] tX: …` from
> other agents, or `[chat p1 #N] you: …` from the human running the fleet —
> treat the human's messages as instructions, not chatter. Most messages
> need no reply — stay silent unless a message changes what you should do,
> and never post acknowledgements ("noted", "no reply needed"). Every post
> is typed into EVERY member's session and costs their attention, so keep
> posts to 1–3 sentences peers must act on: claims ("taking src/wm.rs"),
> blockers, handoffs, done-signals. Never paste reports, findings lists, or
> file-by-file detail into chat — detail belongs in your final answer or a
> file; post the one-line conclusion and where the detail lives. When only
> some members need to act, target them (`chat --to t3 "…"` or a leading
> `@t3`) — a broadcast wakes every member and costs their attention. Post with
> `& $env:FOREMAN_EXE chat "…"` (the agent reads the `FOREMAN_EXE` env
> var; expansion syntax varies by shell — PowerShell uses
> `& $env:FOREMAN_EXE`, bash uses `"$FOREMAN_EXE"`). Check
> `& $env:FOREMAN_EXE chat --history` after long heads-down stretches.

The framing format (`[chat p1 #14] t2: text`) gives the agent provenance
(project, seq number, sender) so it can reference earlier messages and know
which project it is in. The sender is a terminal id (`t2`) for agent posts,
or the reserved `you` for posts the human types into the project's chat
window — `you` is not a terminal and cannot be a dispatch target.

---

## Fire-and-forget reporting pattern

End the worker's prompt with:

> When you have a result, post your summary with
> `& $env:FOREMAN_EXE chat "<summary>"` before exiting.
> (PowerShell: `& $env:FOREMAN_EXE chat "…"`; bash: `"$FOREMAN_EXE" chat "…"`.
> The exe directory is not on PATH inside spawned shells — always invoke via
> the `FOREMAN_EXE` env var.)

This ensures the orchestrator (and any peer workers) see the outcome in the
chat log even if nobody is watching the worker's terminal directly.
