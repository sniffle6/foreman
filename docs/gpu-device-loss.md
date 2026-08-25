# GPU device loss (why foreman renders on OpenGL, not wgpu)

## What it does

`Cargo.toml` pins `eframe` to the **glow** (OpenGL/WGL) renderer instead of the
default wgpu one:

```toml
eframe = { version = "0.34.3", default-features = false, features = [
    "accesskit", "default_fonts", "glow", "wayland", "x11",
] }
```

`default-features = false` is required, not cosmetic — eframe deliberately
prefers wgpu whenever both backends are enabled.

That is the entire fix. There is no recovery code, no restart handler, and no
vendored fork.

## Why it exists

Windows takes the GPU device away from running apps. It happens on sleep, on
resume, and on plain display power transitions — several times a day on a
laptop. Every graphics stack has to cope with it.

wgpu models device loss as a first-class state, and `egui-wgpu` responds to it
by **aborting the process**:

```rust
let Some(mut index_buffer_staging) = index_buffer_staging else {
    panic!("Failed to create staging buffer for index data. ...");
};
```

`Queue::write_buffer_with` returns `None` only on device loss — every other
error class (OutOfMemory, Validation, Internal) reaches wgpu's own
`default_error_handler` and panics there instead, so `None` is unambiguous. The
panic unwinds across the winit callback and kills foreman, taking every PTY with
it.

`egui_glow` has no equivalent. Its entire reaction to a lost context — including
a literal `GL_CONTEXT_LOST` — is `log::error!`, and only in debug builds.

**foreman is renderer-agnostic**, which is what makes this switch cheap. There
is not one wgpu- or GL-specific call in `src/`: every texture goes through
`egui::ColorImage` + `ctx.load_texture`. The only source change the switch
required was `App::on_exit`, whose signature eframe `cfg`s on the renderer
feature.

## How it was decided

Not by reasoning — by running both.

Both builds were launched **at the same time** on the affected laptop and put
through the same device-loss event. The wgpu build took its 11th panic. The glow
build kept running and was still rendering terminals correctly afterwards.

Running them concurrently is what makes that a real result: both processes saw
the identical event on the identical GPU, so the wgpu crash is its own positive
control. Sequential runs could not have proved it — the crash only fires on
roughly two thirds of transitions, so a lone glow survival would have been
indistinguishable from luck.

That 11th panic also killed the original theory for good:

```
Index count: 43308. Required index buffer size: 173232.
Actual size 1926144 and capacity: 1926144 (bytes)
```

The crash was first reported as too much terminal geometry per frame. Here a
**173 KB** allocation failed against a **1.9 MB** buffer — 11× headroom — on a
frame with a *quarter* the geometry of the smallest previous crash. Size was
never the variable. (foreman only ever meshes the viewport anyway;
`frame.rs::plan_paint` walks `metrics.rows().min(grid.screen_lines())`, so
scrollback depth cannot affect the index count.)

## Gotchas

- **Do not switch back to wgpu** without re-running the side-by-side test. The
  panic is still present in `egui-wgpu` 0.36.1, the current release.
- **glow is not panic-free**, it is panic-*rarer*. eframe's `change_gl_context`
  unwraps `make_current()` every frame on Windows (the is-current early-out is
  explicitly disabled there). A `wglMakeCurrent` failure would still abort. What
  changed is that the routine, expected failure — device loss — no longer does.
- **eframe requests a non-robust GL context**, so a GPU reset raises no
  `GL_CONTEXT_LOST` at all; the driver's behavior is undefined-but-typically-
  silent. This is the theoretical risk of the approach: a reset could in
  principle leave garbage on screen rather than erroring. It did not in testing,
  but if foreman ever wakes up visibly corrupt rather than crashed, start here.
- **`NativeOptions::vsync` changes meaning.** It is a no-op under wgpu and live
  under glow.
- **The GUI cannot be seen from the terminal.** Renderer changes need a real
  launch and a screenshot — emoji and a TUI are the acid test, since glow moves
  the texture upload path.
- **A device loss is not always a sleep.** One recorded crash had no
  corresponding Power-Troubleshooter or Kernel-Power event at all. Brief GPU
  power transitions do it too, which is why "just restart on loss" was rejected:
  it would fire at unpredictable moments during normal use.

## The road not taken

`wip/wgpu-device-loss-fix` holds the alternative: a `[patch.crates-io]` fork of
`egui-wgpu` that swaps the panic for a sticky flag, plus `src/gpu.rs` with a
crash-loop guard and an ordered save-and-respawn. It works, and it is kept in
case glow ever proves worse.

It was rejected because it never saved the agents. Every `Session` owns a
`KILL_ON_JOB_CLOSE` job (`src/job.rs`), so the PTY children die with the
process whether it exits cleanly or panics — an ordered restart is still a
restart. It also carried a vendored fork pinned to one `egui-wgpu` version,
taxing every future egui upgrade until [emilk/egui#8452] lands.

Note that surviving the frame is **not** the same as surviving the crash. If
foreman ever does need to keep agents alive across a GUI death, that is session
persistence, and no renderer choice affects it.

[emilk/egui#8452]: https://github.com/emilk/egui/issues/8452

## Key files

- `Cargo.toml` — the `eframe` dependency line; the whole fix
- `src/main.rs` — `App::on_exit` (renderer-`cfg`'d signature),
  `install_panic_logger` / `crash_log_path` (the crash log)
- `src/frame.rs` — `plan_paint`, which bounds tessellation to the viewport
- `src/job.rs` — `KILL_ON_JOB_CLOSE`, why a restart cannot save PTYs
- GH #2 — the original report and the full diagnosis
