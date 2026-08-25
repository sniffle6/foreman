---
name: foreman-debugging-playbook
description: Use when foreman misbehaves and you need the known-failure dictionary — a Session comes up black and never prompts, "rx 4 bytes", "Access is denied (os error 5)" on build, "cannot find -lgcc_eh", resize + Up-arrow prompt corruption, caret strobing in TUIs, dead unclickable UI regions, Ctrl+Scroll/Ctrl+0 zoom doing nothing, chat posts never arriving, "foreman did not respond", dispatch failures, washed-out TUI colors, the app vanishing (foreman_panic.log) — including after sleep or a display power transition (GPU device loss) — flaky PTY tests.
---

# Foreman Debugging Playbook

Symptom-first fault dictionary for foreman's known failure modes. Each entry:
what you see, what actually causes it, the fix or fence, and the story with
evidence so you don't re-run a dead investigation.

**Route in:** general debugging discipline comes first — find the root cause,
never patch the symptom (see the `superpowers:systematic-debugging` skill).
This playbook is the project-specific layer under it: check here **before**
forming hypotheses, because several of these symptoms have already burned
hours and two of them are settled do-not-re-investigate verdicts.

**Domain terms used below** (full detail in **terminal-emulation-reference**):
a *PTY* (pseudo-terminal) is the OS channel a terminal program reads/writes;
*ConPTY* is Windows' PTY implementation; *DSR* (Device Status Report) is the
escape query `ESC[6n` a program sends to ask "terminal, where is your cursor?"
— the program **blocks** until the terminal replies. egui terms (Area, hit
test, immediate mode) live in **egui-immediate-mode-reference**.

Citations are `file` + symbol, never `file:line` — line numbers rot in weeks,
symbol names have survived every refactor so far. `rg -n "fn <symbol>" src/`
lands you on it.

## Quick triage table

| Symptom | Immediate cause | Go to |
|---|---|---|
| Session renders black, shell never prompts, log shows `rx 4 bytes` | Startup DSR never answered (listener dropped `Event::PtyWrite`) | §1 |
| `Access is denied (os error 5)` linking a build | A running `foreman.exe` holds the binary | §2 |
| `cannot find -lgcc_eh` at link | w64devkit GCC 16 dropped `libgcc_eh.a` | §3 |
| Narrow a Session, press Up, prompt corrupts | Upstream ConPTY reflow bug — SETTLED, do not re-investigate | §4 |
| Caret flickers/strobes across a TUI's status line | App doesn't sync-bracket redraws (gate retired 2026-07-15) | §5 |
| A region of the UI ignores all clicks/drags | An egui Area's bounding rect swallows input below it | §6 |
| Ctrl+Scroll / Ctrl+0 font zoom does nothing | egui's built-in zoom steals the gesture | §7 |
| Chat post never appears in a member Session | Injection before the Session is Ready, or member never latches Ready | §8 |
| `foreman open`/`send` fails, times out, or "succeeds" wrong | Control-plane contracts (pipe ownership, 5s timeout, cmd-shim, ok-semantics) | §9 |
| TUI colors flat/washed out (grey boxes) | Capability env vars or OSC color replies missing | §10 |
| Whole app vanishes, opaque exit code | Panic aborted across the winit callback — read `foreman_panic.log` | §11 |
| A PTY test fails in the full suite, passes alone | Pre-Ready injection swallowed under load — re-send pattern | §12 |
| App vanished after sleep, resume, or a display power transition | GPU device loss. Under wgpu that is an unconditional `panic!` + abort — SETTLED: foreman renders on glow | §13 |

---

## §1 Black Session / shell never prompts / `rx 4 bytes` — the DSR trap

**Symptom.** A new Session paints nothing; the shell never shows a prompt. If
byte counts are logged you see a tiny first read — the classic `rx 4 bytes`
(docs/HANDOFF.md §4, item 4).

**Immediate cause.** Shells send the DSR query `ESC[6n` at startup and block
until the terminal answers. The reply is produced by `alacritty_terminal` as
`Event::PtyWrite` — if the event listener discards it, the reply never reaches
the child and it hangs forever.

