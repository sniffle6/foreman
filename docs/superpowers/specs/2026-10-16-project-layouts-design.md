# Project layouts — design

## Intent

Give each Project a Unity-style layout picker: choose a named arrangement,
save the current arrangement, or create one by describing it. Layouts cover
every Win and tab inside that Project, regardless of Content type.

This is an arrangement feature. Applying a layout moves existing Content
without restarting Sessions, clearing viewers, or closing work. The desktop
arrangement and other Projects remain untouched.

The Unity reference describes the interaction: a compact named-layout picker
with save and create actions. It does not require copying Unity's appearance.
Use Foreman's native controls and visual language.

## Agreed product contract

- Layouts belong to a Project, not to a global preset library.
- The picker operates on the active Project.
- Every child Win participates, including viewers, tab stacks, floating Wins,
  and minimized Wins. This is not a terminal-only feature.
- **Save** captures the current arrangement without requiring a prompt.
- **Create…** accepts a natural-language description and shows a proposed
  arrangement before applying it.
- Applying an arrangement preserves all existing Content and live Sessions.

The remaining sections define the proposed behavior needed to implement that
contract. They are design decisions, not claims about existing functionality.

## Ownership and boundaries

A Project's layout library is local user data, keyed by its normalized,
absolute project directory. Use filesystem-aware path equivalence; resolve
aliases when possible without requiring the directory to remain accessible.
Separate worktree directories have separate libraries. Renaming a Project's
display title does not change its library.

Two open Project instances pointing at the same directory share saved layout
definitions. Their current arrangements, selections, previews, and undo state
remain independent. Saving in one instance never rearranges the other.

Closing and reopening a Project retains its library. Moving a project directory
does not automatically migrate the library in this version.

Only the active Project's child Window manager is a target. Project tabs resolve
to their active Project Content. Desktop-level windows, including the Sessions
panel and app settings, are outside the arrangement.

## Picker and Save

Place **Layout ▾** and an adjacent **Save** button in the active Project's
header controls. Keep them accessible when the Project is zoomed or its header
uses a compact presentation. Do not put a separate picker in every child Win.

The picker shows the selected layout name, or **Custom** when none is selected.
Its menu contains this Project's saved layouts and **Create Layout…**. Each
saved entry has a name, a small arrangement diagram, and a selected indicator.
Offer **Save As…**, **Rename…**, and **Delete…** for saved layouts.

Before the first save, the menu explains that layouts are saved for this Project.
Create and Save remain available. With no active Project, disable the controls
and explain that the user must open or select a Project.

### Save behavior

1. If no saved layout is selected, Save asks for a name.
2. If a saved layout is selected, Save updates it from the current arrangement.
3. Save As always asks for a name and creates a separate definition.
4. Trim names and reject empty names or names longer than 80 characters.
5. Compare names case-insensitively within the Project. A duplicate name offers
   explicit replacement or lets the user choose another name.
6. Report success only after the atomic write succeeds. On failure, keep the
   current arrangement and unsaved state and show an actionable error.

Saving includes the tiling tree and ratios, tab grouping and order, floating
geometry and stacking order, and minimized state with its restore placement.
Capture the selected tab within each stack. Store floating positions relative
to the Project's content area so they adapt to different sizes.

Zoom is temporary and is not part of a named layout. Save the underlying
arrangement even while zoomed. Applying a layout exits child-window zoom so
the result is visible. It does not change desktop-level Project zoom.

Manual splits, divider movement, tab regrouping or reordering, floating moves,
and minimize/restore mark the selected layout **Modified**. Adding or closing
Content also marks it Modified. Focus changes, active-tab changes, window
titles, viewer navigation, and Project resizing do not. Active tabs are still
captured when the user explicitly saves.

Saving clears Modified. Deleting the selected definition leaves the live
arrangement in place and changes the picker to Custom. Canceling any dialog
leaves both the library and live arrangement unchanged.

## Create Layout

Create opens a Project-scoped editor with a prompt, the available windows,
and an arrangement diagram. Show Content labels and icons in the diagram;
the geometry alone is insufficient for a mixed terminal/viewer layout.

Example: “Large coding terminal on the left, two other terminals stacked in
the middle, and chat on the right.”

The user can refine the proposal conversationally. Each successful reply
replaces the candidate preview, without moving live windows. Provide:

- **Apply** to use the candidate without creating a saved definition.
- **Save & Apply…** to name, persist, and apply the candidate.
- **Cancel** to discard the editor and candidate.

Apply yields Custom until the user saves it. Save & Apply selects the saved
definition. Editing a proposal does not overwrite an existing saved layout.

For Save & Apply, resolve the name, validate against current Project state,
and persist before changing the live arrangement. A failed save leaves the
editor and candidate available and does not rearrange windows.

### Generation boundary

The model proposes arrangement data only. It receives a bounded inventory of
the target Project's Content: opaque item identifiers, Content kinds, visible
labels, current grouping, and available area. It receives no terminal output,
chat history, image bytes, repository files, or other Projects' inventory.
Explain that the description and visible window labels go to the selected
provider. Use the existing one-shot provider selection pattern and show errors
when the provider is unavailable; manual Save and switching remain available.

