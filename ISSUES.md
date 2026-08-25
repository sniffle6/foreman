# Issue Tracker

Lightweight repo-local issue tracker. Rules:

- Every issue must be **fully self-contained**: written for a reader (or a fresh
  agent session) with zero prior context. Include symptom, evidence, root-cause
  analysis so far, and candidate fixes inline — never "see conversation" or
  assume memory of a prior session.
- New issues get the next number. Move fixed issues to the **Closed** section
  with a one-line resolution and the commit hash.

---

## Open

### #4 — Feature: rename terminals from the sessions panel by double-clicking the name

**Status:** open · **Filed:** 2026-07-10 · **Rescoped:** 2026-08-25 · **Severity:** enhancement (small)

**Request.** Double-clicking a terminal row's name in the sessions panel
(`src/panel.rs`) should start an inline rename, the same way double-clicking a
window's titlebar name already does.

**What already exists** (this issue was filed before any of it, and its
original text claiming "no rename affordance anywhere" is obsolete):

- Inline rename on the window titlebar — double-click the name rect,
  `src/wm.rs:3757`; state is `renaming`/`rename_buf`/`rename_focus` (~422).
- A `begin_rename()` command for the focused window (`wm.rs:2648`).
- Custom names already beat auto-titles: `Tab::fixed` vs `Tab::shell_default`,
  arbitrated by `title_is_auto_managed` (`wm.rs:5536`), so an agent being
  detected in a pane will not clobber a name the user chose.
- Custom names already survive restart: `TabSnap.title` is persisted, and
  restore re-applies the fixed/auto distinction (`wm.rs:756-759`).

**So the remaining work is only the panel affordance.** Panel rows sense
`click` and paint from `WindowManager::panel_model()`; they need
`double_clicked()` handling, a `TextEdit` in place of the label while editing,
and a way to hand the committed string back to the wm's existing rename path —
reuse it rather than adding a second one.

**Gotchas.** Keyboard focus while editing must not leak to the focused
terminal (focus-cascade rules in `src/wm.rs`). And egui fires `clicked()`
before `double_clicked()`, so entering rename will also have run the row's
single-click focus/minimize toggle; suppress the minimize half if that feels
wrong in practice.

---

### #5 — Feature: drag-reorder terminals and projects in the sessions panel

**Status:** open · **Filed:** 2026-07-10 · **Severity:** enhancement

**Request.** In the task-manager/sessions panel (`src/panel.rs`), allow
dragging rows to reorder them: project rows reorder among projects, and
terminal rows reorder among the terminals of their own project. Dragging a
terminal into a *different* project is out of scope for this issue (that's a
move-between-projects feature with PTY/env implications).

**Current behavior.** Row order is not user-controllable. The panel is a
read-mostly view of `WindowManager::panel_model()` (`src/wm.rs` ~1287), which
builds the list by iterating `self.windows` in vec order and each `Win`'s
`tabs` in index order. So panel order today falls out of window-creation /
tab order, and rows only sense clicks (`egui::Sense::click()`) — there is no
drag handling in `panel.rs`.

**Design decision required first.** The `windows` vec order likely doubles as
z-order/stacking (and tab index is the layout-tree/tab-stack identity used by
`TargetPath.tab`), so naively reordering the underlying vecs to move a panel
row can have side effects: restacking windows, or invalidating `TargetPath`s
held elsewhere (panel `click`, pending renames, etc.). Two options:
1. **Separate display order** — a per-panel (or per-wm) ordering key
   (`Vec<WinId>` / sort index) that only the panel sorts by; the wm's vecs
   stay untouched. Safer; panel-only concern; needs persistence if order
   should survive restarts (it should at least survive within a session).
2. **Reorder the source vecs** — panel order, z-order, and tab order stay one
   concept. Simpler mentally but must audit every index-based path
   (`TargetPath.tab`, layout-tree leaf mapping, active-tab index) before
   moving entries.

