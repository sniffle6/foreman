---
name: egui-immediate-mode-reference
description: Use when writing or debugging egui UI code in foreman (src/main.rs, src/wm.rs, src/terminal.rs) — clicks silently dead in one screen region, Ctrl+Scroll zoom inert, smooth_scroll_delta reads zero, an Area drags itself or jumps on its first frame, duplicate widget Ids across nested Projects, Ctrl+C/Ctrl+V not arriving as key events, "no method named update"/"raw_scroll_delta" errors, screen_rect deprecation warnings, or repaint-cadence and per-frame-allocation questions on egui 0.34.3.
---

# egui immediate-mode reference

Domain pack for the GUI half of foreman: *why* egui behaves the way it does
where it has already cost this repo time. Audience: knows Rust, has never used
egui. Symptom→fix triage is **foreman-debugging-playbook**; this file is the
mechanism.

Every claim below is either a symbol you can grep in `src/`, or a fact about the
`egui` / `eframe` / `epaint` **0.34.3** sources — the version pinned in
`Cargo.lock`. The exact source line numbers for the egui-side facts are parked
in one block at the end (§A) so a version bump invalidates that section instead
of this whole file.

## 1. Immediate mode in one section

egui has no retained widget tree. Every frame your code re-executes top to
bottom and re-declares every widget; egui hit-tests, paints, throws it away.
What that costs foreman:

