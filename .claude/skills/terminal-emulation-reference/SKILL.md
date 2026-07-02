---
name: terminal-emulation-reference
description: Use when working on foreman's terminal-emulation layer (src/terminal.rs, input.rs, inspect.rs, caret.rs, geom.rs, frame.rs) or decoding PTY/ConPTY/VT behavior - black pane that never prompts, ESC[6n DSR handshake, Ready gating, bracketed paste ESC[200~, DECCKM/SS3 arrows, SGR/X10 mouse bytes, OSC title/color queries, alternate-screen wheel scrolling, WIDE_CHAR_SPACER, buffer-vs-viewport coordinates, TermMode flags, alacritty_terminal 0.26 API shape, or resize/reflow questions.
---

# Terminal-emulation reference (as it applies to THIS repo)

Domain pack for an engineer with zero terminal-emulation background. Everything
here is verified against the code at HEAD `7fda1c2` (2026-07-01); line numbers
are stamped to that commit. This is the *reference* sibling — symptom-driven
triage lives in **foreman-debugging-playbook**, measurement recipes in
**foreman-diagnostics-and-tooling**, CLI ground truth in
**foreman-run-and-operate**.

Vocabulary is CONTEXT.md's glossary (Session, Ready, Caret gate, Cell metrics,
Quiescence settle, Frame plan). A **Session** is one running terminal — process
+ PTY + emulated screen; never call the whole thing "a PTY".

## 1. PTY, ConPTY, and the master/child split

