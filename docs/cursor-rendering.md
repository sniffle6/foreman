# Cursor rendering (the caret)

## What it does

Foreman draws its own caret. It reads the cursor **state** (position, shape,
visibility) from alacritty's grid model, then paints an amber rectangle itself
(`231,169,63` @ alpha 130). Shapes are honored: `Beam` → thin left bar,
`Underline` → thin bottom bar, block/anything else → full cell. There is no
blink. The caret is drawn only for the **focused** pane that is **not** scrolled
into history.

alacritty owns *where and what shape*; Foreman owns *how it looks*. There is no
alacritty renderer in the build — `alacritty_terminal` is just the parser/grid.

## Why the anti-strobe gate exists

A full-screen TUI (Codex, claude, vim) redraws by moving the cursor all over the
screen — e.g. park it on the status line to write it, then move it back to the
input line. If the app brackets its frame in **synchronized output** (DEC mode
2026, `ESC[?2026h … ESC[?2026l`), the vte parser buffers the whole frame and
applies it atomically, so we never see an intermediate state. Many apps don't.

Foreman repaints at a fixed ~60fps, draining whatever PTY bytes have arrived so
far. For a non-synchronized app, that 16ms tick lands **mid-redraw**, catching
the caret at a transient cell. Result: the caret strobes between the input line
and the status line, or "follows" animating text. (Reported 2026-06: caret
jumping between the typing line and the status line while running Codex.)

## How the gate works

The strobe **teleports** the caret to a far row (status line / message area) and
back, while real line movement steps by a single row (wrap, backspace up,
newline down). So the painted caret position is gated by the Caret gate
(`caret::CaretGate`):

- **Cursor settled** (the *cursor* held the same cell for `CURSOR_SETTLE` =
  50ms) → adopt the model position outright. This is *cursor-position*
  stability, **not** output quiescence: a TUI can stream forever (a spinner,
  a "thinking" animation) while its caret rests, and the caret must still snap
  to that resting cell. Getting this wrong froze the caret at a stale spot after
  Codex's startup gloss until the user typed.
- **Cursor still moving, within one row of committed, and the user typed within
  `INPUT_GRACE` (150ms)** → follow immediately. This is the user editing —
  typing, wrapping, backspacing across a line (caret up one row). It stays
  responsive even with a key held down (auto-repeat keeps refreshing
  `last_input_at`), and never waits on a settle.
- **Cursor still moving, a far (≥2 row) jump** → hold the committed row until the
  cursor settles. This swallows the mid-redraw teleport to the status line.
- **Cursor still moving, single-row, NO recent user input** → also hold. This is
  an autonomous animation — the startup "gloss" sweep walks the write-head
  across adjacent rows on its own; without a keypress behind it there's nothing
  to chase, so the caret stays put until the cursor settles.

`?25l` (hide) is honored immediately — hiding is a deliberate app signal, never
deferred behind quiescence.

**Why the user-input gate:** the ±1 escape hatch exists only to keep user
editing snappy. Tying it to recent keyboard input is what separates a real
backspace (always follows a keypress) from an autonomous shimmer (no keypress).
Dispatched/chat injection does **not** count as user input, so a `foreman send`
that triggers a redraw won't make the caret chase it.

**Known edge:** if an app's status line sits exactly one row from the input
caret, that single-row strobe can leak through *while you're typing* (it looks
like real movement). The dramatic strobes — status line at the screen bottom,
message area at the top — are always far jumps and are always suppressed.

The inspection layer (`foreman snapshot --cursor`) still reports the **raw model**
cursor, not the gated draw — it wants ground truth.

## Tuning / gotchas

- **`CURSOR_SETTLE` (terminal.rs)** — how long the cursor must hold one cell
  before that position is adopted. Too low → mid-redraw chunk gaps re-introduce
  the strobe; too high → the caret lags when legitimately jumping to a distant
  row. 50ms is past one or two frames of scheduling jitter yet feels instant.
  Tracked as *cursor* movement (`cursor_seen` / `cursor_moved_at`), not output
  activity — that distinction is the whole fix for the post-gloss freeze.
- **`INPUT_GRACE` (terminal.rs)** — how long after a keypress single-row moves
  are treated as user editing (and followed at once). Too low → a slow app
  response to a keystroke gets held and the caret lags; too high → an animation
  starting right after you type gets chased. 150ms covers keystroke latency and
  auto-repeat.
- During sustained single-row animation (a spinner, streaming text on one line)
  the caret still tracks that row's column. That's benign — it's the real cursor
  position, not a strobe.
- This is a *display* gate only. It does not change the grid, scrollback, or
  what the app sees; it only decides which cell the amber rect lands on.

## Key files

- `src/caret.rs` — the **Caret gate**. `CaretGate` (owns the de-jitter state +
  `CURSOR_SETTLE` / `INPUT_GRACE`) exposes `observe(model, now) -> CursorDraw`
  and `note_input(now)`; `cursor_to_draw` is the private decision table.
  Driven entirely through those two methods in unit tests by advancing an
  injected `Instant` — both the decision table and the time-based derivation are
  tested here.
- `src/terminal.rs` — `Session` holds one `caret: CaretGate`; `read_input` calls
  `note_input`, and `show()` builds a `CursorModel` and calls `observe`, then
  owns the focus/scroll paint-gate and the egui drawing.
- `src/inspect.rs` — `cursor_info` (raw model cursor for inspection — reports the
  real cursor, never the gated draw).
