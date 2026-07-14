# Terminal Completeness — Epic

**Active remainder plan (2026-07-13):**
[`docs/superpowers/plans/2026-07-13-terminal-completeness-remainder.md`](../superpowers/plans/2026-07-13-terminal-completeness-remainder.md)
covers the requested unfinished owner slice: bold/italic, mouse click/drag/
motion, and scrollback search. The broader phase catalog below remains the
historical record; unrelated unchecked polish is not silently pulled into that
plan.

**Status:** remainder slice implemented; correctness fix pass (2026-07-14) —
**not signed off.** Real bold/italic faces, mouse click/drag/motion, and
bounded scrollback search land under
[`docs/superpowers/plans/2026-07-13-terminal-completeness-remainder.md`](../superpowers/plans/2026-07-13-terminal-completeness-remainder.md).
Correctness fixes: query changes search without Enter; content quiescence
non-sliding; one shared line/time budget per frame; deep-history wrap once;
focused ordinal reconcile; search-bar hit exclusion; topmost mouse ownership;
1003 suppress under any capture/history; same-frame focus-loss block;
unencodable press no ghost drag/release; pairwise-distinct face proof.
Optional resize-preservation stays open (not authorized). Full suite still has
3 pre-existing WM dock test failures. Human acid matrix + screenshot
acceptance still open — do **not** treat this epic as complete.

**Goal:** make the terminal *feel like a finished product* before any more
"built for AI" work. Today it renders a shell and verifies green, but it is not
yet a terminal a developer will tolerate as a daily driver — several basics that
every modern terminal has are missing. This epic closes that gap and nothing
else: no AI features, no window-manager work.

**Read first:** `docs/HANDOFF.md` (gotchas — DSR trap, egui 0.34 painter rules),
then this file. Source under review is `src/terminal.rs` (the `Session`: PTY +
`alacritty_terminal` grid + render + input). The market-viability review that
spawned this epic flagged these as the "Tier 1a" daily-use rejectors.

---

## Why it exists

Foreman's pitch is speed + AI supervision. But the AI agents we run (Claude
Code, Codex CLI) are themselves TUIs, and a developer also drops into vim,
lazygit, htop, less, and git inside these panes constantly. Right now:

- **bold/italic/underline are invisible** — every styled output reads flat;
- **the mouse does nothing in any TUI app** — lazygit/htop/vim mouse is dead;
- **F-keys, Delete, Insert, Alt-combos never reach the shell** — vim and
  readline are crippled;
- **interactive paste isn't bracketed** — multi-line paste submits line-by-line;
- **no scrollback search, no font choice, no window title, no bell.**

A user who hits any one of these in the first ten minutes leaves. The terminal
must be *unremarkable* (in the good way) before the AI layer can matter.

## The acid test (run this before and after every phase)

Open these inside a foreman pane and actually use them. They are the target
workload and they exercise every gap below at once:

1. **Claude Code** and **Codex CLI** — colors, styled text, key handling,
   smoothness under streaming output.
2. **vim** — `:syntax on` (colors + bold), F-keys, `Alt+b`/`Alt+f`, mouse click
   to position cursor, `i`-mode cursor shape.
3. **lazygit** / **htop** — mouse click and scroll on the UI.
4. **less** / **man** — bold headings, `/pattern` search inside the app.

If the agent CLIs themselves degrade, that outranks everything else here.

---

## Scope

In scope: text-attribute rendering, cursor styles, full keyboard, mouse
reporting to apps, selection UX, scrollback search, window title + bell, font
config, clipboard (OSC 52), and heavy-output flow control. Hyperlinks (OSC 8)
are included as the one optional polish item.

