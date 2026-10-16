# Project layouts

Named arrangements of a Project's child windows. Spec (authoritative on the
product contract):
`docs/superpowers/specs/2026-10-16-project-layouts-design.md`.

This doc tracks what has shipped. Status: **model and identity wave only**.
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

Fits a definition to the current inventory (`WindowManager::layout_inventory`:
tiled stacks in tree order, then floating back→front, then minimized
back→front; tabs in stack order). Deterministic, total, and never touches the
live manager. The result is validated against the inventory before it is
returned, so a `Reconciliation` is always applicable.

1. Exact id + kind matches first.
2. Each remaining saved item, in saved order, takes the lowest-id unassigned
   current item of the same kind. Kinds never cross.
3. Unmatched saved items are omitted: tabs leave their stacks, empty stacks
   go, single-child splits collapse, surviving ratios renormalize. A stack
   whose active tab vanished keeps the same slot, clamped.
4. Unmatched current items form one tiled **Additional windows** stack, in
   inventory order with the first tab active, appended as the right child of
   an H split (`ADDITIONAL_RATIO` of the width) or as the root when no saved
   tiled geometry survived.

`Reconciliation::is_exact` is the picker's "apply without a preview" test.
`matched`, `substituted`, `missing`, and `additional` feed the preview.

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
- Capturing a live manager into an `Arrangement` and applying one through the
  deferred mutation boundary; Revert (Apply wave).
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
