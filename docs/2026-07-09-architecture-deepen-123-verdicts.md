# Adversarial review verdicts: deepen candidates 01–03

**Date:** 2026-07-09
**Branch at review:** `fix/control-plane-highs`
**Input:** `docs/2026-07-09-architecture-deepen-123-handoff.md` (locked designs)
**Method:** three independent adversarial subagents (one per candidate), each
verifying every claim against the code with symbol-level reads; verdict-deciding
claims re-verified independently by the coordinating session
(`snapshot_dispatch`, `Session::inject_input`, `Session::flush_pty_replies`,
`control.rs`/`chat.rs` import lists). Vocabulary: module, interface, depth,
seam, adapter, deletion test, two-adapter rule (`codebase-design`).
**Status:** **implemented 2026-07-09** on the re-approved shape (see handoff).  
Historical line numbers in the analysis below are from pre-implementation review.

## Summary

| Candidate | Verdict | Confidence | Locked decisions overturned |
|---|---|---|---|
| 01 Chat viewer | **ship-with-changes** | high | 01-Q2 (file home) — recommend reopening |
| 02 Atomic Snapshot | **ship-with-changes** | high | 02-Q1 (opts→Snapshot), 02-Q2 (thin wrappers) — recommend replacing both |
| 03 Ready gate | **ship-with-changes** | ~75% | none — but three conditions attached |

No candidate is rejected, so **no ADRs are warranted** (nothing hard to
reverse, surprising, or trading off product behaviour). Nothing reopens
ADR-0001/0002/0003; candidate 02's revised shape moves *further away* from
ADR-0001/0002 pressure than the locked design did.

Two grilling locks are overturned by evidence found in code — the user must
re-approve those before implementation (see per-candidate sections).

---

## 01 Chat viewer — ship-with-changes (high)

**Steelman.** The `Content::Chat` arm really is ~317 lines of crew board + log
+ input strip (src/wm.rs `Content::show`, lines 130–447) painting a module
whose model (`ChatRoom`) is already deep and whose view-state struct
(`ChatView`, src/chat.rs:383) has no paint. One `view.show(...)` call makes the
Chat arm exactly the shape of the Term arm (`s.show(ui, rect, active, resp);
false`), and every paint decision about chat gets one home. Depth is real: 6
params + 2 drained fields + a `false` return hide ~317 lines.

**Verified facts that decide it.**

