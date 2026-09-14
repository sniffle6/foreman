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
    `expect_ack` and `--await-ack` were deleted (recover from `4607001`); `--re`
    and `OpenReply.seq` are still live.
  - `(re #N)` renders through the real post path (`ChatLog::post_re` →
    `chat_dispatch` → `ChatMsg::frame`); a post reply returns its `seq` handle.
    (`chat_post_re` as a wm method was deleted.)
  - Pure `resolve_ack` / `AckState` state machine (tested; not yet consumed).
    Both were deleted (recover from `4607001`).
  - `expect_ack` rode the wire but the server did nothing with it. Was deleted
    (recover from `4607001`).
- `a250e37` — **increment 2 foundation: `Session.ready` latch.**
  - Latches true on the first device-status reply flushed in `pump()` (DSR
    answered). `Session::ready()` exposes it. Chose first-reply-flush over the
    contract's "DSR + output idle" — a strict no-output-frame idle never fires
    for a streaming agent (claude), so it would never go ready.

All of the above: 122 tests green.

## DONE — Part 1: delivery cursor + catch-up replay (built 2026-06-27)

The keystone ("no silent drop") is **built and verified** (274 tests green, no
new warnings — it even cleared the dead `Session::ready` warning by consuming
it). See `docs/chat-delivery.md` for how it works. What landed:

- `ChatLog::deliver_after(member_id, after)` — pure replay source, unit-tested.
- Per-member delivery cursor — lives on `MemberState` in `src/chat.rs` (the
  `Tab.last_delivered_seq` field was deleted).
- `WindowManager::chat_tick()` — recursive per-frame sweep, gated on
  `Session::ready()`, injects from the log and skips a member's own posts;
  called from `main.rs` after `show()`. (`chat_delivery_sweep` was deleted;
  this is the name now.) It **replaced** immediate injection:
  `chat_broadcast` / `chat_broadcast_in` were deleted; `chat_dispatch` and
  `chat_post_human` now only append, and the delivery tests drive `chat_tick`.

The original plan (kept for the record — the keystone is unit-testable with the
existing PTY harness, no GUI needed):

1. **Per-`Tab` cursor.** The plan added `last_delivered_seq` on `Tab` in
   `src/wm.rs`. That field was deleted; the cursor lives on `MemberState` in
   `src/chat.rs`.

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

   Gotchas: don't re-inject to the sender's own active tab (`chat_broadcast`
   was deleted; `chat_tick` skips the sender); a member's id for matching `to`
   is `term_tag(window_id)`.

4. Wire `OpenReply.seq` is already returned (increment 1); the cursor work
   makes `--re`/handshake fully functional on the transport side.

## Deferred — Part 2: ack-registry + timeout notice + crew-board badge

Do this in a **dedicated session** (see verification note). Needs the GUI.

- **Ack-registry** on the project manager: when a post arrives with
  `expect_ack` (that field was deleted; recover from `4607001`), record
  (awaited member = its `to`, posted `seq`, armed-at).
  Each frame, for each armed entry compute `resolve_ack(delivered, acked,
  timed_out)` where `delivered` is the member's room cursor (`MemberState.cursor`,
  not `Tab.last_delivered_seq` — that field was deleted) `>= seq` and `acked` =
  the log contains a `Post` from the awaited member with `re == seq`.
  (`resolve_ack` / `AckState` were deleted; recover from `4607001`.)
- **Timeout notice** (on `NeverLanded` / `LandedUnacked` — AckState variants
  that were deleted; recover from `4607001`): a *seqless one-shot*
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
- The well-known pipe `\\.\pipe\foreman` is first-launched; later instances
  still serve their own `FOREMAN_PIPE`. A second debug instance is not
  dispatch-disabled — in-foreman CLIs bind to the host that spawned them.
- The running fleet is the **release** exe, so `cargo build`/`cargo test`
  (debug) are safe and don't relink it — but a GUI we can screenshot needs its
  own visible window we can drive.

So: build Part 1 with unit tests now; do Part 2 (and its build+screenshot pass)
when the fleet isn't occupying the pipe and screen.

## Files

- `src/chat.rs` — `re`, `post_re`, `(re #N)` render, + the `deliver_after`
  helper (Part 1). `AckState` / `resolve_ack` were deleted (recover from
  `4607001`). Cursor lives on `MemberState`, not on `Tab`.
- `src/control.rs` — wire fields + `--re` parsing (done). `--await-ack` was
  deleted (recover from `4607001`).
- `src/terminal.rs` — `Session.ready` latch (done).
- `src/wm.rs` — `chat_tick` (was `chat_delivery_sweep`; that name was deleted),
  the ack-registry, the timeout inject + crew-board badge.
