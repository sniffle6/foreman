# Phase 0 results — paste-simulation fingerprint probe

Protocol: `2026-07-12-paste-sim-phase0-protocol.md` (frozen before any run;
unmodified). Build `68ed2d0` release, pwsh via `preferred_powershell`,
harness `phase0_probe` (`src/terminal.rs` tests, `#[ignore]`d). Captures:
`%TEMP%\phase0-{s1-primary,s2-holdout,s3-control}.bin` (framed).

## Verdict (amended after fifth review — the defensible grading)

- **P/E geometry feasibility: GREEN.** raw == P exact on primary and on the
  blind holdout; delta exact on both. Zero model revisions, so S2 fully
  validates **P**.
- **Pad-scrub mechanism (F2): supported** by the vendored alacritty source
  and the byte evidence.
- **Predicate v3: promising CANDIDATE, not validated.** S2 is v3's *training
  data* (its periodic collisions caused v3 to be invented), the retained scan
  is an approximate Python replay (`phase0_predicate_v3.py`), and that
  approximation never modeled the F2 flag scrub — which hides a latent v3
  bug: the back-fingerprint must treat pad cells as char-only (flags
  don't-care) because the real grid scrubs them. Validation requires the
  spec's Task 0: the real-`Term` byte-at-a-time replay over the checked-in
  fixtures plus a FRESH frozen holdout (PSReadLine prediction/history
  disabled) and a non-BMP P==E control.
- **Pre-registered oracle O1: FAILED as frozen, then revised.** The frozen
  oracle anchors at the last non-space cell; both E endpoints sit one past a
  trailing payload space (S1: oracle 438 vs E 439; S2: 336 vs 337). The
  trailing-space extent model was derived mid-run from the bytes and the
  geometry re-graded under it. Recorded as an oracle failure per protocol —
  not silently re-interpreted.

Fixtures checked in at `tests/fixtures/phase0/` (SHA-256, full):
- s1-primary: `3a80274bc0e2d149749ff8195182468772e44c47acf1ee763693aface0e1a08d`
- s2-holdout: `1b17486e321a9167fb70d445cf3d2284b2582f9ce257411bb98be73281513266`
- s3-control: `973f58d780e237d67c0346cc62fdb6d4a551c809e18b4a317ff9fb0ca1565dbf`

### Re-capture (sidecar generation)

Supersedes the three hashes above. Same S1/S2/S3 geometry expectations
(post raw (10,35)/(10,6)/(10,3)); PSReadLine prediction/history disabled;
sidecar JSON co-located with each `.bin` for Task 0 real-Term replay. S2 is
no longer blind (P already validated); v3 blindness comes from Task 6's S4.

| file | SHA-256 |
|---|---|
| phase0-s1-primary.bin | `59e0c770e72782ff1eb98374251005fcde64cc9f046645e188d76461678efd1d` |
| phase0-s1-primary.json | `12d2e040a8b90efe16ad5aafad0f29704e32f98b93987c5f213d570e42f3494f` |
| phase0-s2-holdout.bin | `46922ec9a712ce034bb4f57b7f14261c83bb799b98b356263682dfdf2945dfe3` |
| phase0-s2-holdout.json | `806ed3b75d1943e205ff02dee8ed107fb5626f947d32714dd7acd514fe10aa01` |
| phase0-s3-control.bin | `861b31f75948a2ea04b489e73187ca6b871195aa96a4d54ae4d0a5e555b25ef2` |
| phase0-s3-control.json | `405570d5ba5d19eb790d9c0aa9d30d4c831996a20aca748d9352c4fdf7f2e99e` |

Provenance (S1 sidecar): commit `68ed2d0`, pwsh 7.5.8, PSReadLine 2.4.5,
OS 10.0.26200.0.

## Original measurements

| | predicted | measured |
|---|---|---|
| S3 control raw | flat 403 (= P = E), no alias | (10,3) = 403, alias None ✓ |
| S1 primary raw | P = flat 435 (10,35) | (10,35) ✓ exact |
| S1 caret target | E = flat 439 = raw + 4 | 439 ✓ (see trailing-space note) |
| S2 holdout raw (BLIND) | P = flat 336 (10,6) | (10,6) ✓ exact — graded blind, model unmodified |
| S2 caret target | E = flat 337 = raw + 1 | 337 ✓ |

The P model (per-UTF-16-unit, assumption A1) and the E delta are exact on
the primary and on the untouched blind holdout. No model revision was needed,
so the holdout counts as full validation **of P** (not of the predicate —
see the amended verdict).

## Findings

**F1 — the echo mechanism.** The paste echo is 11 incremental *full-line
overwrite repaints*: `CUP 1;4` → entire buffer as flowing SGR text → endpoint
CUP (advancing 11;26 → 11;36). No hard-positioned rows, no CR/LF. Intermediate
repaints end in lone surrogates (U+FFFD in the stream) that later repaints
overwrite; the final grid is clean.

**F2 — the pad-flag scrub (root cause of the parked residual).**
`alacritty write_at_cursor` (vendored 0.26, term/mod.rs:993-1008) removes the
previous row's `LEADING_WIDE_CHAR_SPACER` when overwriting a wide cell at
column ≤ 1. PSReadLine's repaint style triggers this on every repaint after
the first: pads persist as *layout* (space cells) but lose their flags.
`docs/wide-chars.md`'s "hard-positioned, zero pads" residual description is
therefore right in observation, wrong in mechanism — the pads were
**scrubbed, not absent** — and c0bdb7c's pad-count validator failed because
it counts a flag the app's own redraw pattern erases. (Doc correction goes in
with the spec commit.)

