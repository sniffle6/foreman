---
name: egui-immediate-mode-reference
description: Use when writing or debugging egui UI code in foreman (src/main.rs, src/wm.rs, src/terminal.rs) — clicks silently dead in one screen region, Ctrl+Scroll zoom inert, smooth_scroll_delta reads zero, an Area drags itself or jumps on its first frame, duplicate widget Ids across nested Projects, Ctrl+C/Ctrl+V not arriving as key events, "no method named update"/"raw_scroll_delta" errors, screen_rect deprecation warnings, or repaint-cadence and per-frame-allocation questions on egui 0.34.3.
---

# egui immediate-mode reference

Domain pack for the GUI half of foreman. Audience: knows Rust, has never used
egui. Everything here is verified against committed HEAD `7fda1c2` and the
`egui 0.34.3` / `eframe 0.34.3` crate sources pinned in `Cargo.lock`
(Cargo.lock:897-899, 861-863) — line numbers are as of 2026-07-01.

Sibling boundaries: symptom→fix triage lives in **foreman-debugging-playbook**;
PTY/ConPTY/VT concepts live in **terminal-emulation-reference**; the compiler
warning baseline lives in **foreman-build-and-env**. This file is the "why does
egui behave like that" reference.

## 1. Immediate mode in one section

egui is an **immediate-mode** GUI: there is no retained widget tree. Every
frame, your code re-executes top to bottom and re-declares every widget from
scratch; egui hit-tests, paints, and throws it all away. Consequences that
shape foreman:

