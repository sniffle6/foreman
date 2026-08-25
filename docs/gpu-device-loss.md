# GPU device loss (the "app vanished after I woke the laptop" crash)

## What breaks

You close the lid. You open it. Foreman is gone — no window, no error, every
agent terminal dead.

What happened: resume from sleep (or Modern Standby) removes the GPU device.
wgpu notices, marks the `Device` permanently invalid (`valid = false`,
wgpu-core `device/resource.rs:669-679`), and from then on every buffer
allocation fails. Stock egui-wgpu handles that with an unconditional `panic!`:

```
panicked at egui-wgpu-0.34.3/src/renderer.rs:971:
Failed to create staging buffer for index data. Index count: ... Required ... Actual ... capacity ...
```

**The size numbers in that message are cosmetic.** They are printed by the
error arm; they are not the failure condition. The real condition is
`device.valid == false`. Anyone who reads that message and starts hunting for a
too-big mesh is chasing a decoy — foreman only ever tessellates the viewport
(`src/frame.rs`), and capping tessellation was measured and ruled out.

How we know it is device loss and not something else: `write_buffer_with`
returns `None` only after `handle_error_nolabel`, and `handle_error_inner`
(wgpu-29.0.3 `backend/wgpu_core.rs:281-299`) returns early for exactly one
error type — `DeviceLost`. Validation, Internal and OutOfMemory all fall
through to `default_error_handler`, which panics with a *different* message
before we could reach the `None` arm. Neither eframe nor egui-wgpu installs an
error scope or an uncaptured-error handler, so that default handler is in
force. `None` therefore means device lost, full stop. The fork logs a loud
`warn!` on that path so that if a future wgpu ever widens it, we notice.

The trigger was confirmed on the affected machine by correlating panic-log
mtimes against Windows Kernel-Power events: every panic sits within seconds of
a "system has resumed from sleep". It is not a TDR (only 3 display-driver reset
events in 60 days, none at a panic time), not heavy paint, not OOM.

## Why we cannot just check a flag before painting

The obvious fix — "test `device_lost()` at the top of the frame and bail" —
does not work, and it is worth writing down why so nobody re-proposes it.

On the *first* failing frame, `Device::lose()` runs **inside** the very call
that then returns `None`:

```
Queue::create_staging_buffer          wgpu-core device/queue.rs:572-586
  -> StagingBuffer::new               wgpu-core resource.rs:1129-1132
    -> device.handle_hal_error        wgpu-core resource.rs:702-711
      -> Device::lose                 wgpu-core resource.rs:5209-5219
```

…and the panic is two lines later. There is no earlier moment at which any flag
is true. The panic has to be *deleted*, not raced.

## What we actually do

1. **A 2-hunk fork of `egui-wgpu` 0.34.3** (`vendor/egui-wgpu`, wired in via
   `[patch.crates-io]`) replaces both `panic!`s in `Renderer::update_buffers`
   with a `queue.write_buffer` fallback and a sticky `device_loss::mark_lost()`.
   The fallback still pushes the same `slices` entries — `Renderer::render`
   `.expect()`s one per mesh, so an early `return` would just relocate the
   crash. A third hunk hardens `Painter::configure_surface` in `winit.rs`, whose
   `.expect("The surface isn't supported by this adapter")` is a second crash
   waiting inside the recovery path.
2. **A device-lost callback** is registered at startup (`src/main.rs`, the
   `run_native` app-creator closure). It is only an *early hint* — wgpu-core
   documents that the closure "might never be called", and `device.destroy()`
   never fires it — but it writes wgpu's own message into the log and saves one
   dead frame when it does fire. The fork's flag is the authoritative detector.
3. **`App::logic`** (not `App::ui` — eframe calls `logic` unconditionally, `ui`
   only when the window is visible) sees the flag on the next frame, saves the
   workspace, respawns foreman through the existing `FOREMAN_WAIT_PID`
   handshake, and `process::exit(0)`s. Not `ViewportCommand::Close`: viewport
   commands are applied *after* the frame paints, so Close would still paint the
   doomed frame.
4. **The panic hook is the net.** If anything still panics with the device
   already lost, the hook respawns before letting the default hook run. With the
   fork in place this branch should be unreachable; if it fires, the fork
   regressed.

## What survives and what does not — read this before calling it "session survival"

**KEPT:** window layout, the project tree, tabs, panel prefs, settings, an
explanatory toast on the new instance, a timestamped log entry.

**LOST:** every PTY child process and the agent running in it, all scrollback,
chat history, and any in-flight `foreman send --settle` reply.

That is not a shortcut we took. Every `Session` owns a `KILL_ON_JOB_CLOSE` job
object (`src/job.rs:36,58-63`) and the PTY master dies with the process, so
`process::exit(0)` destroys the child tree exactly as the panic did. What
changed is: no panic on the common path, the layout is preserved, the save is
ordered rather than whatever the debounce happened to have flushed, the restart
is automatic and labelled, and there is findable evidence.

