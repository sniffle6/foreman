# Tiling tree + floating windows

## What it does

Every window in foreman is in one of two states:

- **Tiled** — it lives in a layout tree (like i3/tmux). Tiles never overlap,
  fill the whole area, and reflow when siblings come and go.
- **Floating** — classic overlapping window with z-order, drag, resize.

Floating windows are a strict upper layer: a float always paints above every
tiled window, and clicking/focusing a tile never raises it above a float.
`z` (raise on focus) only reorders windows *within* a layer — in practice that
means among floats, since tiles never overlap each other. Zoom still renders on
top of everything (it is an overlay, not a z change). See
`WindowManager::draw_order` in `src/wm.rs`.

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

- **Float/tile toggle button** in every header, left of minimize (projects
  keep `+` to its left). The icon shows the current state: 2×2 grid = tiled,
  two offset squares = floating. Clicking toggles with the same semantics as
  leader `F` — popping back in enters the tree at the leaf under the
  window's center.
- **Drag a tiled window's header** → it tears out of the tree instantly
  (siblings absorb the space) and floats under the cursor. A tear-out drag
  keeps its amber drop hints for the whole gesture:
  - edge half of a tile → split that tile on that side
  - center of a tile → merge as a tab onto that window
  - thin band at the area edge → split the whole root (full row/column)
  - drop on another window's **titlebar** → tab-merge (wins over tree hints)
- **Drag a floating window's header** → pure free move: no hints, nothing
  happens on drop. Hold **Shift** at any point during the drag to enable the
  full drop semantics above (hints light up while held).
- **Drag a shared edge** between tiles → moves that divider (adjusts tree
  ratios; clamped so no tile drops below 10% of its split). Dragging the
  OUTER edge of a tile does nothing — tear-out lives on the header drag.

### Hover-revealed headers

Terminal (and chat) windows don't show a titlebar all the time:

- A **lone pane** (single tiled window, one tab, not a project) draws no
  chrome at all, ever — the parent frame is its only frame (tmux-style sole
  pane).
- **Every other non-project window** hides its header until the mouse is over
  that window — same reveal rule as the terminal scrollbar. The content owns
  the FULL window rect (the grid never resizes on hover); the header paints
  OVER the top strip while revealed. It also stays up mid-gesture (rename,
  header drag, tab tear-out) so a fast pointer can't strand a drag.
- **Projects keep persistent headers** — they carry the tab chips, the
  PS/CMD/SH dispatch keys, and the `+` project button.

Everything header-driven (tear-out drag, tab chips, window controls,
double-click rename/maximize) works unchanged: mouse over the window and the
header is there. While revealed it covers the top ~26px of terminal text,
exactly like the scrollbar covers the right edge — move the mouse off and the
text is back.

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
- **Drop gating keys off where the drag STARTED**
  (`WindowManager.drag_from_tree`), not current tree membership — after
  tear-out the window is already floating, but that drag keeps its hints. A
  per-frame `tree.contains` check would kill the hints one frame into every
  tear-out.
- **A tab dragged off a stack** becomes a new floating window mid-gesture
  (`Act::Untab` + grab), so the rest of that drag is a free move — hold
  Shift to snap it into the tree in the same gesture.

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
