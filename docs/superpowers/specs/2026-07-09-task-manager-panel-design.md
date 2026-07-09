# Task-manager panel — design

Status: approved 2026-07-09.
A desktop-level side panel listing every project and its terminal/chat tabs,
grouped by owning project, with open/minimized state and click-to-focus. It
fully replaces the minimize chip taskbars and is the future home of
agent-state badges (see docs/warp-feature-candidates.md §7).

## Decisions (from brainstorm)

- The panel is a **real window in the desktop tiling tree** — on by default,
  collapsible via a header button, tear-out-able/floatable like any window.
- **Full replacement:** both chip taskbars (desktop + per-project,
  `paint_taskbar`) are deleted. Minimized windows appear only in the panel.
  The panel itself can never be closed, so nothing becomes unreachable.
- **Row granularity: every tab, including chat viewers.** One row per project
  tab; under each project, one row per terminal tab and chat viewer,
  background tabs included and marked. The panel is the full truth of what
  exists.
- **Row actions: click-to-focus plus hover-revealed minimize/close buttons**
  (matching the hover-opened project-header menus pattern).
- **Placement: right-edge root vertical split, ~260 px, rail collapse** — the
  header button collapses to a ~36 px icon rail; expanding restores the
  previous width. Sibling re-fit on toggle is normal divider-resize behavior.

## Model & seams

Two deep modules and one shallow view (vocabulary: deep-module / seam /
adapter per the codebase-design skill).

**`TargetPath` — the shared address type.**

```rust
struct TargetPath {
    project: WinId,          // desktop-level window id
    window:  Option<WinId>,  // child window inside the project WM
    tab:     Option<usize>,  // tab index within that window
}
```

Project rows carry just `project`; terminal/chat rows fill all three.

**Read seam: `WindowManager::panel_model(&self) -> PanelModel`** — pure
snapshot walk of projects → nested WMs → tabs. Plain concrete structs, no
traits (one implementation; a trait would be a hypothetical seam):

```rust
struct PanelModel  { projects: Vec<ProjectEntry> }
struct ProjectEntry { path: TargetPath, title: String, minimized: bool,
                      focused: bool, tabs: Vec<TabEntry> }
struct TabEntry     { path: TargetPath, title: String, kind: RowKind,
                      minimized: bool, active_tab: bool, focused: bool,
                      exited: bool }
enum RowKind { Terminal(IconKind), Chat }   // IconKind = existing Claude/Codex/plain detection
```

Rebuilt each frame before the draw pass (dozens of rows; trivially cheap).
This is also the read model a future fleet view or `foreman status --tree`
consumes.

**Write seam: `surface_target(&mut self, TargetPath)`** — one routing method
behind which the whole "make this visible and focused" dance hides: restore
the project → restore the child window → switch the active tab → raise
z-order → run the focus cascade. Exposed to the UI as new path-carrying
variants folded into the **existing** deferred-`Act` queue (one apply loop,
one place where modal-freeze etc. is enforced):

- `Act::FocusPath(TargetPath)`
- `Act::MinPath(TargetPath)`
- `Act::ClosePath(TargetPath)`

**Crew-board retrofit (required, same change):** the chat crew click-to-focus
switches to `Act::FocusPath` through `surface_target`, replacing its bespoke
resurface dance (wm.rs ~1455–1465, "surface it like the taskbar's Restore
does"). The seam gets two real adapters on day one.

**View: `Content::TaskManager(PanelView)` — deliberately shallow.** egui glue
that renders `PanelModel` rows and emits `Act`s. `PanelView` holds transient
UI state (hovered row, scroll) plus `collapsed: bool` and
`expanded_width: f32`.

## Window integration

- Desktop WM only. Created automatically at startup as a right-edge root
  vertical split (~260 px); if state restore yields no panel, one is created;
  the desktop WM maintains at most one.
- The panel `Win` is flagged: **non-closable, non-minimizable, non-tabbable**
  (excluded as tab/merge source and target — projects must not tab with it).
  Tear-out/float and leader-WASD navigation work normally.
- `deserted()` excludes the panel window — closing the last project still
  exits the app; a lone panel does not hold it alive.
- `collapsed` + `expanded_width` persist in `%APPDATA%\foreman\settings.json`
  alongside existing persisted settings.

## UI

- **Expanded (~260 px):** quiet-chrome header band (window-chrome.md) with
  the collapse button. Body: project rows, tab rows indented beneath. Row =
  icon (project glyph / Claude / Codex / chat) + title + state: minimized
  dimmed with marker, focused highlighted, active tab marked, exited
  struck/dim. Plain egui scroll area on overflow. Row styling borrows the
  crew board.
- **Collapsed rail (~36 px):** one icon per project stacked vertically,
  tooltip = title, click = focus project. Collapse button flips to expand and
  restores `expanded_width`. The rail is the future home of per-project
  status dots.
- **Hover actions:** two small right-aligned buttons per row —
  minimize/restore (context-dependent) and close. Close routes through the
  existing close path so the running-session confirm dialog still applies.

## Interactions & keymap

- Click row → `Act::FocusPath` (restores anything minimized along the path).
- Hover buttons → `Act::MinPath` / `Act::ClosePath`.
- New `Command::ToggleTaskManager` (collapse/expand only; not focus) in
  `keymap.rs` — receives a default chord automatically via the
  merge-over-defaults design.

## Removals

- `paint_taskbar` and chip rendering deleted at both levels. `Act::Restore`
  survives (used by routing). The minimize command is untouched — windows now
  minimize "into" the panel.

## Edge cases

- **Modal freeze:** panel interactions are `Act`s through the existing apply
  loop, so the drop-Min/Close-under-a-modal guard (tested, wm.rs ~7054)
  applies unchanged.
- **Zoom overlay:** unaffected; the panel is a tree leaf under the overlay.
- **Stale paths:** clicking a row the same frame its window closes —
  `surface_target` no-ops on missing ids (same tolerance as the current Act
  loop).
- **In-flight rename/drag:** panel-initiated minimize/close reuses existing
  paths that already clear in-flight renames (tested behavior).

## Testing

Unit (existing wm.rs style, no GUI):
- `panel_model()` correctness: grouping, minimized/active/focused flags, chat
  tabs included, background tabs listed.
- `surface_target` matrix: minimized project / minimized child / background
  tab / already-focused no-op / stale path.
- Crew click goes through the seam (behavior unchanged, code path shared).
- `deserted()` ignores the panel; app still quits on last project close.
- Panel refuses close, minimize, and tab-merge (source and target).
- Modal freeze drops panel acts.

Visual: build + screenshot expanded, collapsed rail, and hover states per the
standard verify loop.

## Out of scope (explicit)

- Agent-state badges/status dots (future; this panel is their landing site).
- Drag-and-drop from panel rows into the tree.
- Per-row context menus (v1 is hover buttons only).
