# Chat room — missing features (consensus)

Three agents (t3 coordination, t4 human-supervisor, t5 reliability) sat in the
project chat room and argued about what the room itself still lacks. This is the
agreed, prioritized list. t3 (coordination) was the synthesizer.

The room today (built 2026-06-10): push-injected posts, broadcast + targeted
(`--to` / leading `@tX`), `--history`, a chat viewer with a crew board
(last-heard age + amber stale), a NEW divider, join/exit events, and seq numbers
you can cite. See `docs/epics/agent-dispatch-epic.md` § "Group chat".

## The one big idea: three layers

The discussion converged on a layering. Don't mix them up — each lower layer is
the precondition for the one above:

1. **Transport** — the *guarantee* a message actually arrived and survived a
   restart. Without this, everything above is a lie: a "done" message that
   silently never lands is worse than no message.
2. **Semantics** — *meaning* on the wire. Typed message kinds so agents (and the
   UI) can act on "blocked" / "done" without sniffing for the word.
3. **Surfacing** — turning the data into *supervision*: alerts, badges, search.

Today the room only has layer 3-ish pieces (the viewer) sitting on a layer-1 hole
(fire-and-forget delivery, in-memory log). Fix the bottom first.

## What this is really for: live contract negotiation

The clearest payoff is two agents agreeing a contract *while both build*. A
**frontend agent and a backend agent settling an API shape** is the model case:
frontend posts "I need `GET /orders` → `{id, total, status}`"; backend replies
"`status` is an enum (open/paid/shipped), and `total` is cents not dollars";
they converge in three messages, then build in parallel against the agreed shape.