| Term | Definition (once, here) |
|---|---|
| **Terminal emulator** | A program that renders a character grid and encodes keyboard/mouse into bytes. foreman is one (per Session). |
| **VT / ANSI escape sequences** | In-band control bytes starting with ESC (0x1b): `ESC[...` (CSI), `ESC]...` (OSC), `ESC O x` (SS3). Both directions: apps emit them to draw; terminals emit them to encode keys and answer queries. |
| **PTY (pseudoterminal)** | An OS object that looks like a terminal to a child process. Two ends: the **master** (foreman's side — read output, write input, set size) and the **child/slave** end the shell runs on. |
| **ConPTY** | Windows' PTY. Unlike Unix PTYs it is *mediated by conhost*: the child writes to a hidden console buffer, and ConPTY re-renders that buffer into a VT stream for the master. Consequence: ConPTY **owns a second copy of the screen and reflows it itself on resize** — which is exactly why the settled resize/recall bug exists (`docs/conpty-resize-reflow.md`; see §7). |
| **portable-pty 0.9.0** | The Rust wrapper foreman uses (Cargo.toml; Cargo.lock pins 0.9.0). `native_pty_system()` → ConPTY on Windows. It already sets `PSEUDOCONSOLE_RESIZE_QUIRK` (docs/conpty-resize-reflow.md:34). |
| **alacritty_terminal 0.26.0** | The terminal-emulation *model* crate (Cargo.lock pins 0.26.0): a VT parser (`Processor`) plus a grid state machine (`Term`). foreman feeds PTY bytes in and paints the resulting grid. No GUI of its own. |

The split, as built in `Session::spawn_with` (src/terminal.rs:464-549):

1. `native_pty_system().openpty(PtySize { rows, cols, .. })` → a master/slave pair. Spawn size is a placeholder **80x24**; the first rendered frame resizes to the real cell grid.
2. `pair.slave.spawn_command(cmd)` starts the child (shell or agent); `child.process_id()` is kept as `root_pid` for the process-tree agent-icon scan (`crate::proc`). The slave is then dropped.
3. **Reader thread pattern**: a dedicated thread blocks on `reader.read` (8 KiB buffer) and ships each chunk over an mpsc channel, then calls `note_pty_output()` and `ctx.request_repaint()` (src/terminal.rs:495-509). The GUI thread never blocks on the PTY.
4. The GUI thread drains the channel in `pump()` via `try_recv` and advances the parser: `self.parser.advance(&mut self.term, &bytes)` (src/terminal.rs:713-740). Each drained batch bumps `output_gen` — the freshness counter the **Quiescence settle** machinery polls.

`pump()` runs from `show()` every frame, from `keepalive()` for every non-rendered tab (src/terminal.rs:856-858; wm.rs walks all tabs each frame), and from every snapshot read. A Session that nobody pumps is a Session whose child eventually hangs on an unanswered query — that is the DSR trap.

## 2. Session lifecycle and the DSR handshake

**The trap that already cost hours (do not rediscover):** at startup,
shells/ConPTY send `ESC[6n` — **DSR (Device Status Report)**, specifically the
cursor-position query — and **block until the terminal replies**
`ESC[<row>;<col>R`. A terminal that drops the query = black pane, shell never
prompts (docs/HANDOFF.md §4 gotcha 4: "Never use `VoidListener`" for a live
Session; `VoidListener` is fine for byte-driven *tests*, see src/inspect.rs
tests).

The reply path (all src/terminal.rs):

```
child sends ESC[6n ──reader thread──> pump(): parser.advance()
  └─> alacritty Term computes the answer and emits Event::PtyWrite(reply)
        └─> Listener::send_event appends it to the shared `resp` buffer (:187-191)
              └─> pump() flushes `resp` back into the PTY writer (:718-725)
                    └─> first flush latches `ready = true`
```

**Ready** is a domain state (CONTEXT.md): injected input only lands after a
Session is Ready. Precisely: the latch fires on the first *any* reply bytes
flushed back (in practice the startup DSR reply). Before Ready,
`inject_input()` queues text in `pending_inject` instead of writing it — a
paste sent during the startup scan gets eaten by it (src/terminal.rs:659-671).
After the latch, `pump()` flushes the queue.

Rest of the lifecycle, in order:

| Stage | Mechanism | Where |
|---|---|---|
| Spawn env | `term_env` injects `FOREMAN=1`, `FOREMAN_TERMINAL_ID=t{id}`, `COLORTERM`, `TERM`, `FOREMAN_PROJECT_ID`, `FOREMAN_EXE` | src/wm.rs:787-804 |
| cwd | `CommandBuilder::cwd` from the Project directory | src/terminal.rs:357-359 |
| Argv spawn | `spawn_argv` retries npm `.cmd` shims through `cmd /c`; refuses newline/quote args (cmd re-parses the line — injection) | src/terminal.rs:369-430 |
| Dispatch banner | `inject_note` is DEFERRED to the first `resize()` — written at the 80x24 placeholder it reflows into scrollback on the first-frame shrink | src/terminal.rs:638-640, 770-790 |
| Chat inject | bracketed paste, then a `\r` deferred by `SUBMIT_DELAY` (150 ms) — a back-to-back `\r` folds into the paste as a literal newline (live failure 2026-06-10) | src/terminal.rs:293-300, 659-671 |
| Exit | `child.try_wait()` polled via `exited()`; `exit_to_note()` reports once | src/terminal.rs:572-588 |

## 3. Sequences foreman handles (the one table)

Direction: **app→foreman** = parsed out of PTY output; **foreman→app** = bytes
foreman writes into the PTY.

| Sequence | Direction | What it is | Where handled |
|---|---|---|---|
| `ESC[6n` → reply `ESC[r;cR` | app→foreman→app | DSR cursor-position query; the startup handshake above | Listener `Event::PtyWrite` + `pump()` (terminal.rs:187-191, 718-725) |
| OSC 0/2 (`ESC]0;title ST`) | app→foreman | Window title. Feeds tab **icon detection**: dispatch argv → OSC title → process tree → shell glyph | `Event::Title`/`ResetTitle` → `osc_title` (terminal.rs:192-201); `icon_kind` (:444-462) |
| OSC 10/11/12, OSC 4;N | app→foreman→app | "What are your colors?" (fg/bg/cursor/palette-N). Answer with the RGB foreman **actually paints**, or apps guess light/dark wrong | `Event::ColorRequest(index, format)` → `query_color` (terminal.rs:107-121, 202-209). Index <256 = palette; named slots Foreground/Background/Cursor at 256/257/258 |
| `ESC[200~ … ESC[201~` | foreman→app | **Bracketed paste**: multi-line text lands as one paste block, not per-line submits. Payload **ESC is stripped** so a quoted `ESC[201~` can't end the block early and turn the tail into live keystrokes | mode-gated `paste_seq` (input.rs:160-171) for user paste; unconditional `paste_wrap` (terminal.rs:338-347) for chat inject |
| DECCKM (`ESC[?1h`) | app→foreman | Application-cursor-keys mode: unmodified arrows/Home/End become SS3 `ESC O A..D/H/F` instead of CSI `ESC[A`. **Modified arrows stay CSI** with the modifier param | `TermMode::APP_CURSOR` in `encode_key` (input.rs:262-269) |
| xterm modifier param | foreman→app | `1 + shift + 2*alt + 4*ctrl`, e.g. Ctrl+Right = `ESC[1;5C` | `mods_param` (input.rs:246-252) |
| F1–F4 vs F5–F12 | foreman→app | F1–F4 are SS3 `ESC O P..S`; F5–F12 are tilde codes `ESC[15~`,17,18,19,20,21,23,24 (note the gaps). Insert/Delete/PgUp/PgDn = 2/3/5/6`~` | input.rs:283-312 |
| SGR mouse `ESC[<b;col;row M` / X10 `ESC[M`+3 bytes | foreman→app | Mouse reporting, **1-based, column-first** (opposite of the selection's (row, col)). Wheel up/down = buttons 64/65, no release. X10 bytes are value+32, saturating at 255 (col/row >223 clamp) | `wheel_input` (input.rs:194-227); pointer→cell via `CellMetrics::mouse_cell` (geom.rs:61-67). Only **wheel** is forwarded today — clicks stay local (selection/paste) |
| Alternate screen + alternate scroll | app→foreman | Full-screen TUIs (`ALT_SCREEN`) have no local scrollback; with `ALTERNATE_SCROLL` set the wheel is translated to arrow keys (honoring DECCKM). Precedence: mouse-mode > alt-scroll > local scrollback | `wheel_input` (input.rs:194-244) |
| DECSCUSR `ESC[n q` | app→foreman | Cursor shape (block/beam/underline/hollow/hidden) — parsed by alacritty, read from `renderable_content().cursor.shape` | inspect.rs:96-103; painted via `geom::caret_rect` |
| SGR styling `ESC[4m` etc. | app→foreman | Cell `Flags` (UNDERLINE, STRIKEOUT, INVERSE, DIM, BOLD, ITALIC) → resolved by pure `glyph_style` | terminal.rs:134-151 |
| Wide chars (CJK) | app→foreman | A 2-column glyph = one `WIDE_CHAR` cell + one `WIDE_CHAR_SPACER` placeholder cell. **Text-extraction walks must skip spacers** or output gains stray padding | skipped in both snapshot walks (inspect.rs:82, 148); the Frame-plan *paint* walk deliberately emits the spacer cell as a space (frame.rs:83-95) |
| DEC 2026 synchronized output | app→foreman | Frame-bracketing TUIs *don't* strobe; TUIs that lack it move the cursor all over mid-redraw — that jitter is what the **Caret gate** exists to absorb (§5) | caret.rs module docs |

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
  selection-v1 lesson (docs/terminal-selection.md). **Drift resolved
  (2026-07-01, Phase 4):** selection now lives in alacritty's own
  `term.selection` (buffer coords; `Selection::new` / `update` /
  `selection_to_string`), fed by `Session::sel_point` (pixel → buffer `Point`
  + `Side`) and re-projected for paint by the pure `sel_viewport_range` cull.
  Triple/double/drag/click map to Lines/Semantic/Simple/clear in `show()`.
  The old `sel_anchor`/`sel_head` viewport tuples are deleted. Doc:
  `docs/terminal-selection.md` (now matches the code).
- **Out-of-range `Line`/`Column` indexing panics**, and a panic across the
  winit callback **aborts the whole process**. `pump()` can shrink the grid
  mid-frame (alt-screen swap, reset), so every grid walk clamps to the grid's
  *real* `columns()`/`screen_lines()` first. The one home for that clamp is
  `frame::plan` (frame.rs:10-17, 59-70); snapshots clamp identically
  (inspect.rs:63-72).
- `history_size()` = scrollback length; drives the thumb (`geom::thumb_rect`).

**Cell metrics** (CONTEXT.md seam): all pixel↔cell conversion goes through
`geom::CellMetrics` — `cell_at` (0-based row,col, selection space) vs
`mouse_cell` (1-based col,row, mouse-protocol space). Don't hand-roll either.

## 5. Cursor vs caret

Two different things, per CONTEXT.md:

- The **cursor** is the model's: `term.renderable_content().cursor` (point +
  shape), owned by alacritty, moved by the app's escape sequences.
- The **caret** is what foreman paints. The **Caret gate**
  (`caret::CaretGate`, src/caret.rs) de-jitters it: adopt a cell only after
  the cursor holds it for `CURSOR_SETTLE` = 50 ms (caret.rs:24); follow
  single-row steps immediately only within `INPUT_GRACE` = 150 ms of real
  typing (caret.rs:30); hold far (≥2-row) jumps and autonomous animations;
  honor Hidden instantly. All time is injected — the policy is a pure function
  (`cursor_to_draw`) plus timeline unit tests. Background: docs/cursor-rendering.md.

`frame::plan` then suppresses the caret unless `line >= 0` **and**
`display_offset == 0` (live viewport only); `show()` adds the focus gate.

## 6. alacritty_terminal 0.26 API, exactly as used here

| API | Usage in this repo |
|---|---|
| `Term::new(Config::default(), &D, listener)` where `D: Dimensions` | terminal.rs:513-520; the repo defines tiny `Size`/`Dims` structs implementing `total_lines`/`screen_lines`/`columns` |
| `Term<T: EventListener>` | Production: `Term<Listener>`. Tests: `Term<VoidListener>` driven by fixed bytes (inspect.rs/frame.rs tests). inspect.rs functions are generic over `L: EventListener` so both work |
| `Listener` events consumed (the actual match arms, terminal.rs:186-210) | `Event::PtyWrite(text)` → resp buffer; `Event::Title(t)` / `Event::ResetTitle` → `osc_title`; `Event::ColorRequest(index, format)` → `format(query_color(index))` → resp buffer; **everything else `_ => {}`** |
| `Processor::new()` / `parser.advance(&mut term, &bytes)` | the only way bytes enter the grid (pump, inject_note) |
| `term.renderable_content()` | cursor point + shape (terminal.rs:958-965, inspect.rs:96). Borrows `&Term` — copy fields out before touching `term.grid()` again |
| `term.grid()`, `grid.display_offset()`, `grid.columns()`, `grid.screen_lines()`, `grid.history_size()` | every grid walk (§4) |
| `term.resize(Size)` **paired with** `master.resize(PtySize)` | `Session::resize` (terminal.rs:742-765): model first, then the PTY, then `scroll_display(Scroll::Bottom)` to snap off a stale scroll offset. Guard: no-op under 2 cols / 1 row |
| `term.scroll_display(Scroll::{Top,Bottom,PageUp,PageDown,Delta(i32)})` | scrollback control (keys, wheel, resize snap) |
| `*term.mode()` → `TermMode` | flags consulted: `APP_CURSOR` (arrow encoding), `BRACKETED_PASTE` (paste gating), `MOUSE_MODE` (aggregate) + `SGR_MOUSE` (wheel encoding), `ALT_SCREEN` + `ALTERNATE_SCROLL` (wheel→arrows). Exposed as `Session::term_mode()` so `foreman send --keys` encodes through the same seam (wm.rs:1344, inspect.rs `parse_keys`) |
| `term::cell::Flags` | INVERSE/DIM/UNDERLINE/STRIKEOUT in `glyph_style`; BOLD/ITALIC/WIDE_CHAR reported by `snapshot_cells`; WIDE_CHAR_SPACER skipped in text walks |
| `Selection` / `SelectionType`, `term.selection`, `selection_to_string()`, `viewport_to_point` | Phase 4 selection: click chain + `sel_point` + `sel_viewport_range` cull (terminal.rs); fixture pins in terminal.rs tests |

## 7. Capability advertisement (COLORTERM/TERM) — do not drop

`term_env` (wm.rs:787-804) advertises `COLORTERM=truecolor` and
`TERM=xterm-256color` into every Session. Rationale (incident, 2026-06-28,
docs/epics/terminal-completeness-epic.md §"Color capability advertisement"):
Codex CLI gates its truecolor styling on `COLORTERM` and rendered a flat theme
(no grey input box) in foreman until it was set; verified fixed by screenshot.
`Event::ColorRequest` answering (§3) landed in the same change — apps also
*query* colors to theme themselves, and foreman used to drop those queries.
Removing either regresses TUI appearance silently. The full env-axis inventory
is **foreman-config-and-flags**' home; the ConPTY resize saga chronicle is
**foreman-failure-archaeology**'s.

ConPTY resize corruption (settled, do-not-re-litigate — see
**foreman-change-control**): narrowing past a wrapped prompt then Up-arrow
recall corrupts until Ctrl+L. It is ConPTY's reflow diverging from
alacritty's, with ConPTY reporting a cursor inconsistent with its own repaint
(microsoft/terminal #18725). Four redraw-ownership variants were tested and
failed. Read `docs/conpty-resize-reflow.md` before touching `Session::resize`.

## 8. Where each concept lives

| Concept | File |
|---|---|
| Session (PTY + Term + reader thread), Ready latch, inject, resize, render replay | src/terminal.rs |
| Pure input encoding: keys, paste gating, wheel policy, zoom steps (the Input-encoding seam) | src/input.rs |
| Pure GUI-free screen reads (Snapshot text/cells/cursor) + `--keys` name→byte encoding | src/inspect.rs |
| Caret gate (de-jitter policy + timeline state) | src/caret.rs |
| Cell metrics (pixel↔cell), thumb + caret rect math | src/geom.rs |
| Frame plan (pure per-frame paint geometry; the clamp home) | src/frame.rs |
| Env injection (`term_env`), keepalive walks, send/snapshot plumbing | src/wm.rs |
| Process-tree agent scan from `root_pid` | src/proc.rs |

## When NOT to use this skill

- Diagnosing a live symptom ("black pane", "input eaten", "caret strobing") →
  **foreman-debugging-playbook** first; this pack is the theory behind it.
- Driving `foreman send`/`snapshot`/`status` or measuring behavior headlessly →
  **foreman-run-and-operate** (CLI ground truth) and
  **foreman-diagnostics-and-tooling** (measurement recipes).
- egui/immediate-mode traps (repaint scheduling, `Event::Copy`, painter API) →
  **egui-immediate-mode-reference**.
- The full history of the ConPTY investigation and other dead ends →
  **foreman-failure-archaeology**; invariants and threading model →
  **foreman-architecture-contract**.
- You are an agent *running inside* foreman wanting to dispatch or chat → the
  user-facing **foreman-dispatch** / **foreman-chat** skills, not this one.

## Provenance and maintenance

Written 2026-07-01 against HEAD `7fda1c2` (working tree clean). Line numbers
and constants drift; re-verify from `H:/claude code/foreman` (PowerShell 7+):

| Claim | Re-verify |
|---|---|
| Crate versions 0.26.0 / 0.9.0 | `git grep -n -A1 'name = "alacritty_terminal"' Cargo.lock; git grep -n -A1 'name = "portable-pty"' Cargo.lock` |
| Listener match arms (PtyWrite/Title/ResetTitle/ColorRequest) | `git grep -n "Event::" src/terminal.rs` |
| Ready latch + pending_inject flush | `git grep -n "ready = true" src/terminal.rs` and the tests `session_latches_ready_after_dsr_is_answered`, `inject_before_ready_is_queued_then_flushed` |
| Key/mouse encodings byte-for-byte | `cargo test --lib input` (byte-equality tests; mind the shared target/ lock) |
| Env advertisement | `git grep -n "COLORTERM" src/wm.rs` |
| Selection wiring (alacritty Selection in use since Phase 4) | `git grep -n "term.selection" src/terminal.rs ; cargo test selection` |
| Caret constants 50 ms / 150 ms | `git grep -n "CURSOR_SETTLE\|INPUT_GRACE" src/caret.rs` |
| SUBMIT_DELAY 150 ms | `git grep -n "SUBMIT_DELAY" src/terminal.rs` |
| Spacer-skip locations | `git grep -n "WIDE_CHAR_SPACER" src/` |
| ConPTY bug status (upstream open) | `docs/conpty-resize-reflow.md` header; microsoft/terminal #18725 |
