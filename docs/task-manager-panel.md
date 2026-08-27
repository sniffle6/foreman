# Task-manager panel

Desktop right-edge panel listing every project and its terminal/chat tabs.
Click a row to focus/restore; click the already-focused visible row again to
minimize (taskbar-style). Hover for explicit minimize and close. Fully replaces
the old bottom-left minimize chips.

## Why

Minimized windows used to hide in a small chip taskbar that was easy to miss
and did not show background tabs. The panel is the full truth of what exists
and the landing site for future agent-state badges.

## How it works

- **Read seam:** `WindowManager::panel_model()` builds a plain `PanelModel`
  snapshot each frame (projects → nested windows → tabs).
- **Write seam:** `surface_target(TargetPath)` restores project + child + tab
  and focuses. Panel row clicks go through `Act::FocusPath` →
  `toggle_surface_target`: if the path is already the focused *visible* target,
  minimize it; otherwise surface. "Visible" excludes a focused window covered
  by a zoomed sibling (un-zoom first; do not minimize a window the user cannot
  see). Explicit hover min still uses `MinPath`; crew-board/chat click paths
  call `surface_target` directly (no toggle).
- **Tabbed projects need `ptab`:** nested managers number child windows
  independently (each starts at 1), so when projects are tabbed a bare
  child-id scan always resolves to the first project tab. `TargetPath.ptab`
  records the owning project-tab index; `owning_project_tab` prefers it and
  falls back to the scan only for stale paths.
- **Restore returns to the tree:** `minimize` records whether the window was
  tiled (`Win::min_from_tree`); `unminimize` re-enters the tree at the leaf
  under the window's old center (best effort — the tree may have changed).
  Windows minimized while floating restore floating.
- **Dock edge is sticky:** the panel remembers which edge it occupies
  (`PanelView::dock`, default right). While it has a sibling the live tree
  re-derives the edge each frame; when every project is minimized the panel is
  a sole leaf (no dividers) and the last edge is kept. `tile_new` /
  `unminimize` fall back to inserting on the *opposite* side of that edge, so
  minimize-all → restore does not shove a bottom-docked panel back to the
  right rail. The dock only changes when the user moves the panel in the tree.
- **Sole-leaf strip:** when the panel is the only tiled leaf (all projects
  closed or minimized), layout pins it to a dock strip of the remembered
  `expanded_width` / rail extent instead of filling the desktop. That keeps
  size stable across minimize-all → restore. With `FOREMAN_LANDING`, the
  landing paints in the remaining content rect (`should_show_landing` —
  no visible non-panel window, including all-minimized).
- **Re-pin after tree moves:** any structural tree change that can reshuffle
  ratios (`insert_beside_panel`, drag-drop split/root, `move_dir`,
  `place_split`, float toggle, unminimize) calls `repin_panel` — refresh dock
  from dividers, then `apply_panel_ratio` — so the Sessions panel keeps its
  remembered extent when dragged to another edge or swapped, not `insert_*`'s
  50/50.
- **Collapsed rail is pinned:** while collapsed the desktop re-applies the
  rail extent every frame via `LayoutTree::set_leaf_extent` (which may go below
  `MIN_RATIO`, unlike a normal divider drag), so resizing it — from its own
  edge or a neighbour's — springs back. Works wherever the panel sits in the
  tree, not just as the rightmost root leaf. The pin tries the H axis first
  (right/left dock = width), then falls back to the V axis (bottom/top dock =
  height); a panel with dividers on both axes stays width-pinned.
- **Expanded drags use the panel's pixel floor, not `MIN_RATIO`:** the pinned
  extent (260px default) sits *below* 10% of a wide desktop, so a plain
  `resize_edge` clamp would ratchet — grow the panel by dragging and it could
  never shrink back past ~10% of the screen. Interactive edge drags in `wm.rs`
  call `LayoutTree::resize_edge_soft_min` with `(panel_id,
  PANEL_MIN_EXPANDED)` (76px), which applies that pixel floor whenever the
  panel leaf sits on either side of the dragged divider; every other tile
  keeps the `MIN_RATIO` clamp.
