# Spec: cold workspace persistence

Date: 2026-07-13. Status: design approved (brainstorm); not yet built.

## Problem

Foreman already persists small preferences and MRU state, but not the live
desktop layout. After a restart the user loses open projects, nested terminal
windows, tiling/tabs/floats, focus, and project directories. That is the gap
this feature closes.

### Already on disk (not in scope)

| File | What |
|---|---|
| `%APPDATA%\foreman\settings.json` | `font_size`, `panel_collapsed`, `panel_width` (sessions/task-manager panel) |
| `%APPDATA%\foreman\keybindings.json` | Leader + chords |
| `%APPDATA%\foreman\recents.json` | Up to 5 recent project paths + open kind |

Panel size/collapse is **already implemented** (`src/config.rs`, applied via
`ensure_panel` in `src/main.rs`). This spec does not re-implement panel
geometry. If collapse/width still resets, that is a bug in the existing path.

### Related but different: daemon / live session survival

HANDOFF §5 item 4 and the research-frontier "session persistence" item mean
**true tmux-style survival**: same ConPTY, same child process, same screen after
a GUI restart. That requires a headless session host. **This spec is not that.**

This spec is **cold workspace restore**: reopen projects and windows at saved
directories with a fresh layout of fresh shells. Processes, scrollback, Ready
latches, and chat history do not survive.

## Decisions (from brainstorm)

| Decision | Choice |
|---|---|
| Restore kind | Cold workspace restore only |
| Layout fidelity | Full: desktop + nested managers — tile tree (splits/ratios), tabs, floating rects, minimized + `min_from_tree`, focus (window + active tab), zoom if set |
| Terminal directory | Project `cwd` for every terminal in that project (v1). No live shell `cd` tracking |
| Content to restore | `Content::Terminal` (fresh shell of saved `Shell`) and `Content::Chat`. No re-dispatch of Claude/Codex/`foreman open` commands |
| When to save | Debounced on structural change (~600 ms), plus flush on clean quit |
| Empty workspace | Persist empty too, so deliberately closing everything does not resurrect an old layout after a clean exit |
| Storage approach | Separate `workspace.json` snapshot (not `settings.json`, not an event replay log) |
| Panel prefs | Stay in `settings.json` only; never duplicate into the workspace file |
| Stable `tN`/`pN` across restart | Out of v1 (blocks chat cursor restore later; not required for layout) |
| OS main-window geometry | Out of v1 |
| Live cwd / agent re-launch | Out of v1 |

### Rejected alternatives

- **Embed layout in `settings.json`.** Settings are a flat preference bag (font,
  panel). A full recursive window tree is large state; mixing it couples zoom
  debounces to layout churn and makes "wipe layout without wiping prefs" hard.
  Same separation already used for `recents.json`.
- **Append-only replay log of opens/moves.** Fragile as tree ops grow;
  harder to unit-test than one snapshot; overkill for cold restore.
- **Daemon-first.** Correct for live survival; wrong cost for "put my windows
  back." Ship cold restore first; keep the door open for a later host process.

## Goals and non-goals

### Goals (v1)

1. After restart (or crash after the last debounced write), reopen the previous
   desktop layout with full tiling/tab/float/min/focus/zoom fidelity.
2. Each project restores at its saved `cwd`; every terminal in that project
   spawns a new shell of the saved `Shell` kind in that `cwd`.
3. Chat viewer tabs reopen as empty viewers attached to a new in-memory room
   for that project (chat **log** persistence is a separate design:
   `docs/chat-persistence.md`).
4. Corrupt or missing `workspace.json` never takes the app down; fall back to
   today's startup path (landing flag / auto project / empty desktop).
5. Capture/apply logic is testable without a full GUI loop; disk I/O stays in
   one place using `config::load_json` / `save_json`.

### Non-goals (v1)

| Out of scope | Why |
|---|---|
| Live shell cwd after the user `cd`s | Not tracked today; would need OSC 7 / process query |
| Re-running dispatch / landing agent commands | Avoid surprise agent fleets on every launch |
| Scrollback, Ready, PTY grid | Cold shells only |
| Chat message history / Member ids | Separate JSONL plan; ids are not stable across restart |
| Stable `FOREMAN_TERMINAL_ID` / `FOREMAN_PROJECT_ID` | Same; regenerate on restore |
| Daemon / PTY process survival | Different product |
| Main OS window position/size | Not requested for v1 |
| Serializing `Content::TaskManager` | Always recreated by `ensure_panel` from settings |

