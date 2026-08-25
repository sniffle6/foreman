---
name: foreman-architecture-contract
description: Use when modifying or navigating foreman's core structure (src/wm.rs, src/layout.rs, src/terminal.rs, src/control.rs, src/chat.rs), deciding where new code belongs, or reasoning about focus, coordinates, tabs, threading, or borrows. Triggers: windows painted at the wrong spot after nesting, input reaching two Sessions, egui Id clashes, "already borrowed" BorrowMutError, "foreman did not respond", duplicate dispatches, one panic killing every Session, "can I block the GUI thread?"
---

# Foreman Architecture Contract

The load-bearing design decisions, the invariants that must hold, the seam map,
and the threading model — with WHY, so you don't accidentally re-litigate a
settled decision or violate a contract the rest of the code assumes.

Cites here name a **file and a symbol**, never a line number: symbols survive
refactors, line numbers rot within weeks. `rg -n "fn <symbol>" src/<file>.rs`
lands you on any of them.

Vocabulary is `CONTEXT.md`'s ubiquitous language (Win, Session, Content,
Project, Ready, Deferred action, Quiescence settle, Caret, Cell metrics,
Outbox, Control plane, Snapshot, Dispatch, Leader, Chord, Keymap). Read
`CONTEXT.md` first if any term reads oddly.

## Stack rationale (settled — do not re-litigate)

| Choice | Version (read `Cargo.toml`) | Why |
|---|---|---|
| Native Rust + `eframe`/`egui` | egui 0.34.3 | "Lag in a program like this makes it DOA" — that's why it's native Rust, not Electron/Tauri (`docs/HANDOFF.md`). egui is an immediate-mode GUI library: the whole UI is re-described in code every frame instead of kept as a retained widget tree. Traps and idioms: see **egui-immediate-mode-reference**. |
| `alacritty_terminal` | 0.26.0 | Full VT/ANSI terminal emulation — the grid-engine crate extracted from the Alacritty terminal (also used by Zed; general knowledge, not a repo citation). Foreman does not parse escape sequences itself. Domain pack: **terminal-emulation-reference**. |
| `portable-pty` | 0.9.0 | A PTY (pseudo-terminal) is the OS object a shell believes is its terminal; on Windows the implementation is ConPTY. This crate wraps it. |
| `interprocess` | 2 | The Control plane transport: a Windows named pipe `\\.\pipe\foreman` (`src/control.rs` `PIPE`). |

**The `eframe` line carries `default-features = false` and selects glow, and
that is load-bearing, not a leftover.** eframe prefers wgpu whenever both
backends are enabled; on Windows, sleep and display-power transitions lose the
GPU device, and `egui-wgpu` answers device loss with an unconditional `panic!`
in `update_buffers` that aborts the process — `egui_glow` only logs. Do not
"modernize" it back. The side-by-side A/B that settled it is
`docs/gpu-device-loss.md`; crash evidence is
`%APPDATA%\foreman\foreman_panic.log`.

Rust edition 2024, GNU toolchain (build details belong to
**foreman-build-and-env**, not here). Change classification and gates:
**foreman-change-control**.

## The recursive compositor

ONE `WindowManager` engine (`struct WindowManager`, `src/wm.rs`) runs at two
levels:

- **Desktop level:** `App` in `src/main.rs` hosts one desktop manager full-bleed
  (`WindowManager::new().as_desktop()`, then `desktop.show(...)` once per frame).
- **Nested per Project:** a Project Win's Content is another manager —
  `Content::Project(Box<WindowManager>)`. `Content::show` recurses: the
  Project's content rect becomes the child manager's `area`.

There is no third mechanism. Anything the engine can do (drag, tile, tab,
focus, zoom) works identically at both levels because it IS the same code.

What a Win may hold is `enum Content` in `src/wm.rs` — read it there; the set
grows (chat viewer, image viewer, task-manager panel, settings menu have all
been added since this skill was written) and any list reproduced here would be
short by the time you read it. Only `Content::Project` recurses.

**Focus cascades so exactly one Session reads the keyboard.** `show(...,
active, ...)` takes an `active` flag that ANDs down the recursion to exactly one
leaf terminal. Within a manager, only the focused, non-modal Win is active: the
directory picker, inline rename, and the settings modal each force
`is_focus = false` for every Win so typed text can't also leak into the focused
terminal. The Leader state machine runs only on the desktop manager
(`desktop: bool`), once per frame, before recursion — so command Chords never
reach a PTY.