Out of scope (other epics / later): sixel/kitty image protocols, ligatures
(epaint can't), per-pane split panes, AI state detection, daemon/persistence.

## Current state — honest snapshot (what already works)

So phases don't re-do shipped work:

- Real PTY via `portable-pty`/ConPTY; DSR handshake answered (`pump`,
  `terminal.rs:459`); resize propagates to the PTY every frame (`resize`,
  `terminal.rs:480`).
- Truecolor + 256-color + 16-color ANSI palette all resolve (`resolve`,
  `indexed_rgb`, `terminal.rs:38-84`). **Inverse** and **dim** are honored
  (`terminal.rs:761-771`).
- Scrollback buffer + mouse-wheel scroll + a right-edge thumb indicator
  (`terminal.rs:704-713, 825-842`); Shift+Home/End/PageUp/PageDown scroll the
  buffer (`terminal.rs:559-578`).
- Drag-to-select with copy (Ctrl+C / Ctrl+Shift+C) and right-click / Ctrl+V /
  Ctrl+Shift+V paste (`read_input`, `terminal.rs:530-648`).
- Arrow keys, Home, End, Enter, Tab, Backspace, Esc, and Ctrl+letter control
  codes are sent (`terminal.rs:580-612`).
- Bracketed-paste wrapper exists, but is used **only** for chat injection, not
  for user paste (`paste_wrap`, `terminal.rs:184`).

---

## Phases

Each is an independent session, written to be picked up cold. Effort is rough:
S = a sitting, M = a day-ish, L = multi-day.

**Phase numbers are stable IDs, not the running order** — so references stay put
as the plan evolves. Recommended execution order, by impact and dependency:

1. **Phase 2** (keyboard) + **Phase 1a** (cheap font-independent attributes) —
   cheapest rejectors, no dependencies, immediate acid-test win.
2. **Phase 7** (fonts + a small font config file) — unblocks bold/italic.
3. **Phase 1b** (bold/italic) — done once, on real font faces (no faux-bold
   throwaway).
4. **Phase 3** (mouse reporting).
5. **Phase 4** (selection) → **Phase 5** (search).
6. **Phase 6** (title/bell) — rides Phase 7's settings surface.
7. **Phase 9** (flow control) → **Phase 8** (OSC 52) → **Phase 10** (links,
   optional).

The `REJECTOR` tag marks phases that block daily use — do those first regardless.

### Phase 1a — Font-independent attributes (underline / strikethrough / cursor shape) · S · REJECTOR

**Problem.** The render loop honors only `INVERSE` and `DIM`. `UNDERLINE` and
`STRIKEOUT` are read off the grid by alacritty but silently dropped — the
`TextFormat` is built with `..Default::default()` (`terminal.rs:750-755`).
Separately the cursor is always a filled block: `show` reads `CursorShape` only
to test "not hidden" (`terminal.rs:720`) and ignores bar/underline
(`terminal.rs:815-823`). None of this needs fonts, so it ships first for an
immediate visible win.

**Where it lands.** The per-cell loop and `flush` closure in `show`
(`terminal.rs:736-781`); the cursor draw at `terminal.rs:815-823`.

**Approach.**
- Map `UNDERLINE` → `TextFormat.underline` and `STRIKEOUT` →
  `TextFormat.strikethrough` (both are `Stroke` fields). Render `UNDERCURL` /
  `DOUBLE_UNDERLINE` as a plain underline for now (egui has no squiggle); note the
  downgrade in a comment.
- Fold the new attributes into the run-break check at `terminal.rs:772` so runs
  split when any attribute changes, not just fg/bg.
- Cursor shape: honor `CursorShape::Block` / `Beam` / `Underline` (bar = thin
  vertical rect, underline = thin bottom rect). Optional blink via a time-based
  alpha. Bundled here so the whole cheap-visible-attrs set lands in one session.

**Done when.** `man bash` shows underlines; underlined/struck text renders; vim in
insert mode shows a bar cursor (most shells switch the cursor on insert).

### Phase 1b — Bold / italic · S · REJECTOR · depends on Phase 7

**Problem.** `BOLD` and `ITALIC` are dropped the same way, so emphasized output
reads flat — git diff headers, `ls --color`, man pages, the agent CLIs' own
emphasis. Unlike 1a these need real font faces, which is why they wait for
Phase 7 instead of shipping a faux-bold hack we'd throw away.

**Where it lands.** The same `flush` / run-break code as 1a; selects the
bold/italic `FontId`s registered by Phase 7.

**Approach.**
- **bold**: epaint has **no synthetic bold** — select a registered bold monospace
  face (Phase 7) by `FontId`. (The only pre-Phase-7 option is faux-bold: draw the
  run twice with a 1px x-offset — a throwaway, which is exactly why 1b waits.)
- **italic**: set `TextFormat.italics` if egui 0.34 exposes it; otherwise select a
  registered italic face. epaint has no synthetic slant either.
- Add `BOLD` / `ITALIC` to the run-break check alongside the 1a attributes.

**Done when.** `git --no-pager diff` shows bold headers; vim syntax highlighting
shows bold keywords; italic comments render slanted.

### Phase 2 — Full keyboard (F-keys, Delete, Insert, Alt/Meta, keypad) + bracket user paste · S · REJECTOR

**Problem.** `read_input` (`terminal.rs:580-613`) maps arrows/Home/End/Enter/
Tab/Backspace/Esc and Ctrl+letter — and nothing else. Missing: **F1–F12**,
**Delete**, **Insert**, **unshifted PageUp/PageDown**, and **Alt as Meta**
(`modifiers.alt` is never checked). Readline word-skip (`Alt+b`/`Alt+f`), vim
function keys, and many TUIs are broken. Separately, interactive paste sends raw
clipboard bytes (`terminal.rs:541, 624`) with **no bracketed-paste wrapping**, so
a multi-line paste runs line-by-line and a leading `"` can wreak havoc.

**Where it lands.** A NEW pure module `src/input.rs` (arch candidate A, design
grilled 2026-06-25) holds the encoding; `read_input` (`terminal.rs:530-648`)
shrinks to a thin shell that calls into it. The escape-sequence detail below is
what the module emits.

**Approach.**

*The seam — `src/input.rs` (pure: no `Ui` / `Term` / `Session` / painter):*
- `process_input(events: &[egui::Event], mode: TermMode, has_selection: bool)
  -> InputOutcome` owns the whole event match/filter loop. `InputOutcome {
  pty_bytes, copy, interrupt, scroll, paste_clipboard }`. Real egui/alacritty
  types in, bytes out — tests build a `Vec<egui::Event>` and assert exact bytes.
- `paste_seq(mode, text) -> Vec<u8>` — bracketed-paste wrap gated on
  `TermMode::BRACKETED_PASTE`, ESC stripped (generalizes today's `paste_wrap`;
  the chat path `inject_input` may keep its unconditional wrap).
- `mods_param(m) -> u8` = `1 + shift + 2*alt + 4*ctrl`, feeding the CSI forms.
- Table-driven byte-equality tests live here — the interface IS the test surface
  (today nothing tests key encoding).
- The thin shell (`read_input`) keeps the side effects: gather events, call
  `process_input`, then `send(pty_bytes)` → if `paste_clipboard`, read the
  clipboard and `send(paste_seq(mode, &text))` → `copy` ⇒ `ctx.copy_text` →
  `interrupt` ⇒ send `0x03` → `scroll` ⇒ `scroll_display`. Send `pty_bytes`
  before the clipboard paste when both occur in one frame. (Preserve today's
  Ctrl+C policy: copy if selection, else SIGINT — now a tested decision.)

*What the module emits (the actual gap fixes):*
- F1–F4 `ESC O P/Q/R/S`, F5–F12 `ESC[15~…`, Delete `ESC[3~`, Insert `ESC[2~`,
  PageUp `ESC[5~`, PageDown `ESC[6~`.
- **DECCKM**: arrows + Home/End emit the `ESC O _` form when `mode` has
  `APP_CURSOR`, else `ESC[_` (today they're hardcoded to the CSI form — wrong
  inside vim/less/htop).
- **Modifiers**: CSI `1;<n>` param (via `mods_param`) on arrows/Home/End/F-keys
  when Ctrl/Shift/Alt are held (`Ctrl+Right` = `ESC[1;5C`).
- **Alt/Meta**: Alt+letter/text ⇒ `ESC`-prefixed byte. The wm leader (`Ctrl+B`)
  consumes its chord first, so Alt-meta never collides.
- Defer keypad-application-mode and focus-reporting (mode 1004) — noted TODO.

**Done when.** In vim: F1 opens help, `Alt+b`/`Alt+f` skip words in command mode,
Delete deletes forward. A multi-line paste into vim's insert mode lands as one
block (with `:set paste` or bracketed-paste-aware vim).

### Phase 3 — Mouse reporting to TUI apps · M · REJECTOR

**Status: wheel/scroll forwarding built (2026-06-27), green (310 tests).** The
*scroll* half of Phase 3 shipped; click/drag/motion reporting (below) is still
designed-not-built. What landed:

- `src/input.rs`: `WheelAction { Pty(Vec<u8>), Scrollback(Scroll) }` and the pure,
  byte-tested `wheel_input(delta_lines, mode, col, row) -> WheelAction` (8 tests),
  behind the existing input seam. Precedence: (1) any `MOUSE_MODE` flag → forward
  the wheel as mouse events — SGR `ESC[<64/65;col;row M`, or legacy X10
  `ESC[M`+3 offset-by-32 bytes — one event per line; (2) `ALT_SCREEN` +
  `ALTERNATE_SCROLL` → arrow keys via `encode_key` (so APP_CURSOR is honored);
  (3) else foreman's local scrollback (unchanged). This is why a full-screen TUI
  (Claude, vim, less) finally scrolls — its alt screen has no foreman scrollback,
  so the wheel must reach the app.
- Adapter (`terminal.rs` wheel handler): the `Pty` (forwarding) branch is gated on
  `active`, like the key path — hovering an unfocused pane must never inject
  keys/mouse into it; the read-only `Scrollback` branch stays on any hovered pane.
- Feel: a `Session.scroll_accum` carries the sub-line remainder of
  `smooth_scroll_delta`, so gentle notches aren't rounded to zero (felt dead) and
  fast flicks don't over-emit lines (lurch). `raw_scroll_delta` does not exist in
  egui 0.34 — only the smoothed delta, hence the accumulator.

Still to do for full Phase 3: click / drag / motion via the `encode_mouse` sibling
fn, the Shift-override to force local selection, and suppressing text selection
while an app holds the mouse.

**Problem.** All mouse events are consumed for text selection / scroll
(`terminal.rs:680-713`). There is no path that converts pointer events into mouse
escape sequences, so vim/lazygit/htop/ranger get zero mouse input.

**Where it lands.** A new branch in `show` before the selection handling, keyed
on `self.term.mode()`. The `Listener` (`terminal.rs:113-121`) stays as-is; mouse
bytes go out through `send`.

**Approach.**
- Encoding is a sibling pure fn in `src/input.rs` (arch candidate A):
  `encode_mouse(btn, cell, kind, mode) -> Vec<u8>` — table-tested like the
  keyboard path. The shell computes the cell (`cell_at`) and calls it; the module
  never touches render geometry.
- Read the mouse-mode flags from `term.mode()`: click (`MOUSE_REPORT_CLICK`),
  drag (`MOUSE_DRAG`), motion (`MOUSE_MOTION`), and SGR encoding (`SGR_MOUSE`).
  (Confirm exact `TermMode` flag names against alacritty_terminal 0.26.)
- When a mouse mode is active: the shell translates egui press/release/drag/scroll
  at the hovered cell into the bytes from `encode_mouse` (SGR `ESC[<b;x;yM` press /
  `m` release, or legacy `ESC[M` when SGR is off) and sends them; **suppress** text
  selection while app mouse mode is on.
- When no mouse mode is active: keep today's drag-select behavior unchanged.
- Shift-override: hold Shift to force local selection even when an app grabs the
  mouse (xterm convention) — lets users still copy out of htop.

**Done when.** In lazygit a click selects a panel; in htop clicking a column
header sorts; in vim `set mouse=a` lets a click move the cursor. With Shift held,
drag still selects locally.

### Phase 4 — Selection UX (word / line / wide-char correctness) · S–M

**Problem.** Selection is drag-only and tracks raw `(row,col)` with no semantic
awareness (`selection_text`, `terminal.rs:382-418`; highlight `789-813`). No
double-click word, no triple-click line, and wide (CJK/emoji) cells aren't
accounted for — a 2-column glyph is treated as one column, so selections and
copies drift on CJK text.

**Where it lands.** The mouse handling in `show` (`terminal.rs:680-695`) for
click-count; `selection_text` and the highlight rect math for wide-cell spanning.

**Approach.**
- Use egui's multi-click (`resp.double_clicked()` / triple) or count clicks
  within a time window. Double = expand to word boundaries; triple = whole line.
- Migrate selection to alacritty's own `Selection` type + `term.selection_to_string()`
  (plus its semantic word/line helpers). This is the committed approach: it
  understands word boundaries and wide-char spacers, fixes the CJK drift for free,
  and deletes the hand-rolled grid walk in `selection_text` (`terminal.rs:382-418`).
- One range, two readers (arch candidate C): the render highlight must read the
  SAME `Selection`, not a second hand-rolled row walk (`show`,
  `terminal.rs:789-813`). If only copy migrates, copy uses `Selection` semantics
  while the highlight uses raw rows and the two drift on wide chars — the very bug
  this phase fixes for copy. Both "text to copy" and "cells to highlight" come from
  the one selection.
- Preserve the out-of-bounds safety the hand-rolled version has: it clamps every
  index to the grid's REAL bounds because an alt-screen / resize shrink can strand
  stale selection coords and `Line`/`Column` panic when indexed out of range (the
  comment at `terminal.rs:390`; same hazard guarded in the render loop). Verify the
  drag-tracked anchor/head can't feed `Selection` stale points across a grid shrink.

**Done when.** Double-click selects a whole path/word; triple-click selects the
line; selecting across a line of CJK copies the right characters and the
highlight aligns with the glyphs.

### Phase 5 — Scrollback search + keep scroll position on resize · M

**Problem.** No way to find earlier output — the single most common scrollback
need. Also, every resize snaps the viewport to the bottom (`resize`,
`terminal.rs:488-490`), so a window drag throws away where you were reading.

**Where it lands.** New search state + overlay in `show`; the snap at
`terminal.rs:490`.

**Approach.**
- Build on alacritty's regex search (`RegexSearch` + `term.search_next`/match
  iteration in 0.26 — confirm the API). Ctrl+F opens a small input; type to
  highlight all matches; Enter / `n` / `N` jump; show a match count.
