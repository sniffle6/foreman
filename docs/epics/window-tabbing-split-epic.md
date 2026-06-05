# Epic — Window Tabbing + Split (floating model)

**Status:** designed, not started. Builds on the keyboard-control epic (leader +
data-driven keymap already shipped). Phases are independent sessions, in order:
**tabs → split → rebind** (split needs the tab primitive; the binding rework is a
cheap data-driven consolidation done last).

**Read first:** `docs/HANDOFF.md`, then `docs/epics/keyboard-control-epic.md`
(the keymap/leader system this extends), then this file. Each phase below is written
to be picked up cold.

---

## 1. Why + the model (settled with the user)

Foreman keeps **floating windows** — z-order, click-to-raise, overlap. The user
explicitly chose this over pure tiling: being able to stack a window on top and
re-order by clicking is worth more than tmux/Warp-style forced tiling. We are **not**
building a BSP tile tree.

Density beyond floating comes from two existing/owed primitives composed cleanly:

- **Snap (zones).** The current `Zone` + shared-divider system (`wm.rs` `zone_rect`,
  `split: Vec2`) is unchanged — throw a window to a half/quarter/max. One level deep,
  on purpose.
- **Tabs (new, generic).** Tabbing is a property of a *window*, **restricted by level,
  not by zone**: any window can be tabbed onto any other window *in the same
  `WindowManager`*. Because the compositor is recursive, that means **projects tab with
  projects** (desktop manager) and **terminals tab with terminals** (a project's child
  manager) — and a terminal can never tab onto a project, because they don't share a
  manager. That restriction is **structural, not enforced code**. Tabs are decoupled
  from zones: a tab-stack can float or be snapped anywhere. A stack of one tab is just
  a normal window. Drag a tab out → it untabs into its own floating window. (This is the
  HANDOFF backlog item "tab-merge windows (drop one window onto another)", generalized.)

**Split** ties them together: create a new terminal, snap it to the pointed zone, and
if that zone is already occupied, **tab** the newcomer onto the occupant.

The trade we accept (eyes open): layouts are limited to what the zone grid expresses
(halves/quarters/max) plus tab-stacks — **not** arbitrary tiling (no 70/30, no
recursive subdivision). For AI sessions (a couple of zones + stack the rest as tabs)
this is enough; the ceiling is a deliberate choice.

**Correctness rule that must hold throughout:** only the *active* tab reads the
keyboard and renders. Inactive tabs keep their PTY alive — the `Session` reader thread
runs independently of rendering (see HANDOFF "DSR trap"/pump), so an inactive terminal
keeps producing output and just isn't drawn or fed input. Same for an inactive project
tab and its whole child manager.

---

## 2. Final binding scheme (reached after the rebind phase)

All after the `Ctrl+b` leader. **Ctrl = project level.**

| Action | Terminal | Project |
|---|---|---|
| Focus (move highlight) | `←↓↑→` | `Ctrl+←↓↑→` |
| Snap (move window to zone) | `WASD` | `Ctrl+WASD` |
| Split (new terminal → zone, tab on collision) | `Alt+WASD` | — |
| Tab cycle / last-focus toggle | `Tab` / `Shift+Tab` | `Ctrl+Tab` |
| Zoom, close, rename, new, settings, help | unchanged from keyboard-control epic | |

`Tab` is dual-purpose: if the focused window is a tab-stack, it cycles tabs; otherwise
it toggles to the last-focused window (the current Phase-1-keyboard behavior). `WASD`
maps W=up, A=left, S=down, D=right. `h/j/k/l` focus is **dropped** (it was the part the
user found confusing); re-addable via the settings editor.

Tab-merge and untab are **mouse drag** operations (drop a window on another to tab;
drag a tab off the bar to float). No keybind for merge in v1.

---

## 3. Shared architecture context (all phases)

Recursive compositor: one `WindowManager` per level; `Content::Project(Box<WindowManager>)`
nests another. Focus cascades via `show(.., active, ..)`. Rects are **local**. See the
keyboard-control epic §3 for the leader/keymap anchors — this epic adds commands to that
same `Command` enum + default `Keymap` (`src/keymap.rs`) and `dispatch` (`src/wm.rs`).

Code anchors (line numbers drift — search if stale):

- **`src/wm.rs:184` `enum Content`** — `Terminal(Session)` | `Project(Box<WindowManager>)`,
  with `Content::show` dispatching the render. Tabbing changes how a `Win` holds content,
  not this enum.
- **`src/wm.rs:189` `struct Win`** — `{ id, title, rect, z, minimized, snap, prev,
  content }`. **The tabbing change lives here:** replace `content: Content` with a stack
  (e.g. `tabs: Vec<Tab>` + `active: usize`, `Tab { title, content }`). A len-1 stack must
  render byte-identically to today (no tab bar).
- **`src/wm.rs` titlebar render (~`477`–`610`)** — title text, `is_renaming` field, the
  project shell chips, window controls. The **tab bar** is drawn here when `tabs.len() > 1`.
  `TITLE_H = 26.0`.
- **`src/wm.rs` drag logic (~`375`–`475`)** — title-drag interaction, snap dwell/preview,
  re-anchor on un-snap. **Tab-merge drop detection** hooks here (drop a dragged window's
  title onto another window → merge); **untab** is dragging a tab off the bar.