**Fix / fence.** The wiring already exists and must not be undone:
`Session`'s `Listener` captures `Event::PtyWrite` into a shared buffer
(src/terminal.rs `struct Listener`) and `Session::pump()` flushes that buffer
back into the PTY after the RX chunk that completed the query. A successful first reply plus
the child's first visible paint latch the Session **Ready**; a failed write must
not. Minimized windows still call the headless keepalive path so this handshake
does not time out. **Never give a live Session a
`VoidListener`** — that is exactly the listener that discards `PtyWrite`.

**Nuance — VoidListener is legitimate in one place:** driven `Term` fixtures
in tests. `src/inspect.rs` is generic over `EventListener` precisely so the
same code runs on a live `Term<Listener>` and a test `Term<VoidListener>` fed
fixed bytes with no PTY at all (src/inspect.rs — every fn is
`<L: EventListener>`; used in the test mods of inspect.rs and frame.rs).
A `VoidListener` on a
**live** Session is a bug; on a byte-driven fixture it's the design.

**Story.** Early sessions came up black and the cause was invisible until the
4-byte read (`ESC[6n` is 4 bytes) was traced. Canonized in docs/HANDOFF.md §4
and CLAUDE.md's gotcha list.

## §2 `Access is denied (os error 5)` on build

**Symptom.** `cargo build` fails at the link step with
`Access is denied. (os error 5)`.

**Immediate cause.** A running `foreman.exe` holds a lock on the output
binary. Windows won't let the linker overwrite a running image.

