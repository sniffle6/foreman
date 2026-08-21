# Image viewer (`foreman view`)

`foreman view <file.png>` opens a persistent window showing that PNG —
a normal window like any terminal: tile it, float it, tab it, close it,
minimize it, and it comes back after a restart.

## Why

`foreman icat` (see `docs/terminal-images.md`) prints an image inline into a
terminal pane — great for "look at this screenshot right now," but it scrolls
away with the buffer and is gone on restart. Sometimes you want an image to
stick around as its own thing: a design mockup you're iterating against, a
diff screenshot you keep open beside the code. That's `Content::Image`.

## How to use it

```
foreman view screenshot.png                 # your project, focused window
foreman view screenshot.png --project p2    # a specific project
```

Reply: `{"ok":true,"terminal":"tN","project":"pN"}` — same shape as `open`.
The window tiles in next to whatever's focused, like a dispatched agent
terminal, and (also like dispatch) it doesn't steal your keyboard focus.

Inside the window:
- **Ctrl+Scroll** zooms, centered on the pointer (0.1x–32x of fit-to-window).
- **Drag** pans, once zoomed in.
- **Ctrl+0** resets to fit-to-window, no pan.

A bad path — missing file, not a real PNG — never crashes or leaves a dead
window. It shows a quiet placeholder: the path and a one-line error, dim text,
wrapped to the pane.

## How it works

- `src/imageview.rs` — `ImageView`: decodes with `graphics::decode_png` (the
  same decoder `icat` uses), holds an egui texture created lazily on first
  paint (same cache-by-generation idiom as the kitty-graphics texture cache in
  `terminal.rs`). The fit/zoom/pan math (`fit_rect`, `zoom_around`,
  `clamp_pan`, `clamp_zoom`) is pure and unit-tested without an egui context.
- `src/control.rs` — the `view` verb: `ViewRequest` / `CtrlMsg::View`,
  `parse_view_args` resolves the path to an absolute, canonicalized form
  **client-side** (the GUI may have a different cwd) and rejects a missing
  file or non-`.png` extension before touching the pipe.
- `src/wm.rs` — `Content::Image(ImageView)`, a window kind with no PTY and no
  chat membership. `WindowManager::add_image` tiles it in
  (`tile_new`), title = the file name. Everywhere else `Content` is matched,
  Image falls in with Chat: no process to kill, not a chat member, doesn't
  block quit.
- `src/workspace.rs` — `ContentSnap::Image { path }` persists just the path
  (v1) — zoom and pan reset on restore. Additive enum variant, so old
  `workspace.json` files without it still load fine.

## Gotchas

- **PNG only, v1.** Same decoder as `icat` — no JPEG/GIF/etc.
- **Zoom/pan don't persist.** Only the path is saved; every restore starts
  fit-to-window.
- **No file watching.** Editing the PNG on disk doesn't refresh an open
  viewer — reopen with `foreman view` (or close and re-`view`) to see changes.
- **`FOREMAN_VIEW_TEST=<path.png>`** (debug builds only): opens an image
  window at startup, for screenshotting the happy path without a live control
  pipe. Exists because a debug instance normally can't reach its *own* pipe
  during testing — the user's real foreman already holds
  `\\.\pipe\foreman`, so a debug `foreman view` from inside the debug
  instance would dispatch to the wrong (real) foreman. Not a general-purpose
  flag — see `src/main.rs`'s `if !self.started` block.
- **OSC 8 links** (clickable file paths in terminal output opening a viewer)
  are a natural follow-up, not built — v1 requires the explicit `foreman
  view` command.

## Key files

- `src/imageview.rs` — model + paint + pure fit/zoom/pan math
- `src/control.rs` — `view` verb: request/reply, arg parsing, help text
- `src/wm.rs` — `Content::Image`, `add_image`, `view_dispatch`
- `src/workspace.rs` — `ContentSnap::Image`, capture/restore
- `src/graphics.rs` — `decode_png` (shared with `icat`)