- **`src/wm.rs:391` `focus`**, **`Act` enum (~`200`)**, **`zone_rect`/`Zone`
  (`40`–`140`)**, **`interior_edges` (`145`)** — reused by split + snap.
- **`src/terminal.rs` `Session`** — reader thread pumps regardless of render; `read_input`
  (~`292`) must only run for the *active* tab. This is the keep-alive guarantee.
- **`src/settings.rs` + `src/keymap.rs`** — the rebind phase updates default bindings,
  `Command`/`Group` labels, and the `?` overlay text.

**Build/verify (HANDOFF):** GNU toolchain; kill the app before building; `cargo build
2>&1 | Select-Object -Last 30`; `cargo test --bin foreman`; GUI verified by running the
exe + screenshot (can't be seen from the terminal).

---

## Phase 1 — Tab-stacks (the `Win` primitive)

**Goal:** any window can hold a stack of contents shown as tabs; merge by drag-drop,
detach by drag-out. Works at both levels via recursion.

**Scope:**
1. Restructure `Win` to hold `tabs: Vec<Tab>` + `active: usize` (`Tab { title, content }`).
   Update all constructors/accessors (`push_win`, `add_terminal`, `add_project`,
   `picker_start`, render, dispatch). A len-1 stack renders exactly as today.
2. **Tab bar** in the titlebar when `tabs.len() > 1`: tab labels, active highlight, click
   to switch, per-tab close. Inline rename (`,` / double-click) targets the active tab.
3. **Active-tab-only** focus + render; **inactive tabs keep running** (PTY/reader thread
   alive; just not drawn and `read_input` not called). Verify a backgrounded tab's shell
   keeps producing output.
4. **Tab-merge (drag-drop):** drag a window's titlebar and drop it onto another window in
   the same manager → append source's tab(s) to the target stack, remove the source `Win`,
   focus the merged tab. (The cross-level case can't arise — different managers.)
5. **Untab (drag-out):** drag a tab off the bar → detach into a new floating `Win` at the
   drop point, restoring a sensible size.
6. Binding: `leader Tab` cycles tabs in the focused stack; if not a stack, falls back to
   the existing last-focused toggle. `Shift+Tab` = previous. (Add a `TabCycle`/`TabPrev`
   `Command`; wire defaults — this supersedes the plain-`Tab` last-focused binding.)

**Out of scope:** split, the WASD/arrows rebind, keybind-driven merge. No new deps.

**Acceptance:**
- Drag terminal A onto terminal B → B becomes a 2-tab stack; click/`leader Tab` switches;
  A's shell keeps running while inactive (output present on switch-back).
- Drag a tab off → it floats again as its own window.
- Same merge/switch/detach works for **project** windows on the desktop.
- A single-tab window looks and behaves exactly as before. `cargo build` + tests clean.

---

## Phase 2 — Split  *(depends on Phase 1)*

**Goal:** `Alt+WASD` builds layouts fast by creating + placing terminals, tabbing on
collision.

**Scope:**
1. Add `Command::Split(Dir)` (terminal-level). Dispatch into the focused project's child
   manager.
2. Behavior: create a new terminal, **snap it to the pointed zone** (`Zone::Left/Right/
   Top/Bottom` via the existing snap path). If a window is **already snapped to that
   zone**, **tab** the new terminal onto it instead (reuse Phase 1 merge).
3. Source placement: if the focused source window is **unsnapped**, also snap it to the
   **opposite** zone (left↔right, top↔bottom) for an instant two-pane split. If the source
   is **already snapped**, leave it untouched and just place/tab the newcomer.
4. Bindings: `Alt+W/A/S/D` → `Split(Up/Left/Down/Right)`.

**Out of scope:** project-level split (projects are created via `P`); arbitrary ratios.

**Acceptance:**
- From a maximized terminal, `Alt+D` → source becomes left half, new terminal right half.
- `Alt+D` again (right zone now occupied) → new terminal **tabs onto** the right window.
- `Alt+S` from an unsnapped floating window → source snaps top, new snaps bottom.
- `cargo build` + tests clean.

---

## Phase 3 — Rebind to the WASD scheme + consolidate  *(depends on 1 & 2)*

**Goal:** adopt the final §2 scheme and polish the surfaces. Mostly data-driven edits.

**Scope:**
1. Change `keymap.rs` defaults: terminal focus → arrows only (drop `h/j/k/l`); terminal
   snap → `WASD` (was `Shift+arrows`); add project snap `Ctrl+WASD`; keep project focus
   `Ctrl+arrows`; confirm split `Alt+WASD` and tab `Tab`/`Shift+Tab`/`Ctrl+Tab`.
2. Update the `?` overlay text/groups and the settings editor (`settings.rs`) labels and
   `Group`s so they reflect the new scheme and the split/tab commands.
3. Ensure the keymap merge/round-trip and the editor still pass; update/add tests for the
   new default chords.

**Acceptance:** full §2 scheme works in the running app; `?` overlay and settings editor
show the new bindings grouped correctly; config round-trips; `cargo build` + tests clean.
