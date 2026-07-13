# Workspace persistence

Cold restore of the desktop layout across restarts. **Not** live PTY / agent
process survival (that would be a daemon/client split — see HANDOFF §5).

## What it does

On restart, Foreman reloads the last desktop layout from
`%APPDATA%\foreman\workspace.json`:

- Open projects (each at its saved `cwd`)
- Nested tiling tree (splits + ratios), tabs, floating rects
- Minimized windows (`min_from_tree` so restore re-tiles when appropriate)
- Focus (window + active tab) and zoom, when set
- Terminal **shell kind** (`powershell` / `cmd` / `bash`) and chat viewer tabs

Every terminal spawns a **fresh** shell of that kind at the **project `cwd`**.
No agents are re-dispatched; no scrollback or Ready state is recovered.

## What it does not

| Out of scope | Where that lives (if anywhere) |
|---|---|
| Live ConPTY / child process survival | Future daemon — not this feature |
| Shell cwd after the user `cd`s | Not tracked; always project `cwd` |
| Agent / `foreman open` command re-launch | Deliberate: no surprise fleets |
| Scrollback, grid, Ready latches | Cold shells only |
| Chat message history / member ids | Separate plan: `docs/chat-persistence.md` |
| Task-manager panel window | Always recreated via `ensure_panel` |
| Panel collapse / width | `settings.json` only |
| Font size, keybindings, recents MRU | Their own files under `%APPDATA%\foreman` |
| OS main-window position/size | Out of v1 |

## Persistence map

```text
settings.json      → font_size, panel_collapsed, panel_width, panel_dock
keybindings.json   → leader + chords
recents.json       → MRU open list
workspace.json     → last desktop layout (this feature)
```

I/O goes through `config::load_json` / `save_json` (atomic `.tmp` + rename).
Missing, corrupt, or **future** `version > 1` files load as an empty default
and never take the app down.

## When it saves

1. Structural mutation marks the manager dirty (open/close, split, tab,
   float/min, rename, focus, zoom, tree drop/drag, etc.). Nested project
   managers bubble dirty up via `poll_workspace_dirty`.
2. End of frame: if dirty and quiet for **600 ms**
   (`WORKSPACE_SAVE_DEBOUNCE`), capture + write.
3. Clean quit flushes immediately (`App::flush_workspace` and `on_exit`).
4. **Empty layout is saved too** — closing every project then quitting must
   not resurrect the previous layout on the next launch.

Panel resize/collapse and font zoom do **not** dirty the workspace (they
debounce into `settings.json`).

## Startup order

```text
load settings + recents
load workspace.json
if snapshot has restorable projects:
    apply_workspace  (fresh shells; skip missing cwds)
ensure_panel from settings   # always; panel never in workspace.json
if nothing restored && !FOREMAN_LANDING:
    auto-open project at cwd (legacy path)
# With FOREMAN_LANDING: empty *or all-minimized* desktop shows the landing
# beside the Sessions panel strip (should_show_landing).
discard take_opened + poll dirty   # restore must not write recents or re-save
```

## Gotchas

- **Missing project directories are skipped** (`ApplyReport.projects_skipped`);
  remaining projects still restore.
- **Snapshot ids are file-local.** Runtime `WinId`, `tN`, and `pN` regenerate
  on apply. Fine for layout; chat cursor restore and stable dispatch ids need
  more work later.
- **Multi-tab terminals in one window** may share one `term_id` on restore
  (stamped from the window id). Prefer unique per-tab ids before relying on
  chat/dispatch against restored multi-tab stacks.
- **Over-dirty is intentional.** Focus-only acts and continuous drag/resize
  refresh the debounce timer; writes still only happen after 600 ms idle.
- **Coordinates are local** to each manager's area (never screen space).
- Do not put layout fields on `Settings` — keep the workspace file separate.

## Key files

- `src/workspace.rs` — `WorkspaceSnapshot` / `ManagerSnap` / `NodeSnap`,
  `load` / `save` / `is_empty`, shell string map, tree ↔ snap conversion
- `src/wm.rs` — `capture_workspace` / `apply_workspace` / `ApplyReport`,
  `mark_workspace_dirty` / `poll_workspace_dirty`
- `src/main.rs` — first-frame restore, debounce, `flush_workspace`, `on_exit`
- `src/config.rs` — shared `load_json` / `save_json` only (no layout fields)
- Spec/plan (history):  
  `docs/superpowers/specs/2026-07-13-workspace-persistence-design.md`,  
  `docs/superpowers/plans/2026-07-13-workspace-persistence.md`
