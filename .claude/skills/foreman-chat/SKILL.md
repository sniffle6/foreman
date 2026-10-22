---
name: foreman-chat
description: Use when running inside foreman (the FOREMAN env var is 1) and agents need to coordinate through the project chat room — posting or reading chat, targeting messages with @-mentions, or dispatching a team of workers that discuss/divide work in the shared room.
---

# The foreman project chat room

**Chat mechanics are covered below.** Use `foreman-dispatch` for launching
free-form workers and `foreman-kanban` for cards. Read source or deeper docs
only when diagnosing behavior these instructions do not explain.
Precondition: `$env:FOREMAN` is `1`.

What it is: one room per project. Posts are **injected into every member's
terminal as typed input** — push, not pull; members never poll. Delivery
reaches a member wherever its pane sits, behind another tab or minimized,
so post to it where it is. The transcript is the roster (`tX` ids on every
line). The human watches and posts through the project's chat window.

**Cost rule: target by default.** Every delivered post costs each receiver a
full agent turn. Broadcast only when every member must act; otherwise use
`--to tX` or a leading `@tX`.

**Say only what is new.** The receiver already has its own prompt, card,
and repo. Never restate them, relay what it can read itself, or post routine
acknowledgements. Group related instructions for one receiver into one post.
Send it when it changes what that receiver does next, confirms something
consequential, or corrects an earlier message.

Use normal prose, punctuation, and spacing. Keep messages short by omitting
unnecessary detail. Put longer explanations in a referenced report.

## Recipe: post / read / target

    & $env:FOREMAN_EXE chat "claiming src/wm.rs"          # broadcast post
    & $env:FOREMAN_EXE chat --history                     # last 20 (catch-up read; never joins)
    & $env:FOREMAN_EXE chat --to t3 "rebase first"        # interrupt ONLY t3
    & $env:FOREMAN_EXE chat "@t3 rebase first"            # same, leading-@ sugar
    & $env:FOREMAN_EXE chat --to t2 --to t3 "you two own wm.rs"  # multi-target
    & $env:FOREMAN_EXE chat "@you tests red, need a decision"    # flag the human, wakes NO agent

(bash: `"$FOREMAN_EXE" chat "…"`.) Targeting rules:

- Only a LEADING run of `@tX`/`@you` targets; mid-sentence `@t3` is prose.
- Everyone still sees targeted messages in history/the window — mentions
  filter delivery (whose PTY is interrupted), never visibility.
- Targeted frames read `[chat p1 #N] t1→t3: text` — your id right of the
  arrow means it's addressed to you; act on it.
- Bad targets fail the WHOLE post loudly (unknown/exited/non-member/self,
  exit 1). On a stale id, re-read `--history` — a respawned worker has a
  new id. Client parse errors exit 2.

Delivery timing:

- With agent state enabled, normal posts wait until the receiver is Idle.
- Use `--now` before the message for a steer that must reach a Working agent:
  `foreman chat --now --to t3 "stop editing that file; ownership changed"`.
  It may pass earlier held posts. It uses paste plus Enter, not Esc/cancellation.
- Needs you (permission prompts) holds every post, including `--now`.
- No state yet or state disabled keeps Ready-only delivery. Hooks can be late;
  Claude Esc leaves stale Working, so `--now` also releases steers in that case.
- `--now` is post-only.

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

## Facts and traps

- Membership: dispatched workers auto-join; `kanban dispatch` card workers
  join hearing targeted posts only; any other terminal joins on its
  first post; `--history` never joins. `-p` workers can post but not receive.
- Interactive workers never auto-exit. A finished worker posts its done-signal,
  then may close its OWN pane with & $env:FOREMAN_EXE close (kills its whole
  process tree - the post must come first); the human closes whatever is left.
  Steer or stop a runaway worker with a targeted post.
- Wait for the dispatch reply before posting to the returned terminal id.
  Foreman holds delivery until the receiving Session is Ready. If delivery
  stalls, read the receiver's line in `& $env:FOREMAN_EXE status --json`:
  `ready: false` means it cannot take input yet; `agent_state` `working` or
  `needs_you` means the post is held by design. Sleeping releases nothing.
- A member stuck on `needs_you` is waiting on the human, and so is every
  post to it. Post `@you` naming the pane and what it asks; to put the human
  on it directly, see foreman-dispatch, "Your panes after launch".
- Seq gaps in `--history` are normal (join/exit events consume seqs).
- Inside chat messages avoid literal `"` characters; `--` ends flag parsing
  for dash-leading messages. For worker prompt quoting and cmd-shim errors,
  use `foreman-dispatch`.
