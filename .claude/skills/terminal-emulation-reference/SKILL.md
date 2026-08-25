---
name: terminal-emulation-reference
description: Use when working on foreman's terminal-emulation layer (src/terminal.rs, input.rs, ready.rs, inspect.rs, caret.rs, geom.rs, frame.rs, graphics.rs) or decoding PTY/ConPTY/VT behavior - black pane that never prompts, ESC[6n DSR handshake, Ready gating, bracketed paste ESC[200~, DECCKM/SS3 arrows, SGR/X10 mouse bytes, OSC title/color queries, alternate-screen wheel scrolling, WIDE_CHAR_SPACER, buffer-vs-viewport coordinates, TermMode flags, alacritty_terminal 0.26 API shape, or resize/reflow questions.
---

# Terminal-emulation reference (as it applies to THIS repo)

Domain pack for an engineer with zero terminal-emulation background. Cites are
file + symbol, never line numbers — symbols survive refactors, line numbers do
not. This is the *reference* sibling — symptom-driven triage lives in
**foreman-debugging-playbook**, measurement recipes in
**foreman-diagnostics-and-tooling**, CLI ground truth in
**foreman-run-and-operate**.

Vocabulary is CONTEXT.md's glossary (Session, Ready, Caret, Cell metrics,
Quiescence settle, Frame plan). A **Session** is one running terminal — process
+ PTY + emulated screen; never call the whole thing "a PTY".

## 1. PTY, ConPTY, and the master/child split

Generic VT background in one pass: escape sequences are in-band control bytes
starting with ESC (0x1b) — `ESC[…` (CSI), `ESC]…` (OSC), `ESC O x` (SS3),
`ESC_…` (APC). They flow both directions: apps emit them to draw, terminals
emit them to encode keys and answer queries. A **PTY** is an OS object that
looks like a terminal to a child process, with a **master** end (foreman's:
read output, write input, set size) and a **child/slave** end the shell runs on.

The parts that are specific to this repo:

| Thing | What matters here |
|---|---|
| **ConPTY** | Windows' PTY, *mediated by conhost*: the child writes to a hidden console buffer and ConPTY re-renders that buffer into a VT stream. Consequence: ConPTY **owns a second copy of the screen and reflows it itself on resize** — the whole reason the settled resize/recall bug exists (`docs/conpty-resize-reflow.md`; see §7). |
| **portable-pty** | The Rust PTY wrapper (version pinned in Cargo.lock). `native_pty_system()` → ConPTY on Windows. It already sets `PSEUDOCONSOLE_RESIZE_QUIRK` (`docs/conpty-resize-reflow.md`). |
| **alacritty_terminal** | The terminal-emulation *model* crate (version pinned in Cargo.lock): a VT parser (`Processor`) plus a grid state machine (`Term`). foreman feeds PTY bytes in and paints the resulting grid. No GUI of its own. |

The split, as built in `Session::spawn_with` (src/terminal.rs):

1. `native_pty_system().openpty(PtySize { rows, cols, .. })` → a master/slave pair. Spawn size is a placeholder **80x24**; the first rendered frame resizes to the real cell grid.
2. `pair.slave.spawn_command(cmd)` starts the child (shell or agent); `child.process_id()` is kept as `root_pid` for the process-tree agent-icon scan (`crate::proc`). The slave is then dropped.
3. **Reader thread pattern**: a dedicated thread blocks on `reader.read` (64 KiB buffer — `read()` returns whatever is already available, so a bigger buffer only cuts chunk count under flood; no latency or ordering change) and ships each chunk over an mpsc channel, then calls `note_pty_output()` and `ctx.request_repaint()`. The GUI thread never blocks on the PTY.
4. The GUI thread drains the channel in `pump_at()` via `try_recv` and advances the parser through `advance_scanned`, which splits each chunk at kitty-graphics APC boundaries so `crate::graphics` sees the image commands and alacritty sees the rest. Each drained batch bumps `output_gen` (the freshness counter the **Quiescence settle** machinery polls) and `content_gen`.

`Term::new` is given `Config { scrolling_history: <settings scrollback_lines>, .. }` — scrollback depth is a persisted setting, not the crate default.