| Consequence | Where it bites in foreman |
|---|---|
| **All state lives in your structs** (or in egui's per-context data, § 9). Widgets remember nothing. | `App`, `WindowManager`, `Session` own everything. |
| **Everything re-fits every frame.** Layout is recomputed, not cached. | `WindowManager::show` re-runs `tree.layout(...)` and re-fits every Win; each `Session::show` re-probes cell size and re-resizes. |
| **Per-frame allocations are a perf smell**, because "once" means "60×/second". | The Frame plan (`src/frame.rs`) keeps the per-frame grid walk pure, clamped, and in one place; style runs are batched into one `LayoutJob` per Session per frame (`src/terminal.rs`). |
| **Widgets are identified by `Id`, not object identity.** Same code path, same Id — collisions are silent (§ 6). | Every nested `WindowManager` numbers its Wins from 1 (`next: 1`, `src/wm.rs`). |
| **You can't mutate the model mid-draw.** The draw pass holds a borrow of the thing it draws. | Deferred actions: collect `Act`s during the loop, apply after (§ 12). |
| **Nothing happens unless a frame runs.** Background threads must wake the render loop. | `ctx.request_repaint()` from reader threads (§ 8). |

## 2. Version pin, backend, and the eframe entry point

- egui/eframe/epaint **0.34.3** (`Cargo.lock`).
- **Backend is glow (OpenGL), and that is an invariant, not a default.** Windows
  drops the GPU device on sleep and display power transitions; `egui-wgpu`
  answers with an unconditional `panic!` in `update_buffers`, which aborts the
  process, while `egui_glow` only logs. So `Cargo.toml` sets
  `eframe = { default-features = false, features = [..., "glow"] }` — and
  `default-features = false` is load-bearing, because eframe prefers wgpu
  whenever both backends are compiled in. **`Cargo.lock` still lists
  `egui-wgpu` 0.34.3 as a transitive entry**; a reader who greps the lockfile
  will wrongly conclude wgpu is live. It is not. Read `docs/gpu-device-loss.md`
  (it records the side-by-side A/B) before touching that `Cargo.toml` line.
- **Entry point:** foreman starts eframe via `eframe::run_native(...)` in
  `src/main.rs`; the per-frame body is `impl eframe::App for App` in the same
  file (`git grep -n run_native -- src/main.rs`).
- **egui 0.34 renamed the per-frame entry.** Implement
  `fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame)` — foreman
  does, in `impl eframe::App for App` (`src/main.rs`). In eframe 0.34.3 `ui` is
  the *required* trait method; the pre-0.34
  `fn update(&mut self, ctx: &egui::Context, ...)` is now a deprecated **no-op
  default** ("Use Self::ui instead") that nothing calls. Old-style code that
  only implements `update` fails with "missing: `ui`"; implementing both leaves
  `update` dead. Web tutorials and LLM training data overwhelmingly show
  `update`; do not copy them.
- Inside `ui`, get the context with `let ctx = ui.ctx().clone();` — `Context` is
  a cheap `Arc` clone, `Send + Sync`, and is what you hand to threads.
- One-time startup work is gated by a `started: bool` on `App`, because
  immediate mode has no "init" callback with a live `Context`.

## 3. Trap index

| # | Trap | Symptom | Fix (all in-repo already) |
|---|---|---|---|
| §4 | Text layout needs `&mut` fonts | can't lay out text with `ctx.fonts(...)` | go through the painter: `ui.painter().layout_no_wrap(...)` / `painter.layout_job(job)` |
| §5 | Clipboard duality | Ctrl+C/X/V "not arriving" as key events | match `Event::Copy`/`Cut`/`Paste(_)` **and** the key chords |
| §6 | Id collisions across nested managers | clicks land on the wrong Win; drag state teleports | re-base child Ids: `base.with(("proj", win_id))`; key widgets `base.with((id, "drag"))` etc. |
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
hands you a **read-only** `&FontsView` that cannot lay out; layout is
`FontsView::layout(&mut self, ...)` and needs `fonts_mut`. The ergonomic path —
the only one this repo uses — is the painter's wrappers:

```rust
// measure: the "M" probe that derives Cell metrics (Session::show, src/terminal.rs)
let probe = ui.painter().layout_no_wrap("M".to_string(), font.clone(), FG);
// multi-style text: build a LayoutJob, lay it out, paint the galley
let galley = painter.layout_job(job);
painter.galley(rect.min, galley, FG);
```

Find the call sites with `git grep -n "layout_no_wrap\|layout_job" -- src`.

Note: CLAUDE.md phrases this gotcha as "`ui.fonts(|f|…)` needs `&mut`" — in
0.34.3 there is no `Ui::fonts` at all. The phrasing predates the `FontsView`
split, but its conclusion (use the painter) is exactly right.

## 5. Clipboard duality: events AND keys

On Windows, egui may deliver Ctrl+C / Ctrl+X / Ctrl+V as dedicated
`Event::Copy` / `Event::Cut` / `Event::Paste(String)` events **instead of**
`Event::Key` — and Ctrl+V that arrives as `Paste` already carries the text.
Handle both shapes everywhere keyboard input is interpreted, and dedupe:

- **Session input** (the Input-encoding seam, `input::process_input` in
  `src/input.rs`): `Event::Copy | Event::Cut` and `Event::Paste` arms, plus the
  Ctrl+C/V key branch. The dedupe: a Ctrl+V key only triggers a clipboard read
  when **no** `Paste` event came in the same frame — otherwise you paste twice.
  The GUI shell that applies the outcome is `Session::read_input`
  (`src/terminal.rs`).
- **Leader chord matching** normalizes `Event::Copy`/`Event::Cut` back into
  their key Chords so a Keymap binding on Ctrl+C/Ctrl+X still fires
  (`fn event_chord`, `src/wm.rs`; `Paste` is not chord-normalized — it carries
  text and is consumed by the Session path).
- **Settings chord capture** normalizes the same two events when recording a
  binding (`src/settings.rs`).

Semantics on the Session path: Ctrl+C with a selection = copy; with no
selection = interrupt (0x03); Ctrl+Shift+C = copy without clearing the
selection.

## 6. Id discipline

egui identifies every widget/Area by an `egui::Id` (a hash). Two widgets with
the same Id share hover/drag/focus state **silently** — no panic, just wrong
behavior. Foreman's recursive compositor makes collisions the default: every
`WindowManager` numbers its Wins from 1 (`next: 1`, `src/wm.rs`), so the
desktop's Win 1 and every Project's Win 1 would collide.

The convention (follow it for any new widget):

1. Every `WindowManager::show` takes a `base: egui::Id` and derives all Ids
   from it. The desktop gets `egui::Id::new("desktop")` (`src/main.rs`).
2. Recursing into a Project **re-bases**:
   `wm.show(ui, rect, active, base.with(("proj", win_id)), ...)` (`src/wm.rs`).
3. Per-widget Ids append a role discriminator after the WinId — the tuple is
   **flat**, not nested: `base.with((id, "drag"))`, `(id, "content")`,
   `(id, "rename")`, `(id, "tab", ti)`, `(id, "rsz", key)`, and
   `(id, role.id_str())` for the titlebar controls. Nesting the role
   (`(id, ("tab", ti))`) hashes to a *different* Id than the code produces —
   still unique, so nothing visibly breaks, which is exactly why the drift is
   easy to miss. Enumerate them with `git grep -n 'base\.with((' -- src/wm.rs`.

## 7. Area landmines

`egui::Area` is the primitive for free-floating layers (foreman uses it for OS
chrome; the Wins themselves are painted/interacted manually). Three traps, two
of which caused a real incident (`docs/os-chrome.md` — one Area silently ate
every click in the app):

**7a. An Area blocks input over its whole BOUNDING RECT.** `Area::show`
registers one invisible widget covering the area's recorded bounds (sense
defaults to drag/click/hover by movability), and in egui's hit test any widget
covering the pointer blocks every layer below it — regardless of how inert its
`Sense` looks. Four resize-rim strips in one Area = one full-screen bounding
rect = every click in the app swallowed. Rule: **each disjoint interactive
strip is its own Area** — see the `os_rim_top/bottom/left/right` strips in
`src/main.rs`.