| Consequence | Where it bites in foreman |
|---|---|
| **All state lives in your structs** (or in egui's per-context data, § 9). Widgets remember nothing. | `App`, `WindowManager`, `Session` own everything (src/main.rs:23-43, src/wm.rs:601). |
| **Everything re-fits every frame.** Layout is recomputed, not cached by egui. | The Layout tree is re-laid-out and every Win re-fitted each frame (`tree.layout(...)` inside `WindowManager::show`, src/wm.rs:2296+); each `Session::show` re-probes cell size and re-resizes (src/terminal.rs:867-877). |
| **Per-frame allocations are a perf smell**, because "once" means "60×/second". | The Frame plan (src/frame.rs) exists to keep the per-frame grid walk pure, clamped, and in one place; style runs are batched into one `LayoutJob` per Session per frame (src/terminal.rs:996-1023). |
| **Widgets are identified by `Id`, not by object identity.** Same code path, same Id — collisions are silent (§ 6). | Nested Window managers all number Wins from 1 (src/wm.rs:668). |
| **You can't mutate the model mid-draw.** The draw pass holds a borrow of the thing it's drawing. | Deferred actions: collect `Act`s during the loop, apply after (§ 12). |
| **Nothing happens unless a frame runs.** Background threads must explicitly wake the render loop. | `ctx.request_repaint()` from reader threads (§ 8). |

## 2. Version pin and the eframe entry point

- egui/eframe **0.34.3** (Cargo.lock). eframe is egui's native windowing shim
  (winit + wgpu/glow under the hood); foreman uses it via
  `eframe::run_native(...)` (src/main.rs:460).
- **egui 0.34 renamed the per-frame entry.** Implement
  `fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame)` — foreman
  does at src/main.rs:338-339. In eframe 0.34.3 `ui` is the *required* trait
  method; the pre-0.34 `fn update(&mut self, ctx: &egui::Context, ...)` is now
  a deprecated **no-op default** ("Use Self::ui instead", eframe 0.34.3
  `src/epi.rs:176,189-192`) that nothing calls. Old-style code that only
  implements `update` fails with "missing: `ui`"; implementing both leaves
  `update` dead. Web tutorials and LLM training data overwhelmingly show
  `update`; do not copy them.
- Inside `ui`, get the context with `let ctx = ui.ctx().clone();`
  (src/main.rs:340) — `Context` is a cheap `Arc` clone, `Send + Sync`, and is
  what you hand to threads.
- One-time startup work is gated by a `started: bool` on `App`
  (src/main.rs:341-356) because immediate mode has no "init" callback with a
  live `Context`.

## 3. Trap index

| # | Trap | Symptom | Fix (all in-repo already) |
|---|---|---|---|
| §4 | Text layout needs `&mut` fonts | can't lay out text with `ctx.fonts(...)` | go through the painter: `ui.painter().layout_no_wrap(...)` / `painter.layout_job(job)` |
| §5 | Clipboard duality | Ctrl+C/X/V "not arriving" as key events | match `Event::Copy`/`Cut`/`Paste(_)` **and** the key chords |
| §6 | Id collisions across nested managers | clicks land on the wrong Win; drag state teleports | re-base child Ids: `base.with(("proj", win_id))`; key widgets `base.with((id, role))` |
| §7a | Area blocks by bounding rect | one region of the app silently input-dead | one `Area` per strip, never one Area spanning disjoint rects |
| §7b | Area first-frame default size + `constrain(true)` | half the app input-dead until something forces a re-layout | `.constrain(false)` + `.default_size(rect.size())` on absolute-rect Areas |
| §7c | Areas default `movable(true)` | chrome drags itself around | `.movable(false)` on every chrome Area |
| §8 | Repaint model | UI frozen until mouse moves; or 100% CPU | `request_repaint()` from threads; adaptive `request_repaint_after` backstop |
| §10 | egui steals Ctrl+wheel & Ctrl+0/± | terminal zoom dead, `smooth_scroll_delta` reads 0 with Ctrl held | disable `zoom_modifier` + `zoom_with_keyboard` at startup |
| §11 | No `raw_scroll_delta` in 0.34; wheel arrives smoothed/fractional | gentle scroll does nothing, fast flick over-scrolls | accumulator: `input::wheel_steps` + per-Session `scroll_accum`/`zoom_accum` |
| §13 | 0.34 API drift | `rect_stroke` arity errors; `screen_rect` deprecation warnings | pass `StrokeKind`; deprecation baseline is catalogued in foreman-build-and-env |

## 4. Text layout: go through the painter

Laying text out (measuring, shaping into a `Galley`) mutates egui's glyph cache,
so it needs `&mut` access to the fonts. In 0.34.3 `Context::fonts(|f| ...)`
hands you a **read-only** `&FontsView` that cannot lay out
(egui `src/context.rs:1079`); layout requires `fonts_mut`
(`FontsView::layout(&mut self, ...)`, epaint 0.34.3 `src/text/fonts.rs:718`).
The ergonomic path — the only one this repo uses — is the painter's wrappers:

```rust
// measure: the "M" probe that derives Cell metrics (src/terminal.rs:869-873)
let probe = ui.painter().layout_no_wrap("M".to_string(), font.clone(), FG);
// multi-style text: build a LayoutJob, lay it out, paint the galley
let galley = painter.layout_job(job);          // src/terminal.rs:1023
painter.galley(rect.min, galley, FG);          // src/terminal.rs:1024
```

Usage sites: src/terminal.rs:871,1023; src/wm.rs:253,308,313,341,2432,2702,
2887,2947,3517 (as of 2026-07-01). Note: CLAUDE.md phrases this gotcha as
"`ui.fonts(|f|…)` needs `&mut`" — in 0.34.3 there is no `Ui::fonts` at all
(verified by grep of egui 0.34.3 `src/ui.rs`); the phrasing predates the
`FontsView` split, but its conclusion (use the painter) is exactly right.

## 5. Clipboard duality: events AND keys

On Windows, egui may deliver Ctrl+C / Ctrl+X / Ctrl+V as dedicated
`Event::Copy` / `Event::Cut` / `Event::Paste(String)` events **instead of**
`Event::Key` — and Ctrl+V that arrives as `Paste` already carries the text.
Handle both shapes everywhere keyboard input is interpreted, and dedupe:

- **Session input** (the Input-encoding seam, `input::process_input`):
  `Event::Copy | Event::Cut` at src/input.rs:52, `Event::Paste` at
  src/input.rs:47, plus the Ctrl+C/V key branch at src/input.rs:85-98. The
  dedupe: a Ctrl+V key only triggers a clipboard read when **no** `Paste`
  event came in the same frame (src/input.rs:116-119) — otherwise you paste
  twice. The GUI shell that applies the outcome is `Session::read_input`
  (src/terminal.rs:803).
- **Leader chord matching** normalizes `Event::Copy`/`Event::Cut` back into
  their key Chords so a Keymap binding on Ctrl+C/Ctrl+X still fires
  (`event_chord`, src/wm.rs:1754-1755; `Paste` is not chord-normalized — it
  carries text and is consumed by the Session path).
- **Settings chord capture** normalizes the same two events when recording a
  binding (src/settings.rs:474-475).

Semantics on the Session path: Ctrl+C with a selection = copy; with no
selection = interrupt (0x03); Ctrl+Shift+C = copy without clearing the
selection (src/input.rs:121-131).

## 6. Id discipline

egui identifies every widget/Area by an `egui::Id` (a hash). Two widgets with
the same Id share hover/drag/focus state **silently** — no panic in release
use, just wrong behavior. Foreman's recursive compositor makes collisions the
default: every `WindowManager` numbers its Wins from 1 (`next: 1`,
src/wm.rs:668), so the desktop's Win 1 and every Project's Win 1 would collide.

The convention (follow it for any new widget):

1. Every `WindowManager::show` takes a `base: egui::Id` and derives all Ids
   from it. The desktop gets `egui::Id::new("desktop")` (src/main.rs:375).
2. Recursing into a Project **re-bases**:
   `wm.show(ui, rect, active, base.with(("proj", win_id)))` (src/wm.rs:156).
3. Per-widget Ids are `base.with((id, role))` where `id` is the WinId and
   `role` a `&str` discriminator — e.g. `("close" | "max" | "min" | "float")`
   controls at src/wm.rs:2990, `(id, "drag")` src/wm.rs:2421,
   `(id, "content")` src/wm.rs:2396, `(id, "tab", ti)` src/wm.rs:2725,
   `(id, "rsz", key)` src/wm.rs:3224.

## 7. Area landmines

`egui::Area` is the primitive for free-floating layers (foreman uses it for
OS chrome; the Wins themselves are painted/interacted manually). Three traps,
two of which caused a real incident (docs/os-chrome.md — one Area silently ate
every click in the app):

**7a. An Area blocks input over its whole BOUNDING RECT.** `Area::show`
registers one invisible widget covering the area's recorded bounds (sense
defaults to drag/click/hover by movability — egui 0.34.3
`src/containers/area.rs:226,503-509`), and in egui's hit test any widget
covering the pointer blocks every layer below it — regardless of how inert its
`Sense` looks. Four resize-rim strips in one Area = one full-screen bounding
rect = every click in the app swallowed. Rule: **each disjoint interactive
strip is its own Area** (src/main.rs:242-247, strips named
`os_rim_top/bottom/left/right`).