Agent continuity is the agent CLI's job (`claude --continue`). Real session
survival needs the daemon/client split (`docs/HANDOFF.md`) and is not this.

## The crash-loop guard

Three device losses inside a 10-minute window and we stop restarting. The
record is `%APPDATA%\foreman\gpu-crash.json`
(`{version, last_unix_ms, count}`); the decision is a pure function,
`gpu::decide`, so the table is unit-tested.

Decay is by the time window only — a long-lived successor does **not** reset the
count. That is deliberate: a naive "reset if we ran for two minutes" heuristic
lets a slow crash chain (live 200s, lose the device, respawn, repeat) run
forever. A backwards clock jump (a resume can resync the wall clock by minutes)
counts as "outside the window", i.e. it forgives rather than punishes.

## Where the evidence lives

`%APPDATA%\foreman\foreman_panic.log`, absolute and ISO-8601-timestamped in
local time (GH #3 — it used to be a CWD-relative, undated file, which from an
installed exe is effectively nowhere). It carries:

- one `gpu: backend=... adapter=... name=... driver=...` line per launch, so we
  learn which backend was live at each loss for free;
- `gpu: device lost (<reason>): <wgpu's message>` when the callback fires;
- `gpu: device lost — ordered handoff ...` when the handoff runs;
- the respawn decision, including refusals from the crash-loop guard;
- full panics with backtraces, as before.

Local time is the point: it is what lets you line an entry up against
`Get-WinEvent -FilterHashtable @{LogName='System'; ProviderName='Microsoft-Windows-Kernel-Power'}`.

## Test hooks (debug builds only)

All three compile away in release.

| Env var | What it does |
|---|---|
| `FOREMAN_GPU_LOST_TEST=<secs>` | Flips the device-lost hint by hand after N seconds. No GPU fault at all — exercises the handoff, the guard, the toast and the restart. Fastest signal. |
| `FOREMAN_GPU_DESTROY_TEST=<secs>` | Calls `device.destroy()` once after N seconds. **The primary acceptance hook**: a bit-for-bit reproduction of the production failure. |
| `FOREMAN_FAKE_PANIC=staging` | Panics with the literal upstream message, to exercise the panic hook's string fallback alone. |

`FOREMAN_GPU_DESTROY_TEST` is the right acceptance hook precisely because
`device_destroy` sets `valid = false` *without* invoking the device-lost
closure. It therefore drives the fork's flag — the load-bearing detector — and
not the optimistic callback path. It tests the worst case, not the lucky one.
On stock (unforked) main it produces the exact reported panic; that is the
repro-fidelity check to run before trusting any of the rest.

**⚠ Warning:** a second foreman instance contends for the hard-coded
`\\.\pipe\foreman` (`src/control.rs`, no env override — `listen_retry` retries
rather than refusing), so it can steal `foreman open`/`send`/`snapshot` calls
from the user's live session. Run these only when no agents are dispatching, and
prefer `cargo build --target-dir target/agent`.

## Fork maintenance

The deviation from upstream is **3 hunks in 3 files** plus the `device_loss`
module in `lib.rs`. Both `renderer.rs` and `winit.rs` carry a banner comment
saying so. Re-derive the diff before any eframe bump:

```
diff -ru "$env:USERPROFILE/.cargo/registry/src/index.crates.io-*/egui-wgpu-0.34.3" vendor/egui-wgpu
```

`vendor/egui-wgpu/Cargo.toml` has a bare `[workspace]` table so cargo treats it
as its own workspace root: that keeps its ~250 warn-level clippy lints out of
foreman's build warning baseline, and lets its own tests run with
`cargo test --manifest-path vendor/egui-wgpu/Cargo.toml --lib`. It stays a
one-crate patch because published egui-wgpu depends on egui/epaint/wgpu/winit by
concrete crates.io versions, so nothing else gets duplicated (`cargo tree -d`
confirms).

**Deletion criterion:** the day emilk/egui#8452 (or its successor) ships, delete
`vendor/` and the `[patch.crates-io]` block and keep everything else. Expect
*rework*, not a rebase, past 0.35 — 0.36.0 moved to wgpu v30 and changed the
surface-status API shape.

We deliberately did **not** take PR #8452's other hunks (reordering
`update_buffers` past `get_current_texture`, `needs_render_state_recreate`,
`request_full_texture_reupload`, epaint CPU-side texture retention). They are
unnecessary once the panic is gone, and keeping the fork at 3 hunks is what
makes it cheap to carry and cheap to delete.

## Upstream state (so nobody re-derives it)