- Highlight matches by drawing a translucent rect per match cell in the render
  pass (same mechanism as the selection highlight).
- Resize fix: only snap to bottom when the viewport was already at the bottom
  (`display_offset() == 0`); otherwise preserve the offset across the reflow.

**Done when.** Ctrl+F finds a string scrolled off-screen, highlights it,
`n`/`N` cycle matches; resizing the window while scrolled up keeps your place.

### Phase 6 — Window/tab title (OSC 0/2) + bell · S

**Problem.** `Listener::send_event` only handles `Event::PtyWrite`
(`terminal.rs:113-121`). Shell-set titles (`\e]0;…\a`, common in zsh/bash PS1)
and `Event::Bell` are dropped — so tabs always show a static label and there's no
bell feedback.

**Where it lands.** The `Listener` (add `Event::Title`, `Event::ResetTitle`,
`Event::Bell` arms) and a way to surface the title up to the `Win` titlebar in
`wm.rs`.

**Approach.**
- Capture all alacritty `Event`s into ONE typed sink (arch candidate B), not a
  fresh `Arc<Mutex>` cell per kind. `resp` (the PtyWrite buffer) is already this
  pattern with a single field — generalize it once into a `TermEvents { pty_write,
  title, bell, … }` the `Listener` fills and `pump()` drains in one place; expose
  `Session::title()` / `take_bell()` off it. Phase 8 (OSC 52) adds its clipboard
  fields to the SAME sink rather than a new cell.
  - Fence: keep "first `pty_write` flush ⇒ `ready`" intact (`pump`,
    `terminal.rs:463-470`) — that latch answers the startup DSR; lose it and the
    black-pane trap returns.
