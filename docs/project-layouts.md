# Project layouts

Named arrangements of a Project's child windows. Spec (authoritative on the
product contract):
`docs/superpowers/specs/2026-10-16-project-layouts-design.md`.

This doc tracks what has shipped. Status: **model, identity,
reconciliation (fill missing slots), the saved-layout library on disk, live
Apply / Revert with the Modified flag, and the Project-header picker with
Save**. Not yet: the New Project layout and Create Layout… (later cards of
the `project-layouts` plan).

## What exists

### Layout item identity

Every `Tab` carries a `layout_id` (`Tab::layout_id`, `src/wm.rs`): a `u64`
minted once at construction from a process-global counter. It is **not** the
Win id, the terminal Member id, or the panel `tab_uid`. It travels with the
`Tab` value through merge and untab, so regrouping never changes it.

The id is persisted as `TabSnap::layout_id` (`src/workspace.rs`), an additive
`Option` that is omitted when unset, so v1 workspace files and old builds are
unaffected. Restore keeps a persisted id verbatim and raises the counter past
it (`reserve_layout_id`), so a tab minted later never collides. A tab without
an id (old file) and a duplicate id in the same manager (hand-edited file) each
get a fresh id.

Desktop chrome (`TaskManager`, `Settings`) has an id too, but
`Content::layout_kind` returns `None` for it, so it never enters an inventory.

### The model (`src/project_layout.rs`)

GUI-free; serde only. No egui, no `Content`, no `WindowManager`.

| Type | Meaning |
|---|---|
| `ItemRef { id, kind }` | One layout item: its identity and `ItemKind` (Terminal, Chat, Board, Plan, GitHistory, GitChanges, GitDiff, Image, Project). |
| `Arrangement { windows, tree }` | A complete arrangement. `windows` is a list of `WindowSpec`; `tree` is the tiling tree whose leaves index that list. |
| `WindowSpec { tabs, active, state }` | One tab stack and its `WindowState`: `Tiled`, `Floating { rect }`, or `Minimized { from_tree, rect }`. |
| `NormRect` | Floating / minimized-restore geometry as fractions of the Project's content area, so a saved layout adapts to the current size. |
| `TreeSpec` | `Leaf { window }` or `Split { dir, ratios, children }`. Same shape as `layout::Node`; serde-only so the model stays GUI-free. |
| `LayoutDefinition { id, name, items, arrangement }` | A saved, named layout. `id` is immutable, `name` is editable. |

All persisted types use `deny_unknown_fields`, which is what rejects extra
provider fields later.

### Validation (`validate`, `validate_items`, `validate_name`)

Rejects, with a typed `LayoutError`:

- more than `MAX_ITEMS` items, duplicate ids, unknown ids, an item placed in
  no window or in two;
- an empty stack or an `active` index out of range;
- a tree deeper than `MAX_TREE_DEPTH`, a leaf outside the window list, a leaf
  on a non-tiled window, a window appearing twice, a tiled window absent from
  the tree, a split with fewer than two children or a ratio count that does
  not match;
- non-finite or non-positive ratios, non-finite rects, rects without area;
- an empty name or one longer than `MAX_NAME_CHARS` after trimming.

Ratios are not required to sum to 1; reconciliation renormalizes. Rects may
lie outside 0..=1; `NormRect::to_local` confines them to the area and enforces
the size floor at apply time.

### Reconciliation (`reconcile`)

`reconcile(def, current, inventory)` fits a definition to the Project as it
is now. `inventory` is `WindowManager::layout_inventory` (tiled stacks in tree
order, then floating back→front, then minimized back→front; tabs in stack
order). `current` is the live arrangement of those same items; reconcile
needs it to know which items are minimized and where they restore to.
Deterministic, total, and never touches the live manager or opens anything.
The result is validated before it is returned, so a `Reconciliation` is
always applicable.

1. Exact id + kind matches first, for every slot.
2. Remaining saved slots take the lowest-id unassigned current item of the
   same kind, and a substitute takes the slot's state. Tiled and floating
   slots pick first, then minimized slots, so a minimized slot never takes
   the window a visible slot needed. A visible slot prefers a visible item
   and only then restores a minimized one; a minimized slot takes only an
   already-minimized item. So Apply never hides a visible window, and never
   brings back a minimized one while a visible one of that kind is free.
   Kinds never cross.
