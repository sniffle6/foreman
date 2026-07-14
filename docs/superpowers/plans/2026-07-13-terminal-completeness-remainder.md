# Terminal Completeness Remainder — Implementation Plan

**Goal:** finish the three daily-use gaps still owned by
`docs/epics/terminal-completeness-epic.md`: real bold/italic rendering, TUI mouse
click/drag/motion, and host-side scrollback search.

**Scope fence:** this plan does not pull in the epic's other historical backlog
(font-family selection, bell, OSC 52, flow control, or links). Font-size settings,
zoom, underline/strike/cursor shape, keyboard input, bracketed paste, selection,
and mouse-wheel forwarding already ship and must stay working.

**Source of truth:** read `docs/HANDOFF.md` first. If the optional resize
follow-up is separately authorized, also read `docs/conpty-resize-reflow.md`;
the `resize_anchored` compensation and the DSR/CPR reply path are invariants,
not cleanup opportunities.

## Current baseline

The older epic describes a renderer and input path that have since changed. Plan
against the current seams:

| Area | Shipped seam | Actual remaining gap |
|---|---|---|
| Text paint | `frame::plan_paint` produces per-cell placements; `mono_paint_items` dedupes by `(char, GlyphStyle)`; `Session::show` caches galleys | `GlyphStyle` drops `BOLD`/`ITALIC`; only a regular font face is registered |
| Mouse | `CellMetrics::mouse_cell` maps to clamped 1-based protocol coordinates; `input::wheel_input` forwards wheel events; alacritty exposes 1000/1002/1003/1005/1006 modes | Press, release, held drag, and all-motion are never encoded; selection always wins |
| Search | Alacritty 0.26 provides `RegexSearch`, `RegexIter`, `search_next`, and `scroll_to_point`; selection already has buffer-to-viewport highlight geometry | Raw Ctrl+F becomes PTY byte `0x06`; there is no search model, overlay, navigation, or match paint |
| Optional resize companion | `resize_anchored` keeps ConPTY and the active alacritty grid aligned on height growth | The old Phase 5 also tracked bottom-snap behavior, but it is not one of the three requested features |

## Architecture

Keep `Session::show` as an adapter/orchestrator. Put behavior behind three small,
testable interfaces:

| Module | Interface | Complexity hidden behind it |
|---|---|---|
| New `src/terminal_font.rs` | build font definitions; return the font id for `(size, bold, italic)` | bundled faces, named egui families, fallback ordering, four-style mapping |
| Existing `src/input.rs` | normalized mouse event in, routing decision + PTY bytes out | tracking-mode precedence, SGR/UTF-8/legacy encoding, gesture capture, release/cancel, motion dedupe |
| New `src/search.rs` | search commands + terminal generation in, search view out | regex lifetime, bounded scanning/counting, current match, navigation, stale-range invalidation |

The terminal adapter remains responsible only for translating egui pointer/text
events, applying local selection/paste side effects, and painting returned rects.
Tests exercise the same module interfaces used by production.

Cross-feature precedence is fixed up front:

1. An app-wide modal or inactive pane cannot begin a new mouse-button capture.
   The already-shipped wheel-under-pointer path remains focus-independent.
2. While search is open, its keyboard never reaches the PTY; wheel means local
   history scroll and app mouse reporting is suspended.
3. A main-screen viewport that is scrolled into history is local/read-only;
   clicks on historical cells are never misreported as live app coordinates.
4. Outside search, Shift held at mouse press gives the whole gesture to Foreman
   selection. Otherwise an active app mouse mode owns the whole gesture.
5. Without app mouse mode, today's selection and right-click paste behavior is
   unchanged.
6. A gesture's owner and mouse encoding are frozen at press until its matching
   release, even if Shift, focus, or terminal mode changes mid-drag.

Raw Ctrl+F is free for search. `Ctrl+B, F` and `Ctrl+B, Ctrl+F` still go through
`WindowManager::pump_leader` and retain their terminal/project float commands;
no keymap rebind is needed.