`pump()` runs from `show()` every frame, from `keepalive()` for every
non-rendered tab (wm.rs walks all tabs each frame), and from every snapshot
read. A Session that nobody pumps is a Session whose child eventually hangs on
an unanswered query — that is the DSR trap.

## 2. Session lifecycle, the DSR handshake, and the Ready gate

**The trap that already cost hours (do not rediscover):** at startup,
shells/ConPTY send `ESC[6n` — DSR, the cursor-position query — and **block
until the terminal replies** `ESC[<row>;<col>R`. A terminal that drops the
query = black pane, shell never prompts. Hence CLAUDE.md's fence: **never use
`VoidListener` in a live Session** (it is fine for byte-driven *tests* — see
the inspect.rs/frame.rs tests).

The reply path (src/terminal.rs):

```
child sends ESC[6n ──reader thread──> pump_at(): advance_scanned()
  └─> alacritty Term computes the answer, emits Event::PtyWrite(reply)
        └─> Listener::send_event appends it to the shared `resp` buffer
              └─> flush_pty_replies() writes `resp` back after THAT rx chunk
                    └─> ReadyGate::on_dsr_reply_flushed(sent)
```

**Ready** (CONTEXT.md) is now a separate pure module, `src/ready.rs`
(`ReadyGate`) — Session applies its `Action::Write`s and never decides
readiness itself. Two independent halves must both latch:

- `dsr_replied` — set only by `on_dsr_reply_flushed(true)`, i.e. a *successful*
  flush of the alacritty `resp` buffer.
- `painted` — set by `on_rx_chunk` when `InkScan` sees the first printable byte
  **outside** any escape/control sequence. ConPTY emits control-only chrome
  (DSR, DA1, mode sets, cursor homing) long before the child paints, and input
  written in that window is eaten; first real ink is the observable "child is
  up" signal. The scanner is chunk-boundary safe.

**The split that must not be undone:** `flush_graphics_replies` writes kitty
`a=q` probe answers straight to the writer, deliberately *not* through `resp`
and *not* through the gate — a graphics reply must never fake DSR readiness.
Pinned by `graphics_reply_path_does_not_latch_ready` in terminal.rs.

Before Ready, `inject_input()` → `ReadyGate::try_inject` queues the text in
`pending_inject` instead of writing it (a chat post sent during the startup
scan gets held, not eaten); `poll()` drains the queue once latched. CPR is
latency-sensitive: replies flush after each parsed rx chunk, not after draining
an unbounded backlog, and minimized windows pump every tab headlessly. Either
invariant regressing can make ConPTY screen-buffer queries hit their 500ms
timeout. A failed reply write is deliberately **not** retried — re-queueing
risks duplicate CPRs; it logs instead.

READY_GRACE (a timeout fallback for children that never send DSR) is
deliberately absent — see `docs/followups-latency-and-control.md`.

Rest of the lifecycle, in order:

| Stage | Mechanism | Where |
|---|---|---|
| Spawn env | `term_env` injects `FOREMAN`, `FOREMAN_TERMINAL_ID`, `COLORTERM`, `TERM`, `KITTY_WINDOW_ID`, `FOREMAN_PROJECT_ID`, `FOREMAN_EXE` | src/wm.rs `fn term_env` |
| cwd | `CommandBuilder::cwd` from the Project directory | src/terminal.rs |
| Argv spawn | `spawn_argv` retries npm `.cmd` shims through `cmd /c`; refuses newline/quote args (cmd re-parses the line — injection) | src/terminal.rs `fn spawn_argv` |
| Dispatch banner | `inject_note` is DEFERRED to the first `resize()` — written at the 80x24 placeholder it reflows into scrollback on the first-frame shrink | src/terminal.rs `fn inject_note` |
| Chat inject | bracketed paste, then a `\r` deferred by `SUBMIT_DELAY` (150 ms) — a back-to-back `\r` folds into the paste as a literal newline (live failure 2026-06-10) | src/ready.rs `SUBMIT_DELAY`, `paste_wrap`, `try_inject` |
| Exit | `child.try_wait()` polled via `exited()`; `exit_to_note()` reports once | src/terminal.rs `fn exited` / `fn exit_to_note` |