**egui Id re-basing:** every child manager numbers its Wins from 1 (`next: 1`),
so terminal #1 exists in every Project. egui identifies widgets by hashed stable
Ids; interaction Ids here derive from a base (`base.with((id, "drag"))` etc.),
so without re-basing, two Projects' terminal #1 would collide and interaction
state would misroute. The fix is one line: each Project recursion re-bases with
`base.with(("proj", win_id))` in `Content::show`. If you add any new per-Win
egui Id, derive it from `base`, never from the bare `WinId`.

## The coordinate contract

`Win.rect` is **LOCAL** to its manager's `area` — "local coords (origin =
manager area.min)" (the field comment on `Win.rect`, `src/wm.rs`).

| Conversion | Code |
|---|---|
| local → screen | `let scr = w.rect.translate(area.min.to_vec2());` |
| screen pointer → local | `let local = p - area.min.to_vec2();` |

Mixing the two spaces is a recurring bug class: a rect that looks right on the
desktop and is offset by the Project's origin when nested means you skipped a
translate. Convert exactly once, at the draw/interaction boundary, and keep all
stored state local. The layout tree also works in local space
(`tree.layout(Rect::from_min_size(Pos2::ZERO, asz), ...)`).

## Two window states (and zoom, which is neither)

Every Win is **tiled** iff its id is a leaf of the manager's
`crate::layout::LayoutTree` (the `tree` field, `src/wm.rs`); otherwise it is
**floating**. Rules that follow:

- **Never trust `Win.rect` as persistent state for a tiled Win.** It is
  overwritten every frame from the tree layout (or the zoom rect, or clamped
  back into the area). The float-restore rect lives in `Win.prev` — set on
  tiling (`tile_new`) and taken on tear-out.
- **One-frame rect lag on drop is intentional.** Dropping a Win into the tree
  does NOT set its rect immediately: "Rect refits from the tree next frame (one
  frame at the drop position — invisible at 60fps; intentionally no immediate
  set)". Don't "fix" this.
- **Zoom is an OVERLAY.** `toggle_zoom` (`src/wm.rs`) only sets
  `zoomed: Option<WinId>`; the tree and every other Win are untouched, and a
  floating Win's rect survives a zoom round-trip (unit test
  `zoom_overlays_without_touching_the_tree_or_floating_rect`).
- A lone, tiled, single-tab non-Project Win draws no chrome of its own (the
  "bare" branch in `WindowManager::show`) — the parent frame is its only frame.

The tree itself (insert/remove/layout/drop-targets/divider resize) is pure and
unit-tested in src/layout.rs; deep dive in `docs/tiling-tree.md`.

## Tab invariants

- **`Win.tabs` is never empty** — "closing the last tab closes the window" (the
  invariant comment on `Win.tabs`, `src/wm.rs`). A len-1 stack renders as a
  plain window (no tab bar).
- **Tabbing only within the same manager** (Projects with Projects, terminals
  with terminals). This is **structural, not enforced by a check**: the only
  producer of `Act::Merge` is a manager's own drag loop scanning its own
  `windows` (`merge_target_at` → `merge_windows`, `src/wm.rs`). If you ever add
  a cross-manager move, you must add the enforcement you are currently getting
  for free.
- **Only the active tab renders and reads the keyboard, but inactive Sessions
  stay alive**: each Session's reader thread runs regardless, and
  `Win::keepalive_inactive` (`src/wm.rs`) pumps every background tab's PTY each
  frame so device queries are answered and output is consumed. A background tab
  is hidden, not paused.
- A Session's **Member id** (`Session::term_id`, `src/terminal.rs`) is its
  stable identity for chat/dispatch — stamped once at spawn, equal to the
  `FOREMAN_TERMINAL_ID` env value, and unchanged by tabbing/untabbing/moving.
  `WinId`s are NOT stable identity (tab merges retire them).

## The Deferred action pattern

`WindowManager::show`'s draw pass records a `Vec<Act>` and applies them
**after** the per-Win loop. Why: applying immediately would need `&mut self`
mutation (close, merge, spawn into a nested manager) while the render borrow of
`self.windows[i]` is live — the borrow checker forbids it, and immediate
mutation mid-loop would also invalidate the frame's window ordering.

