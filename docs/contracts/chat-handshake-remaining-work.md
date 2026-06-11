# Chat handshake (#1) — remaining work

> **STATUS 2026-06-11: deferred; inert surface removed.** Live skill testing
> showed the ack problem is mitigated in practice (the eaten-post window is
> avoided by the documented dispatch-then-pause rule; a human or dispatcher
> agent watches the room), so finishing the registry isn't currently
> justified — while an accepted-but-inert `--await-ack` flag was a lying API
> surface. Removed: the `--await-ack` CLI flag, `ChatRequest.expect_ack`, and
> the unconsumed `AckState`/`resolve_ack` state machine (recover them from
> git at increment-1 commit `4607001`). Kept and working: `--re N` threading,
> `OpenReply.seq`, and the `Session.ready` latch from increment 2. If
> unattended fleets ever need self-healing handoffs, restart here.

Handoff note for finishing feature #1 (handoff handshake + delivery-cursor
backstop + catch-up replay). The agreed design is in
`chat-handshake-contract.md` (read that first); this file is just "what's done,
what's left, and where to start."

## Done (committed on `feature/agent-dispatch`)

- `265c19f` — design docs (consensus + contract).
- `4607001` — **increment 1: wire protocol + `(re #N)` rendering.**
  - `ChatRequest += re, expect_ack`; `OpenReply += seq`; all skip-when-default
    (v1 byte-identical). CLI `--re N` / `--await-ack` with validation.
  - `(re #N)` renders through the real post path (`chat_post_re` →
    `chat_dispatch` → `frame`); a post reply returns its `seq` handle.
  - Pure `resolve_ack` / `AckState` state machine (tested; not yet consumed).
  - `expect_ack` rides the wire but the server does nothing with it yet.
- `a250e37` — **increment 2 foundation: `Session.ready` latch.**
  - Latches true on the first device-status reply flushed in `pump()` (DSR
    answered). `Session::ready()` exposes it. Chose first-reply-flush over the
    contract's "DSR + output idle" — a strict no-output-frame idle never fires
    for a streaming agent (claude), so it would never go ready.

All of the above: 122 tests green.

## NEXT — we continue here: Part 1, delivery cursor + catch-up replay

This is the keystone ("no silent drop") and it is **unit-testable** with the
existing PTY test harness — no GUI needed. Do this next.

1. **Per-`Tab` cursor.** Add `last_delivered_seq: u64` (default 0) to `Tab`
   (`src/wm.rs`; init at the three `chat_member: false` construction sites).
   Meaning: the highest log seq this member has been *caught up through* —
   delivered if it was addressed to the member, skipped if not.

2. **A testable log helper** in `src/chat.rs`, e.g.
   `ChatLog::deliver_after(member_id: &str, after: u64) -> Vec<&ChatMsg>`:
   every `Post` with `seq > after` that is addressed to `member_id` (`to`
   empty = broadcast, or `member_id` in `to`). Unit-test it directly.

3. **Unified per-frame delivery sweep** (replaces / absorbs the immediate
   `chat_broadcast` push). For each chat-member tab whose `Session::ready()` is
   true and not exited: for each log seq in `(cursor, last_seq]`, inject
   `frame()` for the addressed Posts (skipping the sender's own), and advance
   the cursor to the max seq scanned — **even for non-addressed entries**, so a
   targeted post does not get re-scanned forever. A member that is not ready at
   post time stays behind and catches up automatically once `ready()` flips.
   - Run the sweep where member sessions are already pumped each frame (the
     project manager's `show`/pump path).
   - This preserves reply-before-inject: the post reply is sent in
     `handle_ctrl`, the sweep injects on a later frame.

   Gotchas: don't re-inject to the sender's own active tab (existing
   `chat_broadcast` already excludes it); a member's id for matching `to` is
   `term_tag(window_id)`; the existing `chat_broadcast_*` tests will need a
   "pump until ready, then tick the sweep" step instead of asserting immediate
   injection.

4. Wire `OpenReply.seq` is already returned (increment 1); the cursor work
   makes `--re`/handshake fully functional on the transport side.

## Deferred — Part 2: ack-registry + timeout notice + crew-board badge

Do this in a **dedicated session** (see verification note). Needs the GUI.

- **Ack-registry** on the project manager: when a post arrives with
  `expect_ack`, record (awaited member = its `to`, posted `seq`, armed-at).
  Each frame, for each armed entry compute `resolve_ack(delivered, acked,
  timed_out)` where `delivered = tab.last_delivered_seq >= seq` and `acked` =
  the log contains a `Post` from the awaited member with `re == seq`. (Reuses
  the already-built `resolve_ack` / `AckState`.)
- **Timeout notice** (on `NeverLanded` / `LandedUnacked`): a *seqless one-shot*
  `inject_input`-style synthetic line into the **sender's** terminal (it is an
  established, ready session) — NOT a log entry, no seq, no new `ChatKind`.
  Shape: `[chat p1 system] no ack from t7 on #19 after Ns`. Text carries the
  two-layer split (resend vs nudge). One-shot, no nag; a late `--re` clears the
  pending registry state (the late ack post itself is the visible resolution).
- **Crew-board badge**: the human projection of the same registry state
  (sent-unacked vs landed-unacked rendered distinctly — see the contract; t4's
  point that one badge would make the human resend a still-working member).
- Config: an ack-timeout duration, wall-clock, distinct from the pipe
  `REPLY_TIMEOUT`. Lean generous (agents work in minutes).

## Verification note (why Part 2 waits)

Part 2's payoff is visual (a badge, a pushed notice) and can't be verified in a
session where the agent fleet is running:
- Only one `foreman` can own the control pipe `\\.\pipe\foreman`; the running
  fleet holds it, so a second debug instance has dispatch disabled.
- The running fleet is the **release** exe, so `cargo build`/`cargo test`
  (debug) are safe and don't relink it — but a GUI we can screenshot needs its
  own visible window we can drive.

So: build Part 1 with unit tests now; do Part 2 (and its build+screenshot pass)
when the fleet isn't occupying the pipe and screen.

## Files

- `src/chat.rs` — `re`, `post_re`, `(re #N)` render, `resolve_ack`/`AckState`,
  + the `deliver_after` helper (Part 1).
- `src/control.rs` — wire fields + `--re`/`--await-ack` parsing (done).
- `src/terminal.rs` — `Session.ready` latch (done).
- `src/wm.rs` — `Tab.last_delivered_seq`, the delivery sweep, the ack-registry,
  the timeout inject + crew-board badge.