## Architecture

### Persistence map after this feature

```text
settings.json      → flat prefs (font, panel collapsed/width)
keybindings.json   → chords
recents.json       → MRU open list (path + kind)
workspace.json     → last desktop layout (this feature)
```

### Module split

| Piece | Owns |
|---|---|
| **`src/workspace.rs` (new)** | Snapshot types, `load`/`save`, pure tree conversion, `capture` / `apply` orchestration. Deep module: callers do not invent JSON shapes |
| **`src/wm.rs`** | Structural dirty hooks; optional disk-free `to_snapshot` / rebuild helpers. Does not call `save_json` itself |
| **`src/layout.rs`** | Convert `Node` ↔ serializable node (or expose enough for workspace to walk). Keep layout pure |
| **`src/main.rs`** | Load snapshot at startup; debounce timer; flush on clean quit |
| **`src/config.rs`** | Unchanged helpers only — **no** workspace fields on `Settings` |

Same pattern as `src/recents.rs`: the window engine reports structure; a small
module owns the on-disk bag.

### Identity

- Snapshot uses **`SnapId`** values stable **only within one file** (u64 or
  string keys). They wire tree leaves to windows in that JSON document.
- On restore, allocate fresh runtime `WinId`s and build `SnapId → WinId`.
- Runtime Member ids (`tN`) and project tags (`pN`) **regenerate**. Acceptable
  for cold layout; call out as a shared prerequisite if chat cursors or daemon
  reattach land later.

### Coordinate contract

Serialized window `rect` / `prev` are **local** to the manager's area (same
contract as live `Win.rect`). Do not store screen coordinates. After restore,
tiled leaves get rects from the tree layout next frame (one-frame lag is
existing intentional behavior).

## On-disk format

**Path:** `%APPDATA%\foreman\workspace.json`  
**I/O:** `config::load_json` / `save_json` (atomic `.tmp` + rename; missing or
invalid JSON → default empty snapshot; stderr warning for corrupt/unreadable).

### Versioning

```json
{
  "version": 1,
  "desktop": { ... }
}
```

| Rule | Behavior |
|---|---|
| `version == 1` | Load normally |
| Missing `version` | Treat as 1 (only format this build writes) |
| `version > 1` unknown to this build | Discard entire file, use empty default, log warning |
| Extra unknown fields | Ignored (`#[serde(default)]` / serde ignore-unknown as used elsewhere) |
| Missing optional fields | Defaults |

### Conceptual schema

```text
WorkspaceSnapshot
  version: u32                    // 1
  desktop: ManagerSnap

ManagerSnap
  cwd: Option<PathBuf>            // Some on project managers; None on desktop
  focused: Option<SnapId>
  last_focused: Option<SnapId>
  zoomed: Option<SnapId>
  windows: Vec<WinSnap>           // z-order: low index = back, high index = front
                                  // (matches typical painter's algorithm; pin with a test)
  tree: Option<NodeSnap>          // tiled leaves; windows absent from tree are floating

WinSnap
  id: SnapId
  active: usize                   // active tab index
  tabs: Vec<TabSnap>              // never empty after filter
  minimized: bool
  min_from_tree: bool
  rect: RectSnap                  // local; required for float / float-restore
  prev: Option<RectSnap>

TabSnap
  title: String
  content: ContentSnap

ContentSnap                        // tagged enum
  Terminal { shell: ShellSnap }   // "powershell" | "cmd" | "bash"
  Chat
  Project { child: ManagerSnap }
  // TaskManager is never written

NodeSnap
  Leaf { id: SnapId }
  | Split { dir: "H" | "V", ratios: [f32], children: [NodeSnap] }

RectSnap
  x, y, w, h: f32                 // local to manager area
```

### Shell serialization

Map existing `Shell` to stable strings. Unknown string on load → `PowerShell`
(default shell today) with a stderr note, not a failed parse of the whole file.

### What is never in the blob

- Live PTY grid / scrollback  
- Chat room log, members, outbox  
- Pending settles, modals (rename, dirpicker, settings, confirm)  
- Injected env values (recomputed on spawn)  
- Task-manager panel window  
- Panel width/collapse (settings only)

## Save path

### When the workspace is dirty

