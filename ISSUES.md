# Issue Tracker

Lightweight repo-local issue tracker. Rules:

- Every issue must be **fully self-contained**: written for a reader (or a fresh
  agent session) with zero prior context. Include symptom, evidence, root-cause
  analysis so far, and candidate fixes inline — never "see conversation" or
  assume memory of a prior session.
- New issues get the next number. Move fixed issues to the **Closed** section
  with a one-line resolution and the commit hash.

---

## Open

### #1 — App-wide crash: egui-wgpu panics on lost GPU device (`Failed to create staging buffer for index data`)

**Status:** open · **Filed:** 2026-07-09 · **Severity:** high (kills every session in the app)

**Symptom.** Foreman vanishes with no dialog while running — typically after
sitting idle in the background for a while. The terminal that launched it shows:

```
thread 'main' panicked at ...\egui-wgpu-0.34.3\src\renderer.rs:971:17:
Failed to create staging buffer for index data. Index count: 26184.
Required index buffer size: 104736. Actual size 219456 and capacity: 219456 (bytes)
error: process didn't exit successfully: `target\release\foreman.exe` (exit code: 101)
```

**Evidence.** `foreman_panic.log` (written by `install_panic_logger`,
src/main.rs) has captured at least two separate occurrences at the same
location (`egui-wgpu-0.34.3/src/renderer.rs:971`), with different index counts
(31968 and 26184). Backtrace: `egui_wgpu::renderer::Renderer::update_buffers`
→ `Painter::paint_and_update_textures` → eframe/winit frame callback. This is
the "whole app vanishes" class documented in the foreman-debugging-playbook
skill §11.

**Root cause (diagnosed 2026-07-09).** Not a foreman drawing bug and not a
buffer-sizing bug: the panic message itself shows the index buffer was large
enough (required 104,736 ≤ capacity 219,456, size a valid multiple of 4).
egui-wgpu 0.34.3 calls `wgpu::Queue::write_buffer_with(...)` and panics when
it returns `None` (renderer.rs:950-976). With valid size and sufficient
capacity, `None` here means **the wgpu device was in an error state — almost
certainly a lost device** (system sleep/resume, display driver reset/update,
or a TDR triggered by other GPU load while foreman idled). egui-wgpu/eframe
0.34 does not handle device loss; it panics instead of recovering, and per
playbook §11 the panic aborts the whole process — every session dies.

**Candidate fixes (unvalidated):**
1. Upgrade egui/eframe/egui-wgpu — check whether newer versions recover from
   device loss instead of panicking (known upstream pain point; check egui
   changelog/issues for device-lost handling before assuming).
2. Try a different wgpu backend (`WGPU_BACKEND=gl`, or DX12 vs Vulkan) —
   device-loss behavior varies by backend/driver; also identify which
   adapter/backend foreman currently gets.
3. Longer term: session persistence / daemon-client split (already an open
   research item) would make any GUI-process crash survivable by keeping PTYs
   in a separate process.

**Repro.** Not deterministic. Leave foreman running, put the machine through
sleep/resume or heavy GPU load in another app, return and interact. Confirm a
crash by reading `foreman_panic.log` in the directory foreman was launched
from.

---

## Closed

_(none yet)_
