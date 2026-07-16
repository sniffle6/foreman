---
name: foreman-architecture-contract
description: Use when modifying or navigating foreman's core structure (src/wm.rs, src/layout.rs, src/terminal.rs, src/control.rs, src/chat.rs), deciding where new code belongs, or reasoning about focus, coordinates, tabs, threading, or borrows. Triggers: windows painted at the wrong spot after nesting, input reaching two Sessions, egui Id clashes, "already borrowed" BorrowMutError, "foreman did not respond", duplicate dispatches, one panic killing every Session, "can I block the GUI thread?"
---

# Foreman Architecture Contract

The load-bearing design decisions, the invariants that must hold, the seam map,
and the threading model — with WHY, so you don't accidentally re-litigate a
settled decision or violate a contract the rest of the code assumes.

**Baseline:** commit `7fda1c2` on `main` (2026-07-01). All `file:line` cites are
against that commit; line numbers drift, symbols mostly don't. Re-verify
commands are in "Provenance and maintenance" at the bottom.

Vocabulary is `CONTEXT.md`'s ubiquitous language (Win, Session, Content,
Project, Ready, Deferred action, Quiescence settle, Caret, Cell metrics,
Outbox, Control plane, Snapshot, Dispatch, Leader, Chord, Keymap). Read
`CONTEXT.md` first if any term reads oddly.

## Stack rationale (settled — do not re-litigate)

| Choice | Version (Cargo.toml/lock, as of 2026-07-01) | Why |
|---|---|---|
| Native Rust + `eframe`/`egui` | egui 0.34.3 | "Lag in a program like this makes it DOA" — that's why it's native Rust, not Electron/Tauri (docs/HANDOFF.md:19-20). egui is an immediate-mode GUI library: the whole UI is re-described in code every frame instead of kept as a retained widget tree. Traps and idioms: see **egui-immediate-mode-reference**. |
| `alacritty_terminal` | 0.26.0 | Full VT/ANSI terminal emulation — the grid-engine crate extracted from the Alacritty terminal (also used by Zed; general knowledge, not a repo citation). Foreman does not parse escape sequences itself. Domain pack: **terminal-emulation-reference**. |
| `portable-pty` | 0.9.0 | A PTY (pseudo-terminal) is the OS object a shell believes is its terminal; on Windows the implementation is ConPTY. This crate wraps it. |
| `interprocess` | 2.4.2 | The Control plane transport: a Windows named pipe `\\.\pipe\foreman` (control.rs:1-6). |

Rust edition 2024, GNU toolchain (build details belong to
**foreman-build-and-env**, not here). Change classification and gates:
**foreman-change-control**.

## The recursive compositor

ONE `WindowManager` engine (src/wm.rs:601) runs at two levels:

- **Desktop level:** `App` in src/main.rs hosts one desktop manager full-bleed
  (`WindowManager::new().as_desktop()`, main.rs:51; `desktop.show(...)`,
  main.rs:375).
- **Nested per Project:** a Project Win's Content is another manager —
  `Content::Project(Box<WindowManager>)` (wm.rs:125-133). `Content::show`
  recurses: the Project's content rect becomes the child manager's `area`
  (wm.rs:156).

There is no third mechanism. Anything the engine can do (drag, tile, tab,
focus, zoom) works identically at both levels because it IS the same code.

**Focus cascades so exactly one Session reads the keyboard.** `show(...,
active, ...)` takes an `active` flag that ANDs down the recursion "to exactly
one leaf terminal" (comment at wm.rs:153-155). Within a manager, only the
focused, non-modal Win is active: the directory picker, inline rename, and the
settings modal each force `is_focus = false` for every Win so typed text can't
also leak into the focused terminal (wm.rs:2345-2353). The Leader state machine
runs only on the desktop manager (`desktop: bool`, wm.rs:626-629), once per
frame, before recursion — so command Chords never reach a PTY.

**egui Id re-basing:** every child manager numbers its Wins from 1
(`next: 1`, wm.rs:668), so terminal #1 exists in every Project. egui identifies
widgets by hashed stable Ids; interaction Ids here derive from a base
(`base.with((id, "drag"))` etc.), so without re-basing, two Projects' terminal
#1 would collide and interaction state would misroute. The fix is one line:
each Project recursion re-bases with `base.with(("proj", win_id))` (wm.rs:156).
If you add any new per-Win egui Id, derive it from `base`, never from the bare
`WinId`.