**F3 — trailing-space geometry.** A payload ending in spaces puts the caret
target (E) one-past those empty-reading cells; the last *non-space* cell is
not the caret anchor. With delta = 1 the raw cursor lands **on** the trailing
space cell.

**F4 — periodic payloads defeat local fingerprints.** S2's rep (11 cells)
divides its width (33): every row end is identical to the tail. The
byte-at-a-time scan showed the local-fingerprint predicate firing at 9 wrong
rows mid-echo. Any tail check of bounded length fails this; only full extent
distinguishes.

## Predicate evolution

Scans were **approximate token/state-boundary replays**, not literal
every-byte real-Term replay: the Python interpreter consumes whole CSI
sequences and whole UTF-8 chars per step and models no flag scrub. The real
replay is spec Task 0. Boundary counts below are token positions.

| Predicate | S1 (20,311 boundaries) | S2 (12,861 boundaries) |
|---|---|---|
| v1: col + strictly-behind + one-past-content | correct alias only | **never fires** (F3: raw on trailing space) — coverage lost |
| v2: col + local tail fingerprint | correct alias only | **9 wrong aliases** (F4) — unsafe |
| **v3: col gate + E′ suffix-empty + full-extent back-fingerprint** | correct alias only (2 firings at/after final CUP) | correct alias only (8 firings, all identical) |