For the variant list, read `enum Act` in `src/wm.rs` — most variants carry a
doc comment saying why they had to be deferred. The one thing a code read does
*not* tell you: **there is no `Act::AddProject`**, and that is deliberate —
project creation flows through `OpenProjectPicker` → the DirPicker modal →
`add_project` on accept, because the directory is chosen across frames.

When you add a window interaction: record an `Act` in the draw pass, handle it
in the apply pass. Do not mutate `self.windows` or a nested manager inside the
draw loop.

## Seam map (name → file:symbol)

These are deliberate, tested seams (CONTEXT.md "Seams & patterns"). One home
each — put new logic of that kind THERE.

| Seam | Where | What it isolates |
|---|---|---|
| Deferred action | `src/wm.rs` `enum Act` + apply pass | Window mutations out of the render borrow |
| Input-encoding seam | `src/input.rs` `process_input(...) -> InputOutcome` | egui events → exact PTY bytes, GUI-free |
| Quiescence settle | `src/wm.rs` `settle_tick`, `PendingSettle`, `advance_settles` | "wait until the Session quiets" without blocking the GUI. The default window is the user setting `Settings::send_settle_ms` (`src/config.rs`); `MAX_SETTLE_MS` is the hard cap and is deliberately under `REPLY_TIMEOUT` so a settle reply always beats the pipe timeout |
| Caret | `src/caret.rs` `draw` | What to paint for the model cursor (pure mapping; the de-jitter gate was retired 2026-07-15) |
| Cell metrics | `src/geom.rs` `CellMetrics` | One frame's pixel↔cell geometry; all clamping in one place |
| Outbox | `src/chat.rs` `ChatRoom::tick` | Per-frame chat delivery decision (who gets which framed lines); the engine wiring (`WindowManager::chat_tick`, `src/wm.rs`) only injects what the Outbox returns |
| Paint plan | `src/frame.rs` `plan_paint(grid, metrics, colors) -> PaintPlan` (plus `text_rows` and `overlays`) | One frame's paint geometry/content for a Session, pure; `Session::show` calls it and replays the result. Clamps the grid walk to the grid's REAL bounds first because a stale index panic aborts the whole process (module docs, `src/frame.rs`) |

## Pure-module map (testable without a GUI)

"Pure" here = drivable entirely from unit tests: no window, no PTY, no event
loop. They may import egui/alacritty **data types** (Rect, TermMode, grids).
All are unit-tested in-module; the test inventory and per-module counts belong
to **foreman-validation-and-qa**.

| Module | Notes |
|---|---|
| src/layout.rs | Tiling tree; imports only `wm::{Dir, WinId}` + egui geometry types |
| src/chat.rs | std-only; room owns NO window ids (wiring re-attaches them) |
| src/input.rs | egui event types + alacritty `TermMode` in, bytes out |
| src/inspect.rs | Snapshot/key-encoding; generic over alacritty's `EventListener`, tests drive a `Term<VoidListener>` with fixed bytes — the ONE legitimate `VoidListener` use |
| src/caret.rs | Pure mapping from the model cursor to what to paint; never reads the clock |
| src/geom.rs | Cell metrics (see seam map) |
| src/frame.rs | Paint plan (see seam map) |
| src/ready.rs, src/keymap.rs, src/theme.rs, src/panel.rs, … | Same shape — decision logic with plain-data inputs. `rg -l "mod tests" src/` lists the current set; the list grows and is not worth reproducing here |
| src/control.rs | **Transport-first**: it parses/serializes and never touches `wm` or `terminal`. It does reach into `crate::inspect` (reply payload types), `crate::chat` (target validation) and `crate::icat` (subcommand dispatch) — those are leaf, GUI-free modules, and that boundary is the one to hold. `WindowManager::handle_ctrl` (`src/wm.rs`) is the sole production consumer of `CtrlMsg` |

## Threading model

**The GUI thread never blocks.** That is the whole model; everything below
serves it.