## The coordinate contract

`Win.rect` is **LOCAL** to its manager's `area` — "local coords (origin =
manager area.min)" (wm.rs:518).

| Conversion | Code |
|---|---|
| local → screen | `let scr = w.rect.translate(area.min.to_vec2());` (wm.rs:2376) |
| screen pointer → local | `let local = p - area.min.to_vec2();` (wm.rs:2466) |

Mixing the two spaces is a recurring bug class: a rect that looks right on the
desktop and is offset by the Project's origin when nested means you skipped a
translate. Convert exactly once, at the draw/interaction boundary, and keep all
stored state local. The layout tree also works in local space
(`tree.layout(Rect::from_min_size(Pos2::ZERO, asz), ...)`, wm.rs:2323-2325).

## Two window states (and zoom, which is neither)

Every Win is **tiled** iff its id is a leaf of the manager's
`crate::layout::LayoutTree` (`tree`, wm.rs:648-650); otherwise it is
**floating**. Rules that follow:

- **Never trust `Win.rect` as persistent state for a tiled Win.** It is
  overwritten every frame from the tree layout (or the zoom rect, or clamped
  back into the area) at wm.rs:2361-2375. The float-restore rect lives in
  `Win.prev` (wm.rs:521) — set on tiling (`tile_new`, wm.rs:742-747) and taken
  on tear-out.
- **One-frame rect lag on drop is intentional.** Dropping a Win into the tree
  does NOT set its rect immediately: "Rect refits from the tree next frame (one
  frame at the drop position — invisible at 60fps; intentionally no immediate
  set)" (wm.rs:2548-2549). Don't "fix" this.
- **Zoom is an OVERLAY.** `toggle_zoom` (wm.rs:2030) only sets
  `zoomed: Option<WinId>`; the tree and every other Win are untouched, and a
  floating Win's rect survives a zoom round-trip (unit test
  `zoom_overlays_without_touching_the_tree_or_floating_rect`, wm.rs:5939).
- A lone, tiled, single-tab non-Project Win draws no chrome of its own (the
  "bare" branch, wm.rs:2378-2409) — the parent frame is its only frame.

The tree itself (insert/remove/layout/drop-targets/divider resize) is pure and
unit-tested in src/layout.rs; deep dive in `docs/tiling-tree.md`.

## Tab invariants

- **`Win.tabs` is never empty** — "closing the last tab closes the window"
  (wm.rs:512-514). A len-1 stack renders as a plain window (no tab bar).
- **Tabbing only within the same manager** (Projects with Projects, terminals
  with terminals). This is **structural, not enforced by a check**: the only
  producer of `Act::Merge` is a manager's own drag loop scanning its own
  `windows` (`merge_target_at`, wm.rs:2248; `merge_windows`, wm.rs:1910). If
  you ever add a cross-manager move, you must add the enforcement you are
  currently getting for free.
- **Only the active tab renders and reads the keyboard, but inactive Sessions
  stay alive**: each Session's reader thread runs regardless, and
  `keepalive_inactive` (wm.rs:537-545, called at wm.rs:2359) pumps every
  background tab's PTY each frame so device queries are answered and output is
  consumed. A background tab is hidden, not paused.
- A Session's **Member id** (`term_id`, terminal.rs:253-257) is its stable
  identity for chat/dispatch — stamped once at spawn, equal to the
  `FOREMAN_TERMINAL_ID` env value, and unchanged by tabbing/untabbing/moving.
  `WinId`s are NOT stable identity (tab merges retire them).

## The Deferred action pattern

The draw pass records `Vec<Act>` (wm.rs:2336) and applies them **after** the
per-Win loop. Why: applying immediately would need `&mut self` mutation (close,
merge, spawn into a nested manager) while the render borrow of
`self.windows[i]` is live — the borrow checker forbids it, and immediate
mutation mid-loop would also invalidate the frame's window ordering.

