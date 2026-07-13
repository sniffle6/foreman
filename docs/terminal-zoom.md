# Terminal text zoom (Ctrl+Scroll)

Change the terminal font size on the fly, like a browser or VS Code.

## What it does

- **Ctrl + mouse wheel** over any terminal pane → grows / shrinks the text.
- **Ctrl + 0** → reset to the default (13px).
- The size is **global**: every terminal pane shares one size, so zooming any
  pane zooms them all.
- The size **persists** across restarts (saved to `settings.json`).

Zoom reflows the pane — bigger text means fewer rows/cols, and the PTY is resized
to match, exactly like dragging a real terminal smaller. That's expected.

## Why it exists

Plain quality-of-life: fixed 13px was too small/large depending on the monitor,
and there was no way to change it.

## How it works (the wiring)

The font size lives as one global value, parked in egui's per-context data
(`terminal::font_size` / `set_font_size`). This avoids threading a font-size
parameter through the recursive window managers — every `Session::show` just
reads the shared value.

The loop each frame (`App::ui` in `src/main.rs`):

1. **Seed**: push the persisted `Settings.font_size` into egui data.
2. **Show**: every pane reads it. A pane with Ctrl held over it intercepts the
   wheel (`Session::show`) and writes a new value; Ctrl+0 (handled in
   `read_input`) writes the default.
3. **Read back**: `App` reads the value back; if it changed, it remembers it and
   arms a ~400ms debounce.
4. **Save**: after the debounce, write `settings.json` once.

The zoom math is a pure function, `input::zoom_step(cur, steps)` (clamped to
6–40px), unit-tested alongside the other input seams.

## ⚠ The non-obvious gotcha: egui steals Ctrl+Scroll and Ctrl+0 by default

egui has its own built-in *whole-UI* zoom, and it grabs exactly these inputs
before we can:

- `input_options.zoom_modifier` defaults to Ctrl, so egui diverts Ctrl+wheel into
  a UI zoom and leaves `smooth_scroll_delta` **zero** — our handler would see
  nothing.
- `zoom_with_keyboard` (default on) consumes Ctrl+0 / Ctrl+± to scale all chrome.

So `App` (startup) turns both off:

```rust
ctx.options_mut(|o| {
    o.zoom_with_keyboard = false;
    o.input_options.zoom_modifier = egui::Modifiers::NONE;
});
```

Without this, Ctrl+Scroll does nothing to the text and instead silently rescales
the entire window. If zoom ever "stops working," check these options first.

## Gotchas

- **Ctrl+Scroll is consumed.** When Ctrl is held, the wheel zooms and does *not*
  also scroll scrollback or get forwarded to the TUI app (it shares the wheel
  block with the Phase-3 mouse-forwarding, so the Ctrl branch comes first).
- **One-frame lag.** A zoom takes effect on the *next* frame (the current frame
  already computed cell size from the old value). Imperceptible at 60fps.
- **Debounced save, not per-notch.** A scroll gesture fires many notches; the
  file is written once after you stop, not on every notch.
- **Ctrl+0 only works while a terminal is focused** (it rides the terminal key
  path). Zoom is a terminal concept, so that's fine.
- Smoothing: like line-scroll, the wheel notch arrives as fractional
  `smooth_scroll_delta`, so a per-pane `zoom_accum` carries the remainder and
  emits whole steps (`input::WHEEL_NOTCH_PX` ≈ one physical notch).

## Key files

- `src/terminal.rs` — `font_size`/`set_font_size` (the global value),
  `Session::show` (the Ctrl+Scroll branch + reading the live size for the font),
  `read_input` (Ctrl+0 reset), `zoom_accum`, `input::WHEEL_NOTCH_PX`.
- `src/input.rs` — pure `zoom_step`, and the `zoom_reset` flag set by Ctrl+0.
- `src/main.rs` — `App` seeds/reads-back/persists the size each frame.
- `src/config.rs` — the size constants and the persisted `Settings`. See
  `docs/settings-persistence.md`.