Mark dirty after successful structural mutation (desktop or nested), including:

- Open/close project or terminal  
- Split, tree move, tab merge/untab, set active tab, close tab  
- Float / re-tile, minimize / restore, maximize if it changes tree membership or `prev`  
- Rename (titles are in the snapshot)  
- Focus change, `last_focused` update, zoom toggle  

Do **not** mark dirty for:

- PTY bytes, paint, selection, caret  
- Font zoom (settings)  
- Panel width/collapse (settings)  
- Chat compose text (room not in v1 snapshot)

### Debounce and flush

| Constant | Value | Notes |
|---|---|---|
| `WORKSPACE_SAVE_DEBOUNCE` | **600 ms** | Same family as `FONT_SAVE_DEBOUNCE` (400 ms); slightly longer because capture walks the tree |

1. Structural change → `workspace_dirty = true`, refresh dirty timestamp.  
2. End of `App::ui`: if dirty and quiet ≥ debounce, `capture` + `save_json`.  
3. Errors: `eprintln!`, never panic, never block the frame.  
4. **Clean quit:** force capture+save once so the last sub-debounce second is
   not lost (in addition to debounce).

### Capture contract

`WorkspaceSnapshot::capture(desktop: &WindowManager) -> WorkspaceSnapshot`:

- Read-only over live state.  
- Skip any window whose tabs are only `TaskManager` (or strip TaskManager tabs).  
- Recurse into `Content::Project`.  
- For `Content::Terminal`, record `Shell` only (not cwd; project manager cwd
  applies).  
- For `Content::Chat`, record the Chat tag only.  
- Tree leaves reference `SnapId`s assigned for this capture.

## Restore path

### Startup order (`App::new` / first show)

```text
settings = Settings::load()
recents  = Recents::load()
snap     = WorkspaceSnapshot::load()

desktop = WindowManager::new().as_desktop()

if snap has at least one restorable project after filtering:
    snap.apply(&mut desktop, &ctx)
    skip default auto-project-at-launch-cwd
else:
    existing behavior (FOREMAN_LANDING / auto project / empty)

// Always after layout is in place:
desktop.ensure_panel(settings.panel_collapsed, settings.panel_width)
```

"Restorable project" means a `ContentSnap::Project` whose `cwd` exists as a
directory at apply time (or a best-effort `is_dir` check once at restore, not
every frame).

### Apply algorithm (per manager)

1. Allocate runtime ids; build `SnapId → WinId`.  
2. Materialize each `WinSnap` with tabs:  
   - `Terminal { shell }` → spawn `Session` with `cwd = this manager's project
     cwd` (desktop-level terminals, if any ever exist, use `None` / process
     cwd — today terminals live inside projects).  
   - `Chat` → new viewer bound to this project manager's `ChatRoom`.  
   - `Project { child }` → create nested manager, set `cwd`, recurse.  
3. Rebuild `LayoutTree` from `NodeSnap`, remapping leaf ids. Drop leaves that
   failed to materialize.  
4. Restore `rect`/`prev`, `minimized`/`min_from_tree`, active tab, focused /
   last_focused / zoomed (only if those ids still exist).  
5. Z-order from snapshot document order.  
6. **Never** restore a TaskManager from disk; `ensure_panel` always installs it.  
7. After apply, panel leaf extent still comes from settings (existing
   `apply_panel_ratio` / pin behavior).

### Edge cases

| Case | Behavior |
|---|---|
| Project `cwd` missing (unplugged drive, deleted folder) | Skip that project; log warning; restore the rest |
| All projects skipped | Treat as empty snapshot → normal empty/landing startup |
| Invalid ratios / empty tree with tiled windows | Normalize ratios if trivial; otherwise leave those windows floating and log |
| Unknown content tag (future format) | Skip tab; if a window ends with zero tabs, drop the window |
| Empty `windows` after filter | Empty desktop; next user open starts clean |
| User closed every project, debounce (or quit flush) wrote empty | Next launch does **not** resurrect the previous layout |
| Crash before debounce | Last successful write wins (may be one structural change behind) |
| Spawn failure for one shell | Drop that tab/window piece; keep siblings |

### Interaction with `deserted()` → quit

Closing the last project still quits the app when landing is off (existing
behavior). Empty workspace is saved so a clean full-exit does not reload old
projects. Crash mid-close can still resurrect the pre-close layout — acceptable
for v1.