**7b. First-frame default size + `constrain(true)`.** On an Area's first frame
egui doesn't know the content size and assumes `spacing.default_area_size` =
**600×400**; `constrain` defaults `true` and shoves the origin up/left so that
phantom size fits on-screen. With absolute-rect content the *recorded* bounds
inflate to origin..strip — for the bottom rim strip that was the whole bottom
half of the app, which per 7a went input-dead. Rule: Areas positioned with
absolute rects set `.constrain(false)` **and** `.default_size(rect.size())`.
Debug move when a region goes dead: dump `ctx.memory(|m| m.area_rect(id))` for
each chrome Area first (`docs/os-chrome.md`).

**7c. Areas default `movable(true)`** — chrome that forgets `.movable(false)`
gets dragged around by the user. Every chrome Area sets it.

**Painting without blocking:** a layer painter
(`ctx.layer_painter(LayerId::new(Order::Foreground, id))`) paints but registers
**no widget**, so it can span the whole screen without eating input — that is
how the app border is drawn (`src/main.rs`, `docs/os-chrome.md`).

## 8. Repaint model

egui only renders when winit wakes it. Foreman's scheme lives in the adaptive-
cadence block at the end of `App::ui` (`src/main.rs`) — read the literals there,
don't trust a copy:

- **Fast path is event-driven:** `ctx.request_repaint()` is thread-safe and
  immediate. The Session reader thread calls it on every PTY chunk (right after
  `note_pty_output()`, `src/terminal.rs`); the Control plane server calls it on
  every dispatch (`src/control.rs`). The repo comment records the measured wake
  as ~0.2 ms — a dated claim; re-measure via **foreman-diagnostics-and-tooling**
  before quoting it.
- **`request_repaint_after` is only an idle backstop**, and on Windows it is
  floored by the OS default timer granularity (~15.6 ms) — asking for less just
  means "as soon as the OS allows". That is an OS scheduling fact, recorded
  here because this is where it bites.
- **Adaptive cadence:** activity = PTY output this frame
  (`terminal::take_pty_output()`, an `AtomicBool` swap) OR any input event OR a
  Control plane message. Recent activity → hot, tight repaint; otherwise idle,
  slow repaint. This avoids pinning 60 fps across many Sessions while idle.

