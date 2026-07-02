# Terminal text selection

## What it does

Mouse selection in a terminal pane, built on alacritty's own `Selection`:

- **Drag** selects a span (`SelectionType::Simple`).
- **Double-click** selects the word/path under the pointer (`Semantic` —
  expands to the nearest semantic escape char; `/` `.` `-` are not escapes, so
  a path like `docs/epics/file.md` or a `--flag` selects whole).
- **Triple-click** selects the line (`Lines`; the copied text keeps its
  trailing `\n`).
- **Plain click** clears the selection.
- **Ctrl+C** copies the selection and clears it (or sends an interrupt if
  nothing is selected); Ctrl+Shift+C copies without clearing. Right-click
  pastes.

The amber highlight stays glued to the **text** it covers — scroll the
scrollback and the highlight rides along with the lines; new output rotates it
into history, exactly like a normal terminal. Wide (CJK/emoji) glyphs select
and copy as whole characters, and the highlight covers both of their columns.

## Why alacritty's Selection

The first version hand-rolled selection with two `(row, col)` tuples stored in
**viewport** coordinates. That broke on scroll (highlight pinned to the screen
while text moved) and treated 2-column CJK glyphs as one column, so copies and
highlights drifted. `alacritty_terminal` ships a complete selection module that
already understands word boundaries, whole lines, wide-char spacers, and
scrollback rotation — so the selection now lives in **alacritty's own slot**
(`term.selection`) and the hand-rolled grid walk is deleted.

"One range, two readers" holds at the source: the single `Selection` feeds
both the copy text (`term.selection_to_string()`) and the highlight range
handed to `frame::plan`, so they can never disagree.

## How it works (in code)

All in `Session::show` / `Session::read_input` (src/terminal.rs):

- **Pixel → selection point:** `Session::sel_point` floors the pixel to a
  viewport cell (`CellMetrics::cell_at`), shifts it into buffer space with
  alacritty's `viewport_to_point(display_offset, ..)`, and picks `Side::Left`
  or `Side::Right` from which half of the cell the pointer is in
  (`CellMetrics::cell_right_half`).
- **Click chain order matters:** egui reports `clicked()` on the same frame a
  double/triple-click completes, so the chain is
  `triple_clicked → double_clicked → drag_started → dragged → clicked-clears`,
  with plain-click-clears LAST.
- **Highlight:** `term.selection.to_range(&term)` gives ordered buffer coords
  clamped to the live grid; `sel_viewport_range` (pure, unit-tested) culls
  them onto the visible viewport as a `frame::SelRange`. An edge row that is
  scrolled out takes the full-row boundary — a start above the viewport
  becomes `(0, 0)`, an end below becomes the bottom-right cell — because the
  range's columns only mean anything on its own first/last row.
- **Copy:** `term.selection_to_string()`; the Ctrl+C copy-vs-interrupt gate is
  `term.selection.to_range().is_some()` (an empty drag yields `None`).

## Gotchas

- **Stale coords can't panic.** Drag-tracked points can outlive a grid shrink
  (alt-screen swap, resize) and `Line`/`Column` indexing panics out of range —
  but `to_range` clamps both ends to the live grid (`grid_clamp`) before any
  indexing, and returns `None` once the selection scrolls off the top. Pinned
  by tests (`out_of_bounds_selection_points_are_clamped_not_panicking`,
  `selection_survives_an_actual_grid_shrink_without_panicking`).
- **Drag-extend after double/triple-click is deliberately not wired.** egui
  carries a click count only on release, not on press, so detecting a
  "double-click then drag" gesture needs hand-rolled timing. Deferred.
- **Drags that start ~10px from the window edge** are captured by the window
  manager's edge-resize grip, not the terminal — pre-existing WM behavior,
  looks like a dead first cell when selecting from column 0 of a full-bleed
  pane.
- The Term itself clears/rotates the selection when content overwrites or
  scrolls it — don't add app-side bookkeeping for that.

## Key files

- `src/terminal.rs` — `Session::sel_point` (pixel → `(Point, Side)`),
  `Session::show` (click chain + the single highlight-range build spot),
  `Session::read_input` (Ctrl+C copy path), `sel_viewport_range` (pure cull) +
  the selection test pins.
- `src/geom.rs` — `CellMetrics::cell_right_half` (which half of a cell).
- `src/frame.rs` — `SelRange` (viewport, ordered, inclusive) consumed by
  `plan`; unchanged by the rewrite, as designed.