| Thread | Created | Blocks on |
|---|---|---|
| GUI/render (winit main) | eframe | Nothing. PTY drain is `rx.try_recv()` (`Session::pump`); control drain is `ctrl.try_recv()` in `App::ui`; settles are deferred cross-frame (`advance_settles`) |
| 1 reader thread per Session | `Session::spawn` (`src/terminal.rs`) | Blocking `reader.read()` on the PTY → sends chunk over mpsc → `note_pty_output()` + `ctx.request_repaint()`. Dies when the PTY closes |
| 1 control serve thread | spawned from `main` | `listener.incoming()` on the named pipe (`control::serve`) |
| 1 short-lived thread per pipe connection | `control::serve` | The **only** reply wait in the app: `rrx.recv_timeout(REPLY_TIMEOUT)`. Capped by `MAX_INFLIGHT` concurrent handlers; over the cap, reject fast |

**Shared-state inventory:**

| State | Type | Actually cross-thread? |
|---|---|---|
| `Session.resp`, `Session.osc_title` | `Arc<Mutex<…>>` (`src/terminal.rs`) | No — shared *ownership* between the `Term`-owned `Listener` and the Session; the Listener fires during `parser.advance` inside `pump()` on the GUI thread. Aliasing, not parallelism, in the current wiring |
| `PTY_OUTPUT` | `static AtomicBool` (`src/terminal.rs`) | Yes — written by reader threads, swapped by the GUI thread for the adaptive repaint cadence (hot tick after input/output, slow idle tick; `App::ui`, `src/main.rs`). Scheduling only, never correctness |
| Chat room | `Rc<RefCell<ChatRoom>>` (`src/wm.rs`) | No — single-threaded by construction; shared between the manager and `Content::Chat` viewers. Borrow discipline: `chat_tick` clones the Rc and drops the `borrow_mut` before injecting |
| Process-table Scanner | `thread_local!` (`src/proc.rs`) | Per-thread; used from the GUI thread |
| Channels | mpsc: PTY bytes (reader→Session), `CtrlMsg` (conn thread→GUI), `OpenReply` (GUI→conn thread) | Yes — the sanctioned cross-thread paths |

**The request flow, in words** (Control plane CLI usage/verbs belong to
**foreman-run-and-operate**; agent-facing usage to **foreman-dispatch** /
**foreman-chat**):

`foreman <verb>` runs as a second foreman.exe process (subcommand short-circuit
in `main`) → one JSON line over the named pipe `\\.\pipe\foreman` → `serve`
accepts and spawns a connection thread → parses into a `CtrlMsg` carrying a
reply `Sender` and the arrival `Instant` → sends it over mpsc to the GUI and
calls `ctx.request_repaint()` so an idle render loop wakes NOW → `App::ui`
drains via `try_recv` → `desktop.handle_ctrl` executes → reply goes back over
the reply channel → the connection thread's `recv_timeout(REPLY_TIMEOUT)`
returns it → one JSON line back → the CLI prints and exits.

Contract riders on that flow (`WindowManager::handle_ctrl`, `src/wm.rs`):

- **Drop stale requests unexecuted**: if `sent.elapsed() >= REPLY_TIMEOUT` the
  client was already told "foreman did not respond"; executing anyway would
  spawn a terminal the dispatcher believes failed (and a retry would duplicate
  it).
- **`open` undoes orphaned spawns** when the reply send fails.
- **Chat replies BEFORE injecting** — an injection cannot be undone, so bytes
  flow only once the client is guaranteed to hear "ok".
- **`send` never blocks**: it enqueues a `PendingSettle`; `advance_settles`
  (called once per frame after `show()` and `chat_tick`, in `App::ui`) replies
  when the terminal quiets or the deadline passes.

## Invariants checklist (violating any of these is a bug)

- [ ] Live Sessions never use `VoidListener` — the `Listener` captures
      `Event::PtyWrite` (DSR replies, color-query answers) and `pump()` flushes
      it back, which is also what latches `Ready`. A `VoidListener` Session
      hangs black forever.
- [ ] The GUI thread never blocks; the only `recv_timeout` lives on pipe
      connection threads.
- [ ] Exactly one Session reads the keyboard (`active` ANDs down; modals
      suppress all).
- [ ] `Win.tabs` never empty.
- [ ] `Win.rect` is local; translate exactly once at the boundary.
- [ ] A tiled Win's rect is derived per frame — never persist or trust it
      across frames; `Win.prev` is the float-restore rect.
