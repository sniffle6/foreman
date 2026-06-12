# Float toggle button + Shift-gated drop snapping

Date: 2026-06-12. Status: approved.

## Problem

When the layout tree fills the area, `LayoutTree::drop_target` covers every
pixel — edge bands split the root, leaf centers tab-merge, leaf edges split.
Releasing a header drag *anywhere* re-inserts the window into the tree. There
is no way to free-move a floating window with the mouse without it snapping
back in.

## What we're building

### 1. Float/tile toggle button in the window header

- A new control in every window's header cluster, immediately left of
  minimize. Projects keep their `+` (new project) button to its left, so the
  project order left→right is: `+` · float-toggle · min · max · close.
- Vector-stroke icon like the existing controls (no font glyphs), showing the
  window's **current** state:
  - tiled → 2×2 grid
  - floating → two overlapping squares
- Clicking focuses the window and toggles its state via the existing
  `toggle_float` logic, generalized to take a `WinId` (today it only acts on
  `self.focused`; leader `F`/`Ctrl+F` keeps working through the same path).
  Pop-in therefore snaps exactly like leader `F`: into the leaf under the
  window's center, split along its longer axis; empty tree → root tile.
  Pop-out restores `Win.prev` (the remembered floating rect).
- Lives in the shared header code in `wm.rs`, so it works at both compositor
  levels (projects on the desktop, terminals inside a project) automatically.

### 2. Shift-gated snapping for floating drags

Drag behavior now depends on where the drag **started**:

- Drag started on a **tiled or zoomed** window → unchanged. It tears out of
  the tree, hints show immediately, and every drop semantic works (tree
  split/tab, root bands, titlebar tab-merge). Drag remains the one-gesture
  way to rearrange the tree.
- Drag started on a **floating** window → pure free-move. No hints, no tree
  insert, no titlebar tab-merge. Holding **Shift** at any point during the
  drag lights up the hints and enables all drop semantics. Releasing without
  Shift leaves the window where it is.

So a floating window enters the tree only deliberately: the toggle button,
leader `F`, leader `WASD`, or Shift-drag. Never by accident.

## Implementation notes

- `src/wm.rs`:
  - New `Act::Float(WinId)`, applied in `apply_acts`.
  - Window-controls loop gains a fourth role; `ctl_w` bumps 88→113
    (terminals) and 116→141 (projects) so the title-drag rect, rename field,
    dispatch keys, and tab-chip collision math keep clearing the cluster.
  - `toggle_float()` → `toggle_float_for(id: WinId)`; the keymap dispatch
    calls it with `self.focused`.
  - New drag-origin field (e.g. `drag_from_tree: Option<WinId>`), set when a
    drag tears a tiled/zoomed window out, cleared on `drag_stopped`. The
    per-frame check `tree.contains(id)` cannot replace it: after tear-out the
    window is no longer in the tree, but that drag must keep its hints.
  - Hint detection (`merge_target_at`, `tree.drop_target`) and the drop
    commit are gated on `drag_from_tree == Some(id) || shift_down`.
- Help overlay drag row and `docs/tiling-tree.md` get the new wording.
- Zoom: the button mirrors leader `F` exactly; no zoom special-casing beyond
  what `toggle_float` does today.

## Testing

- Unit test: `toggle_float_for` on a window that is not focused (roundtrip of
  tree membership + rect restore is already covered by
  `toggle_float_roundtrips_tree_membership_and_rect`).
- The drag gating is egui interaction code with no existing test harness —
  verify by build + run + screenshot per the working agreement.

## Out of scope

- No third window state ("pinned"); the two-state model stands.
- No changes to keyboard tiling commands or the layout tree itself.
- No tooltips on header buttons (none of the existing controls have them).