## File map

| File | Planned change |
|---|---|
| `assets/fonts/` | matched Hack Regular/Bold/Italic/BoldItalic files plus license/provenance |
| `src/terminal_font.rs` | font registration/fallback module and tests |
| `src/main.rs` | declare the two new modules; replace inline font setup with `terminal_font` call |
| `src/input.rs` | search-open outcome; mouse protocol/router; wheel reuses the encoder; byte/state tests |
| `src/search.rs` | bounded scrollback-search state model and `Term<VoidListener>` tests |
| `src/terminal.rs` | style flags/font selection; mouse/search adapters; overlay paint; optional resize policy |
| `src/frame.rs` | neutral viewport-range geometry for selection and search |
| `src/geom.rs` | no new coordinate math expected; retain as the single pointer/cell seam |
| `src/theme.rs` | ordinary/current search highlight and search-bar tokens |
| `docs/` | feature notes, `foreman.md`, HANDOFF, and epic status after verification |

## Phase 1 — Register a matched four-face terminal font

The app currently renders egui's embedded Hack Regular. egui 0.34 has no font
weight in `FontId`, no synthetic bold, and `TextFormat::italics` is a synthetic
mesh shear. The epic requires real faces.

- [ ] Vendor one pinned Hack release as `Hack-Regular.ttf`, `Hack-Bold.ttf`,
  `Hack-Italic.ttf`, and `Hack-BoldItalic.ttf`. Record upstream version, source
  URL, SHA-256 values, and license in `assets/fonts/README.md`; include its
  license file. Do not mix the egui-bundled regular face with three faces from
  an unknown release.
- [ ] Add failing `terminal_font` tests for all four style combinations, family
  primary ordering, the complete ordered existing monospace fallback tail, and
  missing system fallback files. The default Monospace/Proportional family lists
  must remain byte-for-byte/order-equivalent to today's constructed definitions.
- [ ] Move the existing font-definition/fallback construction out of `main.rs`
  into `terminal_font`. Keep the external interface small: one function builds
  `FontDefinitions`, one returns a `FontId` for `(size, bold, italic)`.
- [ ] Register four named egui families. Each begins with its matching Hack face
  and then carries the *entire* current Monospace fallback list in the same order
  (including the existing default symbol/emoji faces and appended YaHei/Segoe
  entries). Named families do not inherit Monospace implicitly. Keep the default
  Monospace/Proportional families unchanged for non-terminal UI.
- [ ] Add a headless egui test that lays out representative glyphs through all
  four families and asserts nonzero glyphs, equal monospaced advance, and
  compatible line height/baseline. This catches corrupt or mismatched assets.

No settings schema change belongs here. User-selectable font family remains a
separate Phase 7 backlog item; the already-shipped global size/zoom stays intact.

## Phase 2 — Carry bold/italic through the cached painter

- [ ] First add failing tests to `glyph_style` for `BOLD`, `ITALIC`, their
  combination, reset behavior, and `DIM | BOLD` (dim changes color, not weight).
- [ ] Add `bold` and `italic` to `GlyphStyle`. Update its literals in
  `frame::text_rows`, terminal paint tests, and helpers. Its existing `Eq + Hash`
  automatically makes style changes split/dedupe galleys correctly.
- [ ] Add an ANSI-parser test whose adjacent cells use SGR regular, bold,
  italic, and bold-italic, proving `frame::plan_paint` exposes four distinct
  placements.
- [ ] Make the `Session::show` `"M"` metrics probe use the new regular terminal
  family. Mixing regular metrics from one face with painted glyphs from another
  is forbidden.
- [ ] In the galley layout closure, select `terminal_font::font_id(font_px,
  style.bold, style.italic)`. Leave `TextFormat::italics` false so the real
  italic face is not sheared twice.