**Fix.** Kill by exe path, never by name — only a `target\`-built instance
holds the lock; a by-name kill also takes down the user's *installed* foreman
(`%LOCALAPPDATA%\Programs\foreman`), which looks like a crash (incident:
2026-07-15). From the repo root:

```powershell
Get-Process foreman -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$PWD\target\*" } | Stop-Process -Force; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 20
```

**⚠ Unless `$env:FOREMAN` is `1`** — then you are running inside the foreman
app and that kill takes down your own host, every terminal in it, and you
(incident: 2026-07-09). Ask the user to close foreman, or build without
touching the running exe: `cargo build --target-dir target/agent`.

**Fence.** A repo hook automates this — but only partially:
`.claude/settings.json` registers a `PreToolUse` hook with **matcher "Bash"**
running `.claude/hooks/kill-foreman.ps1`, which kills any foreman running from
this repo's `target\` dir (by exe path, since 2026-07-15) before any
`cargo build|run|test` command — except when `FOREMAN=1` (session inside
foreman), where it no-ops for the reason above. It fires **only for the Bash
tool** — a cargo command issued through the PowerShell tool gets no kill, so
run the path-filtered kill line yourself there. Build-loop details live in
**foreman-build-and-env**.

## §3 `cannot find -lgcc_eh`

**Symptom.** Link fails with `cannot find -lgcc_eh`.

**Immediate cause.** w64devkit GCC 16 folded exception-handling into `libgcc`;
Rust's GNU target still asks for `-lgcc_eh`. The fix is an empty stub archive
(docs/HANDOFF.md §4, item 3). The exact `ar crs` command and full toolchain
recreation belong to **foreman-build-and-env** — go there; don't improvise.

## §4 Resize + Up-arrow recall corruption — SETTLED, do not re-investigate

**Symptom.** Narrow a Session while a **wrapped** prompt line is on screen,
then press Up-arrow (history recall). The recalled line renders rows too high,
mangling the prompt. Persists until a full redraw.

**Immediate cause.** ConPTY's internal resize reflow diverges from the hosting
terminal's. Older builds returned the wrong host-grid cursor to PSReadLine.
Upstream [#18725](https://github.com/microsoft/terminal/issues/18725) is now
closed by #19535; Foreman bundles that demand-triggered DSR/CPR mitigation.

**Fix / fence.** Cursor synchronization is shipped, but it cannot reconstruct
dropped rows or clear stale PSReadLine text. **Ctrl+L** remains the residual
full-redraw repair. The settled fence is specifically against re-trying redraw
ownership or conhost-parity reflow without a user decision. Full evidence:
`docs/conpty-resize-reflow.md`; registry: **foreman-change-control**.

**Story (both wrong turns already taken).**
1. The original diagnosis blamed a "double reflow" in `Session::resize`
   (`term.resize()` + `master.resize()`). **Disproven** by byte-level tracing
   of the ConPTY↔foreman stream and an A/B against Windows Terminal
   (docs/conpty-resize-reflow.md §"Root cause").
2. "Let ConPTY own the redraw" was then built and tested — vendored
   `portable-pty` with the resize quirk toggled, a grid-reset on resize, and a
   sideloaded ConPTY 1.24 stable. **All four combinations failed** because that build's
   reported cursor is internally inconsistent with its own VT repaint; no
   frontend redraw-ownership strategy can fix that
   (docs/conpty-resize-reflow.md §"What was tried and rejected").

Fuller chronicle: **foreman-failure-archaeology**.

## §5 Caret strobing in TUIs

**Symptom.** The painted caret flickers between the input line and a far row
(status line / message area), or chases an animation.

**Status (2026-07-15): the Caret gate is RETIRED.** The caret now tracks the
model cursor directly every frame (`caret::draw`, src/caret.rs — a pure
mapping; `?25l` hide honored). Why removal was safe, measured: modern TUIs
(Claude Code, Codex) bracket every redraw in DEC 2026 synchronized output,
which the vte parser applies atomically, and PSReadLine redraws arrive
hide-bracketed (`?25l..?25h`) in single PTY chunks — the model cursor stream
is clean by the time the painter samples it. The old gate's 50ms settle /
150ms input-grace holds were themselves the user-visible bug (laggy, flashing
caret while typing). Evidence: docs/cursor-rendering.md and the
`caret_probe_claude_typing` ignored test (src/terminal.rs) — re-run it against
any suspect app.

**If a strobe reappears** (a non-2026 app whose redraw bursts split across
pump batches): probe first, then the agreed fallback is a *far-jump-only*
hold — decided on fresh evidence, NOT a reinstall of the settle/input-grace
gate.

**Discriminator.** Is the *model cursor* or the *painted caret* wrong? Run
`foreman snapshot --cursor` (returns the grid model's cursor,
src/inspect.rs `cursor_info`) and compare with what's painted. Model cursor wrong =
emulation problem (**terminal-emulation-reference**); model right but paint
wrong = paint problem (`frame::overlays` / `show()`).

## §6 Regions of the UI dead to input — the egui Area landmines

**Symptom.** Some rectangle of the app ignores every click, drag, and
double-click — often "windows snapped to the bottom/right half are dead" —
while the rest works.

**Immediate cause.** Two documented egui landmines (docs/os-chrome.md
§Gotchas):
1. An egui `Area` registers an invisible widget over its whole **bounding
   rect**, and any widget covering the pointer blocks every layer below it —
   regardless of its `Sense`.
2. On an Area's **first frame** egui doesn't know the content size, assumes a
   default (~600×400), and the default `constrain(true)` shoves the Area's
   origin up/left so that assumed size fits on screen. With absolute-rect
   content, the *recorded* bounds became origin-to-strip — for the bottom rim
   strip, the entire bottom half of the app — and per landmine 1 everything
   under it went input-dead.

**Fix / fence.** Chrome Areas set `.movable(false)`, `.constrain(false)`, and
`.default_size(...)` (src/main.rs `show_os_chrome`; rationale docs/os-chrome.md). Each
resize-rim strip is its **own** Area — one Area for all four strips has a
full-screen bounding rect and eats every click in the app.

**Debugging tip (verbatim from the incident):** if chrome interaction dies
regionally again, dump `ctx.memory(|m| m.area_rect(id))` for the chrome Areas
**first** (docs/os-chrome.md §Gotchas). General egui trap taxonomy:
**egui-immediate-mode-reference**.

## §7 Ctrl+Scroll / Ctrl+0 zoom dead

**Symptom.** Ctrl+Scroll doesn't resize terminal text; Ctrl+0 doesn't reset it
— or worse, the whole UI chrome scales instead.

**Immediate cause.** egui's built-in zoom is on by default: it diverts
Ctrl+wheel into a whole-UI zoom (zeroing `smooth_scroll_delta`, so foreman's
handler sees nothing) and consumes Ctrl+0/Ctrl+± for chrome scaling.

**Fix / fence.** Startup must disable both (src/main.rs, in `App::ui`'s
one-shot startup block):

```rust
ctx.options_mut(|o| {
    o.zoom_with_keyboard = false;
    o.input_options.zoom_modifier = egui::Modifiers::NONE;
});
```

Foreman's own handler in `src/terminal.rs` then owns the gesture and resizes
only terminal text. If zoom dies after an egui upgrade, check these two
options first.

## §8 Chat post never lands in a member Session

**Symptom.** `foreman chat` returns `{"ok":true,"seq":N}` but a member Session
never shows the injected `[chat pN #N]` line — or shows it without submitting.

Four distinct mechanisms; discriminate in this order:

| Check | Mechanism |
|---|---|
| Is the Session Ready yet? | Bytes injected pre-Ready are eaten by the startup DSR scan. `ReadyGate::try_inject` (src/ready.rs) therefore **queues** posts until Ready and `ReadyGate::poll`, called from `Session::pump()`, flushes them on the frame Readiness latches. Delivery is at-most-late, not lost — *if* the Session ever becomes Ready. |
| Does the member ever latch Ready? | Ready is **two** halves ANDed, both in `ReadyGate` (src/ready.rs): `on_dsr_reply_flushed` (the startup device-status reply actually made it back to the child) and `on_rx_chunk` seeing the first visible glyph (`InkScan`). A worker that never emits a DSR never latches, and queued posts sit forever. Headless print-mode workers (`claude -p`) additionally don't read stdin at all — injected posts hit a dead buffer even when delivered. Receive-capability detection is an **open gap** (docs/chat-missing-features.md, Layer 1). |
| Was the submit folded? | The trailing Enter is a **deferred** `\r` armed `SUBMIT_DELAY` after the text (src/ready.rs — the const's doc comment carries the incident: Claude Code's TUI folds input arriving inside the same burst *into* the paste, so a back-to-back `\r` became a literal newline and the message sat unsubmitted in the input box). |
| Is the tick seeing the whole room? | `WindowManager::chat_tick` (src/wm.rs) collects **every** live member, then `ChatRoom::tick` (src/chat.rs — the **Outbox** step) reconciles presence and delivers only to Ready, non-exited members, advancing per-member delivery cursors. **Invariant: pass the full live set** — a member absent from the `live` slice is reconciled as *gone* and gets an Exited line. A partial set silently evicts members. |

Delivery-order and framing details: docs/chat-delivery.md. Operational chat
usage (as an agent inside foreman): the **foreman-chat** skill.

## §9 Dispatch failures — Control plane contracts

Symptoms and their contracts (transport ground truth lives in
**foreman-run-and-operate**; this is the failure dictionary):

| Symptom | Contract |
|---|---|
| Dispatch dead in one instance; GUI otherwise fine | A second foreman instance can't own the pipe. The server logs `control: pipe unavailable after N attempts (...); agent dispatch disabled` to stderr and returns — GUI still works, dispatch doesn't (src/control.rs `listen_retry`). Check for another running foreman.exe. |
| Client prints `foreman did not respond` | The pipe server waited `REPLY_TIMEOUT` (5s, src/control.rs) for the GUI. **A timed-out request NEVER executes**: every arm of `WindowManager::handle_ctrl` (src/wm.rs) drops a request older than `REPLY_TIMEOUT` unexecuted, and an `open` whose reply channel died has its spawn undone via `close_terminal`. So a retry cannot create a duplicate Session. (One asymmetry: a chat post whose reply channel died **stays in the log** — append-only; only the injection is skipped — so a retried post can duplicate a history line.) |
| `... runs via a cmd-shim ... cannot carry newlines or " in arguments` | Deliberate refusal. Anything routed through `cmd.exe` re-parses the command line: a newline ends the command, an embedded `"` flips quote state. Argv containing `\n`, `\r`, or `"` (`unsafe_for_cmd` inside `Session::spawn_argv`, src/terminal.rs) is refused for `.cmd`/`.bat` targets and for the bare-name `cmd /c` fallback. Flatten the prompt to one quote-free line or install the tool as a native exe. |
| `{"ok":true,...}` but the worker did nothing | **`ok:true` means "a Session opened", not "your command exists."** A bare name that isn't directly spawnable falls back to `cmd.exe /c <argv>` (`Session::spawn_argv`, src/terminal.rs) — cmd spawns fine even when the inner command doesn't exist. The help text says it outright: "The reply is NOT the command's output or result" (src/control.rs, `HELP_OPEN`). Verify with `foreman status` — an instantly-dead worker shows `exited(code)` because status asks the live process, not the tab title. |
| `control pipe stayed busy for 10s` | Client-side `CONNECT_TIMEOUT` (10s, src/control.rs) turned a wedged server into an error. Retry, or find the wedged dispatch. |

## §10 Wrong / washed-out colors in TUIs

**Symptom.** A cross-platform TUI renders flat or grey (the Codex grey-input-
box incident) instead of its real theme.

**Immediate cause.** Two capability channels, both required:
1. **Env advertisement:** every foreman-spawned Session gets
   `COLORTERM=truecolor` and `TERM=xterm-256color` injected
   (`WindowManager::term_env`, src/wm.rs)
   so TUIs enable 24-bit color. Codex keys its truecolor path off `COLORTERM`
   (docs/epics/terminal-completeness-epic.md §truecolor).
2. **OSC color queries:** apps ask the terminal for its palette
   (OSC 10/11/12 for fg/bg/cursor, OSC 4;N for palette entries) to detect
   light/dark and theme themselves. `Listener` answers `Event::ColorRequest`
   with the RGB foreman actually paints (src/terminal.rs — the
   `Event::ColorRequest` arm of `impl EventListener for Listener`).
   Without an answer, apps guess.

**Discriminator.** `foreman snapshot --attrs` returns per-cell resolved RGB
(src/inspect.rs `CellData`) — ground truth for "what color did the emulation
actually resolve", independent of the paint path.

## §11 Whole app vanishes

**Symptom.** Foreman disappears — no dialog, opaque exit code.

**Immediate cause.** A panic inside the egui/winit frame callback unwinds
across the platform event loop and **aborts the process**; the cause is
invisible without a hook.

**Fix / fence.** `install_panic_logger` (src/main.rs) appends a timestamped
`=== foreman panic {ts} ===` entry — message, location, backtrace — to
**`%APPDATA%\foreman\foreman_panic.log`** before the default hook runs
(`crash_log_path` → `crash_log_path_in(config::config_dir())`). Read that file
first:

```powershell
Get-Content "$env:APPDATA\foreman\foreman_panic.log" -Tail 40
```

A bare `foreman_panic.log` in some working directory is the pre-966379b
fallback shape — `crash_log_path_in(None)` only when `APPDATA` is unresolvable.
If you find one at the repo root it is almost certainly a fossil, not evidence:
check the entry timestamps before believing it.

**Status.** No per-Session isolation exists — one Session's paint panic kills
every Session in the app. Open item. The Frame plan seam removes one whole
class of these: grid walks are clamped to the grid's real bounds before
painting, so a stale index can't panic the process (CONTEXT.md §Frame plan).

## §12 Flaky PTY tests — pre-Ready swallow under suite load

**Symptom.** A test that spawns real ConPTY Sessions and injects input fails
in the full suite but passes in isolation (canonical case:
`human_post_appends_with_reserved_id_and_broadcasts_to_all_members`).

**Immediate cause.** The §1/§8 trap in test form: bytes injected before the
child's startup DSR resolves are eaten. In isolation the DSR resolves fast and
a one-shot injection lands; under full-suite load (dozens of concurrent
conhost spawns) it resolves late and the injection is swallowed — nothing ever
re-sends.

**Fix / fence — the pattern (docs/plans/2026-06-11-fix-flaky-chat-broadcast-test.md):**
- pump every Session each loop iteration (`keepalive()`), and
- **re-send the injection every iteration until the child's output proves it
  arrived**, then assert.
- Reference implementation: `chat_broadcast_hits_members_only_excluding_sender`
  (src/wm.rs), which carries a comment naming this exact trap.

**Never serialize the suite to hide it** — serialization only keeps DSR
latency under the timing window; the race remains (same doc, §Diagnosis).
Test evidence standards: **foreman-validation-and-qa**.

## §13 App vanished after sleep / display power transition — GPU device loss

**Symptom.** Foreman is gone after the machine slept, resumed, changed
displays, or the display driver reset. Nothing on screen said so.

**Immediate cause.** Windows drops the GPU device on those transitions.
`egui-wgpu` does not handle that: `Renderer::update_buffers` calls
`write_buffer_with`, gets `None` from a device in an error state, and hits an
unconditional `panic!` — which per §11 aborts the whole process, every Session
with it. `egui_glow` only logs on `GL_CONTEXT_LOST`.

**Fix / fence — SETTLED (`ba803ef`).** foreman renders on **glow**, and
`default-features = false` on the `eframe` line in `Cargo.toml` is load-bearing:
eframe prefers wgpu when both backends are enabled. Do not "modernize" that
line back to wgpu — the upgrade path was ruled out too (newer egui-wgpu keeps
the same panic). The side-by-side A/B through one real device-loss event, and
the crash evidence from `%APPDATA%\foreman\foreman_panic.log`, are in
`docs/gpu-device-loss.md`. Session persistence (PTYs outliving the GUI process)
is still the only thing that would make a GUI death *survivable*; it is open.

---

## Discriminating experiments

Measure, don't eyeball. Full harness recipes live in
**foreman-diagnostics-and-tooling**; these are the four moves this playbook's
entries lean on (all require a running foreman; run them from inside a foreman
Session or pass `--project/--terminal` explicitly):

| Question | Experiment |
|---|---|
| "What is actually on the Session's screen?" (headless ground truth, no pixels) | `foreman send --terminal tN --text "..."` then `foreman snapshot --terminal tN`. `send` waits a Quiescence settle by default — the quiet window is `Settings::send_settle_ms` (src/config.rs, ships at 120ms, user-editable, clamped to 2000 by `sanitize`), and `MAX_SETTLE_MS` (4000, src/wm.rs) is the hard cap on total wait. `--settle-ms 0` = fire-and-forget; `WindowManager::advance_settles` drains parked settles per frame. |
| "Is the emulation wrong or the paint wrong?" | `foreman snapshot --cursor` (model cursor) vs what's painted (same cell since the gate retired) — see §5. `--attrs` does the same for colors — see §10. |
| "What's the fleet state?" | `foreman status` — running/exited(code) for every Session, chat membership, asked of the live process. |
| "Is the chrome wrong?" (pixels, not grid) | Build + screenshot via the **build-screenshot** skill. A Snapshot can't see egui chrome; a screenshot can't see cell attributes. Pick by layer. |

## When NOT to use this skill

- **The full history of an investigation** (who tried what, and every dead end)
  — **foreman-failure-archaeology**; this playbook keeps only the actionable
  verdicts.
- **A brand-new symptom not in the table** — that's a fresh investigation:
  `superpowers:systematic-debugging` discipline, evidence per
  **foreman-research-methodology**, and add the entry here once settled (via
  **foreman-change-control** if it touches a settled verdict).

## Provenance and maintenance

Re-verification one-liners for the load-bearing, drift-prone claims — the ones
where being wrong sends someone down a dead path:

| Claim | Re-verify with |
|---|---|
| The VoidListener fence: it appears only in test fixtures, never on a live Session | `rg -n "VoidListener" src/` — hits must all be inside `#[cfg(test)]` mods |
| Panic log really lands in `%APPDATA%\foreman\` | `rg -n -e crash_log_path -e PANIC_LOG_FILE src/main.rs` — must resolve through `config::config_dir()`, not the CWD |
| Renderer is still glow, not wgpu | `rg -n -A6 '^eframe = ' Cargo.toml` — `default-features = false` plus the `glow` feature; a bare `eframe` line means wgpu is back and §13 is live again |
| ConPTY mitigation/residual status | `Get-Content docs/conpty-resize-reflow.md -TotalCount 8`; check bundled pair + #18725/#19535 status |

If any command's output contradicts an entry, the code wins — update the entry.
