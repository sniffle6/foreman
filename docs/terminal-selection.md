# Terminal text selection

## What it does

Click-drag in a terminal pane selects text. Ctrl+C copies the selection (or sends
an interrupt if nothing is selected); Ctrl+Shift+C always copies. Right-click
pastes. The amber highlight stays glued to the **text** it covers — scroll the
scrollback and the highlight rides along with the lines, exactly like a normal
terminal.

## Why it exists / why it was rewritten

The first version hand-rolled selection with two `(row, col)` tuples stored in
**screen/viewport** coordinates. That broke the moment you scrolled: the highlight
was painted at fixed screen rows (no scroll offset), so it stayed pinned to the
screen while the text moved underneath, and the copy path used the *current*
scroll offset, so the highlighted cells and the copied cells disagreed.

We already depend on `alacritty_terminal`, which has a complete, well-tested
selection module. The rewrite throws away the hand-rolled version and uses it:

- The selection lives in **absolute buffer coordinates** (`Point`, whose `Line`
  goes negative into scrollback), so it is independent of where the viewport is
  scrolled.
- Every frame we re-project it onto the visible rows, so it tracks the text for
  free and clips correctly when it scrolls off the top/bottom.

It is also *less* code than what it replaced.

## How to use (in code)

- **Start a drag:** `self.term.selection = Some(Selection::new(SelectionType::Simple, point, side))`
- **Extend a drag:** `self.term.selection.as_mut().map(|s| s.update(point, side))`
- **Clear:** `self.term.selection = None`
- **Copy text:** `self.term.selection_to_string()` (handles wrapping/scrollback/block)
- **Draw:** read `self.term.renderable_content().selection` — it is an
  `Option<SelectionRange>` already converted to display coordinates via
  `to_range(term)`. Map each buffer line back to a viewport row with
  `viewport_row = line + display_offset`.

`point_at()` turns a pixel position into the `(Point, Side)` the selection API
wants: it floors the pixel to a cell, then uses `viewport_to_point(off, ..)` to
add the current scroll offset so the captured point is in buffer space. `Side`
(left/right half of the cell) decides which edge of the cell the selection
includes.

## Gotchas

- **Buffer vs viewport coordinates is the whole point.** Capture and store in
  buffer space (`viewport_to_point`); convert to viewport only at paint time
  (`line + off`). Never store screen rows — that was the original bug.
- `SelectionType` also offers `Semantic` (word) and `Lines` (line) — wiring
  double/triple-click is a one-line type swap, not new machinery.
- `renderable_content()` borrows `&term`; pull `selection` (a `Copy`
  `SelectionRange`) out of that block before you take `self.term.grid()` again,
  same as the cursor info.

## Key files

- `src/terminal.rs` — `Session::point_at` (pixel → `(Point, Side)`),
  `Session::show` (drag handlers, highlight re-projection, Ctrl+C copy path).
