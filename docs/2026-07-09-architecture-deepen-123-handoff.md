# Handoff: adversarial review of deepen candidates 01–03

**Date:** 2026-07-09  
**Branch at capture:** `fix/control-plane-highs` (confirm with `git status` / `git branch` before acting)  
**Source session:** `/improve-codebase-architecture` → picked 1, 2, 3 → grilling loop (locked below)  
**Status:** design locked → reviewed → re-approved → **implemented 2026-07-09**  
**Verdicts:** `docs/2026-07-09-architecture-deepen-123-verdicts.md`  
**Shipped code:** `Session::snapshot_all`, `src/ready.rs` (`ReadyGate`), `src/chat_view.rs`  
**Repo glossary:** `CONTEXT.md` includes **Ready gate** under Seams & patterns

---

## Re-approval of overturned locks (2026-07-09)

Adversarial review: all three **ship-with-changes**. Three grilling locks overturned
by code evidence; **re-approved as follows** (do not implement the original locks).

| Lock | Original | Re-approved |
|------|----------|-------------|
| **01-Q2** file home | `ChatView::show` in `chat.rs` | **`src/chat_view.rs`** — keep `chat.rs` pure-data (`std` only). Consts + `chat_color` live with the paint. |
| **02-Q1** API shape | `SnapshotOpts` → `Snapshot` in `inspect` | **`Session::snapshot_all(attrs, cursor) -> tuple`** — no new types. Drop `region`. |
| **02-Q2** old methods | Thin wrappers over `snapshot` | **Leave accessors untouched** + doc-comment ("each pumps; multi-field → `snapshot_all`"). |

Also binding from the review (no lock conflict, but ship conditions):

- **01:** Doc `resp`/`rect` precondition + drain-after-`apply_acts` ordering on `show`.
- **02:** Certify via one dispatch call + comment (no deterministic red test available).
- **03:** Action enum frozen at `Write(Vec<u8>)`; DSR/graphics latch decision stays in Session; injected `now` **must** replace the real-sleep submit test; no READY_GRACE.

Implement order still: **02 → 03 → 01**.

---

## Your job in the next session (original brief — review complete)

~~Run an **adversarial review**…~~ **Done.** See verdicts file. Next step is
implementation against the **re-approved** table above, not the original grilling
locks in the candidate sections (those sections are historical).

Suggested review mode (matches prior architecture kills in `docs/adr/`):

1. For each candidate, steelman the proposal in one paragraph.
2. Attack it: deletion test, shallow-pass-through risk, locality loss, two-adapter rule, AI-navigability cost, test surface honesty.
3. Verdict per candidate: **ship / ship with changes / reject** + confidence.
4. If reject, say whether an ADR is warranted (only if hard to reverse, surprising, real trade-off).
5. Cross-check that nothing reopens ADR-0001 / 0002 / 0003.

Prior kills for calibration:

| ADR | Rejected idea |
|-----|----------------|
| 0001 | Typed control `Reply` enum (wire is presence-discriminated bag) |
| 0002 | Shared `visible_cells` walk (frame/inspect divergences are intentional) |
| 0003 | Desktop/Engine type split of `WindowManager` |

Vocabulary for the writeup: **module, interface, implementation, depth, seam, adapter, leverage, locality**. Domain terms from `CONTEXT.md` (Session, Ready, Chat room, Chat viewer, Snapshot, Inspection, Deferred action, …).

---

## How we got here

1. Architecture explore of foreman (`wm.rs` ~7k, `terminal.rs` ~3k, `control.rs` ~2k, `chat.rs` deep model / shallow paint).
2. HTML report (temp, not in repo): eight candidates. Top pick was Chat viewer; Snapshot + Ready gate next.
3. User selected **1, 2, 3**. Grilling locked the decisions in the tables below (user agreed each recommendation).

**Not selected / out of scope for this review unless they undermine 1–3:**

- 04 ControlOps server locality  
- 05 WinId/Dir leaf  
- 06 shared cell style (without shared walk)  
- 07 header chrome completion  
- 08 WM file clusters (speculative; ADR-0003 compatible only as *file* clustering, not type split)

---

## Candidate 01 — Deepen the Chat viewer

### Problem

`ChatRoom` is already a deep module (`docs/chat-delivery.md`). Presentation is not: ~320 lines of crew board + log + input strip live in `Content::show`’s `Chat` arm inside `src/wm.rs` (~lines 130–447). `ChatView` holds scroll/input/click state but has no paint.