`enum Act` variants (wm.rs:552-588, as of 2026-07-01): `Focus`, `Close`, `Min`,
`Max`, `Float`, `Restore`, `AddTerm(WinId, Shell)`, `OpenProjectPicker`,
`SetTab(WinId, usize)`, `CloseTab(WinId, usize)`, `Merge{src, dst}`,
`Untab{id, idx, pos, grab}`. **There is no `Act::AddProject`** — project
creation flows through `OpenProjectPicker` → the DirPicker modal → 
`add_project` on accept.

When you add a window interaction: record an `Act` in the draw pass, handle it
in the apply pass. Do not mutate `self.windows` or a nested manager inside the
draw loop.

## Seam map (name → file:symbol)

These are deliberate, tested seams (CONTEXT.md "Seams & patterns"). One home
each — put new logic of that kind THERE.

| Seam | Where (verified 2026-07-01) | What it isolates |
|---|---|---|
| Deferred action | src/wm.rs `enum Act` (line 552) + apply pass | Window mutations out of the render borrow |
| Input-encoding seam | src/input.rs `process_input(events, mode, has_selection) -> InputOutcome` (line 37) | egui events → exact PTY bytes, GUI-free |
| Quiescence settle | src/wm.rs `settle_tick` (line 34), `PendingSettle` (line 22), `advance_settles` (line 1275) | "wait until the Session quiets" without blocking the GUI. `DEFAULT_SETTLE_MS = 120`, `MAX_SETTLE_MS = 4000` — deliberately under `REPLY_TIMEOUT` (5 s) so a settle reply always beats the pipe timeout (wm.rs:12-18) |
| Caret | src/caret.rs `draw` | What to paint for the model cursor (pure mapping; the de-jitter gate was retired 2026-07-15) |
| Cell metrics | src/geom.rs `CellMetrics` (line 12) | One frame's pixel↔cell geometry; all clamping in one place |
| Outbox | src/chat.rs `ChatRoom::tick` (line 598) | Per-frame chat delivery decision (who gets which framed lines); the engine wiring (`WindowManager::chat_tick`, wm.rs:1576) only injects what the Outbox returns |
| Frame plan | src/frame.rs `plan() -> FramePlan` (lines 45/59) | One frame's paint geometry/content for a Session, pure; `Session::show` replays it (terminal.rs:990). Clamps the grid walk to the grid's REAL bounds first because a stale index panic aborts the whole process (frame.rs:11-17) |

Status note (2026-07-01): `src/geom.rs` and `src/frame.rs` were in-flight TDD
earlier the same day; they are now **committed and wired at HEAD `7fda1c2`**.
Briefings or docs calling them "untracked/red" are stale.

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
| src/inspect.rs | Snapshot/key-encoding; generic over alacritty's `EventListener`, tests drive a `Term<VoidListener>` with fixed bytes (inspect.rs:1-12) — the ONE legitimate `VoidListener` use |
| src/caret.rs | Injected `Instant`s — the gate never reads the clock |
| src/geom.rs | Cell metrics (see seam map) |
| src/frame.rs | Frame plan (see seam map) |
| src/control.rs | **Transport-only**: zero `use crate::` imports — it never touches wm/chat/terminal. Keep it that way; `WindowManager::handle_ctrl` (wm.rs:837) is the sole production consumer of `CtrlMsg` |

## Threading model

**The GUI thread never blocks.** That is the whole model; everything below
serves it.

| Thread | Created | Blocks on |
|---|---|---|
| GUI/render (winit main) | eframe | Nothing. PTY drain is `rx.try_recv()` (`Session::pump`, terminal.rs:714); control drain is `ctrl.try_recv()` (main.rs:359); settles are deferred cross-frame (`advance_settles`) |
| 1 reader thread per Session | `Session::spawn` (terminal.rs:495) | Blocking `reader.read()` on the PTY → sends chunk over mpsc → `note_pty_output()` + `ctx.request_repaint()` (terminal.rs:497-506). Dies when the PTY closes |
| 1 control serve thread | main.rs:468 | `listener.incoming()` on the named pipe (control.rs:258) |
| 1 short-lived thread per pipe connection | control.rs:272 | The **only** reply wait in the app: `rrx.recv_timeout(REPLY_TIMEOUT)` = 5 s (control.rs:315, const at control.rs:10). Capped at `MAX_INFLIGHT = 64` concurrent handlers; over the cap, reject fast (control.rs:256-267) |

