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

- The worker appears as a new terminal in YOUR project (foreman reads
  `FOREMAN_PROJECT_ID` from your environment). Pass `--project pN` to target
  another project, `--cwd <dir>` to set its working directory.
- The reply JSON gives the new terminal's id — NOT the worker's results. This
  is fire-and-watch: the human supervises the worker's terminal. Do not poll
  for results; tell the user the agent is running and where.
- Nothing is Claude-specific: any CLI works after `--` (codex, plain
  commands, build scripts).