Labels and prompt text are untrusted input. Provider output cannot execute
commands, create Content, alter viewer targets, close windows, or change
settings. Reuse the bounded background execution approach behind Theme Expert;
do not block the GUI or run an interactive agent against the repository.

Creation arranges the windows that exist. If the prompt needs a missing window,
explain what to open before retrying. Do not silently launch terminals or open
viewers. This keeps layout creation separate from workspace provisioning.

The candidate must account for every inventory item exactly once, including
inactive tabs and minimized items. The model may regroup, tile, float, or
minimize them. No item may be discarded to satisfy the requested shape.

Capture the target Project identity and arrangement revision for each request.
Structural changes invalidate pending replies and previews. Display a refresh
action rather than applying an outdated proposal. Geometry is recomputed for
the current area after resizing; resize alone need not invalidate a candidate.

Switching Projects hides the editor without retargeting it. Returning to the
original Project can resume it if its revision still matches. Closing that
Project cancels its request. Superseded and canceled replies cannot publish a
preview. Bound request size, response size, conversation history, time, and
concurrency using the existing provider helper; validate output locally.

## Applying saved layouts

Selecting a saved layout applies it immediately when every saved item maps
exactly to the current inventory. When inventory differs, show a reconciliation
preview and require Apply. Switching selection alone never drops Content.

### Stable identity and reconciliation

Each Content-bearing tab needs a persistent layout-item identifier, independent
of its title, runtime Win id, terminal Member id, and containing tab stack.
Preserve that identifier through tab moves and workspace cold restore.
Do not reuse Member ids or change Control-plane identity to implement layouts.

Match saved items by this identifier first. For a saved item that no longer
exists, match an unassigned current item of the same Content kind in stable
creation order. Never substitute a terminal for a viewer or retarget a viewer
to imitate a missing item. Show substitutions in the preview.

If no compatible item exists, omit the missing item. Remove empty stacks and
collapse empty branches; normalize surviving split ratios. A missing item does
not trigger Content creation.

Keep all unmatched current items in an **Additional windows** tab stack appended
as a tiled region on the right. Preserve their prior relative order and choose
a valid active tab. This is an ordinary Win with an ordinary tab stack, not a
third window state. The preview lists these items and their destination.

If there is no saved geometry left, Additional windows fills the Project. If
there are no current items, applying produces an empty Project. Extra or missing
items mark the selected layout Modified; applying never silently overwrites
its definition.

Names and labels help the user recognize items, but are not matching keys.
Generated terminal renames must not change layout membership.

### Apply invariants

- Build and validate a complete arrangement before mutating the manager.
- Move existing Content objects; do not reconstruct them from workspace snapshots.
- Preserve Session processes, terminal grids and scrollback, chat membership,
  viewer targets and view state, and agent title/state ownership.
- Keep every current item exactly once. Keep every tab stack nonempty.
- Keep tiled membership and floating/minimized restore state internally consistent.
- Preserve focus on the previously focused Content when it remains visible;
  otherwise choose the first visible item in arrangement order. An all-minimized
  result has no child keyboard target until a window is restored.
- Confine floating geometry to the Project area and enforce existing size limits.
- Apply through the Window manager's deferred mutation boundary and mark the
  workspace dirty once the transition completes.
- An invalid proposal or failed reconciliation leaves the live arrangement intact.

The underlying state remains tiled or floating. Minimize and zoom retain their
existing semantics; this feature does not add a new window state.

## Revert

After a successful layout switch or candidate Apply, expose **Revert Layout**
in the picker. Retain one in-memory copy of the previous arrangement, containing
identities and geometry rather than duplicate Content or process state.

Revert restores that arrangement, its selected definition, and its Modified
state. It never rewinds terminal output or viewer navigation. It remains valid
across focus changes and Project resizing. A subsequent structural edit or
inventory change invalidates it; disable it with an explanation. Another Apply
replaces the revert point. Revert does not survive restart.

## Persistence

Persist the Project-keyed libraries in a versioned user-local
`project-layouts.json` through the shared protected JSON load/save helpers.
Do not write layout libraries into repositories or add them to app Settings.
Keep saved definitions independently of whether their Projects are open.

Each definition contains an immutable identifier, editable name, item references
and Content kinds, and arrangement data. Store selected-definition identity,
Modified state, and persistent layout-item identities with the Project's
workspace snapshot using additive fields with backward-compatible defaults.

Startup restores the last live arrangement through existing workspace restore;
it does not reapply a named layout on top or dispatch agents. Saved layouts
remain available afterward. If the selected definition was deleted elsewhere,
keep the restored arrangement and display Custom.

Missing library files mean an empty library. Preserve malformed data through
the shared config recovery mechanism before allowing replacement. Unsupported
future schema versions must not be overwritten. Surface save/load failures
without losing live Content or blocking manual window management.

## Architecture and validation