- In `wm.rs`, prefer the live OSC title over the static `Win.title` when present
  (the existing exit-stamp logic in `refresh_exit_titles` is the pattern).
- Bell: visual flash of the pane border (amber) and/or an optional sound; make it
  a setting once Phase 7 has a settings surface. Debounce.

**Done when.** Running `printf '\e]0;hello\a'` retitles the tab; `printf '\a'`
flashes the pane.

### Phase 7 — Font & appearance config (family / size / zoom) · M

**Problem.** Font is hardcoded `egui::FontId::monospace(13.0)` in two places
(`terminal.rs:670, 751`). No family choice, no size, no Ctrl+= / Ctrl+- / Ctrl+0
zoom. Developers want their own monospace (Cascadia, JetBrains Mono, Fira), and
Phase 1b's bold/italic need real font faces.

**Where it lands.** A small font config file (`%APPDATA%\foreman\settings.json`,
mirroring the keymap load/merge in `keymap.rs`) and font registration in
`main.rs` startup; `show`/`flush` read the configured `FontId`.

**Approach.**
- Register user font files into egui's `FontDefinitions` at startup, with
  regular/bold/italic faces so Phase 1b can select them.
- A `TerminalSettings { font_family, font_size }` loaded once; per-pane zoom
  multiplier adjustable with Ctrl+= / Ctrl+- / Ctrl+0.
