# Terminal mouse reporting (click / drag / motion)

## What it does

Forwards pointer press, release, held drag, and (in mode 1003) hover motion to
the app as xterm mouse sequences, so vim (`:set mouse=a`), lazygit, and htop
receive clicks. Wheel forwarding was already shipped; this completes the
click/drag path.

## Why

Without app mouse mode, every TUI treats the mouse as dead chrome. With it,
users need Shift-drag to keep Foreman's local selection.

## How it works

1. Raw `Event::PointerButton` / `PointerMoved` (not `drag_started` — that waits
   for movement; protocols need an immediate press).
2. Cell = `CellMetrics::mouse_cell` (1-based col,row).
3. At **press**, freeze owner + encoding bits:
   - Shift, scrolled-into-history, or no mouse mode → **Local** (selection).
   - Else → **Application** (PTY bytes).
4. Owner stays frozen until the matching release (even if Shift/mode change).
5. Encoding precedence: SGR 1006 → UTF-8 1005 → legacy X10.
6. Tracking: 1000 click only; 1002 +held motion; 1003 +hover. Same-cell motion
   is deduped.
7. Cancel/focus-loss/search-open synthesizes exactly one matching release.

## Gotchas

- New capture requires the pane to be **active**, content-rect containment, and
  **topmost layer ownership** under the pointer (menus/popups above the grid
  do not send TUI mouse bytes).
- Per-button capture (`[Option<MouseCapture>; 3]`); a second button does not
  overwrite the first. `press_sent` is false when the press was unencodable —
  drag/release/cancel then emit nothing (no ghost button-up).
- 1003 hover is suppressed while **any** capture (local or app) exists and
  while viewing history (`display_offset > 0`).
- OS focus loss cancels captures and blocks new presses for the rest of that
  frame (raw events cannot recreate capture mid-frame).
- Hidden/minimized tabs `keepalive` cancel stuck buttons.
- Main-screen history scroll: clicks stay local (no fake live coords).
- Alternate-screen TUIs have no scrollback; clicks always report when mode is on.
- Wheel is unchanged (hover-under-pointer, not Shift-overridden).
- Extra mouse buttons ignored.
- Unencodable legacy/UTF-8 **press** coords are dropped; a valid press with
  release past the limit falls back to the last encodable cell.

## Key files

- `src/input.rs` — `encode_mouse_report`, capture helpers, byte tests
- `src/terminal.rs` — `Session::handle_mouse`, selection suppression
- `src/geom.rs` — `mouse_cell`
