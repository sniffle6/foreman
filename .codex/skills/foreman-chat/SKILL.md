---
name: foreman-chat
description: Use when running inside Foreman (the FOREMAN env var is 1) and Codex agents need to coordinate through the Foreman project chat room, post or read chat, target messages with @-mentions, or dispatch workers that discuss and divide work in the shared room.
---

# Foreman Project Chat

Precondition: `$env:FOREMAN` is `1`. If not, tell the user this needs to run
inside a Foreman terminal.

One chat room exists per Foreman project. Posts are injected into every member's
terminal as typed input. Members do not poll. The transcript is the roster:
every line carries a `tN` terminal id.

## Post / Read / Target

PowerShell:

```powershell
& $env:FOREMAN_EXE chat "claiming src/wm.rs"
& $env:FOREMAN_EXE chat --history
& $env:FOREMAN_EXE chat --to t3 "rebase first"
& $env:FOREMAN_EXE chat "@t3 rebase first"
& $env:FOREMAN_EXE chat --to t2 --to t3 "you two own wm.rs"
& $env:FOREMAN_EXE chat "@you tests red, need a decision"
```

bash:

```bash
"$FOREMAN_EXE" chat "claiming src/wm.rs"
```

Targeting rules:

- Only a leading run of `@tN` / `@you` targets. Mid-sentence `@t3` is prose.
- Everyone still sees targeted messages in history and the chat window.
  Targeting filters which PTY receives the injected interrupt, not visibility.
- Targeted frames read `[chat p1 #N] t1->t3: text`; your id right of the arrow
  means the message is addressed to you.
- Bad targets fail the whole post loudly: unknown, exited, non-member, or self
  targets return exit 1. Re-read `--history` after stale ids.
- Client parse errors exit 2.

## Dispatching a Chat-Capable Team

Interactive workers can receive chat. `codex exec` workers cannot, because they
are non-interactive and exit on completion.

Provider mix rule: when dispatching agents into a Foreman chat room, use BOTH
providers by default no matter whether the orchestrator is Codex or Claude.
Codex workers own research and review. Claude workers own implementation and
verification. Depart from this only if the user explicitly requests one provider
or one CLI is unavailable.

For Codex on Windows, prefer compact one-line, quote-free worker prompts unless
the `codex` command is a native executable. The npm shim cannot carry newlines or
literal `"` through Foreman's dispatch path.

Example:

```powershell
$research = 'You are the Codex research/review worker in this Foreman chat. Research unknowns, inspect designs/diffs, and review implementation for risks. Post only claims, blockers, handoffs, and done-signals with & $env:FOREMAN_EXE chat message. Stay silent unless peers need the result.'
$impl = 'You are the Claude implementation/verification worker in this Foreman chat. Make the code or doc changes and run verification. Post only claims, blockers, handoffs, and done-signals with & $env:FOREMAN_EXE chat message. Stay silent unless peers need the result.'
& $env:FOREMAN_EXE open --title "codex - research" -- codex $research
& $env:FOREMAN_EXE open --title "claude - implement" -- claude $impl
```

Record the returned terminal id (`t3`, `t4`, etc.). Ids are assigned by Foreman.
Use the real ids for targeted kickoffs:

```powershell
& $env:FOREMAN_EXE chat "@t3 review wm.rs; @t4 review terminal.rs"
```

For debate/discussion fleets, put explicit turn rules in each prompt:
"Post in turns. First worker opens, second responds, then alternate. Maximum N
posts each. When you agree, the current speaker posts a line starting CONSENSUS:
and both stop."

## Facts And Traps

- Dispatched interactive workers auto-join. Any other terminal joins on first
  post. `--history` never joins.
- A post fired at the same instant a worker spawns can be eaten by shell startup.
  Wait for the open reply and give the pane a moment before a kickoff post.
- Sequence gaps in `--history` are normal; join/exit events consume sequence ids.
- Avoid literal `"` in chat messages. Use `--` before dash-leading messages.
- A worker cannot close its own pane. Done means it posts the done-signal and
  idles; the human closes the pane.
- Steer or stop a runaway worker with a targeted post.
