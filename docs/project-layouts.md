# Project layouts

Named arrangements of a Project's child windows. Every Project lists the
same layouts (a few built in, then the ones you save), and each Project
instance selects its own. Spec (the product contract, except where this doc
says it was reversed):
`docs/superpowers/specs/2026-10-16-project-layouts-design.md`.

This doc tracks what has shipped. Status: **model, identity,
reconciliation (fill missing slots), the saved-layout library on disk, live
Apply / Revert with the Modified flag, the Project-header picker with Save,
the New Project layout, and Create Layout…** (single-shot, provider
proposed, may add windows that open on Apply). Every wave of the `project-layouts` plan has landed.

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
| `LayoutDefinition { id, name, items, arrangement, origin }` | A saved, named layout. `id` is immutable, `name` is editable. `origin` is the `project_key` of the directory whose tabs `items` came from (see **Whose ids**). |

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
4. An unassigned **visible** item joins the layout's window that holds its
   kind, behind that window's tabs (`stacked`; the front tab stays the
   layout's). Five terminals into a one-terminal layout: one window, five
   tabs. When several windows hold the kind, it is the largest visible one:
   tiled before floating, a tile's size is its share of the tree, a float's
   is its rect, and a tie goes to the first window. A minimized window never
   takes extras. A placeholder counts as its kind, so extras can stack with
   a window that is still to open.
5. What is left (visible items with no visible window of their kind, say a
   Chat when the layout has none) forms one tiled **Additional windows**
   stack, in inventory order with the first tab active, appended as the
   right child of an H split (`ADDITIONAL_RATIO` of the width) or as the
   root when no saved tiled geometry survived. Unassigned **minimized**
   items stay minimized in their own live stack, with its restore placement
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

`matched`, `substituted`, `opened`, `stacked`, `additional`,
`kept_minimized`, and `unfilled` feed the post-Apply summary. `is_exact` is
the Modified test: true when no extra was stacked or went to Additional
windows and every slot was filled. A stacked extra makes the picker show
Modified because the live windows no longer match the saved layout; saving
then saves the stacks.
Substitution and kept-minimized extras do not make an Apply dirty; pending
`to_open` slots do until `with_opened` resolves them, so call it first.

