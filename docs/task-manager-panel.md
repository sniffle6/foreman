# Task-manager panel

Desktop right-edge panel listing every project and its terminal/chat tabs.
Click a row to focus/restore; hover for minimize and close. Fully replaces the
old bottom-left minimize chips.

## Why

Minimized windows used to hide in a small chip taskbar that was easy to miss
and did not show background tabs. The panel is the full truth of what exists
and the landing site for future agent-state badges.

## How it works

- **Read seam:** `WindowManager::panel_model()` builds a plain `PanelModel`
  snapshot each frame (projects → nested windows → tabs).
- **Write seam:** `surface_target(TargetPath)` restores project + child + tab
  and focuses. Panel rows and the chat crew board both route here via
  `Act::{FocusPath,MinPath,ClosePath}`.
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
  rail width every frame via `LayoutTree::set_leaf_width` (which may go below
  `MIN_RATIO`, unlike a normal divider drag), so resizing it — from its own
  edge or a neighbour's — springs back. Works wherever the panel sits in the
  tree, not just as the rightmost root leaf.
- **View:** `Content::TaskManager(PanelView)` — real tiled window, non-closable,
  non-minimizable, non-tabbable. Collapse to a ~36px icon rail (`«` / leader `M`).
- **Close:** always goes through `request_close_*` so the running-process confirm
  still applies.
- **Quit:** `deserted()` ignores the panel — a lone panel does not keep the app
  alive.

## Settings

`%APPDATA%\foreman\settings.json`:

- `panel_collapsed` (bool)
- `panel_width` (f32, expanded px)

## Key files

- `src/panel.rs` — model types + row paint
- `src/wm.rs` — `panel_model`, `surface_target`, `ensure_panel`, path Acts, drains
- `src/layout.rs` — `set_leaf_width`
- `src/keymap.rs` — `Command::ToggleTaskManager` (default leader `M`)
- `src/config.rs` — persistence fields
- Spec/plan: `docs/superpowers/specs/2026-07-09-task-manager-panel-design.md`,
  `docs/superpowers/plans/2026-07-09-task-manager-panel.md`
- Mockup: `docs/superpowers/specs/2026-07-09-task-manager-panel-mockup.html`

## Out of scope (still)

- Agent-state badges / status dots on rail rows
- Drag from panel into the tree
- Per-row context menus
