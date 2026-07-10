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

### #2 — Feature: panel row click = focus if unfocused, minimize if already focused (projects and terminals)

**Status:** open · **Filed:** 2026-07-10 · **Severity:** enhancement

**Request.** In the task-manager/sessions panel (`src/panel.rs`, the desktop
right-edge list of projects and their terminals), a single click on a row
should behave like a taskbar button: if the target window is not the focused
one (or is minimized), the click focuses/restores it; if it is already the
focused window, the same click minimizes it. Applies to both project rows and
terminal rows (for a terminal, "focused" means it is the focused terminal of
the focused project, per the focus cascade).

**Current behavior.** A row click unconditionally records
`self.click = Some(path)` (`src/panel.rs` — row handling around
`resp.clicked()` at ~272, ~597, ~782), which the window manager consumes to
surface/focus the target. Clicking an already-focused row is a no-op; minimize
is only reachable via the row's hover `min` button (~732–754).

**Sketch.** Two options: (a) the panel model already paints rows differently
for the focused target, so it knows focus state at click time — emit a
distinct output (e.g. `toggle: Option<TargetPath>` next to `click`) when the
clicked row is the focused one; or (b) keep the single `click` output and let
the wm, when consuming it, check "is this path already the focused, visible
window?" and minimize instead of surfacing. (b) keeps the model dumber and
puts the policy where minimize/restore already lives (`src/wm.rs`).

**Scope note.** Restoring must respect the existing focus-cascade rules in
`src/wm.rs`; minimized windows restore via the panel today, so the
minimize-on-second-click path must not strand a window with no way back
(the panel row itself remains the restore affordance).

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

## Closed

_(none yet)_