### Locked design

| Decision | Choice |
|----------|--------|
| Outcomes | **Fields + existing drains** — `show` still sets `view.click` / `view.pending_post`; WM keeps `drain_chat_clicks` / `drain_chat_posts`. No return-outcome, no host trait. Matches Deferred action: content must not mutate sibling Wins mid-draw. |
| Home | **RE-APPROVED:** `ChatView::show` in **`src/chat_view.rs`** (not `chat.rs` — pure-data contract). Historical grill lock was wrong. |
| Interface | `show(&mut self, ui, rect, active, resp, id: egui::Id)` — Content builds Id via `base.with((win_id, "chat-input"))`; chat never sees `WinId` |
| Policy move | `chat_color` + `CHAT_BOARD_W` / `CHAT_BOARD_MIN_W` move into **`chat_view.rs`** with paint |
| Content arm after | One call: `view.show(...)`; still returns `false` for nested-focus bool |

### Code anchors

| What | Where |
|------|--------|
| Chat paint arm | `src/wm.rs` `Content::show` → `Content::Chat` |
| ChatView state | `src/chat.rs` `struct ChatView` (~384+) |
| Pure helpers already deep | `scroll_step`, `build_blocks`, `age_label`, `crew`, `blocks` in `chat.rs` |
| Drains | `WindowManager::drain_chat_clicks`, `drain_chat_posts` |
| Doc | `docs/chat-delivery.md` (viewer pulls; wiring is adapter) |

### Deletion test claim

If `ChatView::show` owns board/log/strip, deleting the Content arm deletes behavior. Extracting more paint *helpers* while leaving orchestration in `wm` fails the test.

### Skeptic angles (attack these)

1. **Is this just a file move?** If the interface is as wide as the paint body, depth is unchanged — only navigability. Is that enough to justify egui in `chat.rs`?
2. **egui in the model file.** `chat.rs` is currently almost pure and highly tested. Pulling painter/`TextEdit` into it may hurt “model is the test surface” culture. Would `chat_view.rs` have been better despite user lock A?
3. **Theme + layout constants.** Moving board widths into chat is right for locality, but now `chat` depends on `theme` + egui. Any circular risk via `wm`?
4. **Id stability.** Tabbing/untabbing Win ids — is `base.with((win_id, "chat-input"))` still correct after move? (Should be: Content still owns that.)
5. **No new tests.** Paint stays untested after deepen — claim is locality only. Is the churn worth it without a behavior fix?
6. **chat_color duplication.** Confirm no other call sites of `wm::chat_color` after move.

### Success criteria if shipped

- `Content::Chat` arm ≤ ~10 lines.
- No chat paint left in `wm.rs` except drain + open chat window.
- Existing chat unit tests still pass; no intentional behavior change.
- `cargo test` green.

---

## Candidate 02 — Atomic Session Snapshot

### Problem

`snapshot_dispatch` calls `snapshot_text` then optional `snapshot_cells` then optional `cursor_info`. **Each accessor pumps.** Under active PTY output, one Inspection reply can stitch text/attrs/cursor from different `content_gen`s.

Flagged open medium finding: `docs/2026-07-09-project-review-findings.md` (“structured snapshots are not atomic”).

### Locked design

| Decision | Choice |
|----------|--------|
| API | **RE-APPROVED:** `Session::snapshot_all(attrs, cursor) -> (text, cells, cursor)` — no `SnapshotOpts`/`Snapshot` types |
| Types live in | N/A (tuple is the return; inspect walks stay pure free fns) |
| Old Session methods | **RE-APPROVED:** leave untouched + doc-comment; do not rewrite as wrappers |
| Walks | **Unchanged** — still separate `inspect::snapshot_text` / `snapshot_cells` / `cursor_info` (ADR-0002) |
| Control path | `snapshot_dispatch` → one `session.snapshot_all(req.attrs, req.cursor)` |

### Intended shape (re-approved)

```text
// Session::snapshot_all:
//   self.pump() once
//   text = inspect::snapshot_text(&term, None)
//   cells = attrs.then(|| inspect::snapshot_cells(&term, None))
//   cursor = cursor.then(|| inspect::cursor_info(&term))
//   (text, cells, cursor)
```

### Code anchors

