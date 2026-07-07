# Close-Confirm for Running Subprocesses — Design

**Status:** Design approved in direction (mockup signed off). Three decisions were
defaulted while the user was away — marked **[DEFAULTED]** below and safe to flip
before the plan is executed:

1. **Default button** → *close anyway* (Enter confirms), Windows-Terminal style.
2. **List shape** → *grouped by terminal* (the approved mockup).
3. **Theming** → the modal reads `theme.rs` tokens (renders in the current warm
   palette today); the steel + safety-orange look in the mockup is a *separate*
   whole-app theme migration, out of scope here.

Visual reference (iterated + approved):
`docs/superpowers/specs/2026-07-06-close-confirm-mockup.html`.

## Goal

When the user closes a terminal or project that still has a **running
subprocess** under it, pop a modal confirming the kill and listing each doomed
process as **Name │ Pid**. An idle shell (sitting at its prompt) closes silently,
exactly as today.

## Why

Every `Session` lives in a kill-on-close Windows Job (`src/job.rs`): dropping the
`Session` — closing the pane, tab, or project — takes its whole descendant tree
with it. Right default, but silent: a user who closes a pane running `claude`, a
build, or a dev server loses that work with no warning. This adds a confirmation
gate in front of the *interactive* close paths only.

## Scope

**In scope**

- Confirm modal on interactive terminal close (header X, tab-bar X, leader
  `CloseTerm`).
- Confirm modal on interactive project close (header X, leader `CloseProject`).
- **[DEFAULTED — include]** A second guard on the whole-app quit path (window
  title-bar X / Alt+F4), which bypasses the window manager. Its own task, so it
  can be dropped for smaller scope.

**Out of scope**

- The steel + safety-orange re-theme of `theme.rs` (whole-app change; the modal
  just consumes whatever tokens exist).
- The control-plane `foreman close` path (`CtrlMsg::Close` → `close_dispatch`) and
  the dispatch-undo path (`close_terminal`). Programmatic and headless — a modal
  would break automation. They keep calling the direct, un-gated close.
- Minimizing a window (`minimize`) — nothing is destroyed.
- Live monitoring of the process list while the modal is open (snapshot at open;
  see Edge cases).

## Trigger policy — what counts as "a running subprocess"

A `Session` counts as having a running subprocess iff its shell's `root_pid` has
**≥1 live descendant** in the OS process table that is **not** console-host
plumbing. Denylist of excluded executable names (case-insensitive):

- `OpenConsole.exe`
- `conhost.exe`

Everything else a descendant of the shell (`claude.exe`, `node.exe`, `cargo.exe`,
`python.exe`, `vim.exe`, …) counts and is listed; the shell process itself
(`root_pid`) is never listed. Idle-at-prompt shell → no descendant → closes with
no prompt. The whole policy is the single pure predicate `collect_descendants`,
so switching to "agents only" or "any live shell" later is a one-function change.

## Visual design (from the approved mockup)

- **Flat single surface.** One `WIN_BG` panel; the title and the button row are
  separated from the body by hairline dividers, no filled bands.
- **No warning glyph.** Title is bare text.
- **Lowercase, terse voice**, matching `dirpicker.rs` ("set project directory",
  "open here", "cancel").
- **One accent.** The destructive confirm button carries the theme accent
  (`CARET`/amber today; safety-orange under the future theme). No second color;
  the process rows and headers are `TEXT`/`DIM` only.
- **Process rows** are bare `name … pid`, monospace, pid right-aligned in `DIM`
  with tabular figures. No pips, no per-row rules, no bounding box.
- **Grouped list** (see List shape): terminal-name headers indented from the
  edge, their processes nested a step deeper. A single group renders flat (no
  header) — the terminal-close case.

Exact copy:

| Variant | Title | Lead | Confirm button |
|---|---|---|---|
| terminal | `close this terminal?` | `1 process is still running here:` | `close anyway` |
| project | `close this project?` | `{n} processes still running across {k} terminals:` | `close anyway` |
| quit | `quit foreman?` | `{n} processes still running across {k} projects:` | `quit anyway` |

