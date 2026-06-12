# Tiling tree + floating windows

## What it does

Every window in foreman is in one of two states:

- **Tiled** — it lives in a layout tree (like i3/tmux). Tiles never overlap,
  fill the whole area, and reflow when siblings come and go.
- **Floating** — classic overlapping window with z-order, drag, resize.

This works at BOTH levels of the recursive compositor: projects tile on the
desktop, terminals tile inside their project. Same engine, same rules.

The tree is plain data: leaves are window ids, internal nodes are horizontal or
vertical splits with per-child ratios. A window is tiled if and only if its id
is a leaf (`tree.contains(id)`). There is no "snap zone" concept anymore — the
old 9-zone system (halves/quarters/hold-to-maximize) was deleted.

## Why it exists

The zone system capped layouts at halves and quarters — you could not make
"three columns" or "70/30 with a stacked right side". A tree expresses any
split layout, and floating stays available for things you want on top. The
user chose this two-state model over zones on 2026-06-11 (reversing the
earlier floating-only decision recorded in the tabbing epic).

## How to use it

Keyboard (after the `Ctrl+B` leader):

| Keys | Action |
|---|---|
| `Alt+W/A/S/D` | Split: new terminal on that side of the focused window. A floating source enters the tree first, so you always get the two panes. |
| `W/A/S/D` | Move the focused window within the tree (swap with the neighbor that way; no neighbor → become a full edge row/column). A floating window enters the tree at that edge. |
| `F` / `Ctrl+F` | Toggle float for the focused terminal / project. Re-tiling enters the tree at the leaf under the window's center. |
| `Z` / `Ctrl+Z` | Zoom (tmux-style): render full-area ON TOP; the tree underneath is untouched. Not a tree operation. |
| arrows / `Ctrl+arrows` | Focus moves geometrically — works across tiled and floating alike. |

Mouse:

- **Drag a header** of a tiled window → it tears out of the tree instantly
  (siblings absorb the space) and floats under the cursor.
- **While dragging any window**, amber hints show what a drop would do:
  - edge half of a tile → split that tile on that side
  - center of a tile → merge as a tab onto that window
  - thin band at the area edge → split the whole root (full row/column)
  - drop on another window's **titlebar** → tab-merge (wins over tree hints)
  - drop anywhere else → stays floating
- **Drag a shared edge** between tiles → moves that divider (adjusts tree
  ratios; clamped so no tile drops below 10% of its split). Dragging the
  OUTER edge of a tile does nothing — tear-out lives on the header drag.

New windows tile by default: they split the previously-focused tiled window
along its longer axis, or become the root tile of an empty tree. The chat
viewer window is the exception — it always opens floating.

Tabs are unchanged: a tab stack is just a window (`Win.tabs`), so a tree leaf
with multiple tabs IS a tabbed container sitting in the layout.

## Gotchas

- **Zoom is an overlay, not a layout change.** Un-zooming never reflows
  anything. `WindowManager.zoomed: Option<WinId>`.
- **`Win.prev`** holds the floating rect to restore on tear-out/un-tile.
  Tree-managed rects are recomputed every frame from `tree.layout()` — never
  trust `Win.rect` as persistent state for a tiled window.
- **Drop hints hit-test the tree, not window z-order**: dragging over a
  floating window that overlaps a tile shows the tile's hint "through" it
  (titlebar merge still wins on drop). Known v1 niggle.
- **One-frame rect lag on drop**: committing an insert leaves the rect to the
  next frame's refit. Invisible at 60fps; don't "fix" it by setting rects
  inline.
- **Ratios can sink below 10%** via repeated same-axis splits (insert halves
  the target's ratio without a floor). `resize_edge` clamps so dragging can't
  crush tiles further — and its bounds are built to never panic on such
  degenerate ratios (`f32::clamp` panics if min > max; see the regression
  test in `layout.rs`).
- **At startup** the longer-axis rule sees pre-layout spawn rects (580×380 →
  "wide"), so early windows split `Right`. At runtime it uses live rects.

## Key files

- `src/layout.rs` — the whole tree: `LayoutTree`, `Node`, `insert_root`,
  `insert_split` (flat same-axis siblings), `remove` (collapse + splice),
  `layout()` rect math, `hit_leaf`, `drop_target`, `swap`, `resize_edge`.
  Pure data + math, fully unit-tested, no egui interaction.
- `src/wm.rs` — integration: per-frame refit from `tree.layout()`, drag
  tear-out + drop commit in `show()`, `move_dir` / `place_split` /
  `toggle_float` / `toggle_zoom` / `tile_new` / `detach`.
- `src/keymap.rs` — `TermFloat`/`ProjFloat` commands; `TermSnap`/`ProjSnap`
  kept their serialized names (user keybinding files still work) but are
  labeled "Move …" and dispatch to `move_dir`.
