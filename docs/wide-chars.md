# Wide characters (CJK / emoji)

What: how foreman handles width-2 glyphs (one `WIDE_CHAR` base cell + one
spacer cell) across paint, snapshots, and key input.

## Ownership rules

- **Paint never renders spacer cells** (either spacer flag). The one
  classifier is `CellWide::classify` in `src/input.rs` (flags + base char);
  walks that only need spacer-ness use `CellWide::is_wide_spacer`. A new
  alacritty flag is one edit there — do not add a second inline flag check
  (df46b2d needed 4 edits because there were 4 copies).
- **Snapshot text/cells skip spacers** through the same classifier.
- **Key input doubling** lives only in `wide_key_doubles` (src/input.rs):
  double iff the crossed glyph is non-BMP AND the position is a whole-glyph
  crossing (base for Right/Delete, left-spacer for Left/Backspace) AND not
  alt-screen. Ctrl/Alt crossings stay single except Backspace, because its
  current encoder discards modifiers and sends the same DEL bytes. Everything
  else is single. Do not "fix" a wide-char symptom by adding doubling elsewhere
  — re-run the probe matrix below first.
- **Hold-repeat**: `Session.wide_shadow` carries the simulated row across
  frames and is replaced by a fresh grid sample only after the observed settle
  heuristic: the PTY is quiet for `WIDE_RESAMPLE_SETTLE` (50ms), there is no
  session-owned ongoing key hold, and there was no prior
  key activity within `WIDE_INPUT_GRACE` (150ms). A fresh press may sample a
  settled pre-key grid; once owned, egui's `key_down` bridges repeat delay and
  slow repeat settings so they do not expire a live burst. `output_gen`
  advancing alone is NOT a resample signal — one
  keypress echo on a long soft-wrapped line arrives across many chunks over
  multiple frames, and a grid sampled between chunks reports a transient
  mid-redraw cursor.
- **Invalidated shadows wait for observed post-input activity**: text, paste,
  interrupt, external injection, or an unmodeled chord enters
  `AwaitingEcho { invalidated_gen }`. Standard single encoding remains in
  effect until a later PTY generation is observed and the settle heuristic
  passes; an absent shadow never re-seeds from the same pre-input generation.
  A generation change is not a causal child acknowledgement—unrelated output
  can satisfy it—so this remains a conservative observation-based heuristic.
- **Modeled keys** are Left/Right/Delete without Ctrl/Alt plus Backspace under
  all modifiers (`wide_key_modeled`, src/input.rs). Backspace is the exception
  because `encode_key` currently emits the same raw DEL regardless of modifiers;
  compensation follows bytes actually sent, not the physical chord. Other
  Ctrl/Alt edit/navigation bindings DROP the shadow like Home/End do.

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
cursor. The first fix persisted the shadow only until any `output_gen`
advance; that was enough for short, single-chunk redraws but not the long-line
case below. The current policy keeps it through active holds and partial PTY
redraws (`Session.wide_shadow`).

Re-verification (post-fix, same pwsh session): emoji BS removes exactly one
emoji; `中中中` + BS leaves `中中` (over-delete gone); CJK/emoji Left both
move 2 cells; Delete on emoji base removes one clean emoji; 8 rapid
no-settle Backspace sends over a mixed emoji line leave **zero** U+FFFD.

Wrap-boundary finding (live repro 2026-07-10, fixed): the shadow was ONE
grid row, so with the cursor at col 0 after a soft wrap it knew nothing
about the previous row's tail — Backspace sent a single DEL and
half-deleted the emoji ending that row (probed: raw DEL at the boundary →
`…🥒🤣🤣�`). One U+FFFD per wrap crossing = the diagonal tofu squares.
Fix: `inspect::wide_row_at_cursor` concatenates soft-wrapped rows
(`WRAPLINE`) and drops wrap padding (`LEADING_WIDE_CHAR_SPACER`), so the
boundary does not exist in the shadow. Verified: 44-press Backspace batch
over a 2-row wrapped emoji line clears it completely, buffer stays clean.

