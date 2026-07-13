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

### #1 — App-wide crash: egui-wgpu panics on lost GPU device (`Failed to create staging buffer for index data`)

**Status:** open · **Filed:** 2026-07-09 · **Severity:** high (kills every session in the app)

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

---

### #3 — Feature: Grok as a first-class agent — landing-page button + Grok icon on sessions

**Status:** open · **Filed:** 2026-07-10 · **Severity:** enhancement

**Request.** Add Grok (xAI's CLI agent) alongside Claude and Codex: a Grok
launch button on the landing page, and a Grok icon shown on terminals/tabs that
are running a grok session (everywhere the Claude/Codex logos appear today).

**Current behavior.** Only Claude and Codex are first-class. The landing page
(`src/landing.rs`) has a `SessionKind` enum (~line 13) with per-kind display
name (~377), kind string (~388), launch command (~397), and icon mapping
(~242); `src/main.rs` (~443) launches a shell running the agent, with an error
toast if the binary is missing. Icons live in `src/icons.rs`: `IconKind` with
an `include_str!` SVG from `assets/icons/` (~13), a tint color (~44), and
detection by title/argv substring (~63–90) plus process-tree stem detection in
`src/proc.rs` (`detect_agent`). `src/recents.rs` persists the kind as a plain
string ("claude" | "codex" | ...).

**Sketch.** Mirror the Codex plumbing end to end:
1. `assets/icons/grok.svg` + `IconKind::Grok` (SVG const, tint, `all()` list,
   `from_title`/`from_argv` matching a "grok" substring).
2. `src/proc.rs` stem detection for `grok` (including the node-script case if
   the CLI is a JS entrypoint, like codex.js).
3. `SessionKind::Grok` in `src/landing.rs`: display name "Grok", kind string
   "grok", launch command (verify the actual CLI binary name — assumed `grok`),
   icon mapping, and inclusion in the landing button list (~26).
4. `src/recents.rs` kind mapping for "grok".
5. Update the unit tests that enumerate kinds/icons (icons.rs ~155–207,
   landing.rs ~793–913, proc.rs, recents.rs ~137).

**Open question.** Confirm the grok CLI's install name and how it appears in
titles/process trees on Windows (binary vs `node …\grok.js`) before wiring
detection — Claude/Codex icon detection has already been a bug surface.

---

### #4 — Feature: rename terminals from the sessions panel by double-clicking the name

**Status:** open · **Filed:** 2026-07-10 · **Severity:** enhancement

**Request.** In the task-manager/sessions panel (`src/panel.rs`, the desktop
right-edge list of projects and their terminals), double-clicking a terminal
row's name should turn it into an inline text edit; committing (Enter or focus
loss) sets a user-chosen name for that terminal, Escape cancels. The custom
name should stick — i.e. override the automatic title the terminal would
otherwise display.

**Current behavior.** Terminal names in the panel are display-only. Rows are
painted from `WindowManager::panel_model()` and only handle single click
(`self.click = Some(path)` to surface/focus) plus hover min/close buttons;
there is no `double_clicked()` handling and no rename affordance anywhere.
Titles come from the terminal itself (OSC title set by the shell/agent, with
process-tree fallback via `src/proc.rs` — see `src/terminal.rs` ~356–569),
so they change as the session runs.

**Sketch.**
1. Add a user-override name field on the window/terminal (likely on `Win` in
   `src/wm.rs` or on the terminal session): `custom_name: Option<String>`.
   Display logic everywhere a title is shown (panel row, window header, tab
   label) prefers `custom_name` over the live OSC/process title.
2. Panel edit state: the panel model gets `renaming: Option<TargetPath>` plus
   an edit buffer. `resp.double_clicked()` on a terminal row enters rename
   mode; the row paints a `TextEdit` instead of the label while active. Enter
   commits (wm drains e.g. `rename: Option<(TargetPath, String)>`), Escape or
   clicking elsewhere cancels.
3. Interaction with issue #2 (click = focus/minimize toggle): egui fires
   `clicked()` before `double_clicked()` on the same row, so entering rename
   mode will also have surfaced/toggled the window. Acceptable; if it feels
   bad, suppress the minimize half when a double-click lands.
4. Persistence: decide whether the name survives restart. Terminals themselves
   are not persisted across app runs today, so session-lifetime-only is fine;
   note it in the doc (`docs/task-manager-panel.md`).

**Scope note.** The request is terminals only, but the same mechanism applies
to project rows for free if wanted later. Keyboard focus while editing must
not leak to the focused terminal (the panel edit must swallow keys — see the
focus-cascade rules in `src/wm.rs`).

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