- [ ] Add a cache/dedupe regression: the same scalar in regular/bold/italic/
  bold-italic produces four layouts, while an unchanged subsequent frame still
  produces zero layout calls.

`MonoPaintKey` needs no new field while the installed face set is immutable:
font size is already in the key and cell style is in `GlyphStyle`. A future live
font-family switch must add a font revision or explicitly invalidate every pane.

### Phase 2 acceptance

- Direct SGR output visibly distinguishes all four faces.
- `git --no-pager diff --color=always` and vim syntax show real weight/slant.
- Guides, selection, caret, CJK/emoji fallback, and min/default/max zoom remain
  cell-aligned.

## Phase 3 — Implement mouse protocol encoding and gesture state

Do not drive application reporting from `Response::drag_started`: egui delays it
until movement, but terminal protocols require a press immediately. Iterate raw
`Event::PointerButton` and `Event::PointerMoved` in order, then normalize through
`CellMetrics::mouse_cell`.

- [ ] Add a table-driven failing suite at the `input` mouse interface before
  changing `Session`.
- [ ] Model left/middle/right press, matching release, held-button motion,
  no-button motion, wheel-up, and wheel-down. Extra mouse buttons stay ignored.
- [ ] Implement tracking gates:
  - 1000 / `MOUSE_REPORT_CLICK`: press and release only;
  - 1002 / `MOUSE_DRAG`: press/release plus held-button cell motion;
  - 1003 / `MOUSE_MOTION`: press/release, held motion, and no-button motion.
- [ ] Implement xterm button codes: left/middle/right `0/1/2`, held motion
  `32/33/34`, no-button motion `35`, wheel `64/65`; modifiers add Shift `4`,
  Alt `8`, Ctrl/Command `16`.
- [ ] Implement encoding precedence: SGR 1006, then UTF-8 1005, then legacy.
  SGR release uses the original button code and lowercase `m`; legacy release
  uses code `3 + modifiers`. Drop an unencodable legacy/UTF-8 coordinate instead
  of silently clicking a saturated wrong cell.
- [ ] Route existing `wheel_input` through the same internal encoder without
  changing the current Ctrl+wheel zoom, mouse-mode, alternate-scroll, and local-
  scrollback precedence or one-event-per-notch feel.
- [ ] Add per-button capture state. Freeze gesture owner and the relevant
  `MOUSE_MODE | SGR_MOUSE | UTF8_MOUSE` bits at press. Track last cell and
  modifiers, and dedupe cell-identical motion so 1003 cannot flood the PTY on
  pixel-only movement.
- [ ] Make cancel/focus-loss/tab-hide produce exactly one matching release at
  the last clamped cell. A raw release outside the pane also completes capture.
  `PointerGone` only clears hover motion; it does not itself prove the button is
  up.

Minimum byte cases include:

- SGR left press at `(5,10)`: `ESC[<0;5;10M`;
- SGR right release: `ESC[<2;5;10m`;
- SGR left drag: `ESC[<32;5;10M`;
- SGR no-button motion: `ESC[<35;5;10M`;
- legacy left press: `1b 5b 4d 20 25 2a`;
- legacy release: `1b 5b 4d 23 25 2a`.

Tests must also cover mode rejection, modifier combinations, SGR-over-UTF-8
precedence, legacy 223/224 and UTF-8 2015 limits, press+release in one frame,
mode/Shift changes mid-gesture, outside release, inactive cancellation,
multi-button release, and same-cell motion dedupe.

## Phase 4 — Integrate mouse routing without regressing selection

- [ ] Add the raw-event adapter before the existing selection block in
  `Session::show`. A new capture requires both `active` and ownership by this
  pane's topmost content `Response`; `rect.contains(pos)` alone is forbidden
  because raw egui pointer events are global and overlapping panes also see
  them. This preserves app-wide modal safety and makes the first click on an
  unfocused pane focus-only. Only a pane with an existing capture may accept a
  later global release outside its response.
