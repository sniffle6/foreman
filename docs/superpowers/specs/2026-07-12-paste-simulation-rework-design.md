# Paste-simulation rework — design spec

**Status (2026-07-12):** direction approved; P/E geometry validated by Phase 0
(primary + blind holdout, exact); pad-scrub mechanism (F2) supported;
**predicate v3 is a candidate gated on Task 0**. Scanner deletion is not yet
authorized — it unlocks only when Task 0 and the evidence gates below are
green. Companion docs: `2026-07-12-paste-sim-phase0-protocol.md` (frozen),
`2026-07-12-paste-sim-phase0-results.md` (amended verdict),
`../paste-sim-rework.html` (review-annotated brief, six review passes).
Mode round-trip decision: resolved — minimal streaming sniff (see Epoch
lifecycle).

## Problem

PSReadLine 2.4.5 computes screen positions per UTF-16 unit
(`LengthInBufferCells` does not combine surrogate pairs — upstream #1329).
After a wrapped emoji paste at a pwsh prompt its final CUP parks the cursor
one cell short per emoji deferred at a row margin. c0bdb7c's `CupScanner`
heals one variant by counting non-BMP `LEADING_WIDE_CHAR_SPACER` pads — but
alacritty's `write_at_cursor` overwrite cleanup **scrubs those flags** under
PSReadLine's repaint-in-place style (Phase 0 F2), so the validator fails
closed on large pastes and the caret sits N cells behind. Display-only; edits
stay content-correct.

## Design

### State

`Session.psreadline_cup_scanner: Option<CupScanner>` is replaced by
`psreadline_paste_sim: Option<PasteSim>`:

```rust
struct PasteSim {
    /// P's column — the O(1) acceptance gate (scroll-invariant).
    p_col: usize,
    /// E_flat − P_flat: cells from raw to the caret target E′.
    delta: usize,
    /// Expected E-layout cells of the pasted text, in order: char +
    /// wide-base marker. Pad/spacer cells are char-only (flags don't-care —
    /// the grid scrubs them, F2).
    expected: Vec<ExpectedCell>,
}
```

Computed once at arm time from the pasted string and the pre-paste cursor
(including `input_needs_wrap`: a pending start begins the simulation at the
next row, col 0). If `delta == 0` (no non-BMP margin straddle) nothing arms.

### Arming gates (arm_psreadline_paste_cursor — kept, plus two)

Existing (all fail closed): `Shell::PowerShell`, not `ALT_SCREEN` /
`BRACKETED_PASTE`, non-empty, single-line, `cursor_at_content_end`. New
(sixth review — the modeled-character domain is closed explicitly):
- any `char::is_control` in the text (C0, DEL, **and C1**);
- any zero-width char (ZWJ, variation selectors, combining marks) —
  `ExpectedCell` cannot represent them; rejected until they earn a fixture;
- either simulated endpoint in wrap-pending state, **and** a wrap-pending
  *starting* cursor (unmeasured; fail closed until it has its own fixture);
- `E_flat <= P_flat` (checked subtraction — `delta` must be strictly
  positive and must not underflow);
- payload cell count above a fixed cap (bounds the per-chunk fingerprint
  cost; cap value set in the plan alongside the adversarial benchmark).

**BMP width assumption (documented):** P uses PSReadLine's
`LengthInBufferCells` semantics for BMP units, which is not guaranteed to
equal `unicode-width`. Divergences fail closed via raw ≠ P; fixtures pin the
codepoints actually modeled.

**Arming order (pinned — the broadened clear must not kill its own epoch):**
invalidate any old epoch → compute the candidate from the *pre-paste* state →
send the one exempt arming payload → install the new epoch. Every subsequent
user-input path clears the epoch through one central place;
CPR/graphics-reply writes remain exempt (they use `send`/writer directly and
never pass `send_external_input`).

Arm sites unchanged: `Event::Paste`, Ctrl+Shift+V, right-click `paste_text`,
`feed_text` (`foreman send --text` — verified wm.rs:1091).

### The two simulations (pure, unit-tested; shared width source)

- **E (whole-glyph / alacritty):** per char, `unicode-width` 0.2.2 (the
  single lockfile version alacritty itself parses with). Lazy pending-wrap
  *before* the next char; a width-2 char at the last column emits a deferral
  pad cell and wraps; the cursor never occupies `col == cols`. Emits the
  `expected` cell list and the endpoint.
- **P (PSReadLine #1329):** per UTF-16 unit; a non-BMP char is two width-1
  units that may split at the margin; `x == cols` rolls to `(0, y+1)`
  (assumption A1 — validated exact on both Phase 0 fixtures); no pads.

**[Dual-anchor amendment, 2026-07-12 — supersedes the implicit single
start point.]** Live user repro (the same snippet pasted repeatedly onto one
line) showed the second and later pastes never healed: PSReadLine repaints
the whole logical line from **its own believed caret** and its per-unit math
is compositional, so for a paste landing after a healed one, `P` must anchor
at the **raw grid cursor** while `E` anchors at the **aliased display point**
(the visual flow end). The standing flat divergence between them (`p_lag`,
= the previous alias delta; 0 with no alias) carries into the new `delta`,
so healing accumulates — including for a follow-up paste whose own text has
zero new divergence (delta = lag alone still arms). With no active alias the
anchors coincide and the model reduces exactly to the Phase-0-validated
form; all five frozen fixtures replay unchanged. Evidence: the live
chained-spam regression test (`live_psreadline_chained_paste_spam_heals_
cumulatively`) — pre-fix it pinned display (3,18)/raw (3,19) against flow
end (3,20); post-fix display == flow end with a cumulative alias each round.

**[Burst-extension amendment, same day.]** HELD Ctrl+V (key repeat) outpaces
the echo — PSReadLine repaints the whole growing line per paste, so by row
N the echo lags the ~30ms repeat period and re-sampling the grid per paste
yields wrong anchors (live probe: fail closed, final caret short — the
user's exact report). Fix: Session keeps a `PasteBurst` — the FIRST paste's
settled anchors plus the accumulated text — and each rapid successive paste
re-plans over the concatenation from those anchors (exactly PSReadLine's
buffer model; P is compositional). The burst is created even when the first
paste alone is delta-0 (anchors may become armable later), and dies on any
non-paste user input, mode barrier, width change, unarmable text, or
`4 × MAX_SIM_CELLS` bytes of accumulation (fail closed). Several
`Event::Paste` in one egui frame (repeat under paint load) are one logical
paste — concatenated, compared against the concatenation of their
individual `paste_seq`s. Pure layer unchanged (`text_armable` exported).
Evidence: `live_psreadline_held_paste_burst_heals_at_settle` (30 pastes at
5ms with periodic double-paste frames; red pre-fix at display (9,18) vs
flow end (9,20), green post-fix) and `paste_epoch_install_rules` cases 2/6.

### Acceptance — predicate v3 (candidate; Task 0 validates)

The epoch is an explicit three-phase machine — **Seeking → Accepted → Dead**:

- **Seeking:** after each pumped chunk, evaluate the predicate below. A miss
  (column mismatch, incomplete fingerprint, runtime `input_needs_wrap` on
  the raw cursor) **keeps Seeking** — early echo chunks are expected misses
  and must not kill the epoch before the final repaint.
- **First success → Accepted:** install
  `psreadline_cursor_alias = { raw: cursor, physical: E′ }` and stamp the
  alias generation **once** (`output_gen + 1`; `generation_after` /
  `has_fresh_cursor_alias` unchanged).
- **Accepted:** every later chunk re-runs the predicate. Success **retains
  the original generation** (no re-stamp). **[v4 amendment, 2026-07-12 —
  supersedes the original "failure → permanent Dead."]** Task 0's real-Term
  replay showed PSReadLine's mid-echo full-buffer repaints can validly
  accept, after which a progress CUP parks off-column — permanent Dead
  killed a correct alias (results doc, S2 strike 1). A failed recheck now
  splits: cursor still **on `p_col`** with a broken fingerprint/suffix →
  content corrupted → **Dead, permanently**; cursor **off `p_col`**
  (intermediate CUP) → back to **Seeking**, re-acceptance allowed (a
  re-accept stamps the new generation; Session maps Seeking to a no-op so
  a prior alias holds, display-gated on `raw == cursor`). S4 was frozen
  before v4 existed and re-ran green under it.
- **Dead:** inert until the next arm.

The predicate, on alacritty's raw grid cursor:

0. raw cursor must not be in `input_needs_wrap` state (fail this chunk).
1. **Gate (O(1)):** `cursor.col == p_col`.
2. **E′ suffix empty:** E′ = cursor advanced `delta` cells (flat, rows of
   width `cols`); every cell from E′ to the end of the viewport is empty.
3. **Full-extent back-fingerprint:** walking back from E′, the grid matches
   all `expected` cells. Cell kinds are normalized explicitly:
   glyph base = char + `WIDE_CHAR` required; **trailing wide spacer** =
   `WIDE_CHAR_SPACER` required (rewritten fresh by the final repaint, so it
   survives); **leading deferral pad** = char `' '` only, flags don't-care
   (the F2 scrub erases them — observationally identical to a payload space,
   a documented ambiguity); payload space = char `' '` only. If the walk
   would leave the retained grid/history, **fail closed**.

Phase 0 evidence: v1 (strictly-behind) loses coverage when raw lands on a
trailing-space cell (holdout, delta = 1); v2 (bounded local fingerprint)
accepts 9 wrong rows on a periodic payload (rep divides width). v3 fired only
with the correct alias across ~33k byte boundaries on both captures — in the
approximate Python replay; hence Task 0.

Cost: **O(payload + viewport suffix)** behind the O(1) column gate;
armed-epoch only. Unarmed path: zero cost (strictly cheaper than today's
per-byte second VTE parse). New armed-path benchmark required (the existing
`scanner_overhead_on_plain_and_ansi_floods` covers only the unarmed path).

### Epoch lifecycle

- **Any user-sent bytes end the epoch** (today only CR/LF/^C do). The pinned
  type-X-then-Backspace drop moves from echo-print detection to its cause;
  the first post-paste Backspace still seeds the wide shadow from the alias
  in the same frame before its bytes go out.
- Cleared as today on: resize, submit/interrupt, new paste, image paste, and
  the post-chunk `ALT_SCREEN | BRACKETED_PASTE` mode check.
- **Mode round-trip: DECIDED (sixth review) — minimal streaming mode sniff.**
  A transient alt-screen exit restores the original primary grid, so v3 can
  match across a semantic barrier — and the alias controls wide-key doubling
  (input, not just paint). PasteSim carries a private, allocation-free state
  machine that scans chunk bytes for CSI private modes `?47/?1047/?1049/?2004`
  with `h`/`l`, state retained across chunk boundaries; any hit → Dead. This
  is a byte scanner, not a VTE parser — false positives only lose coverage
  (acceptable). The post-chunk actual-mode check stays as the backstop. The
  two existing round-trip unit tests keep their expectation (drop), retargeted
  at the sniffer.
- **Accepted residual (documented):** child-originated post-acceptance output
  that keeps the cursor on `p_col`, preserves the full extent, and contains
  no sniffed mode transition survives re-verification (today's per-byte
  print detection would drop it).

### Consumers — unchanged

`input_cursor_point`, `display_cursor_point`, `effective_cursor_info`,
snapshot `--cursor`, immediate wide-shadow seeding, all of
`WideShadowState`/`WideKeyLatch`/`keep_wide_shadow`. Grid, CPR replies, VT
parsing stay authoritative. `pump_at` collapses to plain `advance_scanned`
for every chunk (flush-after-exact-chunk CPR cadence untouched) plus the
post-chunk predicate while armed. No wire changes.

### Module shape

`PasteSim` is a deep private module, not a state bag — simulation, predicate
v3, the mode sniffer, and the phase machine all live inside it. Interface:

```rust
PasteSim::try_arm(term, pre_paste, text) -> Option<PasteSim>
PasteSim::observe_chunk(&mut self, term, bytes, next_gen) -> Phase
// Phase = Seeking | Accepted(CursorAlias) | Dead
```

`Session` maps `Accepted` onto `psreadline_cursor_alias`/`_gen` and `Dead`
onto clearing them; it never touches ordering, predicate details, or
generation rules. Production and the Task 0 replay tests drive the **same**
interface — no adapter layer.

### Deletions (single commit, after Task 0 + gates)

`CupScanner`, `CupSink`, `CupScanEvent`, `CupScanResult`,
`advance_psreadline_scanned`, **the free function**
`psreadline_cursor_alias(...)` (the pad-count validator — NOT the
`CursorAlias` struct and NOT the `Session.psreadline_cursor_alias`/`_gen`
fields, which the display/input consumers keep), the `pump_at` scanner
branch, and their unit tests (replaced by sim/predicate tests). Net LOC
expected negative — verified at the gate, not asserted.

## Task 0 — v3 validation gate (blocks implementation beyond itself)

0. **Make the oracle assertable (sixth review):** the checked-in fixtures
   are framed bytes only. Extend the harness to emit a JSON sidecar per
   capture — dimensions, prestate (cursor, `input_needs_wrap`, history),
   raw/pre-CUP endpoints, final-grid hash, term modes, and version
   provenance (commit, pwsh/PSReadLine/ConPTY versions) — and re-capture
   S1/S2/S3 with sidecars (new SHA-256s recorded; re-captured S2 is no
   longer blind, which is fine — it validated P, and v3's blindness comes
   from step 3).
1. Implement `PasteSim` (simulations, predicate v3, mode sniffer, phase
   machine) in Rust behind the module interface above.
2. Replay every fixture through a real `Term<VoidListener>` **one byte at a
   time** from the sidecar prestate, asserting: the phase machine never
   yields a wrong alias at any prefix, coverage fires on the expected
   captures, and the final grid hashes to the sidecar value.
3. Freeze a **new v3 blind holdout** before running it (different cols/
   prompt/payload phase; PSReadLine prediction/history disabled in the
   fixture shell — `Set-PSReadLineOption -PredictionSource None
   -HistorySaveStyle SaveNothing`), plus a **non-BMP P==E control** (emoji
   present but never straddling a margin → must not arm).
4. **Adversarial benchmark:** worst-case armed cost — many matching-column
   chunks against a payload at the arming cap.
5. Kill-switch: any wrong alias or missed coverage → v4 iteration on the
   captures, new frozen holdout again; two consecutive failures → park with
   negatives recorded.

## Decisions and rejected alternatives

| Decision | Rejected alternative | Why |
|---|---|---|
| Dual simulation, exact-match on P | bounded-delta (reviewer sketch) | doesn't pin where PSReadLine parked; weaker fail-closed |
| | grid-only content-end alias | can't distinguish PSReadLine from any cursor-parking app |
| | rely on alacritty `RenderableCursor` | one cosmetic backward snap (term/mod.rs:2371-2377); cannot know the app's intent; doesn't fire on these cells |
| | wait for upstream / mode 2027 / WT-style global console measurement | #1329 stalled since 2019; PSReadLine doesn't negotiate; global measurement degrades every modern TUI |
| Scroll-invariant relative validation | `history_size`-delta scroll compensation | undercounts at the 10k scrollback cap — permanent coverage loss in long-lived sessions |
| Predicate v3 full extent | v1 strictly-behind; v2 local fingerprint | measured dead: coverage loss (F3) and 9 wrong aliases (F4) respectively |
| Per-chunk acceptance + permanent epoch death on failed re-verify | accept only at CUP bytes (today) | requires the second parser this rework deletes; v3's extent check replaces the VT anchor, validated per Task 0 |
| Any-user-bytes epoch end | per-byte print/execute invalidation (CupSink) | needs the second parser; cause-side clearing covers the pinned case |
| P==E → don't arm | always-arm like the scanner | zero ongoing cost for the overwhelmingly common case |

## Evidence gates before "fixed" (unchanged from the reviewed brief)

Pure sim/predicate table tests (both variants' fixtures, trailing-space and
wrap-pending and control-char fail-closed cases, one ZWJ fail-closed pin, the
chunk-boundary-at-P regression); Task 0 replay green incl. fresh holdout;
existing live test `live_psreadline_paste_wrap_uses_the_whole_emoji_endpoint`
passes unchanged; new live multi-row test (caret == E′, first Backspace
doubles); headless snapshot probes on a release build; armed-path benchmark;
unarmed perf test stays green.

## Out of scope

ConPTY resize reflow (`docs/conpty-resize-reflow.md` — alias keeps failing
closed across resize); buffer `U+FFFD` corruption (upstream; foreman renders
faithfully); multiline/mid-line pastes (fail closed as today); non-PowerShell
shells; `docs/wide-chars.md` mechanism corrections land with this spec's
commit (F2: pads scrubbed-not-absent; `send --text` does arm).
