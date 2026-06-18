# Follow-ups: render latency & control plane (2026-06-18)

Snapshot of work shipped in this session and what's left, with enough context to
resume cold. Branch: `main`. See `docs/HANDOFF.md` for the authoritative
architecture doc; this file only covers the latency/control thread.

## Shipped this session

| Commit | What | Why |
|--------|------|-----|
| `1accc46` | Adaptive repaint cadence | Typing/echo latency was floored by a fixed 16 ms repaint metronome. |
| `f3e64ba` | Wake render loop on dispatch | `serve()` had no `Context`; an idle GUI took up to the idle tick to notice a dispatch. |
| `ce61f02` | Encapsulate the PTY-output flag | `pub static PTY_OUTPUT` → private static behind `note_pty_output`/`take_pty_output`. |
| `15f675f` | Thread-per-connection control server | One wedged client parked the single-threaded accept loop and blocked all dispatch. |
| `6ad7f64` | Hold injected input until `ready` | A chat post to a just-launched agent could land mid-DSR-scan and be swallowed. |

### Root cause that tied it together
The app rendered on a fixed `request_repaint_after(16ms)` metronome. **Windows'
~15.6 ms default timer granularity floors any `request_repaint_after` below it**,
so every keystroke and echo waited for a tick (~16–32 ms added on top of the
shell). The actual fast path is the immediate, proxy-driven `ctx.request_repaint()`
the PTY reader threads already fire (~0.2 ms) — the timer is only an idle backstop.
The fix: stay "hot" (fast tick) for 250 ms after any input/PTY-output/dispatch,
idle at 100 ms otherwise; reader threads + `serve()` + winit input all wake the
loop on demand.

## Remaining work (prioritized)

### 1. Per-pane panic isolation — HIGH (queued for the WM branch)
One terminal can abort the whole process: `src/terminal.rs:~727` notes that a
stale alacritty grid index panics, and a panic across the winit callback aborts
the entire app. `install_panic_logger` in `src/main.rs` exists precisely because
of this. For "tmux for AI" running many sessions, one bad pane must not kill the
others.

- **Approach:** wrap each terminal's `show()`/`pump()` in
  `std::panic::catch_unwind(std::panic::AssertUnwindSafe(...))` in `src/wm.rs`
  (where per-terminal `show()` is called); on panic, degrade that pane to an error
  tile instead of re-rendering.
- **Note:** this is belt-and-suspenders — `terminal.rs:~727` already *guards* the
  known index panic defensively; this is about surviving the unknown ones.
- **Blocked on:** `wm.rs`/`layout.rs` have uncommitted WIP; do this after that
  branch lands so the change doesn't tangle with it.

### 2. Grace fallback for ready-gated injection — MEDIUM
`inject_input` now queues a post until the session is `ready` (DSR scan done). A
member that **never emits a device-status reply** never latches `ready`, so a post
to it would stay queued forever. Non-issue for real members (shells + interactive
agents all emit DSR at startup), but it's a sharp edge.

- **Approach:** add a `spawned: Instant` field + a generous `READY_GRACE` (e.g.
  1.5–2 s, comfortably longer than real DSR latency); in `pump()`, latch `ready`
  by timeout as a fallback. Make `READY_GRACE` injectable so the fallback path is
  deterministically testable (TDD), otherwise it's untestable without a contrived
  non-DSR program.

### 3. Control-plane hardening leftovers — LOW
`serve()` is now thread-per-connection with a `MAX_INFLIGHT = 64` backstop. Open
items:
- No per-connection **read timeout** — interprocess' sync stream doesn't cleanly
  expose one, so a wedged handler is reclaimed only when its client goes away
  (bounded by the cap). Add a real timeout if the crate ever supports it.
- Protocol is stringly-typed JSON with v1 assumptions baked into comments
  (`OpenRequest.cmd: String // always "open" in v1`). Fine for now.

## Decisions & known edges (don't re-litigate)

- **vsync left at default (on).** Turning it off did **not** fix the latency (the
  metronome did); with vsync off vs on the typing feel was identical, so it's not
  worth the tearing / heavy-output GPU-spin risk.
- **Render is parse-bound, not draw-bound.** Measured: idle ~0.13 ms/frame; one
  max-rate flood ~0.8 ms; **12 simultaneous max-rate floods ~8 ms avg / 11 ms max**
  (still 60 fps). Cost scales ~linearly per *actively-outputting* terminal (PTY
  parsing), not per visible cell (cells are screen-bounded). Only a pathological
  load (≈20+ continuous simultaneous floods) threatens the 16 ms budget. If you
  ever target 20–30 continuously-noisy agents, the lever is **bounded per-frame
  parsing** (cap bytes fed to the parser per terminal per frame, defer the rest) —
  not worth its complexity/risk before then.
- **The `dead_code` warnings are false positives.** `ready` (getter), `post`,
  `chat_post`, `leaves`, `cwd`/`query`/`selected` are all used only by the test
  suite — they warn because they're `pub` in a *binary* crate and their only
  callers are behind `#[cfg(test)]`. They are NOT dead; don't delete. If a
  zero-warning build is wanted, `#[cfg(test)]` only the two pure test wrappers
  (`post`, `chat_post`) and leave the real component accessors.
- **`shell` field** (`src/terminal.rs`) is write-only (set, never read). Kept by
  decision — reasonable session state to have around; remove only if it stays
  unused (removal threads through `spawn_with`'s signature + 3 call sites).

## How to verify

- `cargo test` (181 tests, no GUI needed).
- For frame-latency work, the throwaway harness pattern: temp-instrument
  `App::ui` in `src/main.rs` to log inter-frame gap + `desktop.show()` duration,
  run the release exe with stderr redirected to a file, type/flood, read the log.
  Remove before committing.
- Drive the GUI from a shell via the control CLI: `foreman status`,
  `foreman open --project p1 -- <cmd>`, `foreman close tN --project p1`.
