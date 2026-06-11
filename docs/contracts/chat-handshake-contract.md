# Contract — feature #1: handoff handshake + delivery-cursor backstop

Status: **approved; being implemented inline in two increments** (2026-06-11).
This is the Phase-1 contract from the parallel-implementation workflow; it pins the
seams so the implementer wires things together mechanically instead of re-deciding
the boundary mid-build.

**Increment 1 — wire surface + pure logic (DONE, 121 tests green):**
- `ChatRequest += re, expect_ack`; `OpenReply += seq` — all skip-when-default,
  v1 byte-identical (control.rs, tested).
- CLI: `--re N`, `--await-ack` with validation (target required; post-only) (tested).
- `(re #N)` rendering threaded through the real post path (`chat_post_re` →
  `chat_dispatch` → `frame`), so a reply shows `(re #N)` to everyone and the post
  reply returns the `seq` handle.
- Pure ack state machine `resolve_ack` / `AckState` (tested; consumed in increment 2).
- `expect_ack` rides the wire but the server does not act on it yet (inert no-op).

**Increment 2 — delivery-cursor mechanism (NOT STARTED):** `Session.ready` latch
(first post-DSR reply flush in `pump()`), per-`Tab` `last_delivered_seq` advancing
only on ready-inject, catch-up replay, the ack-registry tick (arm on `expect_ack`,
read cursor + scan log, `resolve_ack`), and the seqless timeout `inject_input` to the
sender + crew-board badge. Needs build+screenshot verification against a NON-fleet
instance (the running fleet is the release exe; a debug instance is fine to observe).

Negotiated in the p1 chat room by **t6** (wire protocol / `control.rs`) and **t7**
(pump + cursor / `wm.rs` + `chat.rs`), compiled by **t3** (lead). The feature is
item #1 of `docs/chat-missing-features.md`.

> **Note on parallelism:** we decided #1 ships **inline (one agent)** — the cross-module
> surface is the work. So the "file ownership" split below is *documentation of the
> seams*, not a dispatch boundary. The contract still earns its keep: it's the agreed
> design the one implementer builds to.

## The shape in one paragraph

An ack is **not a new verb** — it's a normal chat post carrying a `--re <seq>` pointer,
so the reply stays human-visible and negotiable (this is the "reply IS the ack" design,
and the spot where a frontend/backend-style contract gets argued before work proceeds).
A sender that wants a guaranteed round-trip arms it with `--await-ack`. Underneath,
a per-member delivery cursor advances **only when a message is actually injected into a
ready terminal** — which closes the silent DSR-fresh-spawn drop, makes dedup fall out of
the cursor, and makes write-only `claude -p` members detectable for free. The sender's
ack-registry reads that cursor to tell **"never landed → resend"** apart from
**"landed but unacked → nudge."**

## Wire / protocol surface (owner: t6 — `control.rs`, `chat.rs`)

All new fields use `skip_serializing_if` so untargeted v1 traffic is **byte-identical**.

- `ChatRequest` gains:
  - `re: Option<u64>`  — CLI `--re N`. Skip when `None`.
  - `expect_ack: bool` — CLI `--await-ack`. Skip when false. **Client-side parse error
    if set with no delivery target** (you await an ack *from* someone), mirroring
    `--to` / `--history` validation.
- `OpenReply` (chat post reply) gains:
  - `seq: Option<u64>` — the posted message's seq, returned to the sender as the
    **handle to watch**. Skip when `None`.
- **Ack semantics:** a post with `re = N` counts as an ack **iff** `#N` is an existing
  `Post` **and** the sender is in `#N`'s to-set. Otherwise it posts as a plain citation
  and does **not** close the handshake.
- **Errors:** `--re` to an unknown or non-`Post` seq is a **hard reject** (no message
  created), like a bad `--to`.
- **Rendering:** `frame()` / `line()` append `(re #N)` **only when `re` is set**
  — e.g. `[chat p1 #22] t7→t6 (re #19): ...`. The message's own `#N` stays the leading
  authoritative seq. v1 lines unchanged when `re` is `None`, so a human reading
  `--history` can see which handoff got acked.
- The missing-ack timeout is **async, never a pipe reply** — the post returns
  immediately.

## Mechanism / cursor surface (owner: t7 — `wm.rs`, `terminal.rs`, `chat.rs`)

- `last_delivered_seq: u64` per member, stored on the **`Tab`** (so `ChatLog` stays a
  pure append-only log with no per-viewer state).