**v3, precisely:** on a pumped chunk, if `cursor.col == P.col`: let
E′ = cursor + delta (flat, rows of width `cols`). Require (a) every cell from
E′ to the end of the viewport empty; (b) walking back from E′, the cells
match the full expected E-layout of the pasted text — chars everywhere,
`WIDE_CHAR` on glyph bases, and pad/spacer cells matched as char-only
(flags don't-care — the real grid scrubs them, F2), all
`E_flat − start_flat` cells of it. (b) subsumes strictly-behind,
stale-residue defense, and periodicity defense. Cost:
**O(payload + viewport suffix)** per candidate, armed-only, gated behind the
O(1) column check.

## Scope limits (unchanged from protocol)

Green establishes feasibility on these fixtures under this conhost/ConPTY
build. It is not global safety and not yet permission to delete the scanner.
Still required before that: full spec (predicate v3), implementation plan,
pure sim + acceptance tests in Rust, both live `#[ignore]`d tests green, the
armed-path benchmark, and validation of the production per-chunk evaluation —
established by Task 0's real-Term replay, not by these scans.

Analysis tooling: `phase0_predict.py` (frozen model), `phase0_analyze.py`
(stream decode), `phase0_diff.py` (redraw diff), `phase0_o4_scan.py`
(v1/v2 boundary scans), `phase0_predicate_v3.py` (v3 scan, preserved
verbatim; superseded by the Rust real-Term replay in spec Task 0).

## Task 0 gate (real-Term byte-at-a-time replay)

| fixture | arm | final Accepted | wrong-alias prefixes | post/grid eq |
|---|---|---|---|---|
| S1 primary | yes | yes (10,35)→(10,39) | none | yes |
| S2 holdout | yes | yes (10,6)→(10,7) | none | yes |
| S3 control | no | never | n/a | yes |
| S4 v3 holdout (frozen blind) | yes | yes (13,33)→(13,36) | none | yes |
| S5 non-BMP P==E control | no | never | n/a | yes |

**v3 predicate geometry: GREEN.** One phase-machine revision (v4) was
required after the first S2 run:

- **S2 strike 1 (v3 permanent-Dead recheck):** mid-echo full-buffer repaint
  left the cursor on `p_col` with a matching full-extent fingerprint →
  Accepted at the correct alias, then a progress CUP parked off-column →
  permanent Dead, final phase not Accepted.
- **v4 (not a predicate/oracle change):** recheck miss while still on
  `p_col` (fingerprint/suffix broken) → Dead; recheck miss with cursor
  off `p_col` (intermediate CUP) → Seeking, re-accept allowed. Session
  maps Seeking to no-op so a prior alias can hold across intermediate
  CUPs. Mode-barrier still → Dead.
- **S2 + S4 re-run under v4: GREEN** without fixture or prediction edits.
  S4 was frozen before the v4 design and was not used to invent it.

### S4/S5 fixture hashes (frozen predictions in `phase1_predict_v3.py`)

| file | SHA-256 |
|---|---|
| phase0-s4-v3holdout.bin | `fb57cdbe9f4170c7c5758e7ebd83fdb2cf5fdf129256733562aaa12bc9350e8a` |
| phase0-s4-v3holdout.json | `e69fdfdf747724a5471e2022649c73935877cd1338995e5c6e206884095e9e15` |
| phase0-s5-nonbmp-control.bin | `db6eb129828f3530bdfcfe39a67b62dca69086b64358f7e07f2512dcdb57fe46` |
| phase0-s5-nonbmp-control.json | `9d8d51a1ef0fec89857f916e4a7f89af3cf1680247317984c46f1c5336934248` |

S4 live post (quarantined at capture): raw (13,33), history 173, leading
pads 3 (some retained on this scroll fixture). S5 payload k=4
(`🤣🤣 ` × 40), post (5,3), delta 0, did not arm.

**Predicate status:** v3 geometry + v4 phase machine **validated on these
five fixtures**. Scanner deletion still requires live tests (Task 9) green
on the production path.

### Armed-path benchmarks (Task 7)

`cargo test --release armed_ -- --ignored --nocapture` (2026-07-12):

| case | result |
|---|---|
| worst-case accepted reverify (10k rounds, 199 cols, ~3600 cells) | **28.9 µs/chunk** (ceiling 200 µs; ~7× headroom) |
| column-miss O(1) gate (50k rounds) | **5 ns/chunk** |
| sniffer 1 MiB @ 399×100 Seeking | **855 µs** (~1170 MB/s; ceiling 50 ms) |

Unarmed graphics-scanner guard still green:
`scanner_overhead_on_plain_and_ansi_floods` plain 2.52% / ansi 5.89% overhead.

### Production wiring + scanner deletion

- `Session.psreadline_paste_sim: Option<PasteSim>` replaces `CupScanner`.
- Live tests green **before** and **after** deletion:
  `live_psreadline_paste_wrap_uses_the_whole_emoji_endpoint`,
  `live_psreadline_multirow_paste_aliases_to_flow_end`.
- Deleted: `CupScanner`, `CupSink`, `CupScanEvent`, `CupScanResult`,
  `advance_psreadline_scanned`, free `psreadline_cursor_alias`, and their
  pure unit tests. Kept: `CursorAlias`, Session alias fields, consumers.
- Net: new `src/paste_sim.rs` (~980 lines incl. tests/fixtures harness) vs
  removed scanner path + tests (substantial net complexity move into a deep
  module; production `pump_at` no longer second-parses VT).

### Post-review live repro: chained pastes (dual-anchor fix, 2026-07-12)

User report: spamming the same short emoji snippet (Ctrl+V repeatedly onto
one line) still showed the short caret. Live probe (12 paced pastes of
`"  🥒🤣🤣🤣 🥒🤣🤣🤣 🥒🤣🤣🤣"`, 112 cols) reproduced it: round 3
straddled and healed (alias +1); rounds 4–11 **did not even arm** — with
both simulations anchored at the healed display point, a non-straddling
snippet computed `delta_new = 0` while the standing 1-cell lag persisted.
Final: display (3,18) / raw (3,19) vs true flow end (3,20).

Root cause: PSReadLine repaints the whole logical line from its own
believed caret and its per-unit math is compositional —
`P(prompt, all) = P(raw_prev, new)` — so P must anchor at **raw** and E at
the **display point**, with the standing lag (`p_lag`) carried into delta.
Fix in `plan()`/`try_arm`/`paste_epoch_candidate`; lag-0 reduces exactly to
the validated single-paste model (all five frozen fixtures replay
unchanged). Pinned by `chained_paste_anchors_p_at_raw_and_carries_the_lag`
(pure) and `live_psreadline_chained_paste_spam_heals_cumulatively` (live:
post-fix display == flow end (3,20), cumulative alias every round).

Second live repro (same day): HELD Ctrl+V — key repeat (~30ms) outpaces the
echo because PSReadLine repaints the whole growing line per paste, so each
re-arm sampled a stale grid → fail closed → final caret short (user's pane:
raw (27,0) vs flow end (27,26), no alias). Fix: Session-level burst
extension — keep the first paste's settled anchors + accumulated text,
re-plan over the concatenation per paste; multiple paste events per egui
frame are one logical paste. Pure layer unchanged. Pinned by
`live_psreadline_held_paste_burst_heals_at_settle` (red pre-fix at 5ms
pacing with double-paste frames, green post-fix) and
`paste_epoch_install_rules` cases 2/6.

Separately confirmed in the same session, not foreman bugs: (a) ghost cells
surviving select-all+Backspace and Enter are stale grid content PSReadLine
never erased (its extent math is short by the pad count) — `clear`/`Ctrl+L`
heals, upstream; (b) the sandboxed dev shell's clipboard is isolated from
the user's real clipboard — `Set-Clipboard` there does not affect what the
GUI pastes.
