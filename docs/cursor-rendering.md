# Cursor rendering (the caret)

## What it does

Foreman draws its own caret. It reads the cursor **state** (position, shape,
visibility) from alacritty's grid model every frame and paints it exactly where
the model says it is — no smoothing, no debouncing, no delay. Shapes are
honored: `Beam` → thin left bar, `Underline` → thin bottom bar, block/anything
else → full cell. There is no blink. The **focused** pane gets a filled amber
rect (`231,169,63` @ alpha 130); **unfocused** panes get a hollow full-cell
outline in the same color (the Alacritty/Kitty/Ghostty convention). No caret is
painted while the pane is scrolled into history or while search owns input.
`?25l` (hide) is honored immediately.

alacritty owns *where and what shape*; Foreman owns *how it looks*. There is no
alacritty renderer in the build — `alacritty_terminal` is just the parser/grid.

## History: the Caret gate, and why it was removed

From 2026-06 to 2026-07 the painted position went through a "Caret gate"
(`caret::CaretGate`): adopt a new cell only after the cursor held it for 50ms,
follow single-row moves immediately only within 150ms of real typing, hold far
jumps. It was built for a real incident — Codex's startup animation and
status-line redraws teleported the cursor mid-redraw, and the painted caret
strobed across the screen.

It was removed (2026-07-15) because measurement showed the problem it guarded
against no longer reaches the painter, while its holds were causing the very
jitter users saw:

- **Modern TUIs bracket redraws in DEC 2026 synchronized output.** A probe
  against Claude Code (`caret_probe_claude_typing` in `src/terminal.rs`) showed
  every keystroke redraw wrapped in `?2026h … ?2026l`. The vte parser buffers a
  sync block and applies it atomically, so the sampled model cursor never shows
  a mid-redraw position — 11 keystrokes produced a perfectly clean col-by-col
  cursor trace. Codex does the same (see the pet-CUP test).
- **PSReadLine (plain pwsh) redraws arrive hide-bracketed** (`?25l … ?25h`) in
  single PTY chunks; `pump()` drains a whole chunk before the painter samples,
  so the model trace was equally clean there.
- **No mainstream terminal debounces caret position.** Survey of Alacritty,
  WezTerm, Kitty, Windows Terminal, Ghostty: all five paint the model cursor
  directly, every frame. Strobing is solved upstream (mode 2026, damage/snapshot
  rendering, apps hiding the cursor during redraw), never by delaying the caret.
- **The gate's cost was user-visible.** Every legitimate ≥2-row move (composer
  growing, screen scrolling while streaming) was held 50ms at a stale cell and
  then snapped; any echo slower than the 150ms grace (Claude Code busy) lost
  the "typing" escape hatch and got held too. That read as a laggy, flashing
  caret precisely while typing.

If a non-synchronized app ever strobes again, the agreed fallback is a
far-jump-only hold (no input-grace machinery) — decided on new evidence, not a
reinstall of the old gate.

## Tuning / gotchas

- The caret is a *display* overlay only. It never changes the grid, scrollback,
  or what the app sees.
- `frame::overlays` suppresses the caret unless `line >= 0` and
  `display_offset == 0` (live viewport only); `show()` adds the focus (fill vs
  hollow) and search gates.
- The unfocused hollow outline forces the shape to Block before the rect math,
  so a beam/underline cursor still reads as a visible cell outline.
- The inspection layer (`foreman snapshot --cursor`) reports the **raw model**
  cursor — with the gate gone, painted and model positions are always the same
  cell.

## Key files

- `src/caret.rs` — `CursorDraw` and the pure `draw(line, col, shape)` mapping
  (Hidden honored, everything else painted as-is). The module docs carry the
  full retirement story.
- `src/terminal.rs` — `show()` builds the `CursorDraw`, forces Block for
  unfocused panes, and paints fill (focused) or 1px inside stroke (unfocused).
  `caret_probe_claude_typing` (ignored test) is the evidence probe: spawns a
  real TUI, types with human gaps, dumps raw ConPTY bytes + model-cursor
  samples.
- `src/frame.rs` — `overlays()` turns the `CursorDraw` into the caret rect
  (wide-char spanning, scrollback suppression).
- `src/geom.rs` — `caret_rect` shape→rect math.