**Shared-state inventory** (as of 2026-07-01):

| State | Type | Actually cross-thread? |
|---|---|---|
| `Session.resp`, `Session.osc_title` | `Arc<Mutex<Vec<u8>>>`, `Arc<Mutex<Option<String>>>` (terminal.rs:236-238) | No — shared *ownership* between the `Term`-owned `Listener` and the Session; the Listener fires during `parser.advance` inside `pump()` on the GUI thread. Aliasing, not parallelism, in the current wiring |
| `PTY_OUTPUT` | `static AtomicBool` (terminal.rs:22) | Yes — written by reader threads, swapped by the GUI thread for the adaptive repaint cadence (4 ms hot / 100 ms idle, main.rs:402-419). Scheduling only, never correctness |
| Chat room | `Rc<RefCell<ChatRoom>>` (wm.rs:616) | No — single-threaded by construction; shared between the manager and `Content::Chat` viewers. Borrow discipline: `chat_tick` clones the Rc and drops the `borrow_mut` before injecting (wm.rs:1598-1602) |
| Process-table Scanner | `thread_local!` (proc.rs:120-122) | Per-thread; used from the GUI thread |
| Channels | mpsc: PTY bytes (reader→Session), `CtrlMsg` (conn thread→GUI), `OpenReply` (GUI→conn thread) | Yes — the sanctioned cross-thread paths |

**The request flow, in words** (Control plane CLI usage/verbs belong to
**foreman-run-and-operate**; agent-facing usage to **foreman-dispatch** /
**foreman-chat**):

`foreman <verb>` runs as a second foreman.exe process (subcommand short-circuit,
main.rs:448) → one JSON line over the named pipe `\\.\pipe\foreman` → `serve`
accepts and spawns a connection thread → parses into a `CtrlMsg` carrying a
reply `Sender` and the arrival `Instant` (control.rs:280-305) → sends it over
mpsc to the GUI and calls `ctx.request_repaint()` so an idle render loop wakes
NOW (control.rs:309-314) → `App::ui` drains via `try_recv` (main.rs:359) →
`desktop.handle_ctrl` executes (wm.rs:837) → reply goes back over the reply
channel → the connection thread's `recv_timeout(REPLY_TIMEOUT)` returns it →
one JSON line back → the CLI prints and exits.

Contract riders on that flow (wm.rs:832-884):

- **Drop stale requests unexecuted**: if `sent.elapsed() >= REPLY_TIMEOUT` the
  client was already told "foreman did not respond"; executing anyway would
  spawn a terminal the dispatcher believes failed (and a retry would duplicate
  it).
- **`open` undoes orphaned spawns** when the reply send fails.
- **Chat replies BEFORE injecting** — an injection cannot be undone, so bytes
  flow only once the client is guaranteed to hear "ok".
- **`send` never blocks**: it enqueues a `PendingSettle`; `advance_settles`
  (called once per frame after `show()` and `chat_tick`, main.rs:395-398)
  replies when the terminal quiets or the deadline passes.

## Invariants checklist (violating any of these is a bug)

- [ ] Live Sessions never use `VoidListener` — the `Listener` captures
      `Event::PtyWrite` (DSR replies, color-query answers; terminal.rs:184-212)
      and `pump()` flushes it back, which is also what latches `Ready`
      (terminal.rs:718-725). A `VoidListener` Session hangs black forever.
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
- [ ] Any grid walk clamps to the grid's REAL bounds first (frame.rs doctrine)
      — an index panic crosses the winit callback and aborts the process.
- [ ] The chat room owns no window ids; new egui Ids derive from `base`.

## Known-weak points (stated plainly)