Long-line finding (live repro 2026-07-10, fixed): with a ~15-row wrapped
mixed narrow+emoji input, hold-Backspace still corrupted (`…🥒�` at the
cursor + stray rows below) even WITH the wrap-spanning shadow. Root cause:
the shadow was invalidated the moment `output_gen` advanced, but one
keypress echo on a line that long is redrawn by ConPTY across MANY chunks
over multiple frames — every intermediate frame re-sampled a mid-redraw
grid (cursor transiently anywhere), adopted the garbage as the new shadow,
and the next repeat press doubled (or failed to) from a phantom position.
Short lines redraw in one chunk, which is why the small repro was fixed
first and this one survived. Fix: shadow lifetime is now quiescence-based
(`keep_wide_shadow`, src/terminal.rs) — do not resample during a session-owned
ongoing hold; after release, require 50ms of observed PTY silence and
150ms of key silence. These time windows are a practical quiescence heuristic,
not an acknowledgement from the child that its redraw is complete. In the
same pass, modified edit/navigation sequences switched from "simulate as
no-op" to "drop the shadow": their post-key state is not represented by
row+cursor tracking. Backspace remains modeled under modifiers because its
current encoder sends the same DEL bytes either way.

Paste-only caret finding (live repro 2026-07-10, compatibility-fixed): this
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

Foreman's correction is deliberately narrow and reversible. A single-line
PowerShell paste on the primary screen arms a CUP observer only when the paste
began at the visible line end. At each completed `CSI H/f`, it samples the
natural whole-glyph flow endpoint before applying the CUP, then accepts a
`raw -> physical` cursor alias only when the entire difference is exactly the
count of leading pads whose following wide base is non-BMP across the complete
`WRAPLINE` chain. BMP/CJK pads are deliberately excluded because PSReadLine
already includes them in its coordinate. The alias
feeds caret painting, `foreman snapshot --cursor`, and wide-key shadow
sampling; alacritty's grid cursor, subsequent VT parsing, and CPR replies stay
untouched. Correct CUPs, BMP CJK,
alternate-screen apps, bracketed-paste TUIs, multiline paste, mid-line paste,
resize, and unmatched cursor movement all fail closed to standard behavior.
Pure tests cover one pad, three cumulative pads, a mixed CJK+emoji line, a
correct-CUP/BMP control, and a CUP split across PTY chunks. An ignored live
PowerShell/ConPTY test drives a
real egui `Event::Paste`, observes the one-cell raw/effective split, and proves
the corrected endpoint makes the following Backspace encode two DEL bytes.

This compatibility alias fixes the reported paste-at-end caret and subsequent
held-Backspace starting position without globally rewriting CUP bytes. The
PowerShell session label does not prove PSReadLine is still the foreground
program, however, and an armed primary-screen application that emits the exact
same flow-end/padding pattern is indistinguishable. The append-at-end,
single-line, mode, endpoint, and mutation gates minimize that compatibility
risk and fail closed on any unmatched movement. A custom prompt that itself
soft-wraps across a non-BMP boundary may also fail closed because its pad is
outside PSReadLine's input offset. The alias does not promise to heal stale
cells already emitted by a child redraw; those remain upstream display residue.
Once the final CUP establishes an alias, a fresh wide key may seed from it
immediately without the ordinary 50ms settle delay. A key event that outruns
the paste echo and arrives before that CUP still has no trustworthy endpoint
and falls back to standard single encoding. A natural endpoint exactly in
alacritty's `input_needs_wrap` (last-column/wrap-pending) state is also not
carried by the point-only alias; the verified repros end at ordinary columns 2
and 83 rather than that state.

Cosmetic residue that remains (upstream, do not chase): between the two
DELs of a doubled press the child buffer transiently holds a lone
surrogate; if ConPTY renders that instant it writes a `�` and never clears
cells past its new content end. The buffer itself is clean — typing
continues fine — and `Ctrl+L` heals the display (verified). Same family as
`docs/conpty-resize-reflow.md`.

Review finding (e538e4a, fixed): the shadow originally CLEARED deleted
cells; deleting a narrow char in front of an emoji left a stale cell under
the cursor and the next same-batch Delete under-doubled (half-delete). The
shadow now REMOVES cells and shifts the tail left, matching cooked-editor
semantics (`Left Left Backspace Delete` over `a🤣z` verified live → `z`,
zero U+FFFD).

Known limitation: unmodeled keys (Home/End/Enter/…) and text insertion
drop the shadow — the cursor position is no longer knowable (Home jumps to
the prompt boundary). Wide encoding falls back to single sequences across
frames until a later PTY generation is observed and the quiet-window
heuristic permits a fresh sample. Keep wide-glyph edits and navigation keys
in separate `send` requests when it matters.

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