- The arm reads **zero** `WindowManager` state — structurally guaranteed
  (`Content::show`'s `self` is the `Content` enum, not the manager) and
  confirmed by read: everything comes from `view`, `ui`, `resp`, or passed
  args. The move is safe.
- `wm::chat_color` has exactly two call sites, both inside the arm (wm.rs:190,
  283). No re-export needed. `CHAT_BOARD_W`/`CHAT_BOARD_MIN_W` (wm.rs:54–55)
  are used only in the arm.
- `base`/`win_id` are used exactly once: `base.with((win_id, "chat-input"))`
  (wm.rs:429). Content pre-building the Id yields a byte-identical Id —
  survives tabbing/untab.
- No dependency cycle: chat→theme→egui is a clean DAG; theme.rs never imports
  chat.

**Attacks that landed.**

1. **The file-home lock (01-Q2) should be reopened.** `chat.rs` imports only
   `std::time` today and its own docstring (line 2) declares "Pure data — the
   pipe/wm wiring lives in control.rs / wm.rs." Moving `show` in forces
   `eframe::egui` + `theme` imports and ~317 lines of non-unit-testable paint
   into the file that *is* the tested model surface, falsifying the docstring.
   A new `src/chat_view.rs` preserves the model's purity at zero cost
   (precedent: paint/pure splits like frame.rs vs terminal.rs).
2. **The deletion-test claim oversells.** Deleting `ChatView::show` would not
   delete the whole behaviour: `drain_chat_clicks`/`drain_chat_posts`
   (wm.rs:1498/1538) and their **fixed ordering** (drained after `apply_acts`,
   wm.rs:3527–3532, documented so the member — not the viewer — ends focused)
   survive as host-side interface facts. Fields+drains was the right call
   (matches Deferred action), but the seam still straddles two files; say so.
3. **Hidden interface: the resp/rect coincidence.** The crew-row hit test uses
   `resp.hover_pos()` against rows built in `rect` space; at the chromed call
   site `resp` senses `content_rect` while the arm receives `rect =
   content_paint` (wm.rs:2882–2894). Same region today — but the proposed
   signature promises no such thing. Must become a documented precondition.

**Required changes before code.**

1. *(needs user re-approval — overturns lock 01-Q2)* Home the paint in a new
   `src/chat_view.rs`; keep `chat.rs` std-only. Constants + `chat_color` move
   to `chat_view.rs`, not `chat.rs`.
2. Doc-comment on `show`: `resp` must be a `Sense::click_and_drag` Response
   sensing the same screen rect as `rect`; wheel handling assumes the host has
   not consumed scroll for that rect.
3. Doc-comment: `click`/`pending_post` are drained by the WM after
   `apply_acts`; the ordering is load-bearing.

Success criteria from the handoff stand otherwise (arm ≤ ~10 lines, no
behavior change, tests green).

---

## 02 Atomic Snapshot — ship-with-changes (high; the changes gut the locked shape)

**Steelman.** The torn read is real in mechanism: each of
`snapshot_text`/`snapshot_cells`/`cursor_info` pumps (terminal.rs:903/909/918),
the reader thread fills the rx channel asynchronously (terminal.rs:707–726),
so between the three back-to-back calls in `snapshot_dispatch` (wm.rs:1406–1408)
a chunk can land and one Inspection reply can stitch text at gen N with
cells/cursor at gen N+1. One pump per dispatch is the correct contract, and
`snapshot_dispatch`'s own comment ("Each accessor pumps") shows the hazard was
known and tolerated, not designed.

**Verified facts that decide it.**

- Reachability is narrow: a plain `foreman snapshot` calls only
  `snapshot_text` → one pump → **cannot tear**. Tearing requires `--cursor`
  (2 pumps) or `--attrs --cursor` (3), during active output, in a
  microseconds-wide window, with minor consequence (cursor one gen ahead of
  text). The "medium" severity in the findings doc is generous.
- `Snapshot { text, cells: Option, cursor: Option }` **fails the deletion
  test**: it is byte-for-byte the tuple `snapshot_dispatch` already returns
  (`(Vec<String>, Option<Vec<Vec<CellData>>>, Option<CursorInfo>)`). Delete the
  type and the tuple reappears unchanged — a pass-through container.
- `region` in `SnapshotOpts` is dead on arrival: `SnapshotRequest`
  (control.rs:155–167) has no region field, the CLI never sets one, all three
  dispatch calls pass `None`.
- **The locked wrapper decision (02-Q2) doesn't deliver its stated goal.** The
  handoff's rationale was "wrappers must share pump policy" so the three-pump
  path stops being callable. But wrappers over `snapshot` still pump once *per
  wrapper call* — chaining `snapshot_text()` then `cursor_info()` still
  multi-pumps. Atomicity comes only from making one call; rewriting the
  accessors buys nothing and churns every test caller
  (terminal.rs:2057, 2478, 2579–2580, 2654–2655, 2845/2854, …), each of which
  *wants* its own pump.
- The only production caller of all three accessors is `snapshot_dispatch`
  (control server path, wm.rs:1029). Everything else is tests.
- Wire is untouched either way: `OpenReply` keeps separate `cells`/`cursor`
  Options (control.rs:52–59); control.rs keeps zero `use crate::` imports
  (inspect types are path-qualified). ADR-0001 safe. Only the pump is shared,
  never the walks — ADR-0002 safe.

**Attack that lands hardest: the red test is not writable.** The handoff's
success criterion ("feed/mutate between what would have been three pumps")
cannot be written deterministically with current scaffolding: inspect tests
drive `Term<VoidListener>` with fixed bytes (no channel/thread); Session tests
use real PTYs; a single pump's `while let Ok(...) = rx.try_recv()` drains
*all* queued chunks, so single-threaded stuffing can't land bytes *between*
two accessor calls without a real racing thread. This is true of every design
variant (not a differentiator), but it means the fix is certified by code
review + comment, not red→green.

**Required changes before code** *(needs user re-approval — replaces locks
02-Q1 and 02-Q2)*.

1. Replace `SnapshotOpts`/`Snapshot` with one method:
   `Session::snapshot_all(&mut self, attrs: bool, cursor: bool) -> (Vec<String>,
   Option<Vec<Vec<CellData>>>, Option<CursorInfo>)` — pump once, then run the
   three existing pure `inspect` walks on the same `&self.term`.
   `snapshot_dispatch` collapses to one call. ~8 lines of real change, zero
   new public types. (If a struct is ever wanted, it's a rename-refactor away;
   today it would be interface with nothing behind it.)
2. Drop `region` — nothing populates it (YAGNI until a CLI flag exists).
3. Leave `snapshot_text`/`snapshot_cells`/`cursor_info` untouched — no wrapper
   rewrite. Guard the contract with a doc-comment on the accessors ("each call
   pumps; for a consistent multi-field read use `snapshot_all`") since the
   type system can't.
4. Amend the success criteria: replace the red-test requirement with (a) the
   dispatch site making exactly one Session call, and (b) a comment citing the
   torn-read hazard; update
   `docs/2026-07-09-project-review-findings.md` when fixed.

---

## 03 Ready gate — ship-with-changes (~75%)

**Steelman.** Ready (DSR-flushed ∧ first-paint) + pending-inject queue +
deferred submit is the load-bearing contract Chat delivery gates on, and today
it is six fields (`pending_submit`, `pending_inject`, `dsr_replied`, `painted`,
`ink`, `ready`; terminal.rs:421–443) whose choreography is smeared across
`pump` (:954–970), `inject_input` (:868–881), and `flush_pty_replies`
(:1001–1002). The extraction is *not* field-soup relocation: the honest
interface is 5 methods + **one** action variant (`Write(Vec<u8>)`) + one query
(`ready()`), hiding a genuine state machine — the opposite of the shallow
"interface ≈ implementation" failure the handoff feared. Deletion test passes:
delete the gate and the DSR/ink/ready/queue/submit choreography reappears in
`pump`. CaretGate (src/caret.rs, injected `Instant`s) is the standing
precedent, and `show()` already computes a `now` it threads to the caret.

**Verified facts that decide it.**

- Action enum honest count: **N = 1.** Every Session-facing effect is a PTY
  write (`send(paste_wrap(text))` per drained item; `send(b"\r")` for submit).
  No LatchReady (internal → `ready()` query), no ArmSubmit (internal timer),
  no RequestRepaint (`inject_input`'s own doc: the frame loop pumps every
  session ~16 ms, "no extra repaint plumbing is needed").
- Boundary is honestly separable: `inject_input` is chat-only (verified —
  queues on `!ready`, else paste_wrap + deferred submit); non-chat input goes
  through `feed()`/`paste_text()`(`input::paste_seq`). `paste_wrap` has exactly
  one production caller (inject_input:879). External `ready()` readers
  (wm.rs:1616 chat_tick LiveMember, wm.rs:4696, chat.rs:444) survive a
  delegating shim.
- Re-entrancy reproduces purely: `poll(now)` draining internally is *less*
  re-entrant than today's recursive `inject_input` call from `pump` (:961);
  last-wins `pending_submit` preserves the documented post-merge quirk
  (test :2122–2131).
- The `resp` buffer is `Arc<Mutex<Vec<u8>>>` shared with the alacritty
  Listener (:388) — the gate **cannot** own it. Correct call: gate is told the
  outcome (`on_dsr_reply_flushed(sent)`), never sees writer or resp.

**Attacks that landed (become the conditions).**

1. **The sharpest correctness edge does not move.** `dsr_replied` latches on
   *any successful flush of the resp buffer* — not a parsed DSR
   (flush_pty_replies:1001–1002, deliberate no-retry on failure) — and
   graphics replies use a separate path that must never fake readiness
   (flush_graphics_replies:973–982, comment "a graphics reply must never fake
   readiness"). Post-extraction that edge becomes a **new call-ordering
   contract across the seam** (feed chunk → flush resp → tell gate → poll),
   easy to break by routing a graphics reply into `on_dsr_reply_flushed` or
   polling before flushing. The deletion-test claim "Ready contract testable
   without PTY/Session" is only *partly* true — the DSR-outcome half stays a
   Session integration concern (FailingWriter test :1970 stays put).
2. **~40 of ~70 moved lines are InkScan, which is already pure and already
   unit-tested without a Session** (ink_scan_* :2064/:2084). Moving it in is
   cohesion, not a testability win. Don't sell it as deepening.
3. **The payoff is the timer.** Today's Ready/inject tests spawn a real PTY
   (`cmd.exe /c pause`) and sleep-loop on real DSR; the submit-timing test does
   a real `sleep(SUBMIT_DELAY + 30ms)` (:2126) and both clock reads are
   hard-coded `Instant::now()` (inject_input:880, pump:966). Injected time
   making that test fast and deterministic is the whole justification. If the
   refactor lands and the real-sleep test survives, it bought almost nothing.

**Required changes before code** (no locks overturned; the action enum was
explicitly unlocked).

1. Freeze the action enum at exactly one variant: `Write(Vec<u8>)`. If a
   second Session-facing variant appears during implementation, that is the
   shallow-module warning — push the state back inside the gate instead.
2. The DSR decision point and the graphics/PTY-reply separation stay in
   Session; the gate never sees writer/resp. Add a regression test pinning the
   new seam: flushing a graphics reply must NOT advance `ready()`.
3. Thread the injected `now` through `pump`/`poll` (production passes
   `show()`'s existing `now`; tests pass a controlled clock) and **replace**
   the real-sleep submit test with an injected-time gate test. This is a ship
   condition, not a nice-to-have.
4. Confirmed exclusions hold: no READY_GRACE (hook only, per
   `docs/followups-latency-and-control.md` §2); `inject_note`, raw `feed`,
   graphics flush all stay out.

---

## Cross-cutting risks

- **Three grilling locks are overturned by code evidence** — 01-Q2 (file
  home), 02-Q1 (opts→Snapshot), 02-Q2 (thin wrappers). These were user
  decisions; re-approval is required before implementation. The rest of the
  locks survived attack.
- **02 and 03 both touch `pump`.** Land 02 first (it shrinks to ~8 lines and
  only *calls* pump), then 03's extraction rewrites pump's tail. Order from
  the handoff (02 → 03 → 01) stands; 01 last is right because it's mechanical
  and wants a quiet wm.rs.
- **Pump multiplicity is behaviour, not just cost.** `pump` latches Ready,
  drains the inject queue, and fires deferred submits. Going from three pumps
  to one per snapshot dispatch is semantically safe (extra pumps hit an empty
  channel + idempotent tails) but reviewers of 02 should know pump is not a
  pure "parse" call.
- **ADR check: clean.** Nothing reopens ADR-0001 (wire untouched — `OpenReply`
  keeps separate Option fields), ADR-0002 (walks stay separate; only the pump
  is shared; no shared-walk creep detected in any design), or ADR-0003
  (`WindowManager` untouched as a type).
- **No new ADRs warranted.** No candidate was rejected; every change above is
  cheaply reversible and none is surprising given `CONTEXT.md` + existing
  precedent (CaretGate, frame/inspect split).

## Required design changes before code (consolidated)

| # | Candidate | Change | Needs user re-approval? |
|---|-----------|--------|-------------------------|
| 1 | 01 | Paint home = new `src/chat_view.rs`, not `chat.rs`; consts + `chat_color` go there | **Yes** (overturns 01-Q2) |
| 2 | 01 | Doc the resp/rect precondition + drain-ordering coupling on `show` | No |
| 3 | 02 | `Session::snapshot_all(attrs, cursor) -> tuple`; no `SnapshotOpts`/`Snapshot` types | **Yes** (overturns 02-Q1) |
| 4 | 02 | No wrapper rewrite; accessors untouched + doc-comment | **Yes** (overturns 02-Q2) |
| 5 | 02 | Drop `region`; replace red-test criterion with dispatch-site + comment certification | No |
| 6 | 03 | Action enum frozen at `Write(Vec<u8>)` | No (enum was unlocked) |
| 7 | 03 | DSR decision stays in Session; add graphics-reply-must-not-latch-ready regression test | No |
| 8 | 03 | Injected `now` must replace the real-sleep submit test (ship condition) | No |

## ADR? — no, for all three

Nothing was rejected. If the user instead *rejects* candidate 02's revised
shape and insists on the opts-struct, no ADR either way — the choice is
reversible in minutes. The only future ADR trigger in this area remains a
deliberate protocol v2 (ADR-0001's revisit clause).