**Sketch (assuming option 1).** Rows use `Sense::click_and_drag()`; a drag
past a small threshold enters reorder mode (so plain clicks — see issue #2 —
still work), paints the dragged row floating with an insertion indicator, and
on release emits e.g. `reorder: Option<(TargetPath, usize)>` for the wm to
drain into the display-order key. `panel_model()` then sorts projects and each
project's terminal list by that key.

**Interaction notes.** Must coexist with the row hover min/close buttons
(~732), single-click focus/minimize (issue #2), and double-click rename
(issue #4) — the drag threshold is what keeps these from colliding. The panel
also scrolls vertically; dragging near the panel edge should auto-scroll or at
minimum not break the clamped-scroll behavior from commit 24729ef.

---

### #16 — Text selection vs scrolling: highlight doesn't track content; can't scroll mid-selection

**Status:** open · **Filed:** 2026-07-10 · **Severity:** medium (core-interaction correctness)

**Symptom (as reported).** Selecting text and scrolling interact
inconsistently: (a) the selection highlight often "stays put on the screen"
instead of scrolling with the text it covers; (b) you cannot scroll while a
selection drag is in progress (so a selection can't extend past the visible
viewport).

**Why this is suspicious, not just missing polish.** Selection is stored in
*buffer* coordinates (alacritty's `Selection`, `src/terminal.rs` ~1131–1147;
pointer→buffer mapping at ~1022 accounts for the scrollback offset), and the
contract "highlight sticks to its content" is pinned by unit tests:
`sel_viewport_range_shifts_with_display_offset_so_selection_sticks_to_content`
(~2941) and `height_grow_keeps_selection_on_its_content` (~2669). So the
reported screen-anchored behavior contradicts the tested model — the bug is
in the live path those tests don't cover. Candidates to check:
1. **Paint path:** the per-frame viewport-coords conversion of the one
   `term.selection` (~1239–1264) — e.g. using a stale display offset or the
   overlay cache (`Overlays`/Arc-clone fast path, ~1264) not invalidating on
   scroll, leaving last frame's highlight rects on screen.
2. **Live output while selected:** new lines rotating the grid — resize
   re-anchor rotates the selection (~309–315), but confirm ordinary
   scroll-up from child output does too in our integration.
3. **Mid-drag scrolling:** the selection drag is handed in by the WM
   ("content-area drag", ~1127). While the drag is active, check whether the
   wheel handler (`resp.hovered()` branch, ~1168) still runs, whether wheel
   deltas are consumed elsewhere during a drag, and whether the per-frame
   drag update recomputes the buffer point with the *new* display offset
   (if it reuses press-time offset, scrolling mid-drag would corrupt the
   anchor — possibly why it feels inconsistent).

**Expected behavior (acceptance).**
- A completed selection stays glued to its text: wheel-scrolling moves the
  highlight with the content, off-screen and back, unchanged.
- Wheel-scrolling during an active drag works and extends the selection
  sensibly (anchor stays on its content; the moving end follows the pointer
  over the newly revealed lines).
- Stretch (separate commit if done): drag past the top/bottom edge
  auto-scrolls, the standard terminal way to select more than a screenful.

**Caveats.** `docs/terminal-selection.md` is known to potentially disagree
with the code (see failure-archaeology notes) — trust the code and tests,
not that doc. On the alternate screen there is no scrollback, so the
mid-drag-scroll expectations apply to the primary screen; alt-screen wheel
forwards to the app (see #7/#8) and selection there is screen-static by
nature. Repro before fixing: two panes, generate scrollback (`dir -r` /
long build log), select mid-screen, wheel both ways; then repeat holding the
drag.

### #18 — Chat injection is fire-and-forget: posts get swallowed and silently lost

**Status:** open · **Filed:** 2026-08-25 · **Severity:** high (silent message loss)

**Symptom.** The project chat room delivers posts by injecting them into each
member terminal's PTY as typed input. Sometimes the injected text lands in the
recipient CLI's input field and is never submitted — the synthetic keystrokes
(and the Enter) race the TUI's repaint and get swallowed. Delivery is
fire-and-forget, so nothing notices: the sender sees success, the recipient
never sees the message, and the delivery cursor advances past it anyway.

**Evidence / where it lives.** `ChatRoom::tick` (`src/chat.rs`) produces
per-member `Delivery` batches that the wm writes into the `Session`.
`Tab::last_delivered_seq` advances at hand-off, not on confirmed receipt, so a
swallowed post is skipped rather than retried. There is no ACK path.

**Related but not a fix.** A `--re <seq>` handshake exists (`src/control.rs`,
frames in `chat.rs`) where an agent's reply counts as an acknowledgement. That
is an agent-level courtesy, not delivery detection — it cannot fire for a
message the agent never saw.

**Candidate directions (deliberately unspecified).** An earlier entry specced
a quiescence gate plus echo-detection ACK plus bounded retry; it was dropped
on 2026-08-25 as over-built for a delivery model that is not yet proven. If
this is picked up, weigh simply not injecting into PTYs at all — a pull model
(agents read chat when they choose) removes the failure class rather than
detecting it.

---

## Closed

### #1 — App-wide crash: egui-wgpu panics on lost GPU device (`Failed to create staging buffer for index data`)

**Resolved:** `ba803ef` — switched eframe to the **glow** (OpenGL) renderer, which has no equivalent panic; `egui_glow` only logs on `GL_CONTEXT_LOST`. This was candidate fix #2 below, filed unvalidated on 2026-07-09 and confirmed 2026-08-25 by running both builds side by side through one device-loss event: the wgpu build took its 11th panic, the glow build kept rendering. Candidate #1 (upgrade) stayed ruled out — 0.36.1 still panics. Candidate #3 (session persistence) remains open and is still the only thing that would make a GUI death survivable. Full story: `docs/gpu-device-loss.md`; GitHub issue #2.

**Symptom.** Foreman vanishes with no dialog while running — typically after
sitting idle in the background for a while. The terminal that launched it shows:

```
thread 'main' panicked at ...\egui-wgpu-0.34.3\src\renderer.rs:971:17:
Failed to create staging buffer for index data. Index count: 26184.
Required index buffer size: 104736. Actual size 219456 and capacity: 219456 (bytes)
error: process didn't exit successfully: `target\release\foreman.exe` (exit code: 101)
```

**Evidence.** `foreman_panic.log` (written by `install_panic_logger`,
src/main.rs) has captured at least two separate occurrences at the same
location (`egui-wgpu-0.34.3/src/renderer.rs:971`), with different index counts
(31968 and 26184). Backtrace: `egui_wgpu::renderer::Renderer::update_buffers`
→ `Painter::paint_and_update_textures` → eframe/winit frame callback. This is
the "whole app vanishes" class documented in the foreman-debugging-playbook
skill §11.

**Root cause (diagnosed 2026-07-09).** Not a foreman drawing bug and not a
buffer-sizing bug: the panic message itself shows the index buffer was large
enough (required 104,736 ≤ capacity 219,456, size a valid multiple of 4).
egui-wgpu 0.34.3 calls `wgpu::Queue::write_buffer_with(...)` and panics when
it returns `None` (renderer.rs:950-976). With valid size and sufficient
capacity, `None` here means **the wgpu device was in an error state — almost
certainly a lost device** (system sleep/resume, display driver reset/update,
or a TDR triggered by other GPU load while foreman idled). egui-wgpu/eframe
0.34 does not handle device loss; it panics instead of recovering, and per
playbook §11 the panic aborts the whole process — every session dies.

**Candidate fixes:**
1. ~~Upgrade egui/eframe/egui-wgpu~~ — **ruled out 2026-07-09**: egui-wgpu
   0.35.0 (latest, 2026-06-25) still has the identical `panic!` for both the
   index and vertex staging buffers (`renderer.rs`, `write_buffer_with` →
   `None`), and its changelog contains no device-loss/recovery work. Upgrading
   does not address this crash.
2. Try a different backend (unvalidated) — `WGPU_BACKEND` env or eframe's
   `glow` renderer (avoids egui-wgpu entirely). Caveat: GL contexts also die on
   driver reset, so this may relocate the failure, not remove it. Warp (also a
   GPU-rendered terminal) has this exact class open on Windows/DX12:
   warpdotdev/warp#12132 (DXGI_ERROR_DEVICE_REMOVED → fatal panic, no
   recovery) — device loss is simply unhandled across this stack today.
3. Longer term: session persistence / daemon-client split (already an open
   research item) — the only option that makes the crash *survivable* rather
   than hopefully-avoided; PTYs live in a separate process from the GUI.

**Repro.** Not deterministic. Leave foreman running, put the machine through
sleep/resume or heavy GPU load in another app, return and interact. Confirm a
crash by reading `foreman_panic.log` in the directory foreman was launched
from.

### #3 — Feature: Grok as a first-class agent — landing-page button + Grok icon on sessions

**Resolved:** `SessionKind::Grok` + landing button; `IconKind::Grok` with
`assets/icons/grok.svg`; argv/title/process-tree detection for `grok` stem
(native `grok.exe` under `%USERPROFILE%\.grok\bin`); recents kind `"grok"`.
Launch command is `grok`.

### #6 — Feature: auto-rename a default-named terminal to the agent's name when an agent starts in it

**Resolved:** `Tab.auto_title` set for shell spawns (`add_terminal`);
`refresh_auto_titles` renames to `"{Agent}  ·  #{term_id}"` via
`IconKind::agent_label` when `icon_kind` detects Claude/Codex/Grok. Manual
rename and dispatch titles opt out. Agent exit leaves the name (v1).

### #2 — Feature: panel row click = focus if unfocused, minimize if already focused (projects and terminals)

**Resolved:** `Act::FocusPath` → `toggle_surface_target` in `src/wm.rs` —
already-focused *visible* path minimizes; otherwise surfaces. Covered-by-
sibling-zoom does not count as visible (interacts with #17). Hover min still
`MinPath`. Tests: `focus_path_*`.

### #17 — Sessions-panel click on a sibling should un-zoom a zoomed/maximized subwindow

**Resolved:** `surface_target` clears zoom when the target is a different
window (project-level and nested). Tests:
`surface_target_clears_a_sibling_zoom_*`,
`surface_target_keeps_the_zoom_when_the_target_is_the_zoomed_window`.

### #7 — Feature: wheel-scroll works over any hovered terminal, focused or not

**Resolved:** Dropped the focus gate on `WheelAction::Pty` in `Session::show`.
Hover remains the only gate; wheel Pty (SGR / alt-scroll arrows) is navigation
under the pointer, not typed input. Policy seam
`input::wheel_action_for_hover`; tests: `wheel_pty_forwards_on_unfocused_hover`,
`wheel_pty_forwards_when_focused`, `wheel_scrollback_applies_on_unfocused_hover`.
Keyboard stays focus-gated.

### #8 — Terminal wheel scrolling feels too sensitive / inconsistent

**Resolved:** Wheel accumulates whole physical notches against
`input::WHEEL_NOTCH_PX` (not row height). One notch → `LINES_PER_NOTCH` (3)
scrollback lines or alt-scroll arrows; mouse-reporting emits **one** SGR/X10
event per notch (not per line). Zoom shares the same notch unit. Tests:
`wheel_one_notch_scrolls_lines_per_notch`,
`wheel_sgr_mouse_one_event_per_notch_not_per_line`,
`wheel_full_notch_points_map_to_one_step_independent_of_row_height`,
`wheel_alt_scroll_emits_lines_per_notch_arrows`.