- [ ] Draw pass records `Act`s; mutation happens in the apply pass.
- [ ] Reply-before-inject for chat; drop requests older than `REPLY_TIMEOUT`;
      `MAX_SETTLE_MS` stays under `REPLY_TIMEOUT`.
- [ ] Member id (`term_id`) is the stable identity; never key chat/dispatch
      state on a `WinId`.
- [ ] The renderer stays glow; never re-enable eframe's default features.
- [ ] Any grid walk clamps to the grid's REAL bounds first (frame.rs doctrine)
      — an index panic crosses the winit callback and aborts the process.
- [ ] The chat room owns no window ids; new egui Ids derive from `base`.

## Known-weak points (stated plainly)

| Weakness | Evidence | Status |
|---|---|---|
| No per-Session panic isolation: one panic anywhere in the frame aborts the whole process and kills the entire fleet | `rg catch_unwind src/` returns nothing; the panic logger (`%APPDATA%\foreman\foreman_panic.log`) is post-mortem only | Open. frame.rs's clamp guard removes one panic class, not the blast radius |
| Chat log is in-memory only — restart/crash wipes it, `#N` cites dangle | `rg "std::fs" src/chat.rs` returns nothing | **Designed, not built**: `docs/chat-persistence.md` (append-only JSONL plan, converged 2026-06-27) |
| Toolchain is machine-global | rustup default + w64devkit on PATH; nothing pinned in-repo | Open; recreate steps in **foreman-build-and-env** |
| `docs/HANDOFF.md` drift | HANDOFF.md declares itself authoritative but never mentions frame.rs/geom.rs/caret.rs — one `rg` run per filename over `docs/HANDOFF.md` returns nothing (commands below the table) | Trust map in **foreman-docs-and-writing**; prefer code + this skill for structure |
| Ordinary commits and PRs are ungated by CI | `.github/workflows/release.yml` is the only workflow, and it runs `cargo test` only on `v*` tag pushes, on PRs whose paths touch `release.yml`/`install.ps1`, and on `workflow_dispatch` | Open. Local `cargo test` is the real gate for normal changes — **foreman-validation-and-qa** |
| WSL blind spots | `Shell::Bash` spawns `wsl.exe`; the process-tree agent scan cannot see inside the WSL VM (`src/proc.rs` module docs), so WSL agents rely on the OSC-title path | Known limitation, documented in proc.rs module docs |
| Stringly JSON v1 protocol | wire format is one JSON line with a string `cmd` discriminator, matched by hand in `control::serve`; unknown fields silently ignored by serde defaults | Open; protocol details in **foreman-run-and-operate** |
| Control pipe is same-user trust, a guardrail not a security boundary | "NOT a security boundary — any local process can speak to the pipe and claim any `from`" (`src/control.rs` module docs) | Deliberate scope decision; rationale in **foreman-change-control** |

Re-check the HANDOFF.md drift row (still drifted = every run prints nothing):

```
rg -n "frame\.rs" docs/HANDOFF.md
rg -n "geom\.rs" docs/HANDOFF.md
rg -n "caret\.rs" docs/HANDOFF.md
```

## When NOT to use this skill

- Chasing a live symptom → **foreman-debugging-playbook** (this skill says how
  the machine is *meant* to work; that one says what breaks and why).
- Whether a structural change is even allowed → **foreman-change-control**.

## Provenance and maintenance

Cites here are file + symbol on purpose: `rg` finds them after any refactor, and
a stale symbol name fails loudly instead of pointing at the wrong line. Four
claims are both load-bearing and volatile enough to re-check before you repeat
them:

| Claim | Re-verify with |
|---|---|
| No `Act::AddProject` (project creation goes through the picker) | `rg -n "AddProject" src/wm.rs` — any hit means this section is wrong |
| Renderer is glow, not wgpu | `rg -n "glow" Cargo.toml; rg -n "wgpu" Cargo.toml; rg -n "default-features" Cargo.toml` — the `eframe` line must keep `default-features = false` + the `glow` feature; `wgpu` should appear only in the warning comment |
| Chat log still has no persistence | `rg -n "std::fs" src/chat.rs` — hits mean `docs/chat-persistence.md` got built |
| `MAX_SETTLE_MS` still under `REPLY_TIMEOUT` | `rg -n "MAX_SETTLE_MS" src/wm.rs; rg -n "REPLY_TIMEOUT" src/control.rs` |