- emilk/egui#8265 and #8450 — both open, zero maintainer response.
- emilk/egui PR #8452 — CHANGES_REQUESTED by Wumpf, 2026-08-24.
- gfx-rs/wgpu#7020, #9277, #7075.
- No released egui version fixes this. egui `main` still has both panics
  (reworded and relocated to `renderer.rs:1032/1078`, which is exactly why the
  panic hook keys on our flag first and the message string only as a fallback).

## If foreman ever switches to the glow backend

`cc.wgpu_render_state` disappears and both the tripwire and the fork must be
deleted; the panic-log hardening and the restart machinery survive unchanged.
Glow is a documented escape hatch, not a plan: eframe requests a **non-robust**
GL context and adds per-frame `make_current().unwrap()` panic sites inside
eframe, which are more expensive to fork than what we just did.

## Open question — Phase 2: in-process renderer restart (gated, NOT built)

**Hypothesis.** Session survival is possible if the `App` is moved *out* of
eframe's ownership (a `thread_local!` Workbench plus a zero-sized
`Shell: eframe::App` that forwards `ui`/`on_exit`), so eframe's teardown cannot
drop a `Session` → cannot drop a `job::Job` → cannot kill an agent. Then call
`eframe::run_native` again in a loop, rebuilding Instance/Adapter/Device/
Surface/Window from scratch.

**Why it is gated, not built.** winit's own docs contradict the load-bearing
assumption. `winit-0.30.13/src/platform/run_on_demand.rs:33-35`: *"This API is
not designed to run an event loop in bursts that you can exit from and return to
while maintaining the full state of your application."* And `:48`: *"No Window
state can be carried between separate runs of the event loop."* Windows is
listed as supported, but upstream is steering callers to `pump_app_events`
instead. Buying that assumption costs a `thread_local` App restructure, a new
repaint waker touching every PTY reader thread, `Once` guards around
`control::serve` (a miss silently steals `foreman open` from live agents), and
re-seeding six `ctx.data` singletons. Not paying that on an unproven premise.

**The experiment** (~2 hours, throwaway branch, no interaction with the fork).
Implement only: (a) the thread_local park + `Shell`, using `try_borrow_mut` with
a graceful skip rather than `with_borrow_mut`; and (b) a `loop { }` around
`run_native` driven by `ViewportCommand::Close` on a debug key, with no device
loss involved. Note the creator closure must be **rebuilt** each iteration —
foreman's current one captures a non-`Clone` `mpsc::Receiver` and is `FnOnce`,
so a hoisted `Box::new(creator)` will not compile; the second and later runs
must capture only the thread_local handle and call a new
`App::on_renderer_restart(&cc.egui_ctx)`.

**GO** if, after the second run: every project/terminal is still present;
scrollback is intact; typing reaches the shells; agent output streams *without*
a mouse jiggle (the repaint-waker check — long-lived threads in `terminal.rs`,
`control.rs` and `update.rs` hold a clone of the OLD `Context`); and
`foreman status` from inside a terminal answers **exactly once** (a second
`control::serve` would race `\\.\pipe\foreman` and silently steal dispatches).

**NO-GO** otherwise — and on no-go the fallback is the shipped Phase 1, *not* "a
live process with a frozen window". A frozen window that still accepts
keystrokes into invisible terminals is worse for a multiplexer than a clean,
labelled restart.

If GO, the follow-on work is: `Once` guards on `control::serve` /
`update::spawn` / `skills_install::install` (with the `foreman status` assertion
as its test); an asserted `self.started == true` invariant across re-attach
(otherwise the `!self.started` block restores a *second* desktop from disk
beside the live one); re-seeding `config::seed_live` / `theme::seed_live` /
`keymap::seed_live` / `terminal::set_font_size` / `terminal::set_bell_enabled`
into the new `Context`; a `WindowManager::invalidate_gpu_caches()` clearing
`Session::emoji_textures` / `textures` / `mono_paint` and `ImgState::Ok.texture`;
and window-geometry capture (eframe's `persistence` feature is off, so the
viewport is hard-coded to 1280x800 and every restart would yank the window back).

## Key files

- `src/gpu.rs` — device-loss flags, crash-log I/O, the pure crash-loop guard
  (`decide`), and the respawn primitive.
- `src/main.rs` — `install_panic_logger` (absolute + timestamped + respawn net),
  `App::logic` (the ordered handoff), `App::debug_gpu_test_hooks`, the
  device-lost tripwire and adapter log line in the `run_native` app-creator
  closure, and the `FOREMAN_GPU_RESTART` handshake in `fn main`.
- `vendor/egui-wgpu/src/lib.rs` — the `device_loss` module (the flag).
- `vendor/egui-wgpu/src/renderer.rs` — the two deleted panics, the
  `index_slices` / `vertex_slices` helpers, and their tests.
- `vendor/egui-wgpu/src/winit.rs` — the hardened `configure_surface`.
- `Cargo.toml` — the `[patch.crates-io]` block that wires the fork in.