- [ ] Decide the route at press and keep it for the whole gesture:
  - Shift at press, nonzero scrollback offset, or no app mouse mode -> local;
  - otherwise -> application.
  Releasing Shift during a local drag must not turn it into an app drag; pressing
  Shift mid-app drag must not strand the app's pressed button.
- [ ] In application route, send the immediate press/motion/release and suppress
  local selection mutation, plain-click clearing, and right-click paste.
- [ ] In local route, retain semantic/line/simple selection and paste. Make the
  response queries primary-button-specific so middle/right clicks cannot start
  selection accidentally.
- [ ] In 1002, emit motion only while a captured button is held. In 1003, emit
  hover motion only for the response that actually owns the pointer, not every
  overlapping pane whose rectangle contains it.
- [ ] On inactive/focus-loss/tab-hide, synthesize exactly one release from the
  captured mode/cell and clear that capture. Ignore the later physical release;
  repeated `keepalive` calls must emit nothing. This makes a focus transition
  deterministic and prevents vim/htop retaining a pressed button.
- [ ] Preserve the existing wheel-under-pointer behavior. Shift override applies
  to click/drag selection, not wheel.
- [ ] Add a regression that clicking while the primary grid is scrolled into
  history stays local and emits no app coordinates. Alternate-screen TUIs remain
  unaffected because their active grid has no scrollback.
- [ ] Add an overlapping-floats regression: a raw press over the top pane's
  content yields bytes only from that pane; a press on a titlebar yields no PTY
  bytes from either pane.

Focus-and-forward on the very first click is deliberately not part of this
change. It would require a distinct pointer-permission flag threaded through the
recursive window manager; simply removing `active` would re-enable background or
modal injection.

### Phase 4 acceptance

- Vim `:set mouse=a`: click moves the cursor and an app-owned drag selects in
  vim; Shift+drag produces Foreman's local selection instead.
- Lazygit panels/rows and htop headers react to clicks.
- App-owned right-click never pastes; right-click pastes again after mouse mode
  ends (or for a Shift-owned local gesture).
- Dragging out of a pane and releasing never leaves the application stuck.

## Phase 5 — Build the bounded scrollback-search model

Use alacritty's own search implementation; add no regex crate. The relevant
0.26 interfaces are `RegexSearch::new`, `RegexIter::new`, `Term::search_next`,
and `Term::scroll_to_point`. Full-buffer bounds are `topmost_line/Column(0)` to
`bottommost_line/last_column`.

- [ ] Add `src/search.rs` and test it with `Term<VoidListener>` before adding UI.
- [ ] Model `Closed`, `Editing`, and `Navigating` states. Editing treats the
  field as a regex using alacritty's smart-case behavior; invalid regex is a
  visible nonfatal state and empty query does no work.
- [ ] On a changed query, compile once and start a resumable scan. Search at most
  a fixed line chunk (no more than 1,000 lines) and a small wall-time budget per
  frame, persist the next `Point`, and request another repaint until complete.
  There is no unlimited UI-thread scan—not after 500 ms and not on navigation.
  Next/previous navigation uses the same bounded engine and may show `…` until
  its target is found.
- [ ] Do not retain every match. A query like `.` can match millions of cells.
  Retain only the focused match and visible matches; cap counting (for example
  at 100,000) and report `100000+` honestly when truncated.
- [ ] Initial search anchors at the current viewport rather than always jumping
  to the oldest history. Next/previous use `search_next` and wrap; jumping uses
  `scroll_to_point`.
- [ ] Cache against `(content_gen, cols, rows)` and separately against
  `display_offset`. PTY output or resize invalidates stored `Match` coordinates;
  a scroll-only change only rebuilds visible ranges. Never paint stale points.
- [ ] Coalesce output-driven invalidation: clear stale ranges immediately, but
  restart the bounded scan only after a short quiescence window or an explicit
  navigation command. Continuous output must not restart a full-history scan on
  every `content_gen` change.