Cancel button: `cancel`. Keyboard: **Esc cancels; Enter confirms** (the confirm
button is default — **[DEFAULTED]**; the safe alternative is to make *cancel*
default so a stray Enter can't kill work).

## Architecture

Four seams. Each is a deep module over the messy bits (process-table walking,
egui overlay drawing, the recursive window tree).

### Seam 1 — `src/proc.rs`: descendant enumeration

`proc.rs` already owns a throttled `sysinfo` scanner (`REFRESH_EVERY = 1500ms`), a
private `ProcRow { pid, parent, name, cmd }` table, and a `descends_from` walker.
Add a second public entry point beside `agent_for`, reusing the same scanner:

```rust
/// One live descendant process, for the close-confirm list.
pub struct ProcInfo {
    pub pid: u32,
    pub name: String, // e.g. "claude.exe"
}

/// Live descendants of `root_pid` worth warning about before a kill: every
/// process that descends from `root_pid`, excluding `root_pid` itself and
/// console-host plumbing (OpenConsole.exe / conhost.exe). Throttled through the
/// same scanner as `agent_for`.
pub fn descendants(root_pid: u32) -> Vec<ProcInfo>;
```

Pure, unit-tested core (mirrors `detect_agent`):

```rust
const HOST_PLUMBING: &[&str] = &["OpenConsole.exe", "conhost.exe"];

/// Pure: given a process table, the descendants of `root` worth listing.
fn collect_descendants(table: &[ProcRow], root: u32) -> Vec<ProcInfo>;
```

Keeps rows where `descends_from(table, row.pid, root)` and `row.pid != root` and
`row.name` is not in `HOST_PLUMBING` (case-insensitive). Grouping is *not* done
here — that is a window-tree concern (Seam 4). This seam stays "PID in → flat
descendant list out".

### Seam 2 — `src/terminal.rs`: expose the root PID

`Session` already stores `root_pid: Option<u32>` (used by `icon_kind`). Add the
missing accessor:

```rust
/// The shell's own process id, if the spawn reported one. Root of the
/// descendant scan used by the close-confirm.
pub fn root_pid(&self) -> Option<u32> { self.root_pid }
```

### Seam 3 — `src/confirm.rs` (new): the modal view

Self-contained modal, same shape as `src/dirpicker.rs` (`DirPicker::show(ui) ->
Outcome`). A **pure view over a fixed, already-grouped process list** — it knows
nothing about window ids or how the close is performed.

```rust
pub enum ConfirmOutcome { Pending, Cancelled, Confirmed }

/// One labelled cluster of doomed processes. `scope` is the optional dim suffix
/// on the header ("3 terminals" in the quit variant); None elsewhere.
pub struct ProcGroup {
    pub label: String,               // terminal title, or project title (quit)
    pub scope: Option<String>,
    pub procs: Vec<crate::proc::ProcInfo>,
}

pub struct ConfirmClose {
    title: String,          // "close this terminal?"
    lead: String,           // "1 process is still running here:"
    confirm_label: String,  // "close anyway" / "quit anyway"
    groups: Vec<ProcGroup>,
}

impl ConfirmClose {
    pub fn new(title: impl Into<String>, lead: impl Into<String>,
               confirm_label: impl Into<String>, groups: Vec<ProcGroup>) -> Self;

    /// Total processes across all groups (for callers that want the count).
    pub fn total(&self) -> usize;

    /// Render one frame inside `area` (dim + centered panel), return the outcome.
    /// Renders flat (no headers) when `groups.len() <= 1`, grouped+indented
    /// otherwise. Esc → Cancelled, Enter → Confirmed. Buttons mirror the keys;
    /// the confirm button is visually the default.
    pub fn show(&mut self, ui: &mut egui::Ui, area: egui::Rect) -> ConfirmOutcome;
}
```

`area` is passed so the dim/centering is scoped to the owning manager's rect
(desktop for a project close, the project's own rect for a terminal close),
matching the recursive compositor's "confine to your area" rule.

### Seam 4 — `src/wm.rs`: the gate, grouping, state, routing

**State.** One new field on `WindowManager`:

```rust
/// A close awaiting confirmation because the target has running subprocesses.
pending_close: Option<PendingClose>,

struct PendingClose {
    target: CloseTarget,
    view: crate::confirm::ConfirmClose,
}

enum CloseTarget {
    ActiveTab(WinId),   // header X, leader CloseTerm/CloseProject
    Tab(WinId, usize),  // tab-bar X
}
```

**Grouping (recursive).** Three helpers turn the window tree into `ProcGroup`s:

```rust
/// Groups that closing this one tab would kill. A terminal → at most one group
/// (its own title); a project → one group per terminal inside it; chat → none.
fn groups_in_tab(tab: &Tab) -> Vec<ProcGroup> {
    match &tab.content {
        Content::Terminal(s) => {
            let procs = s.root_pid().map(crate::proc::descendants).unwrap_or_default();
            if procs.is_empty() { vec![] }
            else { vec![ProcGroup { label: tab.title.clone(), scope: None, procs }] }
        }
        Content::Project(wm) => wm.terminal_groups(),
        Content::Chat(_) => vec![],
    }
}

/// One group per terminal window in THIS manager that has running processes
/// (label = the window title). Used for project-close.
fn terminal_groups(&self) -> Vec<ProcGroup>;

/// One group per project window in THIS (desktop) manager that has running
/// processes: label = project title, scope = "N terminals", procs = aggregate
/// across the project. Used for the quit guard.
fn project_groups(&self) -> Vec<ProcGroup>;

/// Flat aggregate across everything in this manager — the cheap "is anything
/// running?" check for the quit gate.
fn all_procs(&self) -> Vec<crate::proc::ProcInfo>;
```

(`tab.title` / window title is the same string the header renders. `terminal_groups`
and `project_groups` skip windows whose process set is empty.)

**The gate.** All interactive close triggers route through one of these instead
of calling `close_*` directly:

```rust
/// Close the active tab of `id`, or — if it has running subprocesses — open the
/// confirm modal instead. On confirm the real `close_active_tab` runs.
fn request_close_active_tab(&mut self, id: WinId);

/// Same, for a specific tab index (tab-bar X).
fn request_close_tab(&mut self, id: WinId, idx: usize);
```

Each gathers groups for the target tab; if the group list is empty → call the
existing `close_active_tab` / `close_tab` immediately (unchanged behavior); else
build the `ConfirmClose` (title/lead/confirm-label chosen from whether the active
tab is a `Content::Project`) and stash it in `pending_close`. No-op if
`pending_close.is_some()` already (never stack two confirms).

**Routing changes (all interactive close entry points):**

| Trigger | Today | After |
|---|---|---|
| `Act::Close(id)` (wm.rs:3389) | `close_active_tab(id)` | `request_close_active_tab(id)` |
| `Act::CloseTab(id, idx)` (wm.rs:3398) | `close_tab(id, idx)` | `request_close_tab(id, idx)` |
| leader `CloseTerm` (wm.rs:1790) | `child.close_active_tab(id)` | `child.request_close_active_tab(id)` |
| leader `CloseProject` (wm.rs:1765) | `self.close_active_tab(id)` | `self.request_close_active_tab(id)` |

`close`, `close_tab`, `close_active_tab` are **unchanged** — still the direct
executors, used by the programmatic paths and now by the modal confirm.

**Rendering + resolution.** In `show_modals` (wm.rs:3425), add a branch beside the
picker/settings ones, same take/show/restore idiom + `swallow_input` after:

```rust
if let Some(mut pending) = self.pending_close.take() {
    match pending.view.show(ui, area) {
        ConfirmOutcome::Pending   => self.pending_close = Some(pending),
        ConfirmOutcome::Cancelled => {}
        ConfirmOutcome::Confirmed => match pending.target {
            CloseTarget::ActiveTab(id) => self.close_active_tab(id),
            CloseTarget::Tab(id, idx)  => self.close_tab(id, idx),
        },
    }
    self.swallow_input(ui);
}
```

`show_modals` runs at both levels, so a terminal-close modal renders inside its
project's WM and a project-close modal renders at the desktop WM — no cross-level
plumbing. `deserted()` must also treat a pending close as "not deserted":

```rust
pub fn deserted(&self) -> bool {
    self.windows.is_empty() && self.picker.is_none()
        && self.settings.is_none() && self.pending_close.is_none()
}
```

### Seam 4b — `src/main.rs`: the quit guard **[DEFAULTED — separable task]**

The window's title-bar X (main.rs:180-181) and Alt+F4 send `ViewportCommand::Close`
straight to the viewport, bypassing every WM close funnel. To guard "kill
everything at once":

- In `App::ui`, read `ctx.input(|i| i.viewport().close_requested())`.
- If a close is requested, `self.force_quit` is unset, and
  `self.desktop.all_procs()` is non-empty → `ctx.send_viewport_cmd(
  ViewportCommand::CancelClose)` and open a desktop confirm titled `quit foreman?`
  built from `self.desktop.project_groups()`.
- On `Confirmed` → set `self.force_quit = true` and
  `send_viewport_cmd(ViewportCommand::Close)` (not cancelled this time).
- On `Cancelled` → clear; app stays open.

The existing `deserted()` → `Close` path (main.rs:377-378) needs no change: by the
time the last project has actually closed, its subprocesses are already gone, so
`all_procs()` is empty and the quit proceeds without a re-prompt. The quit confirm
reuses `confirm::ConfirmClose`; whether main.rs holds its own `Option<ConfirmClose>`
or the desktop WM holds `pending_close` with a `CloseTarget::Quit` variant is an
implementation detail for the plan.

## Data flow (terminal close, the common case)

1. User clicks a pane's header X → `Act::Close(id)` in that project's WM.
2. `request_close_active_tab(id)` → `groups_in_tab(active tab)` →
   `Session::root_pid()` → `proc::descendants(root_pid)`.
3. Empty → `close_active_tab(id)` now (Session drops, Job kills the tree). Done.
4. Non-empty → `pending_close = Some(...)`; nothing closes this frame.
5. Next frames: `show_modals` renders the modal over the project's rect (one
   group → flat list).
6. Esc / Cancel → drop `pending_close`, pane stays.
   Enter / Close anyway → `close_active_tab(id)` runs; tree dies via the Job.

## Edge cases & error handling

- **Scan is best-effort.** `Session::root_pid()` is `None` when the spawn never
  reported a pid → treated as "no subprocesses" → closes silently (same
  best-effort class as the Job assignment).
- **Snapshot, not live.** The process list is captured when the modal opens. A
  listed process exiting while the dialog is up is harmless — confirming closes,
  cancelling keeps the pane. No live refresh (YAGNI).
- **Exited terminal.** A dead `Session` has no live descendants → closes with no
  prompt.
- **Console-host tree shape.** Whether `OpenConsole.exe` appears as a descendant
  of the shell depends on the ConPTY host; the denylist filters it either way so
  an idle shell never false-triggers. The one behavior to verify live (Testing).
- **Modal already open.** `request_close_*` no-ops when `pending_close.is_some()`
  — single modal at a time, same discipline as settings/picker.

## Testing

**Pure unit tests — `proc.rs` (`collect_descendants`, synthetic tables):**

- Direct child of the shell is listed (name + pid).
- Grandchild (nested one level) is listed.
- `OpenConsole.exe` / `conhost.exe` descendants excluded (case-insensitive).
- The shell itself (`root_pid`) is never listed.
- A process under a *different* shell does not leak in.
- Idle shell (only the shell row) → empty list.

**Wm-level tests (`wm.rs`):**

- `request_close_active_tab` on a window whose target has **no** live procs
  closes immediately (window removed) — exercised with a real idle shell
  `Session`, asserting the denylist lets an idle ConPTY shell through.
- `terminal_groups` builds one group per terminal that has procs and skips empty
  ones; the group label is the window title. (Synthetic: a WM with two terminal
  windows, one gated to empty.)
- Constructing a `pending_close` and driving `ConfirmOutcome::Confirmed` removes
  the target window; `Cancelled` leaves it; `Pending` keeps the modal.
- `deserted()` returns false while `pending_close.is_some()`.

The "has procs → opens modal" branch is covered by the `proc.rs` purity tests
composed with the confirm-outcome wiring test — avoiding a flaky test that must
spawn a shell-with-grandchild and race the OS scan.

**Live verification (build + run, not asserted in CI):**

- Spawn a terminal, immediately close it → **no** prompt (idle shell, denylist).
- Run a blocking child (`pause`, or an agent) in a pane, close it → prompt lists
  that child as Name │ Pid; Cancel keeps it, Close anyway kills it (verify via
  `foreman status` / the process is gone).
- Project with two busy terminals → project close prompt shows both terminals as
  indented groups.
- Screenshot the modal to confirm layout/legibility against the mockup.

## Open decisions (defaulted — confirm or override)

1. **Default button** — defaulted to *close anyway* (Enter confirms). Alt: make
   *cancel* default. One-line change in `ConfirmClose::show`.
2. **List shape** — defaulted to *grouped by terminal*. Flat is a strict subset
   (drop `groups_in_tab`'s grouping, emit one anonymous group).
3. **Theming** — defaulted to *`theme.rs` tokens now*; steel + safety-orange is a
   separate whole-app migration. The mockup previews that future look.
4. **App-quit guard (Seam 4b)** — defaulted to *included* as a separable task.
   Drop it and only pane/project closes are guarded.

## Key files

- `src/proc.rs` — new `ProcInfo`, `descendants`, pure `collect_descendants`.
- `src/terminal.rs` — `Session::root_pid()` accessor.
- `src/confirm.rs` — new modal view (`ConfirmClose`, `ProcGroup`, `ConfirmOutcome`).
- `src/wm.rs` — `pending_close` state, `request_close_*` gate, `groups_in_tab` /
  `terminal_groups` / `project_groups` / `all_procs`, `show_modals` branch,
  `deserted()` update, close-trigger routing.
- `src/main.rs` — quit guard (`close_requested` interception) — separable.
- `src/dirpicker.rs` — reference pattern for the modal (not modified).
- `docs/superpowers/specs/2026-07-06-close-confirm-mockup.html` — approved visual.