- `Session.ready: bool` latches on the **first post-DSR-settled frame** — the first
  flushed `ESC[6n` (DSR) reply **plus** output idle. (The principle is pinned; the exact
  detection signal in `terminal.rs`/`Session` is the one piece the implementer must
  de-risk — see Open items.)
- The cursor **advances only on inject-into-a-ready session**, never on a mere
  broadcast-attempt.
- **Catch-up replay:** on a member's ready-frame, replay `seq > cursor` addressed to it
  (broadcast or in its to-set), in order.
- **Dedup IS the cursor:** `seq <= cursor` drops, `seq > cursor` replays once. No
  idempotency field.
- **Write-only members for free:** a `claude -p` print-mode member never goes ready, so
  its cursor never moves and its handoff reads "never landed" forever — that is the #3
  write-only detection, no extra mechanism.

## The seam (where the two halves meet)

- `delivered(member, seq) == cursor[member] >= seq` — the function the protocol side
  reads.
- The **ack-registry** (armed by `expect_ack`, GUI-side, keyed by the `seq` handle) reads
  `delivered(member, seq)` and looks for a matching `--re` from the target, computing:
  - `cursor < seq`                       → **never-landed**  → resend / restart member
  - `cursor >= seq` and no matching `--re` → **landed, unacked** → nudge
  - matching `--re` present                → **acked**          → clear
- This maps to t4/t5's two layers: transport state (landed?) vs semantic state (acked?).
  The supervisor UI must render them **distinctly** — collapsing them makes the human
  resend a member that's merely still working.

## Sender-facing timeout notice (the one point that needed steering)

The lead flagged a gap the agents' first CONSENSUS missed: the timeout was specced as a
"badge/notice", but a badge is human-only — the sender is usually an *agent* whose
`--await-ack` CLI call already returned and cannot see a GUI badge. Badge-only would
silently fail the agent-coordination purpose #1 exists for. t6 + t7 then converged on:

- **The timeout is PUSHED to the sender's terminal** as a seqless one-shot synthetic line
  via the low-level `inject_input` primitive (dispatch-banner / `inject_note` style), e.g.
  `[chat p1 system] no ack from t7 on #19 after Ns`. Authored by a reserved system sender,
  targeted to the sender only. The sender is an established, ready session (long past DSR
  boot), so it lands immediately — no fresh-spawn drop.
- **It is NOT a `ChatLog` entry:** no seq, not in `--history`, **no new `ChatKind`** — so
  the "zero new wire fields" property holds. It **bypasses the cursor entirely** (no read,
  no advance, no dedup).
- The notice carries the cursor's **two-layer split**, not a flat "no ack":
  `t7 never received #19 → resend` vs `t7 got #19, no ack → nudge`.
- **One-shot, no nag.** A late `--re` clears the pending registry + badge state; injected
  terminal text can't be retracted, so the agent's actual resolution is the late ack post
  itself landing in its terminal.
- The crew-board badge is the **human projection** of the same registry state, on top.

### Two inject layers — don't conflate them
Both paths call the same low-level `inject_input`, but they sit at different layers:
- **Catch-up replay** = cursor-driven delivery of missed *logged* posts to a *freshly-
  ready* member (advances the cursor, dedups).
- **Timeout notice** = a *seqless one-shot* to an *already-established* sender (bypasses
  the cursor; not logged).

## Open items (deferred, not blocking the contract)

- **Ack timeout duration:** a config key (distinct from the pipe `REPLY_TIMEOUT`,
  wall-clock). Default TBD by implementer — lean generous (agents work in minutes, not ms).
- **Persistence dependency (#2):** when persistence lands, `seq` MUST stay monotonic
  across restart or every cursor breaks. Cross-feature note from t7; record it on #2.
- **Ready-frame detection:** the agreed *principle* is "first post-DSR-settled frame";
  the concrete signal against `Session`/`terminal.rs` is the implementer's main risk to
  retire first.

## File ownership (seam documentation for the inline implementer)

- `src/control.rs` — `ChatRequest`/`OpenReply` fields, `--re` / `--await-ack` parsing +
  validation, error shapes.
- `src/chat.rs` — `(re #N)` rendering in `frame()`/`line()`; any pure ack-registry state.
  (No new `ChatKind` — the timeout notice is not a log entry.)
- `src/wm.rs` — per-`Tab` `last_delivered_seq`; broadcast path advancing the cursor only
  on ready-inject; catch-up replay on ready-frame; ack-registry timeout → seqless
  `inject_input` notice to the sender + crew-board badge.
- `src/terminal.rs` — `Session.ready` latch (post-DSR-settled detection).