- [ ] Generalize the existing selection buffer-to-viewport conversion and
  per-row rect expansion into neutral range helpers in `frame.rs`. Selection and
  search must share this geometry for wrapped lines, wide chars, scroll offsets,
  and stale-coordinate clamping.

Search tests must prove scrollback (not only viewport) matches, smart case,
invalid regex, current-viewport anchoring, next/previous wrap, correct ordinal/
count cap, wrapped multi-row matches, CJK/emoji spacer coverage, output/resize
invalidation, and empty-query behavior. Instrument the scan seam so tests also
prove one update never exceeds its line/work budget and continuous output does
not cause unbounded rescans.

## Phase 6 — Add search input ownership, overlay, and match paint

- [ ] Extend `InputOutcome` with `open_search`. In `process_input`, recognize
  exact Ctrl/Cmd+F (no Shift/Alt). If it appears anywhere in the frame, return
  an open-search outcome with **no PTY bytes or other terminal side effects from
  any keyboard event in that frame**. Drain its companion text and add
  Ctrl+F+Text, Ctrl+F+Enter, and Ctrl+F+another-key regressions proving neither
  `0x06` nor neighboring input reaches the PTY.
- [ ] In `Session::show`, remember whether search was open at frame start. If it
  was, skip terminal `read_input` for the whole frame even if Escape closes it;
  Escape/Enter/n must never leak. Ctrl+F on a closed search opens/focuses it.
- [ ] Use a stable TextEdit id derived from `term_id`. While `Editing`, Enter
  confirms and moves to `Navigating`. While navigating, Enter or `n` advances,
  Shift+Enter or `N` goes backward, Ctrl+F returns to editing, and Escape closes
  while leaving the viewport at the match. This state split avoids making `n`
  both query text and navigation.
- [ ] Keep that TextEdit's egui focus for the entire open search, including
  `Navigating`, and consume navigation keys plus their companion `Event::Text`
  records before the field sees them. Add `n`/`N` key+text regressions. This
  keeps the desktop's focus-aware `pump_commands` dormant too: search must not
  leak into the PTY, mutate the query accidentally, or arm/dispatch a window-
  manager chord.
- [ ] Surrender the TextEdit focus when its terminal becomes inactive/hidden,
  while retaining the per-session query/results. Reacquire it only when that
  session is active again; a hidden tab must never steal another pane's input.
- [ ] While search owns input, hide the terminal caret, suspend app mouse
  reporting, and make wheel gestures scroll Foreman's history. Pointer activity
  outside the search bar may still use local selection.
- [ ] If search opens during an app-owned mouse gesture, synthesize its one
  matching release before suspending reports. Search must never strand the TUI
  in a pressed state.
- [ ] Paint ordinary matches first, current match second, selection third, then
  caret/thumb. Search rectangles are overlays and must not invalidate the mono
  galley cache.
- [ ] Paint the search bar last inside the pane's normal draw order (not a global
  foreground layer that can leak over a higher floating window). Place it at the
  content rect's top-right without consuming a terminal row or triggering a PTY
  resize. Show query, `current / total` (or capped total), invalid state, and a
  compact key hint.
- [ ] Add theme tokens for ordinary/current matches and the bar. Keep selection
  visually distinct and verify contrast over all ANSI backgrounds.

No `src/keymap.rs` or leader change is expected. Keep its existing float tests
green and add explicit regressions that the prefixed Ctrl+F chord still maps to
project float while Ctrl+B cannot arm the leader during an open search.

## Optional follow-up — legacy Phase 5 resize companion

This is **not part of the user's enumerated three-feature remainder and must not
be implemented without separate authorization**. It is recorded here only
because the historical epic combined it with search under one Phase 5 checkbox.
Search itself must still discard/rebuild match coordinates safely on resize.

If separately authorized, make only the viewport-policy change; do not touch
`resize_anchored` or implement another reflow.

