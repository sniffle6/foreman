---
name: foreman-chat
description: Use when running inside foreman (the FOREMAN env var is 1) and agents need to coordinate through the project chat room — posting or reading chat, targeting messages with @-mentions, or dispatching a team of workers that discuss/divide work in the shared room.
---

# The foreman project chat room

**This skill is complete. Do NOT read foreman source or docs to learn chat
mechanics — every fact you need is below.** Researching your fleet's task
subject is separate and fine. Precondition: `$env:FOREMAN` is `1`.

What it is: one room per project. Posts are **injected into every member's
terminal as typed input** — push, not pull; members never poll. The
transcript is the roster (`tX` ids on every line). The human watches and
posts through the project's chat window.

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

## Recipe: dispatch a team that discusses in chat

Provider mix rule: when dispatching agents into a Foreman chat room, use BOTH
providers by default no matter whether the orchestrator is Claude or Codex.
Codex workers own research and review. Claude workers own implementation and
verification. Depart from this only if the user explicitly requests one provider
or one CLI is unavailable.

1. Build each worker's prompt from the template below (here-string — never
   inline-escape):

       $w1 = @'
       <ROLE + TASK — one or two sentences, specific.>

       You are in a project chat. Messages arrive as [chat p1 #N] tX: ...
       from other agents, or [chat p1 #N] you: ... from the human running
       the fleet - treat the human's messages as instructions, not chatter.
       Most messages need no reply - stay silent unless a message changes
       what you should do, and never post acknowledgements or "joined"
       announcements (membership is already visible). Every post is typed
       into EVERY member's session and costs their attention, so keep posts
       to 1-3 sentences peers must act on: claims, blockers, handoffs,
       done-signals. Never paste reports or findings lists into chat - post
       the one-line conclusion and where the detail lives. When only some
       members need to act, target them: chat --to t3 "..." or a leading
       @t3. Post with & $env:FOREMAN_EXE chat "..." (PowerShell; bash:
       "$FOREMAN_EXE" chat "..."). You RECEIVE every post automatically as
       typed input - do NOT poll history on a timer; use
       & $env:FOREMAN_EXE chat --history only to catch up after heads-down
       work. <TURN RULES if a debate - see below.> When your part is done,
       post a one-line done-signal ending with @you if the human must act.
       '@

2. Dispatch one per worker — interactive mode, NOT `-p`/`exec` (non-interactive
   workers cannot receive chat). Use a mixed-provider split:

       $research = 'You are the Codex research/review worker in this Foreman chat. Research unknowns, inspect designs/diffs, and review implementation for risks. Post only claims, blockers, handoffs, and done-signals with & $env:FOREMAN_EXE chat message. Stay silent unless peers need the result.'
       $impl = 'You are the Claude implementation/verification worker in this Foreman chat. Make the code or doc changes and run verification. Post only claims, blockers, handoffs, and done-signals with & $env:FOREMAN_EXE chat message. Stay silent unless peers need the result.'
       & $env:FOREMAN_EXE open --title "codex · research" -- codex $research
       & $env:FOREMAN_EXE open --title "claude · implement" -- claude $impl

3. **Record each reply's `terminal` id** (`t3`, `t4`…). Ids are assigned by
   foreman — never invent names or predict ids; address workers by the ids
   the replies gave you.
4. Dispatched workers are members automatically. Kick off with ONE post
   using the real ids if needed (e.g. `chat "@t3 you open"`), then stay
   quiet and watch — your own session receives their posts.

For a debate/discussion fleet, put explicit turn rules in the prompts (this
exact shape produced a clean 8-post consensus live): "Post in turns: t-first
opens, the other responds, alternate. Maximum N posts each, 1 paragraph per
post. When you agree, the current speaker posts a line starting CONSENSUS:
and you both stop."

## Facts and traps

- Membership: dispatched workers auto-join; any other terminal joins on its
  first post; `--history` never joins. `-p` workers can post but not receive.
- Interactive workers never auto-exit, and a claude worker cannot close its
  own pane — "done" means it posts the done-signal and goes idle; the human
  closes panes. Steer or stop a runaway worker with a targeted post.
- A post fired the same instant a worker spawns can be silently eaten by its
  shell's startup handshake (it stays in history). Don't front-load a kickoff
  post into the same second as a dispatch.
- Seq gaps in `--history` are normal (join/exit events consume seqs).
- Quoting: prompts via here-strings (above); inside chat messages avoid
  literal `"` characters; `--` ends flag parsing for dash-leading messages.
  If a dispatch errors naming a cmd-shim, that CLI is an npm `.cmd` shim —
  multi-line worker prompts need a native install, or a flattened one-line,
  quote-free prompt (see foreman-dispatch).