**7b. First-frame default size + `constrain(true)`.** On an Area's first frame
egui doesn't know the content size and assumes
`spacing.default_area_size` = **600×400** (egui `src/style.rs:1454`,
`src/containers/area.rs:469-478`); `constrain` defaults `true`
(area.rs:140) and shoves the origin up/left so that phantom size fits
on-screen. With absolute-rect content the *recorded* bounds inflate to
origin..strip — for the bottom rim strip that was the whole bottom half of the
app, which per 7a went input-dead. Rule: Areas positioned with absolute rects
set `.constrain(false)` **and** `.default_size(rect.size())`
(src/main.rs:251-258). Debug move when a region goes dead: dump
`ctx.memory(|m| m.area_rect(id))` for each chrome Area first
(docs/os-chrome.md:64-65; API at egui `src/memory/mod.rs:975`).

**7c. Areas default `movable(true)`** (area.rs:138) — chrome that forgets
`.movable(false)` gets dragged around by the user. Every chrome Area sets it
(src/main.rs:140,249).

**Painting without blocking:** a layer painter
(`ctx.layer_painter(LayerId::new(Order::Foreground, id))`) paints but registers
**no widget**, so it can span the whole screen without eating input — that is
how the 7px app border is drawn (src/main.rs:92-103, docs/os-chrome.md:27-28).

## 8. Repaint model

egui only renders when winit wakes it. Foreman's scheme (src/main.rs:400-419):

- **Fast path is event-driven:** `ctx.request_repaint()` is thread-safe and
  immediate. The Session reader thread calls it on every PTY chunk
  (src/terminal.rs:505, right after `note_pty_output()`); the Control plane
  server calls it on every dispatch (src/control.rs:314). The repo comment
  records the measured effect as ~0.2 ms wake (src/main.rs:402-404, dated
  claim — re-measure via foreman-diagnostics-and-tooling).
- **`request_repaint_after` is only an idle backstop**, and on Windows it is
  floored by the OS default timer granularity (~15.6 ms) — asking for less
  just means "as soon as the OS allows" (repo-documented at src/main.rs:405-409;
  an OS scheduling fact, recorded here because this is where it bites).