| Weakness | Evidence | Status |
|---|---|---|
| No per-Session panic isolation: one panic anywhere in the frame aborts the whole process and kills the entire fleet | zero `catch_unwind` in src/ (grep, 2026-07-01); main.rs:423-426 documents the abort; the panic logger (`foreman_panic.log`) is post-mortem only | Open. frame.rs's clamp guard removes one panic class, not the blast radius |
| Chat log is in-memory only — restart/crash wipes it, `#N` cites dangle | no file IO in src/chat.rs (grep, 2026-07-01) | **Designed, not built**: `docs/chat-persistence.md` (append-only JSONL plan, converged 2026-06-27) |
| No CI | no `.github/` directory (2026-07-01) | Open; local `cargo test` is the only gate (**foreman-validation-and-qa**) |
| Toolchain is machine-global | rustup default + w64devkit on PATH; nothing pinned in-repo | Open; recreate steps in **foreman-build-and-env** |
| `docs/HANDOFF.md` drift | HANDOFF.md (self-declared authoritative) has zero mentions of frame.rs/geom.rs/caret.rs as of 2026-07-01 | Trust map in **foreman-docs-and-writing**; prefer code + this skill for structure |
| WSL blind spots | `Shell::Bash` spawns `wsl.exe` (terminal.rs:164); the process-tree agent scan cannot see inside the WSL VM (proc.rs:10-12), so WSL agents rely on the OSC-title path | Known limitation, documented in proc.rs module docs |
| Stringly JSON v1 protocol | wire format is one JSON line with a string `cmd` discriminator, matched by hand (control.rs:282-304); unknown fields silently ignored by serde defaults | Open; protocol details in **foreman-run-and-operate** |
| Control pipe is same-user trust, a guardrail not a security boundary | "NOT a security boundary — any local process can speak to the pipe and claim any `from`" (control.rs:76-78) | Deliberate scope decision; rationale in **foreman-change-control** |

## When NOT to use this skill

- Setting up or fixing the **build/toolchain** → **foreman-build-and-env**.
- Running the app or the CLI's exact verbs/flags/artifacts →
  **foreman-run-and-operate**; measuring instead of eyeballing →
  **foreman-diagnostics-and-tooling**; build-and-screenshot loop → the existing
  **build-screenshot** skill.
- Chasing a live symptom → **foreman-debugging-playbook**; the history of how a
  dead end was proven dead → **foreman-failure-archaeology**.
- PTY/ConPTY/VT escape mechanics → **terminal-emulation-reference**; egui 0.34
  traps → **egui-immediate-mode-reference**.
- Whether a change is even allowed, and the non-negotiables' incident history →
  **foreman-change-control**.
- You are an agent RUNNING INSIDE foreman wanting to dispatch or chat →
  **foreman-dispatch** / **foreman-chat** (user-facing; do not read source for
  operational mechanics).

## Provenance and maintenance

Written 2026-07-01 against commit `7fda1c2` ("feat(terminal): extract pure
paint/input seams — cell metrics, wheel steps, frame plan"). Every claim was
re-verified by reading the file cited. Re-verification one-liners (PowerShell,
from `H:/claude code/foreman`):

```powershell
git log -1 --oneline                                             # baseline still 7fda1c2?
Select-String -Path src/wm.rs -Pattern "^enum Act" -Context 0,38 # Act variants (still no AddProject?)
Select-String -Path src/wm.rs -Pattern 'base.with\(\("proj"'     # Id re-basing
Select-String -Path src/wm.rs -Pattern "local coords|translate\(area.min" # coordinate contract
Select-String -Path src/wm.rs -Pattern "DEFAULT_SETTLE_MS|MAX_SETTLE_MS"  # settle constants
Select-String -Path src/control.rs -Pattern "REPLY_TIMEOUT|MAX_INFLIGHT|recv_timeout" # pipe contract
Select-String -Path src/terminal.rs -Pattern "try_recv|thread::spawn|AtomicBool"      # threading
Select-String -Path src/control.rs -Pattern "use crate::"        # transport-only (expect: no hits)
Select-String -Path src/chat.rs -Pattern "std::fs"               # chat persistence (expect: no hits until built)
Test-Path .github                                                # CI (expect: False until added)
Select-String -Path docs/HANDOFF.md -Pattern "frame.rs|geom.rs|caret.rs" # HANDOFF drift (no hits = still drifted)
```

Drift-prone facts date-stamped above: line numbers, the Act variant list, geom.rs/frame.rs
commit status, dependency versions, and the known-weak-point statuses (chat
persistence, CI, HANDOFF drift) — re-run the matching command before repeating
any of them.
