# Project layouts — design

## Intent

Give each Project a Unity-style layout picker: choose a named arrangement,
save the current arrangement, or create one by describing it. Layouts cover
every Win and tab inside that Project, regardless of Content type.

This is an arrangement feature. Applying a layout moves existing Content
without restarting Sessions, clearing viewers, or closing work. Windows the
layout has no slot for are gathered into an **Additional windows** stack, never
closed. A slot with no matching
window reuses an existing window of the same kind first, minimized ones
included; only then does Foreman open a plain new one (see **Filling missing
slots**). The desktop arrangement and other Projects remain untouched.

The Unity reference describes the interaction: a compact named-layout picker
with save and create actions. It does not require copying Unity's appearance.
Use Foreman's native controls and visual language.

## Agreed product contract

- Layouts belong to a Project, not to a global preset library. The one
  exception is a single **New Project layout** slot (see **New Projects**).
- The picker operates on the active Project.
- Every child Win participates, including viewers, tab stacks, floating Wins,
  and minimized Wins. This is not a terminal-only feature.
- **Save** captures the current arrangement without requiring a prompt.
- **Create…** accepts a natural-language description and shows a proposed
  arrangement before applying it.
- Applying an arrangement preserves all existing Content and live Sessions.
- Applying never closes anything. Unused windows go to an Additional windows
  stack; missing windows are taken from existing ones first, then opened fresh
  where that is safe.
- New Projects open with a chosen layout. The default is a single terminal,
  which is today's behavior.

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

Modified is an event flag, not a comparison. Structural actions set it; Save
and a successful Apply clear it; Revert restores its previous value. Do not
derive it by comparing the live arrangement with the saved definition.
Floating windows keep pixel rects when the Project resizes, and Apply confines
geometry to the current area, so a comparison would report Modified after a
resize or immediately after Apply. `same_arrangement` distinguishes a
structural edit from a no-op around a single action; it does not compute the
flag.

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

A proposal arranges the windows that exist. Unlike a saved layout, it never
adds slots for Content that is not open. If the prompt needs a missing window,
explain what to open before retrying. Provider output therefore cannot start
processes. A candidate saved with Save & Apply becomes an ordinary definition
and follows **Filling missing slots** when it is applied later.

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

Selecting a saved layout applies it immediately. There is no reconciliation
preview. Apply never closes anything, so a wrong guess costs one Revert, not
work. After an Apply that was not an exact match, show a short, dismissible
summary near the picker: substituted items, opened items, items moved to
Additional windows, and slots that could not be filled. Revert stays one click away.

### Stable identity and reconciliation

Each Content-bearing tab needs a persistent layout-item identifier, independent
of its title, runtime Win id, terminal Member id, and containing tab stack.
Preserve that identifier through tab moves and workspace cold restore.
Do not reuse Member ids or change Control-plane identity to implement layouts.

Match saved items by this identifier first. Then give each remaining saved
slot an unassigned current item of the same Content kind, in stable creation
order. Minimized current items are available and are restored into their slot.
Fill tiled and floating slots before minimized ones, so a minimized slot never
takes the window a visible slot needed. Never substitute a terminal for a
viewer or retarget a viewer to imitate a missing item. List substitutions in
the summary.

A reopened Project starts with one fresh terminal, and its item ids never match
a definition saved earlier. Substitution and the two rules below are the
normal path, not an edge case.

### Filling missing slots

A tiled or floating slot with no compatible current item opens a fresh item
only when its kind can be opened without a target. That means a terminal
running the default shell in the Project directory, or a Project view that
takes no argument. Expected kinds: Chat, Board, Plan, Git history, Git changes.
The Apply wave confirms the list against the existing open paths and records it
in the feature doc. A fresh terminal never runs a startup command and never
starts an agent, even when the saved slot held one.

Omit the slots that cannot be filled this way: kinds that need a target (Image,
Git diff, nested Project) and every minimized slot. Remove empty stacks and
collapse empty branches; normalize surviving split ratios.