Keep layout definitions, validation, reconciliation, geometry adaptation, and
Modified comparison in a GUI-free module. Keep provider execution separate
from that model. The UI owns prompts and previews; the Window manager owns
Content and applies validated transitions. Do not implement live switching by
calling cold workspace restore: it creates fresh Sessions.

Validate item uniqueness, known references, tree shape, bounded depth and size,
finite coordinates and ratios, valid active tabs, and nonempty stacks. Reject
unknown provider fields and unsupported operations. The preview and Apply must
use the same validated arrangement and reconciliation result.

Reuse the existing recursive Layout tree and manager-local coordinate contract.
Do not build a second compositor or change desktop tabbing rules. Thumbnail
diagrams use arrangement data and do not render terminal contents or allocate
new Sessions. Provider work and disk writes must stay off rendering hot paths.

## Acceptance criteria

Use test-first development for new assertable production logic. Verify these
cases through the pure model and supported manager test seams:

1. Two Projects have separate libraries and independent live arrangements.
   Duplicate instances of one directory share definitions without cross-apply.
2. Save, update, Save As, rename, delete, duplicate-name handling, and write
   failure follow the picker contract. Closing/reopening retains definitions.
3. A mixed arrangement round-trips splits, tabs, floats, stacking order,
   minimized restore placement, and active tabs across different Project sizes.
4. Applying and reverting preserve every Content identity and Session instance.
   Include inactive tabs and multiple viewers of the same kind.
5. Missing items, substitute items, extra items, an empty Project, and an
   all-minimized result reconcile deterministically without process creation.
6. Auto-renaming a terminal does not affect matching or Modified. Structural
   edits affect Modified; focus and viewport resizing do not.
7. Invalid output, omitted or repeated items, excessive trees, non-finite
   geometry, unavailable providers, timeouts, cancellation, stale revisions,
   and Project closure leave live arrangements intact.
8. Old workspace files load without layout fields. Cold restore retains item
   mapping and selection. Corrupt/future library data is not silently replaced.
9. Provider input is limited to the target inventory and description. Provider
   output has no path to shell commands, new Content, or unrelated Project edits.

Implementation validation includes repository-required build and test checks.
Inspect screenshots of the picker, naming dialog, Create preview, reconciliation
preview, Modified state, error state, and a mixed applied layout. Include a
compact Project and a zoomed Project. Use Inspection to verify an existing
Session remains usable after Apply and Revert; screenshots alone do not prove
process preservation. Do not claim GUI correctness without image evidence.

## Explicit exclusions

- Global layout libraries, cross-Project application, import/export, or sharing.
- Desktop-level Project arrangements or OS window placement.
- Creating/closing Sessions, dispatching agents, or running startup commands.
- Restoring processes or terminal output after restarting Foreman.
- Image backgrounds, animation, or a general-purpose assistant interface.

## Decisions and rejected alternatives

| Decision | Reason and rejected alternative |
|---|---|
| Directory-owned libraries; selection stays instance-local | A Project keeps its layouts after closing and reopening. A second instance of the same directory can reuse definitions without having its live windows moved. Display-name ownership would break on rename; runtime Project ids do not survive restart. |
| Persistent per-item identity, then same-kind reconciliation | Exact identity protects existing grouping across title changes and cold restore. Same-kind substitution makes a saved layout usable after windows change; the reconciliation preview makes those substitutions explicit. Title matching is unstable under agent naming. |
| Reconciliation requires preview when inventory differs | Applying an exact match remains a fast picker action. Substitution, omissions, and Additional windows need a visible explanation. Silent best-effort matching would hide consequential placement changes. |
| Creation only arranges existing Content | The feature stays about layouts and cannot accidentally launch processes. Workspace provisioning and missing-window creation require a separate behavior contract. |
| Manual layouts precede provider integration | Save, reconciliation, Apply, and Revert establish the same machinery Create must use. Building a separate AI-only apply path would duplicate validation and risk divergent behavior. |

Implementation work is tracked as the `project-layouts` kanban plan. Its waves
are sequential: model and identity; persistence; Apply and Revert; picker and
Save UI; promptable Create. The manual-layout milestone ends after the picker
wave. Each card names its actual blocker and its validation gates.

Before dispatching isolated workers, commit this spec so their checkouts can
read the same contract. Creating the plan does not authorize starting workers
or committing the spec.

## Key files and related designs

- `src/wm.rs` — `WindowManager`, `Content`, `Act`, `capture_workspace`,
  `apply_workspace`: ownership, deferred application, and cold-restore boundary.
- `src/layout.rs` — `LayoutTree`: existing recursive tiling geometry.
- `src/workspace.rs` — `WorkspaceSnapshot`, `ManagerSnap`, `TabSnap`:
  workspace persistence and additive identity metadata.
- `src/config.rs` — `load_json`, `save_json`: protected user-local persistence.
- `src/theme_expert.rs` — `ThemeExpert`, `parse_reply`: precedent for bounded
  provider conversations with locally validated proposals.
- `src/ai_oneshot.rs` — shared provider execution boundary.
- `docs/tiling-tree.md` and `docs/workspace-persistence.md` — existing layout
  and cold-restore constraints.