3. A still-empty **tiled or floating** slot whose kind opens without a target
   keeps its place under a placeholder id and joins `to_open`. Every other
   empty slot is `unfilled` and omitted: its tab leaves its stack, empty
   stacks go, single-child splits collapse, surviving ratios renormalize. A
   stack whose active tab vanished keeps the same slot, clamped.
4. Unassigned **visible** items form one tiled **Additional windows** stack,
   in inventory order with the first tab active, appended as the right child
   of an H split (`ADDITIONAL_RATIO` of the width) or as the root when no
   saved tiled geometry survived. Unassigned **minimized** items stay
   minimized in their own live stack, with its restore placement
   (`kept_minimized`), so a switch never brings back windows the user put
   away.

#### Opening missing slots

Which kinds open fresh. Confirmed against the `wm.rs` open paths when Apply
landed: Apply opens through the same constructors the menus use
(`spawn_shell_tab` for a terminal, `view_tab` for the views, which
`open_*_window` also calls), so the list cannot drift from them.

| Kind | Opens fresh? | Why |
|---|---|---|
| Terminal | yes, any number | default shell in the Project directory; never a command or agent |
| Chat, Board, Plan, GitHistory, GitChanges | yes, **at most one** | built from Project state alone (`open_*_window`), but each is a per-Project singleton |
| Image, GitDiff, Project | no | each needs a target (file, diff, directory) |

The singleton rule is real, not caution: `open_chat_window` and friends
surface the existing viewer instead of opening a second. So a viewer slot
queues an open only when the Project has none of that kind and no earlier
slot already queued one; a second Chat slot is `unfilled`.

A placeholder id is never the id of a current item (they count down from
`u64::MAX`). `Reconciliation::items` lists what the arrangement places: the
inventory plus one placeholder per `to_open` slot.

Apply's job, in order: open one fresh item per `to_open` slot, then call
`with_opened(&[(placeholder, new_item)])`. That swaps each placeholder for the
new id, records it in `opened`, and treats a slot with no pair as a failed
open: the slot leaves the arrangement and joins `unfilled`. It rejects an
unknown or repeated placeholder, a new item of the wrong kind, and an id
already in the inventory. The result has an empty `to_open` and is what Apply
moves Content into.

#### Summary and Modified

`matched`, `substituted`, `opened`, `additional`, `kept_minimized`, and
`unfilled` feed the post-Apply summary. `is_exact` is the Modified test: true
when nothing went to Additional windows and every slot was filled.
Substitution and kept-minimized extras do not make an Apply dirty; pending
`to_open` slots do until `with_opened` resolves them, so call it first.

Switching A → B → A converges: B parks A's extras in Additional windows (and
leaves A's minimized ones minimized), and A takes them back by exact id, so
nothing opens. The window count only grows to the largest layout used.

### Modified comparison (`same_arrangement`)

Compares tree shape and ratios, tab grouping and order, each window's state,
and floating / minimized geometry within a small tolerance. Ignores active
tabs and stacking order, because focus raises windows.

Normalized geometry only compares meaningfully between captures of the same
area: floating windows keep pixel rects across a Project resize, so their
fractions drift. The comparison is therefore for captures taken around a
structural edit, which is how the spec defines Modified; a resize alone must
not trigger it.

### The library on disk (`src/layout_library.rs`)

Saved definitions live in `%APPDATA%\foreman\project-layouts.json`, never in
Settings or a repository:

```json
{
  "version": 1,
  "projects": {
    "h:\\code\\repo": { "dir": "H:\\code\\repo", "layouts": [ /* LayoutDefinition */ ] }
  },
  "new_project": { /* optional LayoutDefinition copy */ }
}
```