### Unused windows

Never close a current item the layout has no slot for. Put every such item
that was visible before the Apply into one **Additional windows** tab stack,
appended as a tiled region on the right. Preserve their prior relative order
and choose a valid active tab. This is an ordinary Win with an ordinary tab
stack, not a third window state. If no saved geometry is left, Additional
windows fills the Project. An unused item that was already minimized stays
minimized, so a switch never brings back windows the user put away.

Switching A → B → A therefore converges. B moves A's extras into Additional
windows; A takes those same windows back instead of opening new ones. The
window count only grows to the largest layout used.

If there are no current items and nothing can be opened, applying produces an
empty Project. Applying never silently overwrites the definition. An Apply is
clean only when nothing went to Additional windows and every slot was filled.
Otherwise it marks the selected layout Modified, because the live arrangement
differs from what the definition describes.

Names and labels help the user recognize items, but are not matching keys.
Generated terminal renames must not change layout membership.

### Apply invariants

- Build and validate a complete arrangement before mutating the manager.
- Move existing Content objects; do not reconstruct them from workspace snapshots.
  New Content comes only from **Filling missing slots**, through the spawn and
  open paths the Project already uses.
- Open fresh items before the rearrangement step, so that step only moves
  Content. A failed open omits its slot and appears in the summary; it does not
  abort the Apply.
- Preserve Session processes, terminal grids and scrollback, chat membership,
  viewer targets and view state, and agent title/state ownership.
- Keep every current item exactly once. Never close Content. Keep every tab
  stack nonempty.
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

Items opened by the Apply stay open after Revert. The previous arrangement has
no slot for them, so they go to Additional windows like any other unused
window. Opening them is part of the Apply and does not invalidate the revert
point.

## New Projects

A new Project, opened from a directory rather than restored at startup, opens
with the **New Project layout**. When it is unset, a new Project opens with
one tiled terminal. That is today's behavior and needs no layout.

Set it from the picker with **Use for new Projects** on a saved layout. Settings
shows its name and offers a reset to **Single terminal**. Setting it copies the
definition into one global slot in `project-layouts.json`. It is not a
reference into the source Project's library, so later edits to the source
layout, or deleting it, leave the slot unchanged. This is one slot, not a
global library.

Opening a Project applies the slot with the normal rules. The Project's first
terminal takes the first terminal slot; the remaining fillable slots open
fresh. The new Project's picker shows Custom, because the directory's library
does not contain that definition. Startup restore never applies the slot.

## Persistence

Persist the Project-keyed libraries in a versioned user-local
`project-layouts.json` through the shared protected JSON load/save helpers.
Do not write layout libraries into repositories or add them to app Settings.
Keep saved definitions independently of whether their Projects are open.

Every library write re-reads `project-layouts.json`, applies only its own
change, and then writes the result. A change is adding, replacing, renaming, or
deleting one definition by id, or setting the New Project slot. Without this, a
second Foreman process would overwrite definitions it never loaded. The common
case is the installed app running alongside a development build. Use a
temporary file name that is unique per process; the shared helper's fixed
`{file}.tmp` lets two writers trample each other. There is no cross-process
lock. Two writes within the same few milliseconds can still lose one change,
which is acceptable for this data.

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
unknown provider fields and unsupported operations. The Create preview, the
post-Apply summary, and Apply must use the same validated reconciliation
result.

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
5. Missing items, same-kind substitutes (minimized ones included), extra
   items, an empty Project, and an all-minimized result reconcile
   deterministically. Visible extras go to Additional windows and minimized
   extras stay minimized; nothing is closed. Only fillable tiled or floating
   slots open fresh items, and fresh terminals run no command. Applying A, then
   B, then A reuses the windows B set aside and opens nothing.
6. Auto-renaming a terminal does not affect matching or Modified. Structural
   edits affect Modified; focus and viewport resizing do not.