## 3. Sequences foreman handles (the one table)

Direction: **app→foreman** = parsed out of PTY output; **foreman→app** = bytes
foreman writes into the PTY. The generic meaning of each sequence is one line;
the column that earns its keep is *where handled*.

| Sequence | Direction | Foreman-specific note | Where handled |
|---|---|---|---|
| `ESC[6n` → reply `ESC[r;cR` | app→foreman→app | The startup handshake in §2 — half the Ready contract | `Listener` `Event::PtyWrite` + `flush_pty_replies` (terminal.rs) |
| OSC 0/2 title | app→foreman | Feeds tab **icon detection**, in this precedence: dispatch argv → OSC title → process tree → shell glyph | `Event::Title`/`ResetTitle` → `osc_title`; `Session::icon_kind` (terminal.rs) |
| OSC 10/11/12, OSC 4;N | app→foreman→app | "What are your colors?" Answer with the RGB foreman **actually paints**, or apps guess light/dark wrong. Index <256 = palette; named slots Foreground/Background/Cursor at 256/257/258 | `Event::ColorRequest(index, format)` → `query_color` (terminal.rs) |
| `\a` (BEL) | app→foreman | Visual-bell latch: sticky since the ring instant, cleared when the pane takes keyboard focus. A *sibling* of title/color handling — never the PtyWrite/Ready path | `Event::Bell` → shared `bell` slot (terminal.rs) |
| `ESC[200~ … ESC[201~` | foreman→app | Bracketed paste. Payload **ESC is stripped** so a quoted `ESC[201~` can't end the block early and turn the tail into live keystrokes | mode-gated `input::paste_seq` for user paste; unconditional `ready::paste_wrap` for chat inject |
| DECCKM (`ESC[?1h`) | app→foreman | Unmodified arrows/Home/End become SS3; **modified arrows stay CSI** with the modifier param | `TermMode::APP_CURSOR` in `input::encode_key` |
| xterm modifier param | foreman→app | `1 + shift + 2*alt + 4*ctrl` | `input::mods_param` |
| F1–F4 vs F5–F12 | foreman→app | F1–F4 are SS3 `ESC O P..S`; F5–F12 are tilde codes with gaps (15,17,18,19,20,21,23,24) | `input::encode_key` |
| SGR mouse / X10 mouse | foreman→app | **1-based, column-first** (opposite of the selection's (row, col)). X10 bytes are value+32, saturating at 255. Wheel **and** press/release/drag/hover-motion are all forwarded once the app sets a mouse mode. Ownership is decided **once at press**: Shift held, main-screen scrollback offset, or no app mouse mode → Local (selection/paste); otherwise Application. Owner *and* the mode bits (`MOUSE_MODE`, `SGR_MOUSE`, `UTF8_MOUSE`) are snapshotted at press and frozen until the matching release, so a mode change mid-drag can't split a press from its release. Encoding precedence SGR 1006 → UTF-8 1005 → legacy X10; tracking 1000 click-only / 1002 +held drag / 1003 +hover. Full gotcha list: `docs/terminal-mouse-reporting.md` | `input::mouse_press_owner`, `MouseCapture`, `mouse_app_event`, `encode_mouse_report`, `mouse_button_code`, `mouse_motion_allowed` (wheel: `input::wheel_input`); routed in `terminal::Session::handle_mouse`; pointer→cell via `geom::CellMetrics::mouse_cell` |
| Alternate screen + alternate scroll | app→foreman | Full-screen TUIs (`ALT_SCREEN`) have no local scrollback; with `ALTERNATE_SCROLL` the wheel becomes arrow keys (honoring DECCKM). Precedence: mouse-mode > alt-scroll > local scrollback | `input::wheel_input` |
| DECSCUSR `ESC[n q` | app→foreman | Cursor shape, parsed by alacritty, read from `renderable_content().cursor.shape` | src/inspect.rs; painted via `geom::caret_rect` |
| SGR styling | app→foreman | Cell `Flags` (UNDERLINE, STRIKEOUT, INVERSE, DIM, BOLD, ITALIC) → resolved by pure `glyph_style` | src/terminal.rs `fn glyph_style` |
| Wide chars (CJK/emoji) | app→foreman | A 2-column glyph = one `WIDE_CHAR` cell + one spacer (`WIDE_CHAR_SPACER`, or `LEADING_WIDE_CHAR_SPACER` at wrap). **Text-extraction walks must skip spacers** or output gains stray padding | one classifier: `input::CellWide::classify` / `is_wide_spacer` (paint plan, both snapshot walks). Foreman does **NOT** double keys or model the input row — whole-glyph editing is PSReadLine's job (`src/psreadline.rs` `WIDE_EDIT_FIX`, RightArrow deliberately unbound); see docs/wide-chars.md "Why the terminal-side approach failed" |
| DEC 2026 synchronized output | app→foreman | Frame-bracketing TUIs don't strobe. `advance_scanned` force-flushes a buffered sync prefix (`sync_bytes_count` / `stop_sync`) at each graphics cut so an image placement samples the cursor as of that byte, not the previous frame | src/terminal.rs `fn advance_scanned`; caret.rs module docs |
| Kitty graphics APC (`ESC_G…`) | app→foreman→app | alacritty's vte discards APC, so `crate::graphics` gets a parallel feed of the same bytes. Unsupported commands are skipped silently — the failure mode is "image doesn't show", never a corrupted pane | src/graphics.rs |

## 4. Grid model: buffer vs viewport, and the panic hazard

alacritty's grid is indexed `grid[Line(i32)][Column(usize)]`. **`Line` is
signed**: `Line(0)` is the top of the live screen; **negative lines reach into
scrollback**. `display_offset()` (usize) is how many lines the viewport is
scrolled back. The conversion used everywhere in this repo:

```
buffer_line = Line(viewport_row as i32 - display_offset as i32)
```

Rules that already earned their scars:

- **Never store screen rows** for anything that must track text across
  scrolling — capture in buffer space, convert at paint time. That is the
  selection-v1 lesson. Selection now lives in alacritty's own `term.selection`
  (buffer coords; `Selection::new` / `update` / `selection_to_string`), fed by
  `Session::sel_point` (pixel → buffer `Point` + `Side`) and re-projected for
  paint by the pure `sel_viewport_range` cull. Triple/double/drag/click map to
  Lines/Semantic/Simple/clear in `show()`. The old `sel_anchor`/`sel_head`
  viewport tuples are deleted. Doc: `docs/terminal-selection.md`.
- **Out-of-range `Line`/`Column` indexing panics**, and a panic across the
  winit callback **aborts the whole process**. `pump()` can shrink the grid
  mid-frame (alt-screen swap, reset), so every grid walk clamps to the grid's
  *real* `columns()`/`screen_lines()` first. Paint clamps in `frame::plan_paint`
  and `frame::overlays`; snapshots clamp identically in `inspect::clamp_region`
  (one function so the two can never drift apart).
- `history_size()` = scrollback length; drives the thumb (`geom::thumb_rect`).

**Cell metrics** (CONTEXT.md seam): all pixel↔cell conversion goes through
`geom::CellMetrics` — `cell_at` (0-based row,col, selection space) vs
`mouse_cell` (1-based col,row, mouse-protocol space). Don't hand-roll either.

## 5. Cursor vs caret

Two different things, per CONTEXT.md:

- The **cursor** is the model's: `term.renderable_content().cursor` (point +
  shape), owned by alacritty, moved by the app's escape sequences.
