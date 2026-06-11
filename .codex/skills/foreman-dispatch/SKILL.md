---
name: foreman-dispatch
description: Use when running inside Foreman (the FOREMAN env var is 1) and Codex needs to launch an agent or command in a new visible terminal, dispatch or spawn a worker, open a pane with a prompt, or run a task in its own Foreman project terminal.
---

# Dispatch a Visible Agent Into Foreman

Precondition: `$env:FOREMAN` is `1`. If not, tell the user this needs to run
inside a Foreman terminal.

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

bash:

```bash
"$FOREMAN_EXE" open --title "agent - <short-label>" -- codex "<task prompt>"
```

Anything works after `--` (`codex`, `claude`, build scripts, custom commands).
The reply JSON gives the new terminal id and project id:

```json
{"ok":true,"terminal":"t3","project":"p1"}
```

The ids are assigned by Foreman. Do not invent them or predict them.

## Prompt Quoting

Foreman passes argv per argument, but Windows command shims still matter. If the
tool runs through a `.cmd` or `.bat` shim, Foreman refuses prompts containing
newlines, carriage returns, or literal `"` because `cmd.exe` cannot carry them
reliably.

Practical rule for Codex on Windows:

- For the npm-shim `codex`, use one-line, quote-free prompts.
- For multi-line prompts, use a native executable if available, or flatten the
  prompt before dispatch.
- Do not hand-escape a long prompt inline in PowerShell; simplify it instead.

Example one-line prompt:

```powershell
$prompt = 'Review src/wm.rs for local-vs-screen coordinate regressions and report findings only'
& $env:FOREMAN_EXE open --title "agent - review" -- codex $prompt
```

If Foreman reports a `cmd-shim` error, flatten the prompt to one quote-free line
or install/use a native executable for that tool.

## Targeting

- The worker opens in your current project by default (`FOREMAN_PROJECT_ID`).
- Use `--project pN` to target another Foreman project.
- Use `--cwd <dir>` to set the worker's process directory.

Examples:

```powershell
& $env:FOREMAN_EXE open --project p2 --title "agent - tests" -- codex "Run cargo test and summarize failures"
& $env:FOREMAN_EXE open --cwd "C:\code\repo" --title "cmd" -- cmd.exe
```

Coordinating multiple workers, posting updates, or using `@tN` targeting goes
through the `foreman-chat` skill. It carries the mixed-provider rule: Codex for
research/review, Claude for implementation/verification.