Switching A → B → A converges: B stacks A's extras with matching windows or
parks them in Additional windows (and leaves A's minimized ones minimized),
and A takes them back by exact id, so nothing opens. The window count only grows to the largest layout used.

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
  "version": 2,
  "layouts": [ /* LayoutDefinition, in creation order */ ],
  "new_project": { /* optional LayoutDefinition copy */ }
}
```

**One list for every Project.** Every Project's menu shows the whole list,
and a name is unique across it (case-insensitive). Selection is not shared:
it stays per Project instance (see **Picker state**). Layouts outlive their
Projects (closing one deletes nothing).

Why one list: a definition is only window kinds and geometry, nothing about
a directory, so a layout saved in one repo is just as useful in the next.
The original spec kept a library per directory; the user reversed that and
asked for one shared list.

**Whose ids.** A definition's item ids are the `layout_id`s of the tabs it
was saved from, and they only mean something in that directory. So `create`
and `update` record `origin = project_key(dir)`, and the picker's Apply
keeps exact-id matching only when `origin` is this Project's key. Otherwise
it renumbers the items first (`detach_ids`, the same step the New Project
layout uses), and slots fill by kind, oldest item first. Without that, a
tab whose id happened to equal one in a layout saved elsewhere (ids are
reminted after a restart) would be pinned to that slot, say into a small
corner pane while a fresh shell took the main one. Rename keeps `origin`;
saving over a layout from another Project moves it there.

`project_key(dir)`: absolute, `.`/`..` resolved, symlinks/junctions/subst
drives/8.3 short names resolved as far down the path as it exists, no
`\\?\` prefix, no trailing separator, lowercase on Windows. So two Project
instances of one directory have one key, while a worktree under
`.foreman/worktrees/` is a different directory. A deleted directory keeps
the key it had, because its parent is still resolved; only a whole missing
drive falls back to the lexical spelling.

Gotcha: resolve the deepest *existing* ancestor, not just the whole path.
Canonicalizing the whole path or nothing made the key flip from
`runneradmin` to `RUNNER~1` the moment a CI temp directory was deleted (the
v0.8.0 release gate).

**Version 1 files.** Version 1 kept one list per directory
(`"projects": { "<key>": { "dir", "layouts" } }`). Reading one migrates it
in memory (`From<LibraryFileV1>`): the lists are joined in key order, ids
are kept (so workspace selections still resolve), each definition's
`origin` becomes its old key, and a name an earlier directory or a built-in
already holds gets the folder name appended: "Coding" from `web` becomes
"Coding (web)", then "Coding (web 2)". The file keeps the v1 shape until the
next write, so an older Foreman can still use it until then; after that it
sees version 2 and refuses to touch it (`FutureVersion`).

**How to use it.** `LayoutStore::user()` (or `LayoutStore::at(dir)` in tests)
then, per the picker contract:

| Picker action | Call |
|---|---|
| Save with nothing selected, Save As | `create(project, name, items, arrangement, OnConflict::Reject)` |
| …user picked "Replace" on a duplicate | same with `OnConflict::Replace`: overwrites the other definition in place, keeping its id |
| Save with a layout selected | `update(project, id, items, arrangement)` |
| Rename… | `rename(id, name, on_conflict)`; Replace deletes the other holder of the name |
| Delete… | `delete(id)`; already gone is `Ok` |
| Use for new Projects / reset | `set_new_project_layout(Some(&def))` / `(None)` |
| Menu contents | `builtin_layouts()`, then `layouts()`; `new_project_layout()` |
| Look up an id or selection | `find_layout(saved, id)`, `resolve_selection(saved, selected)`: built-ins first, then `saved` |

`project` is the directory the windows are captured from; it only sets
`origin`. Names are trimmed and checked (`validate_name`), duplicates
compare case-insensitively across the whole list, and the arrangement is
validated before anything is read or written. `StoreError` says why a call
failed and its `Display` is the user-facing message: `NameTaken` (offer
Replace or another name), `ReservedName` (a built-in's name: no Replace,
type another), `ReadOnly` (a built-in's id), `NotFound` (deleted elsewhere:
show Custom), `Invalid`, `FutureVersion`, `Io`. A call that returns `Err`
wrote nothing, so the UI reports success only on `Ok`.

### Built-in layouts

`builtin_layouts()` (`src/layout_library.rs`) defines the few that ship with
Foreman, in code: terminals side by side, a main terminal with a stack
beside it, a grid, and a terminal with the chat. Read the function for the
current set.

- Every Project lists them first, under **Built-in**. They are never in the
  file, so a new Foreman can change or add them.
- Read-only: no Rename or Delete in the menu, and the store refuses
  (`ReadOnly`). Save with one selected opens Save As, prefilled
  "<name> copy". Their names are reserved (`ReservedName`).
- Their ids start `builtin:` (`is_builtin`); saved ids are hex, so the two
  never collide. A Project can select one, and the workspace saves that
  selection like any other.
- No `origin`, so Apply always fills them by kind. A slot the Project has
  nothing for opens a plain shell (or the chat), like any layout's missing
  windows. Applying the grid to a Project with one terminal opens three
  shells.
- **Use for new Projects** works on them too.

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
matches by id and anything the Apply opened stacks with a window of its
kind (else lands in Additional windows). It opens nothing. It restores the old selection and Modified value, then the
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
with the same tooltips, so the title keeps room. Under `LAYOUT_CTL_MIN_W`
(140px) there is no room at all and they are not drawn. A zoomed Project keeps its
header, so the controls stay. They act on the stack's active Project tab,
the one the header draws. Clicking them in an unfocused Project focuses it
first. Child windows get no picker.

**The label** is the selected layout's name, or **Custom**. An amber dot
after it means Modified.

**The menu** has two groups: **Built-in**, then **Saved**. Each row has a
small diagram, the name, a check on the selected one, a "Modified" tag on it
when set, and icons: Use for new Projects (a window with a plus) on every
row, Rename… and Delete… on saved rows only.

**A long list scrolls.** The two groups sit in a scroll area that stops short
of the window's bottom edge (never shorter than three rows); everything from
Create Layout… down stays fixed below it. The menu opens already scrolled to
the selected layout. Rows have fixed heights, so `selected_row_center`
computes the offset and hands it to the scroll area before the first visible
frame. egui's `scroll_to_rect` lands two frames late, so the menu would show
the top and then jump. The bar uses the theme's `scroll_thumb` token and rests
at `THUMB_DIM_FLOOR`, like the Sessions panel's and the terminals' thumbs,
instead of egui's text-colored bar that hides until hovered.

Gotcha: egui measures a new Area in an invisible first frame, placed as if it
were 600px wide, so the menu's rect is wrong for that one frame. A test that
reads the menu's position must run one more frame first. The reveal is kept
until a frame that is actually drawn (`show_layout_menu` returns whether it
was).
Below: **Save** (its hint says whether it updates a layout, saves a copy of
a built-in, or asks for a name), **Save As…**, and **Revert Layout** once an
Apply has happened (shown disabled, with the reason, after an edit retired
the revert point). With nothing saved, the saved group says to arrange the
windows and Save. A read error shows there too; the built-ins still list.
**Create Layout…** sits between the list and Save (see below).

The diagram is drawn from arrangement data only (`diagram_cells`): tiled
windows by tree and ratios, floating ones outlined on top, minimized ones
not drawn, and a one-letter kind mark where it fits (`>` terminal, `C` chat,
`B` board, …) so a mixed layout reads at a glance.

**Selecting a row applies it.** It queues `LayoutRequest::Apply { select:
true }` on the Project being drawn, with the ids detached unless the layout
was saved from this directory (**Whose ids**); the next frame runs it. The header then
reads `take_layout_outcome`. An exact Apply says nothing. Otherwise a
dismissible summary sits under the picker, built from the applied
`Reconciliation` (`summary_lines`): which windows stood in for saved ones,
what opened, what stacked with matching windows ("Stacked with matching
windows: beta, gamma"), what moved to Additional windows, which slots could
not be
filled, and which opens failed (with the error, not shortened). It has a
**Revert Layout** button. A failed Apply or Revert shows its error instead.

**Save and friends.**

| Action | What happens |
|---|---|
| Save, Custom | Name dialog, then `create(…, Reject)`; selects the new layout |
| Save, a layout selected | `update` in place, no prompt; clears Modified |
| Save, a built-in selected | The Save As dialog, prefilled `<name> copy`; the built-in never changes |
| Save, selected layout deleted elsewhere | Picker goes to Custom and the name dialog opens with "no longer exists" |
| Save As… | Name dialog prefilled `<name> copy`; always a new definition |
| Duplicate name (any case) | Dialog stays; "already exists — replace it, or type another name". Replace overwrites it in place, keeping its id, and selects it. Typing clears the offer |
| A built-in's name (any case) | Dialog stays with the error; there is no Replace |
| Rename… | Dialog prefilled; Replace deletes the other holder of the name, and if that was selected the picker goes to Custom |
| Delete… | Confirm dialog; the windows stay put; if it was selected, Custom |
| Cancel, Escape, or a click outside | Nothing changes, on disk or on screen |
| A write fails | The dialog stays with the error and the typed name; selection and Modified are untouched |

Success is reported (a short "Saved …" note) only after the write returned
`Ok`.

### The New Project layout

**What.** One global slot in `project-layouts.json` (`new_project`). When
it is set, a Project opened from a directory starts with that arrangement
instead of one terminal. Unset (the default, shown as **Single terminal**)
is exactly the old behavior.

**Set it** from any Project's Layout menu: the window-with-a-plus icon on a
layout's row, built-in or saved (**Use for new Projects**). The icon turns amber on the
row the slot still matches. **See and reset it** in Settings → Window
Manager → New Project layout: the value is the layout's name or Single
terminal, and **Reset** writes Single terminal back. Both go through
`set_new_project_layout`, so they re-read, change only the slot, and write.

**It is a copy.** Setting it copies the whole definition into the slot.
Saving over, renaming, or deleting the source layout later leaves the slot
as it was (and the source row loses its amber mark once they differ).

**What opening does.** `add_project` and `add_project_with_command` (the
directory picker, the landing page, the startup auto-project when there is
no workspace to restore) spawn the first terminal as before, then
`queue_new_project_layout` reads the slot once and queues
`LayoutRequest::NewProject` on the new Project. Its first frame runs the
normal Apply: Filling missing slots, so the first terminal takes the first
terminal slot (a commanded agent terminal included), fillable slots open a
plain shell or view, and target kinds (Image, GitDiff, Project) stay empty.
Then, unlike a picker Apply:

- the picker shows **Custom** (the slot is a copy, not one of the listed
  layouts, even when it was copied from one);
- there is no revert point: the lone terminal it replaced was never on
  screen;
- a Project not yet laid out, or a dialog open, makes it wait for a later
  frame instead of failing.

A non-exact result shows the usual summary, titled "Opened with “name”,
with changes".

**Startup restore never applies it.** `apply_workspace` rebuilds Projects
from the snapshot and never calls `add_project`.

Gotchas:

- The slot's item ids are renumbered (`detach_ids`) before reconcile.
  They were minted in another Project, often another launch, and ids
  restart at 1 each launch, so the new terminal could share an id with a
  later slot and be pinned there by exact match. With no exact matches,
  slots fill by kind in order.
- An unreadable slot (future version, I/O error) opens one terminal and
  toasts why. A malformed file goes through the shared recovery like any
  library read.
- Settings reads the slot when the pane is drawn first, when a pane is
  selected, and when the Settings window becomes active again, never per
  frame. So a slot set from a picker while Settings sits open shows up the
  next time you click into Settings.

**Disk.** `LayoutStore` re-reads the file on every call, so the picker keeps
a copy: read once when the header first draws (the label needs the name),
again when the menu opens, and after each write. Never per frame. Any other
Project sees a save the next time its menu opens; its windows never move,
and a Project that has the saved-over layout selected keeps its own
Modified flag (Modified is an edit event, not a comparison). The built-ins
need no read.

**Keyboard.** While the menu or a dialog is open, it owns the keys: that
Project's terminals are frozen (`live` is false, `terminal_owns_keyboard`
says no), and the desktop's leader and close funnels stand down
(`any_layout_capture`). Escape closes or cancels. Enter submits a name, but
never confirms Delete or Replace; those need a click. Both keys are
consumed. Popups belong to the focused Project: when focus leaves it (a
click, focus-follows-mouse), or it is not drawn this frame (minimized, or a
background tab), its menu closes and its dialog is canceled. So an Enter
typed into another Project can never confirm this one's dialog, and no
invisible dialog keeps terminals frozen.

**Testing it.** A click becomes a `PickerAction`, and `run_layout_action`
does the work, so tests drive the bindings without a pointer.
`layout_ui.store` points a test at a temp library; without it, tests read a
never-written temp path, never the user's file. To screenshot the open menu
without clicking, launch a debug build with `FOREMAN_LAYOUT_MENU_TEST=1`
(and `FOREMAN_NO_LANDING=1`, so a Project opens): it opens the focused
Project's menu at startup (`debug_open_layout_menu`).

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

### Create Layout… (`src/wm/layout_create.rs`, `src/layout_prompt.rs`)

**What.** Describe an arrangement in words; one provider call proposes one
**candidate** for the windows that are open, plus any new windows the
description needs ("chat on the right" with no chat open, "like Rider");
you Apply it, Save & Apply it, or Cancel. Single-shot: no conversation. To
change it, edit the description and press **run again**; the new candidate
replaces the old.

**New windows open on Apply, never from the reply.** The reply only
declares them; the preview draws them with an accent border, labels them
"New terminal", "New chat", …, and says "Apply opens 2 new windows:
Terminal, Git changes". Apply (or Save & Apply, or picking the saved layout
later) opens them the way a saved layout opens its missing windows: the
default shell in the Project directory with no command, or an empty
viewer. No agent starts and nothing runs.

**Where.** Layout ▾ → **Create Layout…** opens an editor centered on the
Project. It shows: what is sent and to whom ("Sent to Codex (Settings:
session-title provider): your description and the window labels below"),
the list of windows with their icons, labels, and kinds, the description
box, **run** (Ctrl+Enter), and once a reply is in, the provider's one-line
message and a diagram of the candidate with each window's icon and label
(stacked tabs listed under the active one, minimized windows listed below
the diagram, new windows marked as above). Then **cancel**, **apply**,
**save & apply…**. A reply that would leave the windows exactly as they are
is not offered: the editor says "That is already how the windows are
arranged" instead of enabling Apply.

**What the provider gets** (`Inventory::wire_json`), and nothing else:

```json
{"area": {"w": 1200, "h": 800},
 "items": [{"id": 1, "kind": "terminal", "label": "claude: auth refactor"}, …],
 "windows": [{"place": "tiled", "tabs": [1]}, {"place": "minimized", "tabs": [4]}, …]}
