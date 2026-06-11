---
name: foreman-dispatch
description: Use when running inside foreman (the FOREMAN env var is 1) and you need to launch an agent or command in a new visible terminal — dispatch/spawn a worker, open a terminal with a prompt, run a task in its own pane.
---

# Dispatch a visible agent into foreman

**This skill is complete. Do NOT read foreman source or docs to learn
dispatch mechanics — every fact you need (including quoting safety) is
below.** Researching your task's subject matter is separate and fine.
Precondition:
`$env:FOREMAN` is `1`; if not, tell the user this needs to run inside foreman.

## The two commands (PowerShell)

    # Watchable (default): interactive — streams live, human can steer or
    # answer permission prompts in the worker's pane. Stays open when done.
    & $env:FOREMAN_EXE open --title "agent · <short-label>" -- claude "<task prompt>"

    # Fire-and-forget: exits on completion. Pane shows a dim
    # "── dispatched: … ──" banner then NOTHING until the final answer
    # (claude -p buffers; --verbose doesn't change that — verified).
    & $env:FOREMAN_EXE open --title "agent · <short-label>" -- claude -p "<task prompt>"

bash: `"$FOREMAN_EXE" open --title "agent · x" -- claude "<prompt>"`.
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
  cannot carry newlines or `"` inside arguments — foreman REFUSES that
  dispatch loudly (`{"ok":false,…}` naming the cmd-shim) rather than
  truncate it. On that error: flatten the prompt to one `"`-free line, or
  have the user install claude natively.
- PowerShell itself is the bigger hazard: in a double-quoted string,
  backticks ESCAPE and `$` expands. The here-string pattern above sidesteps
  all of it. Never hand-escape a long prompt inline.
- **Variadic-flag trap:** `--allowedTools` etc. swallow every following word.
  Prompt IMMEDIATELY after `claude`/`claude -p`, flags after the prompt.

## Facts

- The worker opens in YOUR project (`FOREMAN_PROJECT_ID` from your env).
  `--project pN` targets another project; `--cwd <dir>` sets its directory.
- The reply JSON (`{"ok":true,"terminal":"t3","project":"p1"}`) gives the new
  terminal's id — **ids are assigned by foreman; you cannot pick or predict
  them.** It is NOT the worker's result: fire-and-watch, do not poll. Tell
  the user the agent is running and where.
- A `-p` worker's scrolled-away transcript is recoverable at
  `~\.claude\projects\<munged-cwd>\<session>.jsonl`.

**Coordinating multiple workers, or anything involving the project chat
room? → use the foreman-chat skill.**