**Who owns a library.** The key is `project_key(dir)`: absolute, `.`/`..`
resolved, symlinks/junctions/subst drives resolved when the directory is
reachable, no `\\?\` prefix, no trailing separator, lowercase on Windows. So
two Project instances of one directory share definitions, while a worktree
under `.foreman/worktrees/` is a different directory with its own library.
`dir` is only for people reading the file. Libraries outlive their Projects
(closing one deletes nothing) and an unreachable directory keeps its lexical
key. Moving a directory does not migrate its library.

**How to use it.** `LayoutStore::user()` (or `LayoutStore::at(dir)` in tests)
then, per the picker contract:

| Picker action | Call |
|---|---|
| Save with nothing selected, Save As | `create(project, name, items, arrangement, OnConflict::Reject)` |
| …user picked "Replace" on a duplicate | same with `OnConflict::Replace`: overwrites the other definition in place, keeping its id |
| Save with a layout selected | `update(project, id, items, arrangement)` |
| Rename… | `rename(project, id, name, on_conflict)`; Replace deletes the other holder of the name |
| Delete… | `delete(project, id)`; already gone is `Ok` |
| Use for new Projects / reset | `set_new_project_layout(Some(&def))` / `(None)` |
| Menu contents | `project_layouts(project)`, `new_project_layout()` |

Names are trimmed and checked (`validate_name`), duplicates compare
case-insensitively within one Project, and the arrangement is validated
before anything is read or written. `StoreError` says why a call failed and
its `Display` is the user-facing message: `NameTaken` (offer Replace or
another name), `NotFound` (deleted elsewhere: show Custom), `Invalid`,
`FutureVersion`, `Io`. A call that returns `Err` wrote nothing, so the UI
reports success only on `Ok`.

**Two processes.** There is no cache. Every call reads the file fresh, and
every change re-reads, edits only its own definition by id (or the New
Project slot), and writes. So the installed app and a dev build saving
different definitions both keep them. The write goes through
`config::save_json_in`, whose staging file is unique per process. There is
no lock: two writes within a few milliseconds can still lose one.

**Bad files.**

- Missing file: empty library. Reading never creates it.
- Malformed JSON, or an unknown field: the shared config recovery backs the
  bytes up as `project-layouts.json.corrupt-…` and queues a toast before the
  next write may replace them. Strict parsing is on purpose: trimming unknown
  fields on rewrite would silently delete a newer build's data.
- `version` above `LIBRARY_VERSION`: every call returns `FutureVersion` and
  the file is neither backed up nor written. **Any change to the file's
  shape, a definition's included, must bump `LIBRARY_VERSION`.**
- Unreadable (locked, a directory): `Io`, and no write follows.

### Picker state in the workspace (`LayoutSelection`)

Selection and Modified belong to one Project instance, not to the library.
`WindowManager` keeps a `LayoutSelection { selected, modified }`
(`layout_selection` / `set_layout_selection`; setting a different value
marks the workspace dirty). Capture writes it to `ManagerSnap::layout`,
omitted when Custom and unmodified, so old files load as Custom. Cold restore
puts it back alongside the tab `layout_id`s and does not reapply the layout.
A selection whose definition was deleted elsewhere stays as saved;
`resolve_selection(layouts, selected)` returns `None` and the picker shows
Custom. `modified` is set by the edit watch below.

### Apply and Revert (`src/wm/layouts.rs`)

Runs on a Project's child `WindowManager`. Refuses the desktop.

**How to use it.** The picker calls `request_layout(LayoutRequest::Apply {
def, select })` or `request_layout(LayoutRequest::Revert)` on the active
Project's child manager. The manager runs it at the top of its next `show`
(the deferred mutation boundary: area known, nothing borrowed, the same frame
draws the result) and keeps the result for `take_layout_outcome()`. `select:
true` is a saved layout and selects it; `false` is a Create candidate and
leaves the picker on Custom. `layout_revert_status()` says whether **Revert
Layout** is enabled; `mark_layout_saved(id)` is what Save calls after a
successful write. `apply_layout` / `revert_layout` are the same steps run
now, for tests.

**What Apply does, in order.**

1. `capture_arrangement` turns the live windows into an `Arrangement`
   (normalized against the current area; a zoomed float reports the rect it
   un-zooms to) plus the inventory.
2. `reconcile` against the definition. An invalid definition stops here.
3. Open one fresh item per `to_open` slot: a plain shell (the default shell
   setting, in the Project directory, no command, no agent, no chat join) or
   a view. A failed open goes into `failed_opens` and its slot is dropped by
   `with_opened`; the Apply carries on.
4. `rearrange` checks the arrangement places exactly the tabs that exist,
   then takes every `Tab` out of its window and puts it into the new windows.
   The `Tab` values move, so the Session, its process, grid, scrollback,
   Member id, chat membership, viewer target and view state, title, and agent
   state all come along. Nothing is rebuilt from a snapshot.
5. Select the definition, Modified = `!is_exact()`. Store the revert point.
   Mark the workspace dirty.

A window keeps the id of the window its active tab came from when it can, so
the panel and focus see the same window. Child zoom is exited. Floats are
confined to the area with the `MIN_W`/`MIN_H` floor. Focus stays on the
previously focused Content if it is a visible active tab; otherwise the first
visible window in arrangement order; nothing when everything is minimized.
Other Projects and the desktop are never touched.

Refused, with the arrangement intact: the desktop (`NotAProject`), a Project
never laid out (`NotLaidOut`), an open close-confirm (`Busy`; its target is a
tab index that must not move), and Content that is not a layout kind.

**Revert.** One level, in memory: the pre-Apply arrangement (ids and
geometry, never Content) and selection. Revert reconciles that saved
arrangement against the live windows like any definition, so every old item
matches by id and anything the Apply opened lands in Additional windows. It
opens nothing. It restores the old selection and Modified value, then the
point is gone. A structural edit after the Apply retires it
(`RevertStatus::Invalidated`; the reason text is
`LayoutApplyError::RevertInvalidated`). Focus changes and resizes do not.

**Modified is an event, measured per frame.** `mark_workspace_dirty` sets a
probe (every structural edit already calls it, along with focus and other
noise). At the end of `show`, if the probe is set, `watch_layout_edits`
captures the arrangement and compares it with the one from the last check
using `same_arrangement`. Different means a structural edit: Modified goes on
(only when a layout is selected) and the revert point is retired. Same means
focus, active tab, title, zoom, or viewer navigation: nothing happens. The
comparison is always between two live captures in the same area, never
against the definition.

Gotcha: a resize **re-measures without comparing**. Floats keep pixel rects
and get clamped into a smaller area, so comparing across a resize would
report an edit nobody made. The cost is that an edit landing in the very
frame the area changed is not counted. A Project that is not on screen (an
inactive Project tab) runs neither the request nor the watch until it is
shown.

### The picker and Save (`src/wm/layout_picker.rs`)

**Where.** Every Project header has **Layout ▾** and **Save** just left of
its ⋯ and ✕. Under `LAYOUT_CTL_FULL_MIN_W` (520px) of header width they turn
into icon buttons (a split-pane mark with ▾, and a down-arrow-into-tray),
with the same tooltips, so the title keeps room. A zoomed Project keeps its
header, so the controls stay. They act on the stack's active Project tab,
the one the header draws. Clicking them in an unfocused Project focuses it
first. Child windows get no picker.

**The label** is the selected layout's name, or **Custom**. An amber dot
after it means Modified.

**The menu** says "Layouts for this Project", then one row per saved layout:
a small diagram, the name, a check on the selected one, a "Modified" tag on
it when set, and Rename… / Delete… icons. Below: **Save** (its hint says
whether it updates a layout or asks for a name), **Save As…**, and **Revert
Layout** once an Apply has happened (shown disabled, with the reason, after
an edit retired the revert point). An empty library says layouts are saved
for this Project. There is no Create Layout… and no Use for new Projects
yet; those belong to later waves, and an inert entry would only mislead.

The diagram is drawn from arrangement data only (`diagram_cells`): tiled
windows by tree and ratios, floating ones outlined on top, minimized ones
not drawn, and a one-letter kind mark where it fits (`>` terminal, `C` chat,
`B` board, …) so a mixed layout reads at a glance.

**Selecting a row applies it.** It queues `LayoutRequest::Apply { select:
true }` on the Project being drawn; the next frame runs it. The header then
reads `take_layout_outcome`. An exact Apply says nothing. Otherwise a
dismissible summary sits under the picker, built from the applied
`Reconciliation` (`summary_lines`): which windows stood in for saved ones,
what opened, what moved to Additional windows, which slots could not be
filled, and which opens failed (with the error, not shortened). It has a
**Revert Layout** button. A failed Apply or Revert shows its error instead.

**Save and friends.**

| Action | What happens |
|---|---|
| Save, Custom | Name dialog, then `create(…, Reject)`; selects the new layout |
| Save, a layout selected | `update` in place, no prompt; clears Modified |
| Save, selected layout deleted elsewhere | Picker goes to Custom and the name dialog opens with "no longer exists" |
| Save As… | Name dialog prefilled `<name> copy`; always a new definition |
| Duplicate name (any case) | Dialog stays; "already exists — replace it, or type another name". Replace overwrites it in place, keeping its id, and selects it. Typing clears the offer |
| Rename… | Dialog prefilled; Replace deletes the other holder of the name, and if that was selected the picker goes to Custom |
| Delete… | Confirm dialog; the windows stay put; if it was selected, Custom |
| Cancel, Escape, or a click outside | Nothing changes, on disk or on screen |
| A write fails | The dialog stays with the error and the typed name; selection and Modified are untouched |

Success is reported (a short "Saved …" note) only after the write returned
`Ok`.

**Disk.** `LayoutStore` re-reads the file on every call, so the picker keeps
a copy: read once when the header first draws (the label needs the name),
again when the menu opens, and after each write. Never per frame. A second
instance of the same directory sees another's save the next time its menu
opens; its windows never move.

**Keyboard.** While a dialog is open, that Project's terminals are frozen
(`live` is false and `terminal_owns_keyboard` says no) so typing goes to the
name field. Enter submits, Escape cancels.

**Testing it.** A click becomes a `PickerAction`, and `run_layout_action`
does the work, so tests drive the bindings without a pointer.
`layout_ui.store` points a test at a temp library; without it, tests read a
never-written temp path, never the user's file.

Gotchas:

- **"No active Project" has no surface.** The controls live in a Project's
  header, so with no Project open there is nothing to disable. The disabled
  look ("Open or select a Project to use layouts.") appears only for a
  child manager without a directory, which the app never creates, and
  "Answer the open dialog first." while a close confirmation is up.
- Applying uses the menu's copy. If another instance deleted that layout a
  moment ago, the Apply still runs (the definition is in hand) and the
  picker shows its name until the next read, then Custom.
- The summary sits over the top-right of the Project, often on the
  Additional windows header. Dismiss it with ✕.

## Not yet built

- Opening a new Project with `new_project_layout()`, and **Use for new
  Projects** in the menu (New Project wave).
- Create Layout… and the provider boundary (promptable Create wave).

## Key files

- `src/project_layout.rs` — the model, validation, reconciliation, comparison,
  geometry adaptation, and their tests
- `src/layout_library.rs` — `project-layouts.json`: `LayoutStore`,
  `project_key`, `resolve_selection`, and their tests
- `src/config.rs` — `load_json_from` / `save_json_in`, the shared recovery
  and atomic write the library rides on
- `src/wm/layouts.rs` — Apply, Revert, `rearrange`, `capture_arrangement`,
  the Modified edit watch, and their manager tests (including real-shell
  tests that drive Sessions through `send` / `snapshot` across Apply and
  Revert, directly and through the picker, and a `show`-level test of the
  drain and end-of-frame watch hooks)
- `src/wm/layout_picker.rs` — the header picker, menu, dialogs, summary,
  `PickerAction` / `run_layout_action`, `summary_lines`, `diagram_cells`,
  and their tests (including real-frame clicks on the header buttons)
- `src/wm.rs` — `Tab::layout_id`, `Content::layout_kind`,
  `WindowManager::layout_inventory`, the `layout` selection field, and
  capture/restore of both; `spawn_shell_tab` and `view_tab`, the open paths
  Apply shares; the `show` hooks; `header_layout` placing the picker and
  Save (`LayoutCtlLayout`), and the header draw calling
  `show_layout_controls`
- `src/workspace.rs` — `TabSnap::layout_id`, `ManagerSnap::layout`
- `docs/workspace-persistence.md` — the snapshot it rides on
- `docs/tiling-tree.md` — the live tree an `Arrangement` mirrors