7. Invalid output, omitted or repeated items, excessive trees, non-finite
   geometry, unavailable providers, timeouts, cancellation, stale revisions,
   and Project closure leave live arrangements intact.
8. Old workspace files load without layout fields. Cold restore retains item
   mapping and selection. Corrupt/future library data is not silently replaced.
9. Provider input is limited to the target inventory and description. Provider
   output has no path to shell commands, new Content, or unrelated Project edits.
10. Modified is set only by structural events. A Project resize, and an Apply
    at a different size from the save, both leave it clear.
11. Two processes that save different definitions to one library both keep
    their definitions.
12. A new Project opens with the New Project layout when it is set, and with
    one terminal when it is unset. Startup restore never applies the slot.
    Editing or deleting the source layout does not change the slot.

Implementation validation includes repository-required build and test checks.
Inspect screenshots of the picker, naming dialog, Create preview, post-Apply
summary, Modified state, error state, and a mixed applied layout. Include a
compact Project and a zoomed Project. Use Inspection to verify an existing
Session remains usable after Apply and Revert; screenshots alone do not prove
process preservation. Do not claim GUI correctness without image evidence.

## Explicit exclusions

- Global layout libraries (beyond the single New Project slot), cross-Project
  application, import/export, or sharing.
- Desktop-level Project arrangements or OS window placement.
- Closing Sessions or other Content; opening anything outside **Filling missing
  slots**; dispatching agents or running startup commands.
- Restoring processes or terminal output after restarting Foreman.
- Image backgrounds, animation, or a general-purpose assistant interface.

## Decisions and rejected alternatives

| Decision | Reason and rejected alternative |
|---|---|
| Directory-owned libraries; selection stays instance-local | A Project keeps its layouts after closing and reopening. A second instance of the same directory can reuse definitions without having its live windows moved. Display-name ownership would break on rename; runtime Project ids do not survive restart. |
| Persistent per-item identity, then same-kind reconciliation | Exact identity protects existing grouping across title changes and cold restore. Same-kind substitution makes a saved layout usable after windows change; the reconciliation preview makes those substitutions explicit. Title matching is unstable under agent naming. |
| Apply immediately and show a summary; no reconciliation preview | Apply never closes anything, so no outcome loses work, and Revert undoes any of them. A reopened Project's ids never match, so a preview would appear on almost every switch and defeat a quick picker. Rejected: preview whenever inventory differs (the original design). |
| Saved layouts fill missing slots; provider proposals do not | A reopened Project starts with one fresh terminal, so arrange-only layouts would collapse to one window. Filling target-free slots with plain Content (no commands, no agents) keeps the library usable. Provider output keeps the stricter rule, so model text never starts processes. Rejected: arrange-only everywhere (layouts useless after reopening); full provisioning with commands (a separate feature). |
| Unused visible windows go to an Additional windows stack | Closing would destroy work. A visible stack means nothing quietly disappears after a switch, and the next layout that needs those windows reuses them. Already-minimized extras stay minimized, so a switch never reopens windows the user put away. Rejected: minimizing every extra into its own window (keeps the layout's shape exact, but windows vanish into the Sessions panel). |
| Modified is an event flag | Comparing against the definition drifts with resize and with Apply-time geometry confinement, which would report Modified with no user edit. |
| New Project layout is one copied global slot | A new directory has no library. A reference into another Project's library breaks when that definition is renamed or deleted, and a full global library stays excluded. |
| Manual layouts precede provider integration | Save, reconciliation, Apply, and Revert establish the same machinery Create must use. Building a separate AI-only apply path would duplicate validation and risk divergent behavior. |

Implementation work is tracked as the `project-layouts` kanban plan. Its waves
are sequential: model and identity; reconciliation rework (fill missing slots,
keep minimized extras minimized); persistence; Apply and Revert; picker and Save
UI; New Project layout; promptable Create. The manual-layout milestone ends
after the New Project wave. Each card names its actual blocker and its
validation gates.

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