| What | Where |
|------|--------|
| Triple pump dispatch | `src/wm.rs` `snapshot_dispatch` (~1387–1409) |
| Session accessors | `src/terminal.rs` ~903–920 |
| Pure walks + tests | `src/inspect.rs` |
| Wire flags | `SnapshotRequest.attrs` / `.cursor` in `control.rs` |

### Deletion test claim

One Session method that pumps once is real depth for the “consistent multi-field read” contract. Leaving the three-pump path callable for control fails the test (hence wrappers must share pump policy).

### Skeptic angles (attack these)

1. **Is the bug real in practice?** Prove with a failing test (synthetic feed between pumps) or kill as theoretical. Adversarial bar: write the red test first.
2. **Wrappers that still pump thrice if misimplemented.** Code review must ensure `snapshot_text` does not call `pump` independently of a shared path that pumps three times when someone chains them for attrs+cursor.
3. **Region:** control path always passes `None` today. Does opts need region now, or YAGNI until a CLI flag exists?
4. **Type home:** putting `Snapshot` in `inspect` while Session returns it is good — but does `control`/`OpenReply` then depend on inspect more deeply? Already does for `CellData`/`CursorInfo`.
5. **ADR-0002 creep:** any pressure to “share the walk while we’re here” must be rejected; only share the *pump*.
6. **Test-only multi-field path:** if production rarely uses attrs+cursor together, cost/benefit?

### Success criteria if shipped

- New unit test: feed/mutate between what *would* have been three pumps; single `snapshot` returns consistent gen/view.
- `snapshot_dispatch` one call.
- ADR-0002 still holds (no shared walk).
- Wire reply shape unchanged (presence bag).

---

## Candidate 03 — Ready gate module

### Problem

Ready (DSR ∧ first paint) + pending inject queue + deferred `\r` after chat paste is the load-bearing Session contract for Chat room delivery. Logic is field soup on `Session`, interleaved with `pump`/graphics/spawn. Historical bug: DSR alone was not enough (passthrough ConPTY) — see comments ~2026-07-03 chat-delivery regression.

### Locked design

| Decision | Choice |
|----------|--------|
| Boundary | **Ready + chat inject only** — not `inject_note`, not raw `feed`, not graphics reply flush |
| Name / file | **`ReadyGate` in `src/ready.rs`** (parallel to `CaretGate` / `caret.rs`) |
| Time | **Injected `Instant`**; SUBMIT_DELAY inside gate; **no READY_GRACE in this change** (leave hook for follow-up in `docs/followups-latency-and-control.md`) |
| InkScan | **Moves inside** ReadyGate; Session feeds raw rx chunks |
| I/O | **Pure actions** — gate never holds `Write`; Session applies `Write(bytes)` etc. |
| Glossary | **Ready gate** term added to `CONTEXT.md` |

### Intended interaction (illustrative — review may redesign)

```text
// Session::pump / inject_input
gate.on_rx_chunk(&bytes)           // InkScan + painted
gate.on_dsr_reply_flushed(ok)      // only on successful CPR write
for action in gate.poll(now) { apply_to_writer(action) }  // deferred \r, flush queue
gate.try_inject(text, now)         // queue or emit paste_wrap + schedule submit
gate.ready() -> bool
```

Exact action enum is **not** locked — fair game for design-it-twice / adversarial rewrite.

### Code anchors

| What | Where |
|------|--------|
| Session Ready fields | `src/terminal.rs` `dsr_replied`, `painted`, `ink`, `ready`, `pending_inject`, `pending_submit` |
| pump Ready logic | `Session::pump` ~923–970 |
| inject_input | ~868–881 |
| paste_wrap | ~526–535 (chat always brackets; distinct from `input::paste_seq`) |
| InkScan | ~344–370 |
| flush_pty_replies latches dsr | ~984–1014 |
| Tests | ready/inject/ink tests in `terminal.rs` cfg(test) — must move or keep green via Session facade |
| Follow-up grace | `docs/followups-latency-and-control.md` §2 |
| Product meaning | `CONTEXT.md` Ready + new Ready gate |

### Deletion test claim

A gate that only renames fields without shrinking Session’s interface fails. Real deepen = Ready contract testable without PTY/Session, and Session becomes a thin adapter (feed chunks / apply writes).

### Skeptic angles (attack these)