- Ligatures: out of scope — epaint shapes glyph-by-glyph. Note it explicitly so
  it isn't re-litigated.
- Keep the file minimal — just these font keys. Do NOT build a general settings
  system here; later phases (e.g. Phase 6's bell sound) add their own keys to the
  same file when they actually need them.

**Done when.** Changing font + size in the config (or via zoom keys) takes effect
on all panes; bold/italic from Phase 1b render with the registered faces.

### Phase 8 — Clipboard completeness (OSC 52) · M · polish

**Problem.** Apps can't set/read the system clipboard via OSC 52 — breaks
tmux-over-SSH copy and neovim clipboard integration.

**Where it lands.** OSC handling reaches the `Listener` as
`Event::ClipboardStore` / `Event::ClipboardLoad` (confirm names) — add arms that
go through `arboard` (already a dep; `read_clipboard` exists, `terminal.rs:177`).

**Approach.** On store, write to `arboard`; on load, reply with the clipboard
contents through the `resp` write-back path. Guard size and consider a paste-safe
policy (don't auto-execute). Default the read side to off or prompt, since a
remote app reading your clipboard is a mild risk.

**Done when.** `printf '\e]52;c;%s\a' $(echo hi | base64)` puts `hi` on the
system clipboard.

### Phase 9 — Heavy-output flow control · M · protects the "fast" claim

**Problem.** The reader thread sends every 8 KB chunk (`terminal.rs:305-318`) and
`pump` drains the whole channel each frame (`terminal.rs:459-462`) with no
batching or back-pressure. A verbose agent (exactly our workload) flooding stdout
can build a backlog and stutter the UI — undermining the one thing foreman sells.

**Where it lands.** The reader thread and `pump`.

**Approach.**
- Cap bytes parsed per frame (e.g. a few hundred KB) and carry the remainder to
  the next frame, so one noisy pane can't stall the whole compositor.
- Coalesce reads in the reader thread; optionally drop intermediate frames under
  sustained flood (alacritty's grid is the source of truth, so skipping render
  frames is safe — only the final state must be correct).
- Measure: `yes`, `cat` a large file, a chatty build. Watch frame time.

**Done when.** `yes` in one pane keeps the UI at full frame rate and other panes
stay responsive.

### Phase 10 (optional) — Clickable URLs / OSC 8 hyperlinks · M · polish

Detect URLs in the grid (and honor OSC 8 hyperlink ranges) and make them
clickable with a hover underline. Lowest priority; do last or defer.

---

## Definition of done — "the terminal feels complete"

The acid-test apps above all behave correctly, plus this checklist (in execution
order):

- [x] Underline / strikethrough render (Phase 1a) — `glyph_style`, unit-tested; visual look not yet acid-tested
- [x] Cursor shape (block / bar / underline) follows the app (Phase 1a) — blink still optional, not done
- [x] F-keys, Delete, Insert, PageUp/Down, Alt-as-Meta reach the shell (Phase 2) — `src/input.rs`, 27 tests
- [x] User paste is bracketed when the app supports it (Phase 2) — mode-gated `paste_seq`
- [~] Font family/size configurable; zoom keys work (Phase 7) — size/zoom shipped; family picker still open
- [x] Bold / italic render with real font faces (Phase 1b) — Hack four-face set; `docs/terminal-font-styles.md`
- [x] Mouse works in vim / lazygit / htop; Shift forces local select (Phase 3) — wheel (2026-06-27) + click/drag/motion (2026-07-14); `docs/terminal-mouse-reporting.md`
- [x] Double-click word, triple-click line; CJK selection is correct (Phase 4) — alacritty `Selection` in `term.selection`, acid-tested (word/path/line highlights + CJK copy hex-verified); see `docs/terminal-selection.md`
- [~] Ctrl+F searches scrollback; resize keeps scroll position (Phase 5) — **search done** (`docs/terminal-scrollback-search.md`); **resize preservation** still open (optional follow-up, not authorized with the remainder)
- [ ] Tab title follows OSC 0/2; bell gives feedback (Phase 6)
- [ ] UI stays at frame rate under `yes` (Phase 9)
- [ ] OSC 52 clipboard (Phase 8)

> The cursor-shape item is folded into Phase 1a: `show` reads `CursorShape` only
> to test "not hidden" (`terminal.rs:720`) and always draws a filled block
> (`terminal.rs:815-823`). Honor bar/underline shapes (and optional blink) there.

## Suggested grouping for sessions

Per the project's hybrid execution preference, batch the small rendering/input
phases and give the larger ones their own session. Order matches the execution
order above:

- **Session A:** Phase 2 (keyboard) + Phase 1a (underline/strikethrough + cursor
  shape) — all in `show`/`read_input`, cohesive, cheap, biggest first-impression
  win. No font dependency.
- **Session B:** Phase 7 (fonts + a small font config file), then Phase 1b
  (bold/italic) on top of the registered faces — fonts must land before bold.
  Phase 6 (title/bell) can land here too if there's capacity, adding its own
  bell key to that file.
- **Session C:** Phase 3 (mouse reporting) — its own, needs care with mode flags.
- **Session D:** Phase 4 + Phase 5 (selection + search) — related grid work.
- **Session E:** Phase 9 (flow control), Phase 8 (OSC 52), Phase 10 (links) as
  capacity allows.

Build/verify each per `docs/HANDOFF.md` §3 (kill app → `cargo build` →
`cargo test` → run + the acid test). Don't claim a phase done without running its
acid-test app.

## Color capability advertisement (2026-06-28, built + green)

Codex CLI rendered its grey input box in Windows Terminal but not in foreman.
Root cause: foreman wasn't telling cross-platform TUIs it does 24-bit color.

- **`term_env` now injects `COLORTERM=truecolor` + `TERM=xterm-256color`**
  (`src/wm.rs`). Codex gates its truecolor styling (the grey input-box fill) on
  `COLORTERM`; without it, it falls back to a flat theme. `COLORTERM` is the one
  that fixed the box (verified by screenshot); `TERM` is the sensible companion.
- **`Listener` now answers `Event::ColorRequest`** (`src/terminal.rs`). An app
  can query a terminal's colors (OSC 10 fg / 11 bg / 12 cursor, OSC 4;N palette)
  to detect a light/dark background and theme itself; foreman was *dropping*
  those queries (alacritty surfaces them as `ColorRequest`, we ignored the
  event). `query_color(index)` maps alacritty's color-table index to the RGB we
  actually paint (`<256` = palette via `indexed_rgb`; 257 = `BG`; else `FG`) and
  the reply rides the PTY-write path, same as the DSR answer. Two unit tests.

Note: the cwd here is literally `H:\claude code\foreman`, so beware string
matching — that "claude" lives in the *prompt*, never in the OSC title or a
color reply, so it doesn't interfere.

## Key files

- `src/terminal.rs` — `Session`, `show` (render + mouse + cursor), `read_input`
  (now the thin shell that calls `input::process_input`), `Listener` (PTY-write
  today; one typed events sink tomorrow — candidate B), `resize`,
  `selection_text`, `paste_wrap`.
- `src/input.rs` (NEW, Phase 2) — the pure input-encoding seam (arch candidate A):
  `process_input` / `InputOutcome` / `paste_seq` / `encode_mouse` / `mods_param`
  + byte-equality tests. The interface is the test surface.
- `src/wm.rs` — surfacing the live OSC title on the `Win` titlebar (Phase 6);
  follow `refresh_exit_titles`.
- `src/main.rs` — `mod input;` declaration; font registration at startup (Phase 7).
- `src/keymap.rs` — the load/merge/persist pattern to clone for the new
  `settings.json` (Phase 7).
- Cargo deps already present: `alacritty_terminal` (mode flags, search,
  `Selection`), `arboard` (OSC 52), `unicode-width` (wide-cell math).
