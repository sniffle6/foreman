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
- **Collapsed rail is pinned:** while collapsed the desktop re-applies the
  rail extent every frame via `LayoutTree::set_leaf_extent` (which may go below
  `MIN_RATIO`, unlike a normal divider drag), so resizing it — from its own
  edge or a neighbour's — springs back. Works wherever the panel sits in the
  tree, not just as the rightmost root leaf. The pin tries the H axis first
  (right/left dock = width), then falls back to the V axis (bottom/top dock =
  height); a panel with dividers on both axes stays width-pinned.
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
  height when bottom/top-docked; the key name is persisted, don't rename it)

## Key files

- `src/panel.rs` — model types + row paint; horizontal painters
  (`paint_columns`, `paint_strip`, `paint_rail_h`, `hscroll`, `paint_chevron`)
- `src/wm.rs` — `panel_model`, `surface_target`, `ensure_panel`, path Acts,
  drains, `apply_panel_ratio` (H→V axis fallback)
- `src/layout.rs` — `set_leaf_extent` (axis-aware; `set_leaf_width` wraps it)
- `src/keymap.rs` — `Command::ToggleTaskManager` (default leader `M`)
- `src/config.rs` — persistence fields
- Spec/plan: `docs/superpowers/specs/2026-07-09-task-manager-panel-design.md`,
  `docs/superpowers/plans/2026-07-09-task-manager-panel.md`,
  `docs/superpowers/plans/2026-07-10-panel-horizontal-mode.md`
- Mockups: `docs/superpowers/specs/2026-07-09-task-manager-panel-mockup.html`
  (vertical), `2026-07-10-task-manager-panel-horizontal-mockup.html`

## Out of scope (still)

- Agent-state badges / status dots on rail rows
- Drag from panel into the tree
- Per-row context menus
