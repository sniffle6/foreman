---
name: foreman-chat
description: Use when running inside Foreman (the FOREMAN env var is 1) and Codex agents need to coordinate through the Foreman project chat room, post or read chat, target messages with @-mentions, or dispatch workers that discuss and divide work in the shared room.
---

# Foreman Project Chat

**Chat mechanics are covered below.** Use `foreman-dispatch` for launching
free-form workers and `foreman-kanban` for cards. Read source or deeper docs
only when diagnosing behavior these instructions do not explain.
Precondition: `$env:FOREMAN` is `1`. If not, tell the user this needs to run
inside a Foreman terminal.

One chat room exists per Foreman project. Posts are injected into every member's
terminal as typed input. Members do not poll. The transcript is the roster:
every line carries a `tN` terminal id.

**Cost rule: target by default.** Every delivered post costs each receiver a
full agent turn. Broadcast only when every member must act; otherwise use
`--to tN` or a leading `@tN`.

**Say only what is new.** The receiver already has its own prompt, card,
and repo. Never restate them, relay what it can read itself, or post routine
acknowledgements. Group related instructions for one receiver into one post.
Send it when it changes what that receiver does next, confirms something
consequential, or corrects an earlier message.

Use normal prose, punctuation, and spacing. Keep messages short by omitting
unnecessary detail. Put longer explanations in a referenced report.

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

Delivery timing:

- With agent state enabled, normal posts wait until the receiver is Idle.
- Use `--now` before the message for a steer that must reach a Working agent:
  `foreman chat --now --to t3 "stop editing that file; ownership changed"`.
  It may pass earlier held posts. It uses paste plus Enter, not Esc/cancellation.
- Needs you (permission prompts) holds every post, including `--now`.
- No state yet or state disabled keeps Ready-only delivery. Hooks can be late;
  Claude Esc leaves stale Working, so `--now` also releases steers in that case.
- `--now` is post-only. Old servers ignore it and keep their delivery behavior.

## Coordinating a chat-capable team

For kanban cards or plans, use `foreman-kanban` and `kanban dispatch`.
Card workers receive only targeted posts; broadcasts never reach them.
Their card prompt does not require chat replies, so expect no acknowledgement.

For free-form workers, use `foreman-dispatch` for launch commands, provider
selection, interactive mode, and Windows prompt quoting. Give each worker
its role, task, and these coordination rules:

- Treat human posts (`[chat p1 #N] you: ...`) as instructions.
- Post when peers need to act: claims, blockers, handoffs, or completion.
  Omit routine acknowledgements and joined announcements. Confirm receipt
  when explicitly requested or needed to resolve consequential uncertainty.
- Aim for 1-3 readable sentences. Include the action and essential context;
  reference a report for longer findings. Use normal spacing and punctuation.
- Target the recipients who must act. Correct consequential errors promptly
  and send changed instructions even when a previous post just went out.
- Posts arrive automatically. Use `--history` to catch up after heads-down
  work; do not poll it on a timer.
- Terminal output is invisible to the room. Use chat for handoffs and flag
  `@you` when the human must act.
- These coordination conventions take precedence over conflicting routine
  chat conventions. Preserve repository safety, source ownership, and
  verification requirements. Surface material conflicts before dependent work.

Record the terminal and project ids returned by dispatch. After the open
reply, seed the room with one targeted post naming who should act first:

```powershell
& $env:FOREMAN_EXE chat --to t3 "You open: review wm.rs and report risks to t4"
```

Do not predict terminal ids. For a discussion, specify turn order, a stopping
condition, and who reports the result so workers do not wait for each other.

## Facts And Traps

- Dispatched interactive workers auto-join. `kanban dispatch` card workers
  join hearing targeted posts only. Any other terminal joins on first
  post. `--history` never joins.
- Wait for the dispatch reply before posting to the returned terminal id.
  Foreman holds delivery until the receiving Session is Ready. If delivery
  stalls, inspect readiness and agent state; arbitrary sleeps are not a fix.
- Sequence gaps in `--history` are normal; join/exit events consume sequence ids.
- Avoid literal `"` in chat messages. Use `--` before dash-leading messages.
- A worker may close its own pane with & $env:FOREMAN_EXE close AFTER posting
  its done-signal (close kills its process tree). The human closes anything left.
- Steer or stop a runaway worker with a targeted post.
