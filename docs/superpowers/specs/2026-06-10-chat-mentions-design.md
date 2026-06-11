# Project Chat @-Mentions — v2 Design (Agent-Debate Consensus)

**Date:** 2026-06-10
**Status:** Drafted — consensus reached; rows 1–4 and 6 implemented 2026-06-10
(see `2026-06-10-chat-mentions-impl-design.md`). Row 5 (quiescence gating)
still needs its own spike.
**NOT uniformly shovel-ready:** decision-table rows 1–4 and 6 are ready to
implement; row 5 (quiescence gating) is design-settled but its core
mechanism is an **unsolved problem** — see the warning after the TL;DR.
**Builds on:** `docs/superpowers/specs/2026-06-10-agent-group-chat-design.md`
(the shipped v1 group chat), `docs/epics/agent-dispatch-epic.md`
**Provenance:** This design was produced *by* the feature it extends — two
dispatched Claude agents ("Architect" pro-mentions, "Skeptic" pro-broadcast)
debated it live in the p1 project chat room and converged. Full transcript in
the appendix. Treat the consensus as a strong proposal, not a human-approved
decision.

## TL;DR — the consensus design

> **@-mentions are a pure delivery filter, never a visibility filter.**
> A mentioned message interrupts only the targeted terminal's PTY, but every
> message — mentioned or not — still lands in the single shared project chat
> history. Targets are the stable terminal ids (`tX`) already stamped on every
> chat line, so **the transcript is the roster**: no name registry, no
> orchestrator-side cache. Unknown or stale ids fail loudly at send time.
> Broadcast remains the default. **Quiescence-gated injection is adopted for
> all deliveries** (mentioned and broadcast alike). Scope is frozen there —
> no threads, no nicknames, no DMs, no read-state.

## ⚠ Before scheduling this as a feature, read this

**Quiescence gating (decision-table row 5) has an unsolved "how" at its
center, and the consensus logic leans on it.** Three things a fresh session
must know:

1. **"Between turns" is not observable from where foreman sits.** Foreman
   talks to a PTY — bytes in, bytes out. It cannot see whether the agent is
   mid-tool-call, parked at a permission prompt, or idle at its input box.
   Any gate is a heuristic (output-idle threshold, prompt-redraw detection,
   …), and heuristics here fail in the worst direction: inject at the wrong
   moment and you corrupt an agent's in-flight work — the exact failure the
   gate exists to prevent. The existing 150 ms deferred-`\r` hack (commit
   `45f4725`) shows how fragile this layer already is; that fix took a live
   failure to discover.
2. **The debate's logic is load-bearing on the gate.** Skeptic conceded the
   splice/disruption argument *because* gating was assumed to solve it,
   which let the debate split the problem into WHEN (gating) vs WHETHER
   (mentions). If gating proves unbuildable, mentions must carry the
   splice-safety argument too, and the rationale in the decision table is
   no longer quite the one that was agreed.
3. **Consequence for planning:** rows 1–4 and 6 (delivery-filter semantics,
   tX addressing, loud send-time errors, broadcast default, scope freeze)
   can be implemented directly. Row 5 needs a spike or its own design pass
   FIRST. Do not schedule this doc as one feature.

## Context for a zero-context session

### What exists today (v1, shipped on `feature/agent-dispatch`)

- `foreman chat "<msg>"` posts to a per-**project** room; `foreman chat
  --history [N]` reads. CLI fills `project`/`from` from `FOREMAN_PROJECT_ID` /
  `FOREMAN_TERMINAL_ID` env vars (present in every PTY foreman spawns).
- Transport: the existing control named pipe (`\\.\pipe\foreman`), JSON line
  request/reply, `chat` verb — `src/control.rs`.
- The room log is in-memory on the project's nested `WindowManager`
  (`Vec<ChatMsg> { seq, from, text }`) — `src/wm.rs`. Dies with foreman.
- **Delivery is broadcast injection**: every post is written into every
  *member* terminal's PTY as typed input (bracketed paste + a **deferred
  `\r`** ~150 ms later — a back-to-back `\r` gets folded into Claude Code's
  paste-burst detection and never submits; fixed in commit `45f4725`).
  Injection path: `Session::inject_input` in `src/terminal.rs`.
- Framing as seen by an agent: `[chat p1 #14] t2: <text>` — project, seq,
  sender terminal id.
- Membership (`chat_member: bool` on `Tab`): dispatched terminals auto-join;
  others join on first post; `--history` never joins. Sender never receives
  its own post.
- A read-only `Content::Chat` viewer window per project shows the log to the
  human.
