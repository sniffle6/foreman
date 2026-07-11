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
| 6 | Delete on emoji (doubled → 2×`CSI 3~`), live user test | 1 emoji deleted, caret shifted, tofu left | **forward-Delete is grapheme-aware**; doubling eats 1.5 emoji |

Verdict: **BRANCH B, amended.** conhost's cooked editing is UTF-16-unit-based
for Backspace and cursor movement, grapheme-based for forward-Delete. Cell
width (2 columns) is NOT the editing unit — surrogate-pair-ness is:

- non-BMP glyph (emoji, 2 units): Backspace/←/→ need **2** sequences
- BMP wide glyph (CJK, 1 unit): everything needs **1** sequence
- Delete: always **1** sequence

Hold-Backspace corruption root cause: correct-per-press doubling still
desyncs during key repeat because each frame re-sampled the **stale** grid
(echo not yet landed) and restarted the shadow simulation from the old
cursor. Fix: persist the shadow row across frames until `output_gen`
advances.

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