## Error handling

| Failure | Behavior |
|---|---|
| `APPDATA` unset / config dir unusable | Empty snapshot; no save attempts that panic |
| Read missing file | Empty default, silent (like settings) |
| Read corrupt JSON | Empty default + stderr warning |
| Write failure | stderr only; keep in-memory desktop; retry on next dirty cycle |
| Partial apply | Best-effort remaining windows; never abort the process |

## Testing

### Unit (no real multi-window GUI required)

1. **Serde:** missing fields → defaults; unknown fields ignored; version `2`
   → treated as unloadable / empty by load policy tests.  
2. **Tree round-trip:** `Node` ↔ `NodeSnap` preserves H/V, ratios, leaf
   identity mapping.  
3. **Capture → apply structural equality:** hand-built manager with nested
   project, split ratios, two tabs (terminal + chat), one floating win,
   one minimized, custom titles, focus/zoom → capture → apply into a new
   manager → assert tree shape, titles, shells, cwds, min/float flags, focus
   (spawn may use a test double or skip live PTY if a pure "content kind"
   rebuild path is extracted).  
4. **Missing project path** filtered on apply.  
5. **TaskManager** never appears in capture output.  
6. **Empty snapshot** serializes and loads cleanly.

Prefer extracting pure capture of structure vs Session spawn so most tests
need no ConPTY. If apply must spawn, gate those tests or use the existing
Cmd-spawn patterns carefully (they are slower/flakier).

### Manual acceptance

1. Two projects, nested splits, tabbed terminals, one float, one renamed tab,
   panel collapsed or resized → restart → layout + panel prefs return.  
2. Kill process after debounce → last layout returns.  
3. Close all projects, wait debounce (or clean quit) → restart → **no**
   resurrection of old projects.  
4. Unplug/missing project path → other projects still restore.

## Implementation phases (for the plan doc)

Suggested order for an executable plan; not a commitment to PR count:

1. **Schema + I/O** — `workspace.rs` types, load/save, serde tests.  
2. **Tree conversion** — `Node` ↔ `NodeSnap` pure tests.  
3. **Capture** — walk live WM; no apply yet.  
4. **Apply** — rebuild managers + spawn; skip missing paths.  
5. **Wire dirty + debounce + startup + quit flush** in `main.rs` / `wm.rs`.  
6. **Edge cases + docs** — feature doc `docs/workspace-persistence.md`,
   config-axis note, HANDOFF distinction (cold vs daemon).

## Docs and glossary (when shipping)

- Feature doc: `docs/workspace-persistence.md` (What it does / gotchas / Key
  files).  
- Note in `docs/settings-persistence.md` that workspace is a **separate** file.  
- HANDOFF / research-frontier: cold restore ≠ daemon session survival.  
- Optional CONTEXT.md term: **Workspace snapshot** — cold layout bag for the
  desktop; not live Session survival.  
  `_Avoid_:` session persistence (ambiguous with daemon).

## Open follow-ups (explicitly not v1)

1. Live shell cwd (OSC 7 / process table).  
2. Stable project/terminal ids across restart (chat cursors + daemon).  
3. Re-dispatch last agent command opt-in.  
4. OS window geometry.  
5. Chat JSONL history (`docs/chat-persistence.md`).  
6. Headless session host (true process survival).

## Key files (target)

| File | Role |
|---|---|
| `src/workspace.rs` | Snapshot model, load/save, capture/apply |
| `src/wm.rs` | Dirty hooks; structure walk; rebuild entry points |
| `src/layout.rs` | Tree serialize helpers if needed |
| `src/main.rs` | Startup restore, debounce, quit flush |
| `src/config.rs` | Existing `load_json`/`save_json` only |
| `src/panel.rs` / settings | Unchanged panel prefs path |

## Success criteria

Cold restore is done when:

1. A multi-project layout with splits, tabs, float, minimize, focus, and zoom
   returns after a clean restart with project dirs intact.  
2. Panel width/collapse still comes only from `settings.json` and still works.  
3. Fresh shells open at the **project** cwd; no agents auto-relaunch.  
4. Corrupt `workspace.json` leaves the app usable.  
5. Empty-after-close + clean quit does not resurrect the previous layout.  
6. Unit tests cover serde, tree round-trip, and capture/apply structure; manual
   path confirms restart behavior.