- [ ] In `Session::resize`, record whether `display_offset == 0` before
  `resize_anchored`. Snap to `Scroll::Bottom` only if it was already at bottom.
  For a nonzero offset, let alacritty's grid resize/reflow adjust it; do not
  restore the old numeric offset manually.
- [ ] Add pure tests: bottom remains offset zero; a scrolled marker remains
  visible through height change; wrapped scrolled content stays near the same
  reading point through width change; an active search drops/rebuilds stale
  coordinates and returns to its focused match.
- [ ] Re-run existing `resize_anchored` and selection tests unchanged.
- [ ] Run the ignored ConPTY probes from `docs/conpty-resize-reflow.md`:
  `typed_echo_lands_on_the_prompt_after_a_height_grow`, `resize_recall_probe`,
  and `resize_drag_probe`. The viewport change must not regress cursor/typing
  behavior. Known stale-wrap/content residuals remain known and must not be
  claimed fixed.

If a ConPTY probe regresses, revert only this viewport-policy subtask, keep
search, and split the epic's combined Phase 5 checkbox into “search” and
“resize preservation” with the latter still open. Do not weaken
`resize_anchored` to force the checkbox green.

## Phase 7 — Integrated verification and documentation

### Automated loop

Run focused tests after each phase, then the full suite. On Windows:

```powershell
if ($env:FOREMAN -eq '1') {
    cargo build --target-dir target/agent
    cargo test --target-dir target/agent
} else {
    Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
    cargo build
    cargo test
}
```

Do not use `cargo test --lib`; this is a binary crate.

### Visual/interactive matrix

- [ ] Font fixture: paint identical regular/bold/italic/bold-italic strings,
  inspect `foreman snapshot --attrs`, and capture a screenshot. Structured attrs
  prove parser state; only the screenshot proves weight/slant.
- [ ] Raw mouse probe: enable 1000, 1002, 1003, and 1006 in turn and display
  received bytes. Verify immediate click, held drag, hover motion, Shift local
  override, right-click suppression, and outside release. Also exercise vim and
  any available lazygit/htop install.
- [ ] Search fixture: generate at least 300 numbered history lines with repeated
  and unique terms. Verify off-screen jump, visible/current highlights, count,
  invalid regex, Enter/n/N wrap, wrapped regex, wide characters, resize, and
  streaming output while the bar owns input.
- [ ] Verify nested panes/tabs: only the focused session accepts Ctrl+F/new mouse
  capture, inactive PTYs keep pumping, and switching tabs never leaves a hidden
  search field or mouse button owning input.
- [ ] Build/launch the requested target, capture `win.png` through the repo's
  build-screenshot workflow, and inspect narrow/default/zoomed panes. Do not use
  Win32 mouse automation without telling the user; mouse “feel” remains a human
  acceptance check.

### Documentation/status

- [ ] Add concise feature notes for font styles, mouse reporting, and search.
- [ ] Update `docs/foreman.md` and the current-state section of `docs/HANDOFF.md`.
- [ ] Update the epic's Phase 1b, remaining Phase 3, and search/checklist status
  only after their automated and acid tests pass. Split the old combined Phase 5
  checkbox so optional resize preservation remains visibly open unless it was
  separately authorized and verified. Keep font-family selection open.

## Final definition of done

- Bold, italic, and bold-italic are real matched faces and remain grid-aligned.
- Vim/lazygit/htop receive correct click/release/drag/motion bytes; Shift-drag
  still selects locally; no lost release leaves a TUI stuck.
- Ctrl+F searches the focused pane's full history with bounded work, visible and
  current highlights, count/error state, and non-leaking navigation.
- The render cache still performs zero layouts on unchanged frames, 1003 motion
  is cell-deduped, and search does not rescan the full history every paint.
- Build, full tests, screenshot inspection, and manual mouse/search acid tests
  are recorded before the three-feature remainder is marked complete. ConPTY
  probes are additionally required only if the optional resize follow-up runs.