Never poll from a thread by sleeping and hoping a frame runs — send data over a
channel, then `request_repaint()`.

## 9. Per-context data: `ctx.data` for cross-cutting globals

`Context` carries a typed key-value store (`ctx.data_mut(|d| ...)`) that
survives across frames. Foreman parks the global terminal font size there so
any Session's Ctrl+Scroll handler can update it without threading a parameter
through the recursive Window managers:

- `terminal::font_size(ctx)` / `set_font_size(ctx, px)` wrap `get_temp` /
  `insert_temp` under a fixed Id (`src/terminal.rs`).
- `App::ui` **seeds** it from persisted settings each frame, every
  `Session::show` reads it, and `App` **reads it back** after the draw to
  persist changes behind the `FONT_SAVE_DEBOUNCE` window (`src/main.rs`).
- Same pattern for the icon texture cache (`get_temp_mut_or_default`,
  `src/icons.rs`) and the bell-enabled flag (`src/terminal.rs`).

The font constants (`DEFAULT_FONT_SIZE`, `MIN_FONT_SIZE`, `MAX_FONT_SIZE`,
`FONT_ZOOM_STEP`) live in `src/config.rs` — catalogued in
**foreman-config-and-flags**; persistence mechanics in
`docs/settings-persistence.md`.

## 10. Input theft: egui's built-in zoom eats Ctrl+wheel and Ctrl+0/±

egui ships a whole-UI zoom that grabs exactly the inputs terminal zoom needs:

- `input_options.zoom_modifier` defaults to `Modifiers::COMMAND` (= Ctrl on
  Windows): while Ctrl is held, wheel events are diverted into UI zoom and
  `smooth_scroll_delta` reads **zero** — your handler sees nothing, with no
  error.
- `zoom_with_keyboard` defaults `true` and consumes Ctrl+0 / Ctrl+± to scale
  all chrome.

Foreman disables both once at startup (`src/main.rs`, in the `started` gate):

```rust
ctx.options_mut(|o| {
    o.zoom_with_keyboard = false;
    o.input_options.zoom_modifier = egui::Modifiers::NONE;
});
```

If terminal zoom "stops working", check these options first
(`docs/terminal-zoom.md`). Ctrl+0 is then consumed in the Input-encoding seam
as `zoom_reset` (`src/input.rs`) so the shell never sees a stray NUL.

## 11. Wheel smoothing: no `raw_scroll_delta`, carry a remainder

egui 0.34.3 exposes only `smooth_scroll_delta`; `raw_scroll_delta` does not
exist in this version (grep of the crate source finds no hit). A physical wheel
notch arrives as smoothed per-frame **fractions**, so naive `delta / line_height`
rounds gentle scrolls to 0 and over-emits fast flicks. The fix is the
accumulator seam:

- Pure: `input::wheel_steps(accum, delta, unit) -> (whole_steps, remainder)`
  (`src/input.rs`, unit-tested in the same file).
- Per-Session carried state: both `scroll_accum` and `zoom_accum` carry
  remainders against `input::WHEEL_NOTCH_PX` (50.0) — the repo's calibration of
  "one physical notch" (`src/terminal.rs`). `src/imageview.rs` is a third user
  of the same unit.

**Accumulating against row height was Issue #8** and is pinned against by a
regression test in `src/input.rs` (`// Issue #8 cause (1): accumulate against
WHEEL_NOTCH_PX, not row height`). Do not "restore consistency" by switching the
scroll accumulator to row height — that *is* the bug.

Use the same pattern for any new fractional-input feature.

## 12. The borrow shape: Deferred actions