- **Horizontal mode:** when the panel's content rect is wider than tall
  (bottom/top dock), `PanelView::show` flows content left-to-right. Derived
  per-frame from the rect — no new state, no persistence; move the leaf back
  to a tall slot and it flips back. Three states:
  - **Columns** (expanded, body ≥ 2 rows): one ~200px group per project —
    project row on top, its tab rows below — vertical hairline between
    groups, horizontal scroll. Same `paint_row` as vertical mode.
  - **Strip** (expanded, body < ~48px): one line of inline chips — project
    chip then its terminal chips, hairline between projects. Click surfaces;
    no hover min/close (expand to manage). Chip labels truncate at ~90px.
  - **Rail** (collapsed): a 36px-tall strip, project icons left-to-right,
    expand chevron at the far right inside the strip — no header band (36px
    can't fit band + body).
  Wheels have no x axis, so `smooth_scroll_delta.y` (plus `.x` for trackpads)
  drives the horizontal scroll offset.
- **Overflow follows the terminal scrollbar design:** expanded vertical mode
  paints a vertical thumb at the right edge; expanded horizontal columns and
  strip modes paint a horizontal thumb at the bottom edge. Both use the
  terminal's resting/hot bar sizes, enlarged interaction band, minimum grab
  extent, grab-point-preserving drag, centred track click, and fade curve. The
  thumb only exists when content overflows. Collapsed rails stay visually quiet
  but remain wheel-scrollable on their visible axis, because the thumb's
  interaction band would consume too much of the compact rail.
- **Scrollbar geometry is axis-generic:** `src/geom.rs` owns the shared
  `ScrollAxis` math for bar placement, hit/track bands, hot growth, and the
  drag inverse. The panel reserves that interaction band before painting rows
  or chips, and the edge inset remains derived from `wm::RESIZE_BAND`, so the
  window resize handle and scrollbar do not overlap.
- **Collapse glyph orients to the shrink axis:** `»`/`«` when right-docked,
  chevrons when bottom/top-docked (top/left mirror). The `⌃`/`⌄` codepoints
  are tofu in egui's default fonts, so chevrons are drawn as vector strokes
  (`panel::paint_chevron`) in both the expanded header and the rail.
- **View:** `Content::TaskManager(PanelView)` — real tiled window, non-closable,
  non-minimizable, non-tabbable. Collapse to a ~36px icon rail (`«` / leader `M`).
- **Close:** always goes through `request_close_*` so the running-process confirm
  still applies.
- **Quit:** `deserted()` ignores the panel — a lone panel does not keep the app
  alive.

## Settings

`%APPDATA%\foreman\settings.json`:

- `panel_collapsed` (bool)
- `panel_width` (f32, expanded px along the dock axis — width when side-docked,
  height when bottom/top-docked; the key name is persisted, don't rename it).
  Capped at `PANEL_MAX_SIDE` (420) side-docked / `PANEL_MAX_EDGE` (240)
  top/bottom-docked, and never more than half the available axis.
- `panel_dock` (`"Left"` / `"Right"` / `"Up"` / `"Down"`, default `"Right"`) —
  edge the panel occupies; restored via `ensure_panel` on next launch

## Key files

- `src/panel.rs` — model types + row paint; axis-aware scrollbar input/paint;
  horizontal painters (`paint_columns`, `paint_strip`, `paint_rail_h`,
  `paint_chevron`)
- `src/geom.rs` — shared axis-generic scrollbar geometry and terminal wrappers
- `src/wm.rs` — `panel_model`, `surface_target`, `ensure_panel`, path Acts,
  drains, `apply_panel_ratio` (H→V axis fallback)
- `src/layout.rs` — `set_leaf_extent` (axis-aware; `set_leaf_width` wraps it)
- `src/keymap.rs` — `Command::ToggleTaskManager` (default leader `M`)
- `src/config.rs` — persistence fields
- Spec: `docs/superpowers/specs/2026-07-09-task-manager-panel-design.md`
- Mockups: `docs/superpowers/specs/2026-07-09-task-manager-panel-mockup.html`
  (vertical), `2026-07-10-task-manager-panel-horizontal-mockup.html`

## Out of scope (still)

- Agent-state badges / status dots on rail rows
- Drag from panel into the tree
- Per-row context menus