- The **caret** is what foreman paints: `caret::draw` is a pure mapping —
  `?25l` honored instantly, everything else drawn exactly where the model says,
  every frame. No debouncing, no blink. Focused pane = filled rect; unfocused
  panes = hollow Block outline. The old de-jitter "Caret gate" (50 ms settle /
  150 ms input grace) was retired 2026-07-15: DEC 2026 sync blocks (Claude
  Code, Codex) and hide-bracketed single-chunk redraws (PSReadLine) keep the
  model cursor stream clean, and the gate's holds were themselves the visible
  lag/flash. Evidence + fallback policy: docs/cursor-rendering.md.

`frame::overlays` then suppresses the caret unless `line >= 0` **and**
`display_offset == 0` (live viewport only); `show()` adds the focus gate. On a
wide glyph the caret covers the full base+spacer span (either cell), so a block
never looks half-stuck mid-emoji.

## 6. alacritty_terminal API, exactly as used here

*(This table is shaped by the pinned alacritty_terminal major — re-check the
whole section on a version bump, not row by row.)*

| API | Usage in this repo |
|---|---|
| `Term::new(Config { scrolling_history, .. }, &D, listener)` where `D: Dimensions` | src/terminal.rs; the repo defines tiny `Size`/`Dims` structs implementing `total_lines`/`screen_lines`/`columns` |
| `Term<T: EventListener>` | Production: `Term<Listener>`. Tests: `Term<VoidListener>` driven by fixed bytes. inspect.rs functions are generic over `L: EventListener` so both work |
| `Listener` events consumed | `Event::PtyWrite` → `resp` buffer; `Event::Title`/`ResetTitle` → `osc_title`; `Event::ColorRequest(index, format)` → `format(query_color(index))` → `resp`; `Event::Bell` → bell latch; **everything else `_ => {}`** |
| `Processor::new()` / `parser.advance(&mut term, &bytes)` | the only way bytes enter the grid (via `advance_scanned`) |
| `parser.sync_bytes_count()` / `parser.stop_sync(&mut term)` | force-flush a buffered DEC 2026 block so a mid-stream sample sees the real cursor |
| `term.renderable_content()` | cursor point + shape. Borrows `&Term` — copy fields out before touching `term.grid()` again |
| `term.grid()`, `display_offset()`, `columns()`, `screen_lines()`, `history_size()` | every grid walk (§4) |
| `term.resize(Size)` **paired with** `master.resize(PtySize)` | `Session::resize`: model first (through `resize_anchored`, which cancels alacritty's scrollback pull on a height *grow* so the grid keeps ConPTY's anchoring — otherwise the child's next absolute CUP repaint lands mid-scrollback), then `scroll_display(Scroll::Bottom)` to snap off a stale offset, then the PTY. Guard: no-op under 2 cols / 1 row. Also drops graphics placements (`graphics.on_resize`) — reflow invalidates their anchors |
| `term.scroll_display(Scroll::{Top,Bottom,PageUp,PageDown,Delta(i32)})` | scrollback control (keys, wheel, resize snap) |
| `*term.mode()` → `TermMode` | flags consulted: `APP_CURSOR` (arrow encoding), `BRACKETED_PASTE` (paste gating), `MOUSE_MODE`/`SGR_MOUSE`/`UTF8_MOUSE` (mouse encoding), `ALT_SCREEN` + `ALTERNATE_SCROLL` (wheel→arrows). Exposed as `Session::term_mode()` so `foreman send --keys` encodes through the same seam (wm.rs → `inspect::parse_keys`) |
| `term::cell::Flags` | INVERSE/DIM/UNDERLINE/STRIKEOUT in `glyph_style`; BOLD/ITALIC/WIDE_CHAR reported by `snapshot_cells`; spacers skipped in text walks |
| `Selection` / `SelectionType`, `term.selection`, `selection_to_string()`, `viewport_to_point` | selection: click chain + `sel_point` + `sel_viewport_range` cull (terminal.rs); fixture pins in terminal.rs tests |

## 7. Capability advertisement (COLORTERM/TERM/KITTY_WINDOW_ID) — do not drop

`term_env` (src/wm.rs) advertises `COLORTERM=truecolor` and
`TERM=xterm-256color` into every Session. Rationale (incident, 2026-06-28,
docs/epics/terminal-completeness-epic.md §"Color capability advertisement"):
Codex CLI gates its truecolor styling on `COLORTERM` and rendered a flat theme
(no grey input box) in foreman until it was set; verified fixed by screenshot.
`Event::ColorRequest` answering (§3) landed in the same change — apps also
*query* colors to theme themselves, and foreman used to drop those queries.
`KITTY_WINDOW_ID=1` is the narrowest signal that makes agents pick the kitty
graphics protocol; `TERM` stays truthful, because foreman implements the
graphics *subset* in src/graphics.rs, not all of kitty. Removing any of these
regresses TUI appearance silently. The full env-axis inventory is
**foreman-config-and-flags**' home; the ConPTY resize saga chronicle is
**foreman-failure-archaeology**'s.

ConPTY resize corruption (read before touching `Session::resize`): ConPTY's
reflow diverges from alacritty's. Foreman now bundles #19535's lazy cursor
resynchronization, but content reflow still differs and `Ctrl+L` still heals
residual artifacts. The settled fence applies to the four failed
redraw-ownership variants and full conhost-parity reflow, not to the adopted
cursor mitigation. `FOREMAN_RX_DUMP` interleaves resize markers with raw
ConPTY bytes if you need a live repro. Details: `docs/conpty-resize-reflow.md`.

## 8. Where each concept lives

| Concept | File |
|---|---|
| Session (PTY + Term + reader thread), inject, resize, render replay | src/terminal.rs |
| Ready gate (pure): DSR half, InkScan paint half, inject queue, submit delay | src/ready.rs |
| Pure input encoding: keys, paste gating, wheel policy, wide-cell classification | src/input.rs |
| Pure GUI-free screen reads (Snapshot text/cells/cursor, `--tail`) + `--keys` name→byte encoding | src/inspect.rs |
| Caret draw decision (pure model-cursor → paint mapping; gate retired) | src/caret.rs |
| Cell metrics (pixel↔cell), thumb + caret rect math | src/geom.rs |
| Paint plan + overlays (pure per-frame geometry; the clamp home) | src/frame.rs |
| Kitty graphics subset (pure: APC parse, store, placements) | src/graphics.rs |
| PSReadLine wide-char edit fix injected at pwsh spawn | src/psreadline.rs |
| Env injection (`term_env`), keepalive walks, send/snapshot plumbing | src/wm.rs |
| Process-tree agent scan from `root_pid` | src/proc.rs |

## When NOT to use this skill

- A live symptom ("black pane", "input eaten", "caret strobing") →
  **foreman-debugging-playbook** first; this pack is the theory behind it.
- egui/immediate-mode traps (repaint scheduling, `Event::Copy`, painter API) →
  **egui-immediate-mode-reference**.
- You are an agent *running inside* foreman wanting to dispatch or chat → the
  user-facing **foreman-dispatch** / **foreman-chat** skills, not this one.
  (This pack's description names terminals and PTYs, so it is the easiest skill
  to load by mistake from inside a pane.)

## Provenance and maintenance

Cites are symbols, not line numbers. The claims below are the ones that are
both load-bearing and prone to silent drift — re-verify from
`H:/claude code/foreman`:

| Claim | Re-verify |
|---|---|
| Crate versions (§1, §6) | `git grep -n -A1 'name = "alacritty_terminal"' Cargo.lock; git grep -n -A1 'name = "portable-pty"' Cargo.lock` |
| Ready needs BOTH halves, and graphics replies never latch it | `cargo test ready::` plus `session_latches_ready_after_dsr_is_answered`, `ready_waits_for_the_childs_first_paint`, `graphics_reply_path_does_not_latch_ready` in terminal.rs |
| Listener match arms (PtyWrite/Title/ResetTitle/ColorRequest/Bell) | `git grep -n "Event::" src/terminal.rs` |
| Key/mouse encodings byte-for-byte | `cargo test input::` (byte-equality tests; mind the shared target/ lock) |
| Caret gate stayed retired (§5) — pure `draw` mapping only | `git grep -n CaretGate src/ ; git grep -n CURSOR_SETTLE src/` — **zero hits is the pass**; any hit means the gate came back |
| Selection wiring (§4) | `git grep -n 'term.selection' src/terminal.rs ; cargo test selection` |
| Env advertisement (COLORTERM/TERM/KITTY_WINDOW_ID) | read `fn term_env` in src/wm.rs — the list is the code |
| Spacer-skip locations (one classifier, no key doubling) | `git grep -n "CellWide" src/` |
| ConPTY cursor mitigation + residual status | `docs/conpty-resize-reflow.md` header; microsoft/terminal #18725/#19535 |