- **Adaptive cadence** (as of 2026-07-01, src/main.rs:410-419): activity =
  PTY output this frame (`terminal::take_pty_output()`, an `AtomicBool` swap,
  src/terminal.rs:22-32) OR any input event OR a Control plane message. Hot =
  activity within the last **250 ms** → `request_repaint_after(4 ms)`; idle →
  **100 ms**. This avoids pinning 60 fps across many Sessions while idle.

Never poll from a thread by sleeping and hoping a frame runs — send data over
a channel, then `request_repaint()`.

## 9. Per-context data: `ctx.data` for cross-cutting globals

`Context` carries a typed key-value store (`ctx.data_mut(|d| ...)`) that
survives across frames. Foreman parks the global terminal font size there so
any Session's Ctrl+Scroll handler can update it without threading a parameter
through the recursive Window managers:

- `terminal::font_size(ctx)` / `set_font_size(ctx, px)` wrap
  `get_temp`/`insert_temp` under a fixed Id (src/terminal.rs:315-333).
- `App::ui` **seeds** it from persisted settings each frame (src/main.rs:374),
  every `Session::show` reads it (src/terminal.rs:867), and `App` **reads it
  back** after the draw to persist changes behind a 400 ms debounce
  (src/main.rs:376-390, `FONT_SAVE_DEBOUNCE` src/main.rs:46).
- Same pattern for the icon texture cache (src/icons.rs:106).

Constant values (default 13.0, clamp 6.0–40.0, step 1.0) live in
src/config.rs:16-22 — catalogued in **foreman-config-and-flags**; persistence
mechanics in docs/settings-persistence.md.

## 10. Input theft: egui's built-in zoom eats Ctrl+wheel and Ctrl+0/±

egui ships a whole-UI zoom that grabs exactly the inputs terminal zoom needs
(defaults verified in egui 0.34.3 source):

- `input_options.zoom_modifier` defaults to `Modifiers::COMMAND` (= Ctrl on
  Windows; egui `src/input_state/mod.rs:118`): while Ctrl is held, wheel
  events are diverted into UI zoom and `smooth_scroll_delta` reads **zero** —
  your handler sees nothing, with no error.
- `zoom_with_keyboard` defaults `true` (egui `src/memory/mod.rs:316`) and
  consumes Ctrl+0 / Ctrl+± to scale all chrome.

Foreman disables both once at startup (src/main.rs:347-350):

```rust
ctx.options_mut(|o| {
    o.zoom_with_keyboard = false;
    o.input_options.zoom_modifier = egui::Modifiers::NONE;
});
```

If terminal zoom "stops working", check these options first
(docs/terminal-zoom.md:41-61). Ctrl+0 is then consumed in the Input-encoding
seam as `zoom_reset` (src/input.rs:100-105) so the shell never sees a stray
NUL.

## 11. Wheel smoothing: no `raw_scroll_delta`, carry a remainder

egui 0.34.3 exposes only `smooth_scroll_delta` (egui
`src/input_state/mod.rs:243`); `raw_scroll_delta` does not exist in this
version (grep of the crate source finds no hit). A physical wheel notch
arrives as smoothed per-frame **fractions**, so naive `delta / line_height`
rounds gentle scrolls to 0 and over-emits fast flicks. The fix is the
accumulator seam:

- Pure: `input::wheel_steps(accum, delta, unit) -> (whole_steps, remainder)`
  (src/input.rs:151-155; unit-tested at src/input.rs:718+).
- Per-Session carried state: `scroll_accum` (unit = row height) and
  `zoom_accum` (unit = `ZOOM_NOTCH_PX` = 50.0, the repo's calibration of
  "one physical notch" — src/terminal.rs:286-290,305; applied at
  src/terminal.rs:922-930).

Use the same pattern for any new fractional-input feature.

## 12. The borrow shape: Deferred actions