The Win draw loop iterates `self.windows` under one mutable borrow; a titlebar
click cannot close/retab/refocus mid-loop without invalidating the draw order
(and the borrow checker won't let you anyway). The pattern — CONTEXT.md's
**Deferred action** — is: the loop only pushes variants of the private
`enum Act` (`src/wm.rs`); `apply_acts` runs after the render borrow is
released. Rationale, seam map, and threading model are owned by
**foreman-architecture-contract** — this section only names the egui constraint
that forces the shape.

## 13. 0.34 API drift you will hit

| API | 0.34.3 truth | In-repo |
|---|---|---|
| `eframe::App::update` | deprecated; implement `ui(&mut Ui, &mut Frame)` | `impl eframe::App for App`, `src/main.rs` |
| `Painter::rect_stroke` | takes a 4th arg `StrokeKind` (`Inside`/`Middle`/`Outside`) | `git grep -n rect_stroke -- src` |
| `Context::screen_rect` | deprecated ("split into `viewport_rect()` and `content_rect()`") — still used, so it sits in the accepted warning baseline | `src/main.rs` |
| `InputState.raw_scroll_delta` | gone; only `smooth_scroll_delta` (§ 11) | — |
| `Ui::fonts` | gone; layout via painter helpers (§ 4) | — |

The full compiler-warning baseline (what is accepted vs. what is new noise) is
owned by **foreman-build-and-env** — check there before "fixing" deprecations;
churning them is a change-control matter (**foreman-change-control**).

## §A. egui 0.34.3 source cites

These are the only line numbers in this file. They point into the **egui /
eframe / epaint 0.34.3** crate sources under your cargo registry, not into
`src/`. They are pinned to that version: if `Cargo.lock` moves off 0.34.3,
treat this whole section as unverified and re-check it — nothing else in the
file depends on it.

| Fact (section) | Source |
|---|---|
| `Context::fonts` yields a read-only `&FontsView` (§4) | egui `src/context.rs:1079` |
| layout needs `&mut`: `FontsView::layout(&mut self, ...)` (§4) | epaint `src/text/fonts.rs:718` |
| `App::update` deprecated no-op default (§2, §13) | eframe `src/epi.rs:176,189-192` |
| `Area::show` registers one widget over its bounds (§7a) | egui `src/containers/area.rs:226,503-509` |
| first-frame default area size 600×400 (§7b) | egui `src/style.rs:1454`, `src/containers/area.rs:469-478` |
| `constrain` defaults `true` (§7b) | egui `src/containers/area.rs:140` |
| `movable` defaults `true` (§7c) | egui `src/containers/area.rs:138` |
| `Memory::area_rect` debug accessor (§7b) | egui `src/memory/mod.rs:975` |
| `zoom_modifier` defaults `Modifiers::COMMAND` (§10) | egui `src/input_state/mod.rs:118` |
| `zoom_with_keyboard` defaults `true` (§10) | egui `src/memory/mod.rs:316` |
| only `smooth_scroll_delta` exists (§11) | egui `src/input_state/mod.rs:243` |
| `rect_stroke` 4th arg `StrokeKind` (§13) | egui `src/painter.rs:447-453` |
| `screen_rect` deprecation text (§13) | egui `src/context.rs:2843` |

## When NOT to use this skill

- **A live symptom to triage** ("clicks dead", "zoom broken", "app frozen") →
  start at **foreman-debugging-playbook**; come back here for the mechanism.
- **PTY/ConPTY/VT/alacritty questions** → **terminal-emulation-reference**.

## Provenance and maintenance

Re-verify from the repo root `H:/claude code/foreman`:

| Claim | Re-verify |
|---|---|
| glow-only backend, no wgpu (§2) | `git grep -n -A6 '^eframe = ' -- Cargo.toml ; git grep -n wgpu -- Cargo.toml` — the feature list must contain `glow` with `default-features = false`, and `wgpu` must appear only in comments |
| egui/eframe version pin (§A validity) | `git grep -n -A1 'name = "egui"$' -- Cargo.lock` |
| zoom-theft opt-out still present (§10) | `git grep -n "zoom_modifier" -- src/main.rs` |
| wheel unit is `WHEEL_NOTCH_PX`, not row height (§11) | `git grep -n "WHEEL_NOTCH_PX" -- src` |
