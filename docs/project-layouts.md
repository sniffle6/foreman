# Project layouts

Named arrangements of a Project's child windows. Spec (authoritative on the
product contract):
`docs/superpowers/specs/2026-10-16-project-layouts-design.md`.

This doc tracks what has shipped. Status: **model, identity, and
reconciliation (fill missing slots) only**.
No picker, no persistence file, no Apply/Revert, no provider integration yet;
those are later cards of the `project-layouts` plan.

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
   same kind. Minimized current items count, and a substitute takes the
   slot's state (a minimized terminal can land in a tiled slot). Tiled and
   floating slots pick first, then minimized slots, so a minimized slot never
   takes the window a visible slot needed. Kinds never cross.
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

Which kinds open fresh, checked against the `wm.rs` open paths:

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

## Not yet built

- `project-layouts.json` library and the `config::load_json` / `save_json`
  wiring (persistence wave).
- Capturing a live manager into an `Arrangement` (the `current` argument
  `reconcile` needs), opening the `to_open` slots, and applying the result
  through the deferred mutation boundary; Revert (Apply wave). The existing
  `open_*_window` helpers surface an existing viewer and float the new one
  with `next_slot`, so Apply needs an open path that only creates the Content.
- The **Layout ▾** / **Save** controls and dialogs (picker wave).
- Create Layout… and the provider boundary (promptable Create wave).

## Key files

- `src/project_layout.rs` — the model, validation, reconciliation, comparison,
  geometry adaptation, and their tests
- `src/wm.rs` — `Tab::layout_id`, `Content::layout_kind`,
  `WindowManager::layout_inventory`, capture/restore of the id
- `src/workspace.rs` — `TabSnap::layout_id`
- `docs/workspace-persistence.md` — the snapshot it rides on
- `docs/tiling-tree.md` — the live tree an `Arrangement` mirrors
