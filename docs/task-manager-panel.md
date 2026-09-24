# Task-manager panel

Desktop panel listing every project and its terminal/chat tabs. Docks to any
edge; default is the right.
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
- **Drag to reorder (presentation-only):** rows/chips in the expanded modes
  drag to reorder Project groups, and Session/chat/image rows within their
  Project. One `PanelReorder` intent drains into `Act::ReorderPanel`;
  `WindowManager` re-resolves source/anchor by stable identity — every row
  carries its tab's runtime uid (`Tab::tab_uid`), so a ref follows its row
  through any same-frame structural shift or cancels if the row is gone —
  rejects cross-project and Project↔Session drops, and rewrites that scope
  to dense
  per-tab `panel_order` ranks. The real tab strip, tiles, z-order, and focus
  never move. Unranked (`None`) rows sort after ranked ones in structural
  order, so new tabs append. A 2px marker (clipped to the panel body) shows
  the drop slot; edge auto-scroll follows the active `ScrollAxis`; collapse
  or an orientation flip cancels the gesture. Collapsed rails don't drag (v1).
- **Project folder icon folds nested rows:** clicking the folder glyph on a
  project row (or the matching strip chip) hides or shows that project's
  session/chat/image children. The rest of the row still surfaces the project.
  Fold state lives on the project's `Tab` and is saved in `workspace.json`,
  so it survives panel reorder, tab moves, and workspace restore. Folder
  clicks resolve by runtime tab uid; old workspaces default to expanded. A
  disclosure triangle on the icon points right when collapsed and down when
  expanded. Collapsed rails already show only project icons, so a rail click
  still surfaces rather than folding. A latched Bell on a hidden child
  promotes to the project row/chip so the ring is not lost.
- **Project rows have a hover `+`:** left of the min/close buttons on a
  project row (expanded vertical and Columns modes). Click spawns a
  default-shell terminal into *that* project through `PanelBtn::AddTerm` →
  `Act::LaunchPath(TargetPath, Launch)` → `WindowManager::add_terminal`.
  Hover opens the same grouped launcher as the titlebar: Agents, Shells,
  Project tools, and New project. Its menu clamps to the desktop, opening
  leftward from a right-docked panel. The path's `tab` names the project tab,
  so a background tab of a tabbed-projects window is activated first, and
  each launch surfaces the project. Project tools show a dot when already
  open. Strip and rail modes have no `+` (expand to manage).
- **Elided titles get a hover tooltip:** rows (expanded modes) and strip chips
  attach egui's `on_hover_text` with the full title only when the `…`
  truncation actually kicked in (`Galley::elided`), and never while a reorder
  drag is live. A fully visible name hovers quietly.
- **Tabbed projects need `ptab`:** nested managers number child windows
  independently (each starts at 1), so when projects are tabbed a bare
  child-id scan always resolves to the first project tab. `TargetPath.ptab`
  records the owning project-tab index; `owning_project_tab` prefers it and
  falls back to the scan only for stale paths.
- **Restore returns to the tree:** `minimize` records whether the window was
  tiled (`Win::min_from_tree`); `unminimize` re-enters the tree at the leaf
  under the window's old center (best effort — the tree may have changed).
  Windows minimized while floating restore floating.
- **Preferred size survives fitting:** `PanelView::expanded_width` is the
  user's preferred extent, carried between dock axes through the existing
  `panel_width` setting. `panel::effective_extent` applies the destination
  cap (420px at the sides, 240px at top/bottom, and available-space bounds)
  without overwriting that preference. Slow desktop resizing on either axis,
  collapse, and sibling removal preserve it; growing the space restores it.
- **Dock edge is sticky:** explicit panel placement chooses `PanelView::dock`.
  Structural changes retain that edge while its divider exists, otherwise
  choose a compatible edge deterministically from the tree. A sole leaf keeps
  the remembered edge. Rectangle aspect never chooses the sizing axis.
- **Sole-leaf strip:** with every project closed or minimized, the panel uses
  the same effective extent as a strip at its remembered edge. The landing
  occupies the remaining content rectangle. Restoring or opening a project
  inserts it opposite that edge and re-applies the preferred panel extent.
- **Geometry follows structural changes:** `repin_panel` normalizes a tiled
  panel after detach, insert, swap, restore, or float toggle. Area changes on
  either dimension normalize before placements. Unchanged frames do no extra
  sizing work and never read rendered rectangles back into preferences.
- **Explicit divider resizing saves size:** a drag on the panel or its
  neighbor reads fresh tree geometry, bounds the extent, and updates the
  preference. `LayoutTree::resize_edge_soft_min` permits the panel's 76px
  expanded floor below ordinary `MIN_RATIO`. `set_leaf_extent` selects the
  closest divider across both sides of the chosen axis. Collapsed panels
  remain at the 36px rail extent, including drags from a neighbor's edge,
  and keep their expanded preference.
- **Floating geometry:** movement retains the ordinary floating rectangle.
  Explicit floating width resizing and collapse/expand use width bounds,
  regardless of the remembered dock. Re-docking uses the expanded preference
  along the destination axis.
- **Exact drop hints:** `WindowManager::drop_proposal` computes the accepted
  layout with panel bounds before drawing the amber rectangle. Release uses
  the same resolver through `Act::Drop`, against current state. Panel tab
  merges have no hint and leave the dragged window floating.
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

- `src/launcher.rs` — shared launcher choices, groups, and keymap hints.
- `src/hover_menu.rs` — shared grouped hover menu.
- `src/panel.rs` — model types + row paint; axis-aware scrollbar input/paint;
  horizontal painters (`paint_columns`, `paint_strip`, `paint_rail_h`,
  `paint_chevron`); folder fold (`toggle_folder`, `vertical_content_height`,
  `PanelView` `collapsed_folders`)
- `src/geom.rs` — shared axis-generic scrollbar geometry and terminal wrappers
- `src/wm.rs` — `panel_model`, `surface_target`, `ensure_panel`, path Acts,
  drains, `apply_panel_reorder` + `Tab::panel_order` (drag ordering),
  `apply_panel_ratio` (H→V axis fallback)
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