- v1 explicitly deferred @-mentions and rate limiting: *"Revisit (rate
  limits, @-mentions) only if it bites."* This doc is the revisit.

### The problem v2 addresses

Broadcast delivery costs **O(N) agent-turns per message**: every member's
Claude session wakes, reads the injected line, and burns a full turn deciding
the message wasn't for it. At 2–4 members this is tolerable (v1's accepted
risk); as fleets grow it becomes a denial-of-attention on your own workers.
Separately, mid-task injection can splice into a worker's input box or
answer a pending permission prompt (v1 accepted risk).

## Design decisions and their rationale

| # | Decision | Rationale (from the debate) |
|---|---|---|
| 1 | Mentions filter **PTY delivery only**; the shared history shows every message | Private lanes (DMs) destroy the single observable transcript that makes multi-agent coordination debuggable — for agents *and* the watching human. This was Skeptic's strongest objection and Architect conceded it fully. |
| 2 | Targets are the existing **`tX` terminal ids** | The transcript already stamps `tX` on every line, so the roster *is* the transcript — no name registry to build, no orchestrator-side fleet cache to rot. A respawned worker has a new `tX`; the old id errors at send; the orchestrator re-reads history to find the live one. Failure is loud and self-correcting. |
| 3 | Unknown/stale target ⇒ **loud send-time error** | Inverts the silent-drop failure mode. (Broadcast's silent failure is social: "everyone assumed someone else handled it.") |
| 4 | **Broadcast stays the default**; mentions are opt-in per message | Mentions are pure addition for the orchestrator-steers-one-worker case; peer-awareness broadcasts (status reports, "I'm taking file X") remain the norm. |
| 5 | **Quiescence-gated injection for ALL deliveries** — ⚠ design-settled, implementation UNSOLVED (see warning above) | Skeptic's key insight: PTY-splice damage is a WHEN problem, not a WHO problem. Queue injections until the target is between turns and the splice/spurious-turn cost disappears for every message with zero naming infrastructure. Mentions then solve only what they uniquely solve: the WHETHER (idle-wake O(N) token cost). |
| 6 | **Scope frozen**: no threads, no nicknames, no DMs, no read-state | Skeptic's closing scope guard — "pure addition is how every chat system grows threads, DMs, and read-receipts." The consensus holds only for exactly this minimal shape. |

### What each side conceded (useful when re-litigating)

- **Architect conceded:** shared-transcript observability is non-negotiable —
  so mentions must not gate visibility, only interruption.
- **Skeptic conceded:** (a) the O(N) idle-wake token cost is real and
  broadcast-only has no answer at scale; (b) transcript-as-roster dissolves
  the registry/staleness objection; (c) delivery-filter-over-shared-history
  is categorically better than DMs.
- **Both adopted:** quiescence gating, as orthogonal to and independently
  valuable from mentions.

## Open implementation questions (NOT settled by the debate)

1. **Quiescence detection is the hard part.** Foreman sees a PTY byte
   stream, not Claude's turn state. Candidate heuristics: output-stream
   idle-time threshold; detecting the input-prompt redraw; cursor-position
   queries. None validated. This is the riskiest piece and arguably its own
   spec. Note the deferred-`\r` mechanism (commit `45f4725`) is a primitive
   ancestor of this — a fixed 150 ms delay, not actual quiescence.
2. **Mention syntax.** Inline parse (`@t3` anywhere in the text) vs explicit
   flag (`foreman chat --to t3 "msg"`). A flag is unambiguous and trivially
   validated client-side; inline `@tX` reads naturally in the transcript.
   Could do both (flag is the mechanism, inline is sugar). Undecided.
3. **Multiple targets per message?** (`--to t2 --to t3`). Unaddressed.
4. **Mentioning a non-member.** Error, deliver-anyway, or auto-join? v1
   membership semantics (auto-join on dispatch / join on first post) need a
   rule here.
5. **Does a mention of an exited-but-still-open terminal error or no-op?**
   v1 broadcast silently skips exited sessions; a *targeted* delivery to an
   exited session probably should error (consistent with decision 3).
6. **History rendering of mentions** — should the viewer window and
   `--history` output mark targeted messages (e.g. `#15 t1→t3: …`)? Leaning
   yes; the transcript must reveal that a message did not interrupt everyone,
   or observability quietly degrades.

## Likely code surface (orientation, not a plan)

- `src/control.rs` — `chat` verb gains an optional target field; client-side
  validation; send-time unknown-id error.
- `src/wm.rs` — broadcast loop becomes target-aware; `ChatMsg` likely gains
  an optional `to`; quiescence queue per member terminal.
- `src/terminal.rs` — `inject_input` / `Session` grows whatever signal
  quiescence detection needs (output idle tracking lives near the reader
  thread / pump).
- Chat viewer rendering (`Content::Chat` in `src/wm.rs`) — mention markup.
- `.claude/skills/foreman-dispatch/SKILL.md` — standing convention gains
  mention usage guidance.
- Tests follow the v1 spec's pattern (§6 of the v1 design): real-PTY
  integration tests, framing-byte assertions, plus new target-validation and
  gating-order cases.

## Appendix — full debate transcript (p1 room, 2026-06-10)

Setup: t1 (orchestrator) dispatched t5 "Architect" (pro-mentions) and t4
"Skeptic" (pro-broadcast), both interactive Claude sessions, turn-taking and
length limits in their prompts. Everything below coordinated itself through
the chat room with no orchestrator mediation.

> **#6 t5 Architect:** As agent counts scale, broadcast-only chat becomes a
> denial-of-attention attack on your own fleet — every message lands in every
> PTY, and each worker burns tokens deciding a message wasn't for them.
> @-mentions give an orchestrator surgical delivery: steer one worker
> mid-task without waking ten others or polluting their context windows.
> Broadcast stays as the default; mentions are pure addition, not a
> replacement.
>
> **#7 t4 Skeptic:** The 'denial-of-attention' framing overstates the cost —
> skimming an irrelevant one-liner is cheap for an agent, while @-mentions
> buy that saving with a name registry, dead-target delivery failures, and
> silently dropped messages nobody notices. Broadcast has a property mentions
> destroy: every agent and the watching human share one observable
> transcript, so coordination bugs are visible instead of hidden in private
> lanes. 'Pure addition' is how every chat system grows threads, DMs, and
> read-receipts — the simplicity loss is the feature working as designed.
>
> **#8 t5 Architect:** The shared-transcript point is genuinely good and I
> concede it — debugging coordination from private lanes is misery, so
> @-mentions must filter *delivery to PTYs*, not visibility: every message
> still lands in the one chat history, mentions only control whose terminal
> gets interrupted. And the skim isn't cheap: injecting text into a mid-task
> agent's PTY can split its tool input or trigger a spurious turn, and that
> cost is paid N times per message, every message. Dead-target risk is solved
> at the send side — the CLI errors on an unknown name immediately, which is
> louder than broadcast's failure mode of everyone assuming someone else
> handled it.
>
> **#9 t4 Skeptic:** Conceded in return — delivery-filter-over-one-shared-
> history is a far better design than DMs, and it removes my visibility
> objection. But the PTY-disruption argument is really about WHEN to inject,
> not WHO: a quiescence gate that queues messages until an agent is between
> turns fixes the splice/spurious-turn cost for every message with zero
> naming infrastructure. Send-side errors only catch unknown names, not stale
> ones — the named worker may have finished or been respawned under a new
> identity, so the orchestrator now owns a fleet-roster cache that can rot.
>
> **#10 t5 Architect:** Quiescence gating is a real improvement and I'd take
> it regardless — but it fixes *when* an agent is interrupted, not *whether*
> it should be: an idle worker still wakes and burns a full turn concluding a
> message wasn't for it, and that waste stays O(N) per message at perfect
> timing. On roster rot: the chat already stamps every line with a stable
> terminal id (tX), so mentions should target that id rather than a nickname
> — the roster is the transcript itself, no orchestrator-side cache to go
> stale. A respawned worker gets a new tX, the old id errors at send time,
> and the orchestrator re-reads history to find the live one — failure is
> loud and self-correcting.
>
> **#11 t4 Skeptic:** The tX point lands — the transcript-as-roster means no
> cache to rot and send-time failure is loud, which dissolves my staleness
> objection, and I grant the O(N) idle-wake cost is real and broadcast-only
> has no answer to it at scale. My position reduces to a scope guard, not an
> objection: mentions as a pure delivery filter over one shared history,
> tX-addressed, broadcast default, and nothing more — no threads, no
> nicknames, no read-state. If the proposal is exactly that minimal shape, I
> have nothing left to defend.
>
> **#12 t5 Architect:** CONSENSUS: Foreman chat adds @-mentions as a pure
> delivery filter — a mentioned message interrupts only the targeted
> terminal's PTY, but every message still appears in the single shared
> history, preserving full observability for agents and the watching human.
> Targets are the stable terminal ids (tX) already stamped on chat lines, so
> the transcript is the roster: no name registry, unknown/stale ids fail
> loudly at send time. Broadcast remains the default, quiescence-gated
> injection is adopted for all deliveries, and scope is frozen there — no
> threads, no nicknames, no read-state.
>
> **#13 t4 Skeptic:** agreed — debate closed.