1. **Premature module?** Ready tests already pass through Session with real/light PTY. Does extraction pay for itself, or is it rearrange-the-deck-chairs?
2. **Action enum surface area.** If poll returns many variants and Session still special-cases each, interface ≈ implementation (shallow).
3. **DSR signal honesty.** `dsr_replied` latches on *any* successful flush of `resp` buffer, not a parsed DSR. Moving code must not “fix” that into a false precision, and must not let graphics replies fake Ready (already separated — `flush_graphics_replies` vs `resp`).
4. **Queue flush re-entrancy.** `pump` flushes `pending_inject` by calling `inject_input` while Ready is true — each schedules submit. Coalesce / multi-post quirks are documented; extraction must preserve them or deliberately change with tests.
5. **SUBMIT_DELAY + Instant injection.** Who passes `now` on every pump? Easy to pass wall clock inconsistently vs test clock.
6. **READY_GRACE scope creep.** Review should reject shipping grace “while we’re here” without product sign-off (inject before child can read).
7. **paste_wrap location.** Stays in terminal vs moves to ready? Locked design says gate decides paste+submit; wrap helper co-location is open.
8. **Two adapters?** Only Session will ever drive ReadyGate — one adapter. Defense: pure module for testability (CaretGate precedent), not swappability. Attack: “one adapter = hypothetical seam.”

### Success criteria if shipped

- All existing Ready/inject/ink Session tests still pass (moved or via facade).
- New pure ReadyGate tests: chunk splits, DSR-without-paint, paint-without-DSR, queue-then-ready, submit delay with injected Instant.
- No behavior change for chat delivery integration tests.
- `Session::ready()` remains the external signal for `chat_tick` / LiveMember.

---

## Suggested implementation order (if review says ship)

1. **02 Snapshot** — smallest, correctness bug, low coupling  
2. **03 ReadyGate** — extract + move tests; behavior-preserving  
3. **01 Chat viewer** — mechanical paint move after Session/WM quieter  

Do **not** bundle all three in one commit without review approval.

---

## Explicit non-goals (do not re-litigate in this review)

- Typed `Reply` / request enum wire change → ADR-0001  
- Shared frame/inspect grid walk → ADR-0002  
- Split `WindowManager` into Desktop + Engine types → ADR-0003  
- Implementing ControlOps / WinId leaf / header chrome / WM directory split  
- READY_GRACE product behavior (document hook only)  
- Per-pane panic isolation  

---

## Related reading (priority order)

1. `CONTEXT.md` — Ready, Ready gate, Snapshot, Inspection, Chat room, Deferred action, Caret gate  
2. `docs/chat-delivery.md` — ChatRoom interface; engine is thin adapter  
3. `docs/2026-07-09-project-review-findings.md` — atomic snapshot finding  
4. `docs/adr/0001-*.md`, `0002-*.md`, `0003-*.md` — rejection patterns  
5. `docs/followups-latency-and-control.md` §2 — READY_GRACE (future)  
6. `src/caret.rs` module docs — pattern model for ReadyGate purity  
7. Skills (if loaded): `codebase-design`, `foreman-architecture-contract`, `foreman-agent-state-campaign`, `foreman-change-control`  

---

## Deliverable from the adversarial session

Write a short verdict doc (or PR comment style) with:

```text
## 01 Chat viewer — ship | ship-with-changes | reject (confidence)
## 02 Atomic Snapshot — …
## 03 Ready gate — …
## Cross-cutting risks
## Required design changes before code (if any)
## ADR? yes/no per rejection
```

Optional: if a candidate is killed for a load-bearing reason future explorers would re-suggest, offer an ADR draft (user must accept).

---

## Session notes / decisions log

User answers during grilling (all chose the recommended option unless noted):

| # | Topic | Locked |
|---|--------|--------|
| 01-Q1 | Outcome handoff | Fields + drains |
| 01-Q2 | File home | `chat.rs` |
| 01-Q3 | show args | + `id: egui::Id` |
| 01-Q4 | color/board consts | Move into chat |
| 02-Q1 | API shape | opts → Snapshot |
| 02-Q2 | old methods | Thin wrappers |
| 02-Q3 | type home | `inspect` |
| 03-Q1 | boundary | Ready + chat inject only |
| 03-Q2 | name/file | ReadyGate / ready.rs |
| 03-Q3 | time/grace | Instant inject; no grace yet |
| 03-Q4 | InkScan | Inside gate |
| 03-Q5 | I/O | Pure actions |

HTML architecture report path (ephemeral):  
`%TEMP%\architecture-review-20260709-205411.html` — may be gone; this handoff is the durable source.