### #6 — Feature: auto-rename a default-named terminal to the agent's name when an agent starts in it

**Status:** open · **Filed:** 2026-07-10 · **Severity:** enhancement

**Request.** When an agent (Claude, Codex, later Grok — see #3) starts running
inside a terminal whose title is still the default, automatically rename the
terminal to the agent's name (e.g. "Claude · #3"). If the user has renamed the
terminal, never overwrite their name.

**Current behavior.** A tab's `title: String` (`src/wm.rs` ~164) is set once
at creation and only changes via the manual rename editor (`Command::Rename`,
wm.rs ~1909/~3077 — commits to `tabs[a].title`). Defaults are:
- plain terminal: `"{shell.label()}  ·  #{id}"` (`add_terminal`, wm.rs ~447),
  e.g. `PowerShell  ·  #3`;
- dispatched agent: explicit `--title` or `"agent · {argv[0]}"`
  (`add_terminal_cmd`, wm.rs ~1147).
Hand-typing `claude` into a shell changes the icon (agent detection already
exists: OSC-title match, throttled process-tree fallback —
`Session::icon_kind`, src/terminal.rs ~566–580, `detect_agent` in src/proc.rs)
but the tab title stays "PowerShell · #3".

**Sketch.**
1. Track "still default": store the generated default on the tab (e.g.
   `default_title: Option<String>`, or a `user_named: bool` set by the rename
   editor and by explicit dispatch `--title`). Comparing against the known
   default pattern also works but breaks if the pattern ever changes.
2. In the per-frame pass where the wm already consults the session (icon
   refresh), when `icon_kind()` transitions from None/shell to an agent AND
   the title is still the default, set `tabs[i].title` to the agent's display
   name, keeping the id suffix: `"Claude  ·  #3"`.
3. When the agent exits back to the shell, either leave the name (simple) or
   revert to the stored default (nicer — and `default_title` from step 1 makes
   it trivial). Decide at implementation; leaving it is acceptable v1.
4. A manual rename (or a panel rename, #4) permanently opts the terminal out
   of auto-renaming.

**Scope note.** Agent display names should come from the same source as the
icon mapping (`IconKind` → name) so #3's Grok gets this for free. The
dispatched-agent path (`add_terminal_cmd`) already names the window
sensibly — this issue is mainly for agents hand-launched inside a shell and
for the landing-page launch path (`add_project_with_command`, wm.rs ~524,
which types the agent command into a default-named PowerShell terminal).

---


### #9 — Chat injection gating + delivery ACK/retry, composed from EXISTING quiescence signals (fixes stuck-input)

**Status:** open · **Filed:** 2026-07-10 · **Severity:** high (messages silently lost in real use) · **Priority:** 1 of the chat-reliability series (#9–#15) · **Depends on:** nothing

**Background (shared by #9–#15).** The project chat room delivers posts by
injecting them into each member terminal's PTY as typed input (push model —
`ChatRoom::tick` in `src/chat.rs` ~598 produces per-member `Delivery`
batches; the wm writes them into the `Session`). The
chat-mentions design doc (`docs/superpowers/specs/2026-06-10-chat-mentions-design.md`)
already flags quiescence-gating of this injection as a known unsolved gap.
The state model across this series is deliberately **minimal**: passive
safe-to-inject (this issue) + self-reported turn-boundary state (#12).
Richer semantics ("reviewing", "blocked on X") belong in chat messages, not
the state system.

**Symptom.** Chat posts injected into a recipient CLI sometimes sit in its
input field and never submit — the synthetic keystrokes (and the Enter) race
the TUI's rendering and get swallowed mid-repaint. Delivery is
fire-and-forget; nobody notices the loss. Worst observed chat bug.

**Request — delivery-safety, NOT agent-state.** Three parts:
1. **Gate: do NOT build new detection.** Compose the quiescence signals that
   already exist in-process into a per-terminal "safe-to-inject" gate:
   `Session::ready` + `output_gen` (src/terminal.rs), the output-settle
   machinery (`advance_settles`, `DEFAULT_SETTLE_MS=120`, src/wm.rs), and the
   cursor-rest gate (src/caret.rs, `CURSOR_SETTLE=50ms`). Hold keystroke
   injection for a member until the gate opens.
2. **ACK:** after injecting, verify the frame was consumed — injected text
   echoed back / input buffer cleared — rather than assuming.
3. **Retry:** if swallowed, re-inject with backoff (bounded attempts);
   sender gets per-recipient **delivered/pending** status (`foreman chat`
   reply path in `src/control.rs`).

**Notes.** The delivery cursor (`Tab::last_delivered_seq`, chat.rs ~436)
currently advances at hand-off; with ACK it should advance only on confirmed
delivery so a swallowed message is re-delivered, not skipped. Echo detection
must tolerate TUIs that render input boxes (transformed echo) — match the
message body substring in recent output; treat no-echo-within-timeout as
swallowed. Keep the gate/ACK seams pure and unit-testable (synthetic output
streams), per the PTY-test conventions. This issue gates *when it is safe to
type*; it does not claim to know the agent is done — that ambiguity is #12's
domain.

---

### #10 — Route idle/join/exit chat notifications to the human only, never into agent PTYs

**Status:** open · **Filed:** 2026-07-10 · **Severity:** enhancement (trivial, high value) · **Priority:** 2 of the chat-reliability series · **Depends on:** nothing

**Symptom.** Idle notifications (and join/exit housekeeping noise) are
delivered into agent members' terminals as typed input. For an agent this is
pure noise — a fake user message forcing a context switch; only the human
watching the chat window needs it.

**Request.** Deliver idle/join/exit notifications to the human's chat viewer
(`Content::Chat`) only — visible in the log/transcript, never injected into
member PTYs.

**Notes.** `src/chat.rs` already has an entry class that is "never injected
into PTYs and never appears in `--history`" (chat.rs ~9, ~237 — system
entries are excluded from `deliver_after`). Implementation is likely: post
idle notifications as that class (or a variant that shows in the viewer and
`--history` but is excluded from delivery) rather than as ordinary member
posts. Verify with a two-member room: trigger an idle notification, confirm
the human viewer shows it and neither agent PTY receives bytes.

---

### #11 — Chat seen-seq dedupe: live injection and `--history` share one delivered marker

**Status:** open · **Filed:** 2026-07-10 · **Severity:** bug (duplicate delivery) · **Priority:** 3 of the chat-reliability series · **Depends on:** nothing

**Symptom.** A member can receive the same message twice — once via live PTY
injection and once by reading `foreman chat --history` — because the two
paths don't share a seen-seq marker. Messages carry seq numbers, so dedupe
is cheap; the marker just isn't shared.

**Request.** One per-member delivered-seq marker consulted by both paths:
live injection already advances `Tab::last_delivered_seq` (chat.rs ~436,
consumed by `deliver_after` ~240); `--history` (served in `src/control.rs`)
should default to a "since my cursor" view so a catching-up agent doesn't
re-read what was already injected. Full history stays available behind a
flag (`--all`) — agents legitimately re-read context.

**Design note.** Decide the semantics deliberately: does reading history
*consume* (advance the cursor, so live injection skips those seqs), or is
the cursor advanced only by injection with history default-filtered by it?
The latter is safer (no risk of a history read suppressing live delivery the
recipient never saw). Interacts with #9: once ACK exists the cursor means
"confirmed delivered" — a history read must not fake an ACK.

---

### #12 — `foreman state` cooperative verb + CLI hook adapters (turn-boundary signal; campaign-gated)

**Status:** open · **Filed:** 2026-07-10 · **Severity:** enhancement · **Priority:** 4 of the chat-reliability series · **Depends on:** nothing hard; **must route through the agent-state campaign** (`.claude/skills/foreman-agent-state-campaign/SKILL.md`)

**Request.** Implement the campaign's pre-planned cooperative verb —
`foreman state working|blocked|done|idle` — and ship adapters that auto-wire
it for known CLIs. Per the campaign, **self-reporting is the primary
mechanism, not a fallback**: passive PTY signals cleanly detect *working*
(output flowing), but done vs idle vs waiting-on-you is observationally
ambiguous (TUI spinners keep the screen changing while parked at an input
box). Heuristics for needs-input are explicitly out of scope, as are the
campaign's fenced-off approaches (keyword-sniffing screen text, parsing
agent session files).

1. **Verb:** new control-plane subcommand in `src/control.rs` (same
   named-pipe plane as `open`/`chat`/`close`; `FOREMAN_TERMINAL_ID` makes it
   self-targeting). **Additive wire change — ask-first per
   foreman-change-control**; this issue notes the gate, it does not preempt
   the decision. The exact state vocabulary is owned by the campaign (its
   G1–G3 anti-flap validation gates and phase structure apply) — don't
   finalize the word list in this issue.
2. **Claude Code adapter:** wire Claude Code's Stop hook (fires at end of
   turn) and Notification hooks to call `foreman state`, installed via the
   existing skill/config-install mechanism (`src/skills_install.rs` pattern —
   best-effort, never blocks launch).
3. **Codex adapter:** same via Codex's `notify` config.
4. **Degradation:** unadapted/unknown CLIs simply have no turn-boundary
   state; consumers (e.g. #13) fall back to #9's safe-to-inject gate only.

**Notes.** Agent identity for choosing an adapter already exists
(process-tree scan in `src/proc.rs` + OSC-title fallback). Staleness: a
`working` report from a process that died must not wedge consumers — clear
state on process exit (foreman owns the PTY and sees it) plus a long
staleness timeout back to unknown. Self-reported state is a scheduling
signal, not a security boundary.

---

### #13 — Turn-boundary chat queueing: hold routine posts until the recipient's turn ends; `--urgent` bypass

**Status:** open · **Filed:** 2026-07-10 · **Severity:** enhancement · **Priority:** 5 of the chat-reliability series · **Depends on:** #9, #12

**Symptom.** Chat posts land mid-task as fake user input while the recipient
agent is mid-turn, forcing a context switch. There is no way to hold routine
messages until the agent finishes its turn.

**Request.** Default chat delivery waits for the recipient's **turn
boundary** — #12's self-reported state (`done`/`idle`) when available,
falling back to #9's passive safe-to-inject gate for CLIs with no adapter.
Add an `--urgent`/`--interrupt` flag to `foreman chat` preserving today's
immediate-inject behavior; urgent should be human-only or role-gated
(see #14) so agents can't stampede each other.

**Notes.** Two layers compose, and stay distinct: turn-boundary decides
*when a message becomes eligible*; #9's gate + ACK decide *when the
keystrokes are physically safe and whether they landed*. The per-member
delivery cursor already provides ordering — a held member accumulates and
then receives the batch in seq order (existing `deliver_after` semantics).
Beware starvation: an agent that never reports `idle`/`done` must not hold
messages forever — #12's staleness fallback plus the sender-visible
`pending` state from #9 (so a human can escalate with `--urgent`) cover it.

---

### #14 — Chat member roles: `--role` on open/dispatch, role in roster and message frames

**Status:** open · **Filed:** 2026-07-10 · **Severity:** enhancement · **Priority:** 6 of the chat-reliability series · **Depends on:** nothing

**Symptom.** Chat members don't know each other's function: agents burn chat
posts announcing what they are, and self-assign work outside their intended
scope.

**Request.**
1. `--role <string>` on `foreman open`/dispatch (`src/control.rs`) and on
   first join for members that joined by posting; stored on the member
   (`src/chat.rs` member records, join-order roster ~464).
2. Role rendered in the roster and in **every** chat frame — injection
   framing and history lines share one format (chat.rs ~85–115:
   `[chat p1 #14] t2: text` becomes `[chat p1 #7] t1(reviewer): ...`).
3. Roster-with-roles injected into each member's context at join and on any
   membership change, so every agent always knows who's who without asking.

**Notes.** Role is a free-form string label, not an enforcement mechanism —
scope discipline still comes from prompts; the role line just gives agents
the information. Changing the frame format touches the transcript/wire
format agents parse — **ask-first per foreman-change-control** (OpenReply /
wire-compat rules), and keep untargeted frames byte-identical where existing
chat.rs frame tests assert on them. `--role` also pairs with #13's "urgent
is role-gated".

---

### #15 — (Optional, parallel) OSC 133 spike — verify ConPTY passthrough of semantic prompt marks, detect-only

**Status:** open · **Filed:** 2026-07-10 · **Severity:** spike (1 day, timeboxed) · **Priority:** 7 of the chat-reliability series — optional, parallel; **nothing above depends on it**

**Request.** Run the spike exactly as scoped in
`docs/warp-feature-candidates.md` §1 (verdict there: SPIKE-FIRST):
1. Verify the vendored-OpenConsole ConPTY actually passes `ESC]133;D;N`
   (command-finished-with-exit-code) marks through to foreman — passthrough
   is currently **unverified**; if ConPTY eats them, the whole feature is
   dead and the spike ends there.
2. **Detect-only** — interception seam is `advance_scanned`
   (src/terminal.rs:242), no alacritty fork needed. No shell-profile
   injection in this spike.
3. Check empirically whether Claude Code/Codex emit 133 marks around
   embedded tool executions (expectation from the analysis: marks stop
   flowing while a TUI agent runs, so this signal helps **plain-shell panes
   only**).

**Outcome.** A written verdict feeding the agent-state campaign
(`.claude/skills/foreman-agent-state-campaign/SKILL.md`) as a candidate
passive signal for plain-shell panes — it does not change the campaign's
position that self-reporting (#12) is the primary turn-boundary mechanism
for agent TUIs.

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

---

## Closed

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