The Win draw loop iterates `self.windows` under one mutable borrow; a titlebar
click cannot close/retab/refocus mid-loop without invalidating the draw order
(and the borrow checker won't let you anyway). The pattern — CONTEXT.md's
**Deferred action** — is: the loop only pushes variants of the private
`enum Act` (Focus/Close/Min/Max/Float/Restore/AddTerm/SetTab/CloseTab/Merge/
Untab..., src/wm.rs:552-588); `apply_acts` runs after the render borrow is
released (src/wm.rs:3394-3403). Rationale, seam map, and threading model are
owned by **foreman-architecture-contract** — this section only names the egui
constraint that forces the shape.

## 13. 0.34 API drift you will hit

| API | 0.34.3 truth | Evidence |
|---|---|---|
| `eframe::App::update` | deprecated; implement `ui(&mut Ui, &mut Frame)` | eframe `src/epi.rs:176,189`; src/main.rs:339 |
| `Painter::rect_stroke` | takes a 4th arg `StrokeKind` (`Inside`/`Middle`/`Outside`) | egui `src/painter.rs:447-453`; src/main.rs:98-103,316-328 |
| `Context::screen_rect` | deprecated ("split into `viewport_rect()` and `content_rect()`") — still used at src/main.rs:87, so it sits in the accepted warning baseline | egui `src/context.rs:2843` |
| `InputState.raw_scroll_delta` | gone; only `smooth_scroll_delta` (§ 11) | grep egui 0.34.3 src |
| `Ui::fonts` | gone; layout via painter helpers (§ 4) | grep egui `src/ui.rs` |

The full compiler-warning baseline (what is accepted vs. what is new noise) is
owned by **foreman-build-and-env** — check there before "fixing" deprecations;
churning them is a change-control matter (**foreman-change-control**).

## 14. Verifying UI changes

The GUI cannot be observed from a terminal. Build + screenshot via the
**build-screenshot** skill; headless Session verification (send/Snapshot over
the Control plane) and measurement recipes are owned by
**foreman-diagnostics-and-tooling**. Don't claim a UI change works without one
of those (working agreement, CLAUDE.md).

## When NOT to use this skill

- **A live symptom to triage** ("clicks dead", "zoom broken", "app frozen") →
  start at **foreman-debugging-playbook**; come back here for the mechanism.
- **PTY/ConPTY/VT escape/alacritty questions** → **terminal-emulation-reference**.
- **Build errors, toolchain, warning baseline** → **foreman-build-and-env**.
- **Why the architecture is shaped this way / threading model / seams** →
  **foreman-architecture-contract**.
- **Config constants and settings persistence** → **foreman-config-and-flags**.
- **Running the app / Control plane CLI usage** → **foreman-run-and-operate**;
  agents *inside* foreman use **foreman-dispatch** / **foreman-chat**.
- **Screenshot/measure workflows** → **build-screenshot**,
  **foreman-diagnostics-and-tooling**.

## Provenance and maintenance

Written 2026-07-01 against committed HEAD `7fda1c2` (working tree clean) and
crate sources for egui/eframe/epaint 0.34.3 from crates.io. Line numbers drift;
re-verify from the repo root `H:/claude code/foreman` (PowerShell 7+):

| Claim | Re-verify |
|---|---|
| egui/eframe version pin | `git grep -n -A1 'name = "egui"' -- Cargo.lock` |
| `App::ui` entry (not `update`) | `git grep -n "fn ui(&mut self" -- src/main.rs` |
| zoom-theft opt-out present | `git grep -n "zoom_modifier" -- src/main.rs` |
| painter text-layout sites | `git grep -n "layout_no_wrap\|layout_job" -- src` |
| clipboard duality handled | `git grep -n "Event::Copy" -- src` |
| Id re-base + (id, role) keying | `git grep -n 'with(("proj", win_id))\|with((id, role))' -- src/wm.rs` |
| one Area per rim strip, constrain(false) | `git grep -n "os_rim_top\|constrain(false)" -- src/main.rs` |
| adaptive cadence constants (250 ms / 4 ms / 100 ms) | `git grep -n "from_millis(250)\|cadence" -- src/main.rs` |
| wheel accumulator seam | `git grep -n "fn wheel_steps\|scroll_accum\|zoom_accum" -- src` |
| ctx.data font-size global | `git grep -n "fn font_size\|insert_temp" -- src/terminal.rs` |
| Deferred `Act` collect/apply | `git grep -n "enum Act\|fn apply_acts" -- src/wm.rs` |
| `screen_rect` still in baseline | `git grep -n "screen_rect" -- src` |
| egui-source claims (Area defaults, 600×400, `Modifiers::COMMAND`, no `raw_scroll_delta`) | `cargo doc -p egui --no-deps` or read the 0.34.3 sources under your cargo registry; if `Cargo.lock` moved off 0.34.3, re-check every row of § 13 |
