# Wide characters (CJK / emoji)

What: how foreman handles width-2 glyphs (one `WIDE_CHAR` base cell + one
spacer cell) across paint and snapshots — and why foreman deliberately does
**not** compensate for PowerShell/PSReadLine's broken wide-char caret math.

## Status: fixed IN PSREADLINE, not in the terminal (2026-07-13)

Windows' line editor edits by **UTF-16 unit**. A non-BMP emoji is two units,
so one Backspace deletes half a glyph (leaving `�`) and one Left parks the
caret inside it. Foreman spent three escalating attempts compensating for
this from the terminal side — key doubling, a simulated "wide shadow" input
row, and a dual-simulation paste caret alias (`src/paste_sim.rs`). Every one
of them modeled the shell's state, every one desynced, and the last one broke
arrow-key navigation outright. **All of it is deleted (~2,600 lines).**

The fix now lives where the bug lives. At pwsh spawn, foreman asks PSReadLine
— which owns the input buffer and always knows the truth — to bind
**Backspace / Delete / LeftArrow** to surrogate-aware handlers
(`src/psreadline.rs`, PowerShell as a const). **Foreman models nothing.** It
sends one keypress per keypress and paints alacritty's cursor.

Measured, live, on the production spawn path
(`live_psreadline_wide_edits_delete_and_cross_whole_emoji`):

| | stock pwsh | with the fix |
|---|---|---|
| Backspace over emoji | `�`, glyph split | whole glyph deleted, zero `�` |
| Delete over emoji | glyph split | whole glyph deleted |
| Left over emoji | parks mid-glyph | crosses the whole glyph |
| Delete with caret parked INSIDE a glyph | splits it | removes the glyph whole |
| **Held** Backspace, 30 repeats over a wrapped line | half-glyphs | 30 whole glyphs, zero `�` |

The held-repeat case is the one that defeated every terminal-side model: with
no state to desync, key repeat is just key repeat.

### RightArrow is deliberately NOT bound (do not "finish the set")

Binding RightArrow — **even to a handler that immediately delegates back to
`ForwardChar`** — destroyed PSReadLine's active-suggestion state, so the inline
prediction could no longer be accepted. That shipped once and was reported
within minutes ("I can't press right arrow to autocomplete anymore"). Measured:
with the override in place, the shell ran the typed prefix instead of the
suggestion.

The cost of leaving it stock is that Right steps one UTF-16 unit and can park
the caret *inside* an emoji. That is handled, not ignored: Backspace and Delete
each cover the caret-inside-a-pair case and still remove the glyph whole, so no
edit can split a surrogate pair. `wide_edit_fix_binds_three_keys_and_never_rightarrow`
pins the decision.

Same lesson, second instance: a handler must **delegate to the built-in** in
every case it does not specifically own. The first version of these handlers
replaced the built-ins outright and silently killed `Ctrl+A` + Backspace
(selection delete), because deleting an active selection is what the built-in
`BackwardDeleteChar` does.