```

plus the description as one JSON string. Ids are 1..n, never real layout
ids. Labels are tab titles with control characters removed, cut to 60
characters. No terminal output, chat, files, image bytes, or other
Projects. The rules ride as the system prompt (Codex: prepended), the data
after them. The run goes through `ai_oneshot` (no tools, scrubbed env,
scratch dir `%APPDATA%\foreman\layout-create`), 60 s deadline, 32 KB reply
cap, a 48 KB request cap, and at most `MAX_IN_FLIGHT` (2) runs alive across
the app. The provider and model are the session-title ones, the same as
commit-message drafting.

**What the provider may return** (`parse_reply`, strict):

- `{"message", "new"?: [{"id", "kind"}], "windows": [{"tabs", "active"?,
  "place", "rect"?}], "tree"?}` where `place` is tiled / floating /
  minimized and the tree is `{"window": i}` or `{"split": "row"|"column",
  "sizes", "children"}`; or
- `{"message", "missing": [kinds]}` when the description needs an image,
  diff, or Project window that is not open. Those need a target, so they
  can't be new. The editor says what to open ("This needs windows that are
  not open: Image. Open them, then run again.") and shows the provider's
  message.

`new` (`new_items`): at most `MAX_NEW_ITEMS` (6); kinds terminal, chat,
board, plan, git_history, git_changes only; a viewer (chat, board, plan,
git history, git changes) only when the Project has none, and once; a wire
id that is not listed and not repeated; tiled or floating, never
minimized (a minimized slot never opens). Each gets a real id past the
highest live id.

Unknown fields anywhere, an unknown or repeated id, an omitted item, a rect
on a tiled window or none on a floating one, rect values outside 0..1,
non-finite or non-positive sizes, a tree deeper than `MAX_TREE_DEPTH`, and
markdown fences are all errors. The arrangement then goes through the same
`validate` as a saved layout, against exactly the inventory plus the new
windows. So a candidate moves what exists and adds only what it declared.

**Staleness.** Each manager keeps an arrangement **revision**
(`layout_revision`). The edit watch bumps it on every structural edit, and
every Apply and Revert bumps it. A request and its candidate remember the
revision they started at. Each frame the editor is open, a mismatch drops
them and shows "Windows changed — run again". A resize does not bump it:
the candidate's geometry is normalized and gets refit at Apply.

**Supersede and cancel.** One request per Project instance. Run again drops
the old receiver, so its reply has nowhere to land, and clears the
candidate at once. Closing the editor (Cancel, Escape, opening the menu,
the header's Save / Save As), switching away from the Project (focus
leaves it, it is minimized or a background tab, its header gets too narrow
for the layout controls), or closing it drops the editor and its request.
Nothing resumes: reopening starts empty. A click elsewhere in the same
Project does not close the editor, because a request may still be running.
While open it owns the keyboard like the other dialogs: Escape backs out of
the name row, then cancels; Enter submits only from the name field;
Ctrl+Enter runs, except while a name is being typed.

Dropping a request does not kill the provider: the CLI runs until it
answers or hits the 60 s deadline, holding one of the `MAX_IN_FLIGHT`
slots. A run that finds both slots taken says so and keeps the candidate
it already has (the slot is taken before anything is cleared).

**Apply** checks the candidate against the live windows now
(`check_candidate`: same revision, every live item matched by id, and the
windows reconcile would open are exactly the candidate's `added`), then
queues `LayoutRequest::Candidate { added, select: false }`. The drain
re-checks and runs the normal Apply in **strict** form: a stand-in, an
extra, an empty slot, or any open besides `added` (say a window the
candidate placed has since closed) is `LayoutApplyError::Stale` and
nothing moves or opens. The new windows open first, then everything moves.
The picker shows Custom; the revert point is set; a short "Applied. It
stays Custom until you save it." If a new window fails to open, the notice
is the usual summary ("Applied with changes", with what was left out).
Revert puts the old arrangement back; windows Apply opened stay open,
stacked with a window of their kind, as with any Apply.

**Save & Apply…** asks for a name in the editor. On submit it checks the
candidate (stale → dropped, nothing written), then writes it with
`LayoutStore::create` (the new windows are saved as ordinary items), and
only after `Ok` queues the strict Apply with `select: true`. Picking that
layout later opens whichever of its windows are missing, like any saved
layout. A duplicate name offers **replace & apply**; any other
write failure shows the error and keeps the editor, the candidate, and the
windows as they were. If the windows change in the gap between the write
and the next frame, the Apply refuses and the notice says the layout was
saved and can be chosen from Layout ▾.

Gotchas:

- The tests never spawn a provider. In test builds the request stores its
  sender on the editor (`test_tx`), so a test can answer late, answer a
  superseded request, or drop the worker. `generate` itself is only run by
  hand (screenshots of a real Codex reply are the evidence).
- Grok takes its prompt on argv, so a Grok reached only through a `.cmd`
  shim refuses the JSON prompt (see `docs/ai-oneshot.md`).
- The revision misses an edit made in the very frame the Project resized
  (the watch re-measures instead of comparing). The strict Apply still
  refuses any inventory change, so the worst case is a regroup the
  candidate did not see being rearranged, never a window lost or an
  undeclared one opened.
- Foreman has no editor or file-tree window. "Like Rider" or "like VS
  Code" maps onto terminals and git viewers as the provider sees fit (in
  testing: new terminals for the "project pane" and "editor").

## Key files

- `src/project_layout.rs` — the model, validation, reconciliation, comparison,
  geometry adaptation, and their tests
- `src/layout_library.rs` — `project-layouts.json`: `LayoutStore`, the
  version 1 migration (`LibraryFileV1`), `builtin_layouts`, `project_key`,
  `find_layout`, `resolve_selection`, and their tests
- `src/config.rs` — `load_json_from` / `save_json_in`, the shared recovery
  and atomic write the library rides on
- `src/wm/layouts.rs` — Apply, Revert, the `NewProject` and strict
  `Candidate` requests, the arrangement revision,
  `rearrange`, `capture_arrangement`,
  the Modified edit watch, and their manager tests (including real-shell
  tests that drive Sessions through `send` / `snapshot` across Apply and
  Revert, directly and through the picker, and a `show`-level test of the
  drain and end-of-frame watch hooks)
- `src/layout_prompt.rs` — Create's provider boundary: `Inventory` (what
  is sent), `build_prompt`, the strict `parse_reply`, `generate`, the
  in-flight cap, and their tests
- `src/wm/layout_create.rs` — the Create editor: request, supersede, stale
  checks, Apply / Save & Apply, the labeled preview, and their tests
- `src/wm/layout_picker.rs` — the header picker, menu, dialogs, summary,
  `PickerAction` / `run_layout_action`, `summary_lines`, `diagram_cells`,
  `queue_new_project_layout`, and their tests (including real-frame clicks
  on the header buttons)
- `src/settings_menu.rs` — the New Project layout row (value + Reset)
- `src/wm.rs` — `add_project` / `add_project_with_command` queueing the New
  Project layout; `Tab::layout_id`, `Content::layout_kind`,
  `WindowManager::layout_inventory`, the `layout` selection field, and
  capture/restore of both; `spawn_shell_tab` and `view_tab`, the open paths
  Apply shares; the `show` hooks; `header_layout` placing the picker and
  Save (`LayoutCtlLayout`), and the header draw calling
  `show_layout_controls`
- `src/workspace.rs` — `TabSnap::layout_id`, `ManagerSnap::layout`
- `docs/workspace-persistence.md` — the snapshot it rides on
- `docs/tiling-tree.md` — the live tree an `Arrangement` mirrors
