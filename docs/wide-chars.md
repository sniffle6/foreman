# Wide characters (CJK / emoji)

What: how foreman handles width-2 glyphs (one `WIDE_CHAR` base cell + one
spacer cell) across paint, snapshots, and key input.

Status: evidence recorded 2026-07-10; fix in progress per
`docs/superpowers/plans/2026-07-10-wide-char-tofu-and-drift.md` (Task 4B,
amended). Ownership rules section will be finalized with the fix.

## Evidence 2026-07-10 (pwsh 7.5.8, ConPTY, foreman HEAD df46b2d)

All probes ran headlessly via `foreman send` / `foreman snapshot --cursor`
against a live PowerShell session; codepoints read straight from the
snapshot text. "Doubled" = foreman's wide-key doubling fired (2 sequences
per physical press).

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

Hold-Backspace corruption root cause: correct-per-press doubling still
desyncs during key repeat because each frame re-sampled the **stale** grid
(echo not yet landed) and restarted the shadow simulation from the old
cursor. Fix: persist the shadow row across frames until `output_gen`
advances (`Session.wide_shadow`).

Re-verification (post-fix, same pwsh session): emoji BS removes exactly one
emoji; `中中中` + BS leaves `中中` (over-delete gone); CJK/emoji Left both
move 2 cells; Delete on emoji base removes one clean emoji; 8 rapid
no-settle Backspace sends over a mixed emoji line leave **zero** U+FFFD.

Known limitation: `foreman send --keys` samples the live grid per REQUEST.
A burst of separate no-settle send calls can still race the echo — put the
whole burst in one `--keys "Backspace Backspace …"` list (simulated
in-batch) or use `--settle-ms`. The live keyboard path has no such gap
(shadow persists across frames).

Also observed, out of scope: after any corruption, ConPTY's line redraw
leaves `�` residue mid-line and on rows below the prompt; `Ctrl+L` heals.
Same family as the settled resize-reflow divergence
(`docs/conpty-resize-reflow.md`) — do not chase.

Gotcha for readers: `�` (U+FFFD) in the grid means someone put a lone
surrogate half in the console buffer. That is buffer corruption upstream of
foreman's renderer; foreman paints it faithfully.

Key files: `src/input.rs` (CellWide + doubling policy), `src/frame.rs`
(paint spacer skip), `src/inspect.rs` (snapshot spacer skip),
`src/terminal.rs` (cursor-row sampling / shadow persistence).