**Still upstream, still unfixed (accepted):** the *caret position* after a
wrapped emoji paste. PSReadLine's `LengthInBufferCells` miscounts surrogate
pairs when it renders (#1329), so its caret parks short of the visible text
end. No key handler can reach that math, and Windows Terminal and VS Code show
it too. `Enter` or `Ctrl+L` resyncs. Foreman does **not** compensate — that
compensation is exactly what broke navigation before.

### Two gotchas that cost real time

- **The UTF-8 line in `WIDE_EDIT_FIX` is load-bearing.** Running *any*
  ScriptBlock key handler makes PSReadLine re-render the line through
  `[Console]::OutputEncoding`, which defaults to a legacy codepage — without
  setting it to UTF-8, every emoji on the line renders as `?` the moment you
  press Backspace. Measured, not theorized.
- **`$n = 2` only for surrogate pairs, never for cell width.** CJK `中` is
  width-2 on screen but a *single* UTF-16 unit — doubling it over-deletes.
  That was the old terminal-side approach's probe-#3 bug; the handlers key off
  `IsHighSurrogate`/`IsLowSurrogate`, not width.

## Ownership rules

- **Paint never renders spacer cells** (either spacer flag). The one
  classifier is `CellWide::classify` in `src/input.rs` (flags + base char);
  walks that only need spacer-ness use `CellWide::is_wide_spacer`. A new
  alacritty flag is one edit there — do not add a second inline flag check
  (df46b2d needed 4 edits because there were 4 copies).
- **Snapshot text/cells skip spacers** through the same classifier.
- **Key input is never doubled, rewritten, or modeled.** One physical press =
  one encoded sequence. Foreman does not track a shadow of the input row and
  does not predict where the shell's caret "should" be. Whole-glyph editing is
  PSReadLine's job (`src/psreadline.rs`).
- **The painted caret is alacritty's caret.** No alias, no correction layer.
  `foreman snapshot --cursor` reports the same point.
- **Wide-char editing belongs to the shell, not the terminal.** If a wide-char
  edit symptom appears, fix it in `WIDE_EDIT_FIX` (or upstream). Do not add
  terminal-side compensation — see "Why the terminal-side approach failed".

## Evidence 2026-07-10 (pwsh 7.5.8, ConPTY, foreman HEAD df46b2d)

**This section measures CONHOST/PSReadLine, not foreman.** It is retained
because it is the ground truth about the platform. The doubling it refers to
was foreman's (now deleted) compensation, used here only as an experimental
lever to reveal how the shell edits.

All probes ran headlessly via `foreman send` / `foreman snapshot --cursor`
against a live PowerShell session; codepoints read straight from the
snapshot text. "Doubled" = two sequences were sent per physical press.

| # | Probe | Result | Meaning |
|---|-------|--------|---------|
| 1 | `🥒🤣🤣🤣`, one Backspace **press** (doubled → 2×DEL) | `🥒🤣🤣`, clean pairs | ambiguous alone (see #2) |
| 2 | one **raw** `0x7f` via `--text` (no doubling) | `🥒🤣�` — lone surrogate U+FFFD, plus `🤣 ����` redraw junk mid-line | **Backspace deletes one UTF-16 unit** — half a surrogate pair |
| 3 | `中中中`, one Backspace press (doubled) | only `中` remains — **two chars eaten** | BMP wide chars are 1 unit; doubling **over-deletes CJK** |
| 4 | `中中`, one Left press (doubled) | cursor col 31 → 27 (−4 cols, two chars) | arrows are unit-based too; doubling **over-moves CJK** |
| 5 | `🤣🤣`, one Left press (doubled) | cursor col 31 → 29 (−2 cols, one emoji) | doubling **correct** for non-BMP arrows |
| 6 | Delete on emoji (doubled → 2×`CSI 3~`), live user test | 1 emoji deleted, caret shifted, tofu left | doubling fired from a **spacer-parked** cursor and crossed into the next glyph (see E2/E3) |
| E | one `CSI 3~` (single) on emoji base | `�🤣` — half-deleted | forward-Delete is unit-based too (first amendment "grapheme-aware" was wrong) |
| E2 | raw 2×`CSI 3~` on emoji base (Home first) | one emoji cleanly deleted | doubled Delete correct **on a base** |
| E3 | raw 1×`CSI 3~` on CJK base | whole `中` deleted | single Delete correct on BMP base |

Verdict: **BRANCH B, amended twice.** conhost's cooked editing is
UTF-16-unit-based for Backspace, Delete, and cursor movement — uniformly.
Cell width (2 columns) is NOT the editing unit — surrogate-pair-ness is:

- non-BMP glyph (emoji, 2 units): crossing/removing a **whole** glyph needs
  **2** sequences (Backspace/Delete/←/→ alike)
- BMP wide glyph (CJK, 1 unit): everything needs **1** sequence
- parked mid-glyph (cursor on a spacer): **1** sequence finishes the glyph;
  doubling there crosses into the neighboring glyph (probe #6's tofu)

Cursor CELL movement follows glyph width (2 cells per whole crossing for
both emoji and CJK), independent of sequence count.

**Historical (the shadow chase, 2026-07-10 — machinery now deleted).**
Correct-per-press doubling still desynced under key repeat: each frame
re-sampled a **stale** grid (the echo had not landed) and restarted the
simulation from the old cursor. Three successive fixes — persist across
`output_gen`, span soft-wrapped rows, then gate resampling on observed
quiescence (50ms PTY silence + 150ms key silence + no owned hold) — each
cured its repro and left the next one alive. The lesson that survives: **the
grid always lags the input stream**, so any model seeded from the grid
during typing is seeded from the past. All of this code is gone; the
sequence is recorded so nobody rebuilds it step by step.

Paste-only caret finding (live repro 2026-07-10 — upstream, NOT fixed): this
is separate from shadow lifetime and reproduces before any edit key. At 102
columns, a prompt ending at zero-based col 27 followed by 74 `a` cells leaves
one cell at the margin. Pasting `🤣` produced this final PSReadLine redraw:

```text
CUP 7;28  +  74 x "a"  +  🤣  +  CUP 8;2
```

Alacritty correctly defers the complete width-2 glyph, inserts one
`LEADING_WIDE_CHAR_SPACER`, and naturally ends at zero-based `(7,2)`. The
final CUP parks it at `(7,1)`, inside the emoji spacer. Repeating a mixed unit
over a long line accumulated six such pads: PSReadLine's CUP ended at col 77
while the rendered text ended at col 83.

The exact cause is PSReadLine 2.4.5, not a stale shadow or an old ConPTY.
`ConvertOffsetToPoint` iterates UTF-16 `char`s and
`LengthInBufferCells(char)` explicitly does not combine surrogate pairs, so
`🤣` is measured as width 1 + 1 and can split at the margin. BMP `中` is one
UTF-16 char of width 2 and already defers correctly. The limitation remains in
PSReadLine main; see
[Render.cs](https://github.com/PowerShell/PSReadLine/blob/v2.4.5/PSReadLine/Render.cs#L1364-L1418),
[Render.Helper.cs](https://github.com/PowerShell/PSReadLine/blob/v2.4.5/PSReadLine/Render.Helper.cs#L49-L120),
and upstream issue
[#1329](https://github.com/PowerShell/PSReadLine/issues/1329). Foreman already
ships Microsoft's newest ConPTY package, so a package bump cannot repair this.

## Why the terminal-side approach failed (do not re-litigate)

Every compensation foreman built was a model of a shell that is lying about
its own geometry. Three escalating attempts, each fixing its predecessor's
symptom and exposing a worse one:

1. **Wide-key doubling + wide shadow** (be4901b…c0bdb7c). One press = two
   sequences when crossing a non-BMP glyph, driven by a simulated copy of
   the input row. Correct per-press, but the shadow must be re-sampled from
   the grid, and the grid *lags the input stream* — every long-line hold
   re-anchored on a mid-redraw screen. Fixed with quiescence gating, wrap
   spanning, cell-shift semantics… and it still desynced.
2. **CUP interception** (c0bdb7c). Watch PSReadLine's final cursor-position
   escape and alias it forward by the pad count. Failed closed on exactly
   the big pastes that hurt (F2: alacritty scrubs the pad flags the
   validator counted).
3. **Dual-simulation paste alias** (`src/paste_sim.rs`, 2026-07-12).
   Simulate both width models, alias the caret to the whole-glyph flow end
   on an exact grid fingerprint match. It worked — the *caret* landed
   correctly — and that is precisely what exposed the fatal flaw.

**The fatal flaw:** the alias is display-only, but **PSReadLine's internal
caret is genuinely wrong**, and PSReadLine is what processes your arrow
keys, Backspace, and Home/End. Painting the caret at the true text end while
the shell edits from a different coordinate made navigation *worse*: Left/
Right jumped over glyphs and could not come back (live report, 2026-07-12).
A visibly-wrong caret you can navigate with beats a correct-looking caret
you cannot. Fixing the display without fixing the shell's model is not a
fix; it is a lie that the editing keys then contradict.

There is no display-layer fix. The only real fixes live upstream
(PSReadLine #1329 — `LengthInBufferCells` does not combine surrogate pairs)
or in a full local line-editor, which foreman is not. Windows Terminal and
VS Code exhibit the same bug and also do not compensate.

**Retry condition:** only if PSReadLine ships a fix (then delete nothing —
just verify), or if foreman ever owns line editing itself. A new "narrow,
reversible" compensation layer is not a new idea; it is attempt #4.

Research record (measurements, byte traces, the P/E width models, and the
Phase 0 protocol): `docs/superpowers/specs/2026-07-12-paste-sim-phase0-protocol.md`
and `…-results.md`. The code they describe no longer exists.

## Residue you will still see (upstream — do not chase)

- After a wrapped emoji paste, the caret sits short of the text end. Enter or
  `Ctrl+L` resyncs. PSReadLine also stops clearing cells at its (short)
  believed content end, so stale glyphs can survive select-all + Backspace;
  `clear`/`Ctrl+L` heals them.
- `�` (U+FFFD) in the grid means a lone surrogate half is in the console
  buffer — buffer corruption upstream of foreman's renderer, which foreman
  paints faithfully. Backspace on an emoji deletes one UTF-16 unit (half a
  glyph) because conhost's cooked editing is unit-based; that is the shell's
  behavior, not foreman's.
- ConPTY line redraws leave `�` residue mid-line and below the prompt after
  corruption. Same family as `docs/conpty-resize-reflow.md`.

## Key files

- `src/psreadline.rs` — the whole shipped fix: `WIDE_EDIT_FIX`, the pwsh
  Set-PSReadLineKeyHandler bindings, and the tests that pin RightArrow as
  deliberately unbound.
- `src/input.rs` — `CellWide::classify` / `CellWide::is_wide_spacer`, the one
  home for width classification; key encoding.
- `src/frame.rs` — paint spacer skip (`plan_paint`, `overlays`) and the
  wide-caret span.
- `src/inspect.rs` — snapshot spacer skip (`snapshot_text`, `snapshot_cells`).
- `src/terminal.rs`, `src/emoji_raster.rs` — emoji raster/stamp paint.