This is exactly why #1 below is a *handshake*, not fire-and-forget: the reply is
where the contract gets negotiated and fixed *before* either side commits to the
wrong thing. Typed kinds (#4) and claims (#6) keep the two sides from colliding.
Broadcast-and-hope can't do this; a reply-based handshake can.

## Steering is a norm, not a feature

The room only pays off if members *use* it to steer. If the lead — or anyone —
reads the chat and spots a bad assumption, a wrong contract, or two agents about
to collide, they say so and redirect *right then*. Catching a mistake in chat
before it becomes committed work is the cheapest fix there is. The #1 handshake is
the built-in moment for this (the reply = agree / question / counter), but it
applies any time someone is reading. Passive announcing ("done X") under-uses the
room; active steering is the point. No code required — but the alerts (#7) and
filter-by-kind (#9) below exist to make steering *possible* once the room is busy.

---

## Layer 1 — Transport (fix these first)

### 1. Handoff handshake — the assignee replies, and that reply IS the ack  ← the keystone

> **Status 2026-06-11: partially built, then deliberately deferred.** `--re N`
> threading and `OpenReply.seq` are live; the `--await-ack` registry surface
> was built inert and removed (see the status note in
> `contracts/chat-handshake-remaining-work.md` — recovery point `4607001`).
> The social half — "the reply IS the ack" — works today via `--re` plus
> worker-prompt norms; only the automated missing-ack backstop is deferred.

A handoff is not done when the message is *sent*. It is done when the assignee
*replies in chat*. The verification is a real chat response, not a silent flag.

**Why it matters.**
- **Proof it landed AND was read.** A reply can only come from an agent that
  actually received and processed the post. No reply inside a reasonable window =
  the handoff did not land (dropped, or the member is write-only / heads-down) —
  and now you *know*, instead of assuming and finding out hours later.
- **Iterate before forcing work.** The reply is the moment to push back —
  "that contract is wrong, what about X?" A short back-and-forth converges on a
  better design *before* an agent is committed to building the wrong thing.
  Fire-and-forget skips this and bakes in the mistake.
- Applies to **handoffs / assignments / claims** (the targeted posts), not every
  message. Requiring a reply to plain chatter would just be noise.

**Backstop (reliability, t5's point).** Keep a per-member delivery cursor
(`last-delivered-seq` + catch-up replay on the member's next *ready* frame)
underneath. It closes the silent DSR-fresh-spawn drop and — crucially — lets a
*missing* reply be told apart from "they replied but the assignment itself never
displayed." The cursor is also the idempotency key for retries (a re-injected
post at-or-below the cursor is a dup). So: handshake on top for the negotiable,
human-visible ack; cursor underneath for the guarantee.

**Effort/risk.** Low for the reply convention (it is how agents already talk);
low-medium for a sender-side "awaiting ack from t6" tracker + timeout; medium for
the cursor backstop (defining the post-DSR "ready frame" correctly, or you re-open
the drop you were closing).

### 2. Persistence (log survives restart)
Append the log to a per-project file; reload on start; keep seq monotonic across
restarts.

**Why it matters.** The log is an in-memory `Vec` — foreman exit or crash wipes
it. Every cited `#N` then dangles mid-task, the delivery cursors reset, and a
searchable/jumpable transcript is worthless if it evaporates on restart. This is
a reliability gap AND the precondition for search and jump-to-#N below.

**Effort/risk.** Medium. Append-only JSONL is simple; the care is in reload +
keeping seq stable across restarts. Low-ish risk.

### 3. Receive-capability detection (write-only members)
Detect members that can post but cannot *receive* — headless `claude -p` print
-mode workers don't read stdin, so injected posts hit a dead buffer. Exclude them
from delivery expectations and flag them on the crew board.

**Why it matters.** Membership today assumes every member can receive. A
write-only worker silently misses every assignment and the room never notices —
a real correctness hole, not cosmetics.

**Effort/risk.** Low-medium. Detection is the hard part (know how a member was
spawned / whether its PTY is interactive); the flag + exclusion is easy. Low risk.

---

## Layer 2 — Semantics (build on transport)

### 4. Structured message kinds (claim / blocked / done / result)
A typed `--kind` field instead of keyword-sniffing prose.

**Why it matters.** Keying behavior off the literal word "blocked" is fragile. A
small typed vocabulary is the clean trigger for alerts, the filter key for the
viewer ("show me all open blockers" = one click), and the field that status/wait
queries. One primitive, many consumers — it earns its keep on the wire AND on the
dispatcher's desk. Note: this is *meaning*, layered on top of the transport
guarantee — a typed "done" that never arrives is still a lie, so #1 comes first.

**Effort/risk.** Low-medium. `ChatMsg` already carries a `ChatKind` (post vs
join/exit); extend it, add a `--kind` flag, thread it through framing. Low risk.

### 5. Round-trip: status / wait verbs + result-file convention
A sender can query "did t6 finish?", block until a member reports done, and read
the result from a known file path instead of scrolling chat. (Already flagged
"designed-for, not built" in the epic's Out-of-scope.)

**Why it matters.** This is what turns the room from a bulletin board into real
delegation: hand off work, then wait for the result, instead of polling
`--history`. It sits directly on #1 (the cursor gives `wait` a real delivery fact
to report against) and #4 (status reports a `done`/`result` kind).

**Effort/risk.** Medium. New verbs plus blocking semantics over a single-shot
pipe with `REPLY_TIMEOUT` need care (long-poll vs client poll). Result-file path
convention is easy.

### 6. Explicit task claims / claims registry
A `claim` / `release` verb and a claims list, surfaced as a claims column on the
crew board.

**Why it matters.** "taking src/wm.rs" is prose nobody enforces — two agents can
grab the same file and waste parallel work. A soft advisory claim makes ownership
visible to agents *and* to the human at a glance.

**Effort/risk.** Low-medium. A claim map keyed by string + three verbs + a
crew-board column. Keep it advisory (no hard locks) and risk stays low.

---

## Layer 3 — Surfacing (turn data into supervision)

### 7. Human push-notifications
Desktop / sound alert on `@you`, on a `kind=blocked`, or when a member has been
waiting/blocked past a threshold.

**Why it matters.** The human isn't staring at the chat window. Supervision should
*push, not pull*, so a blocker doesn't sit unseen while the human polls. The
threshold variant unblocks fast instead of waiting for the agent to nag.

**Effort/risk.** Low-medium. OS notification + sound + a threshold timer. Risk is
notification noise — debounce it.

### 8. Delivery-failed badge + richer presence
A visible "delivery to t7 failed" marker in the viewer, and a crew board that
shows "behind by N msgs" rather than only last-heard age.

**Why it matters.** This is the UI projection of the delivery cursor (#1): a
missed work-assignment surfaces to the supervisor instead of dying silently, and
presence answers "who is keeping up" not just "who was last heard from".

**Effort/risk.** Low. Read the cursor, render it. Depends on #1 existing.

### 9. Search / filter + jump-to-#N
Filter the log by member, text, or kind; click a cited `#N` to scroll to that
message.

**Why it matters.** The crew board click-focuses a *terminal*; it does nothing for
the chat log itself. Once 8 agents flood the room you can't grep the conversation.
Filter-by-kind is what makes #4 pay off ("all open blockers"). Jump-to-#N makes
seq citations navigable.

**Effort/risk.** Low-medium. Viewer filter state + scroll-to-seq. Cross-restart
usefulness depends on persistence (#2).

---

## Explicitly NOT recommended
- **Keyword-sniffing for blocked/done** — rejected in favor of typed kinds (#4).
- **Agent-teams integration** — already rejected in the epic (parsing another
  tool's private state files is fragile).

## Key files (where this work would land)
- `src/chat.rs` — the model: `ChatLog` / `ChatMsg` / `ChatKind`, crew rows,
  `build_blocks`. Delivery cursor, kinds, persistence (de)serialization, claims
  map, filter logic live or start here.
- `src/wm.rs` — chat post/broadcast paths, `refresh_chat_view`,
  `drain_chat_posts`. Per-member catch-up replay and the crew-board columns
  (claims, behind-by-N, write-only flag) hook in here.
- `src/control.rs` — pipe protocol + arg parsing. New verbs (`status`, `wait`,
  `claim`, `release`, `--kind`) and receive-capability detection land here.
- `src/main.rs` — per-frame control drain; push-notification trigger point.
- `docs/epics/agent-dispatch-epic.md` — § "Group chat" and § "Out of scope" for
  the current built/unbuilt boundary.
