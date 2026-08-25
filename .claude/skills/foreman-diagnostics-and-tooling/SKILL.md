---
name: foreman-diagnostics-and-tooling
description: Use when verifying foreman behavior with evidence instead of eyeballing — proving a Session received input, reading its screen headlessly (foreman send / foreman snapshot, --settle-ms, --attrs, --cursor), measuring frame latency or a perf regression, reading foreman_panic.log after the app vanished, debugging a wrong tab icon (Claude/Codex detection), dead mouse regions, Crew board staleness, or dating a symptom in git history (git log -S, git fsck --no-reflogs, test census).
---

# Foreman diagnostics and tooling — measure, don't eyeball

Every claim about foreman behavior can be backed by a tool. This skill maps
question → instrument → how to read the result. Repo root: `H:/claude code/foreman`.
All commands are PowerShell 7+. Code is cited by file + symbol, not by line
number — `rg -n "fn <symbol>" src/` finds it.

## Tool picker

| Question | Instrument | Evidence produced |
|---|---|---|
| Did the Session receive my input? What's on its screen? | `foreman send` → `foreman snapshot` (headless loop, below) | Grid text (a Snapshot) |
| Does the UI *look* right (layout, colors, icons, chrome)? | build-screenshot skill | Pixels (`win.png`) |
| Show the human an image/screenshot in-pane (FOREMAN=1) | `foreman icat <file.png>` (foreman-icat skill) | Rendered in your pane |
| What Sessions exist and are they alive? | `foreman status` | Line-per-Session listing |
| What did the agents say / who went quiet? | `foreman chat --history N`, Crew board | Room log tail, presence ages |
| Is rendering slow? | Throwaway frame harness (below) | ms/frame numbers |
| Why did the whole app vanish? | `foreman_panic.log` | Panic message + backtrace |
| Why is this tab's icon wrong? | 4-layer detection table (below) | Which layer fired |
| Why is this screen region mouse-dead? | `ctx.memory area_rect` dump (below) | Recorded Area rects |
| When did this string/behavior appear? | `git log -S`, `git fsck --no-reflogs` | Commits, dangling work |

A **Snapshot is the grid as text — a read, never a side effect** (it never
writes to the child program; it does pump pending output first so the read is
current). A **screenshot is pixels**. Use a Snapshot to prove terminal
*content/behavior*; use a screenshot only for *visual* claims (paint, layout,
icons). Snapshots are scriptable and diffable; screenshots are neither.

## The headless verification loop (the killer tool)

Drive input into any Session and read back its rendered screen without the
window, the mouse, or the user's keyboard. Full verb/flag/exit-code reference:
**foreman-run-and-operate** skill. Operational usage *by agents inside foreman*
belongs to the user-facing **foreman-dispatch**/**foreman-chat** skills.

```powershell
# Self-target from inside any foreman terminal (env-driven — no flags needed):
foreman send --text "pwd" --keys "Enter"
foreman snapshot

# Target any Session explicitly (works from any local process):
foreman send --project p1 --terminal t3 --text "cargo test layout::" --keys "Enter"
foreman snapshot --project p1 --terminal t3

# Structured reads (reply becomes ONE JSON line instead of text rows):
foreman snapshot --project p1 --terminal t3 --cursor   # {row, col, shape}
foreman snapshot --project p1 --terminal t3 --attrs    # per-cell fg/bg RGB + style flags
foreman snapshot --project p1 --terminal t3 --tail 80  # last 80 buffer lines, not the viewport
```

Self-targeting reads `FOREMAN_TERMINAL_ID` + `FOREMAN_PROJECT_ID`, injected
into every foreman-spawned terminal (`src/control.rs` `send_main`/`snapshot_main`).
Exit codes: 0 ok, 1 refused/unreachable, 2 bad arguments.

Or run the bundled wrapper (requires a **running** foreman; exits nonzero if
unreachable):

```powershell
pwsh -NoProfile -File ".claude/skills/foreman-diagnostics-and-tooling/scripts/verify-terminal.ps1" `
  -Project p1 -Terminal t3 -Text 'echo hi' -Keys Enter
```

### Quiescence settle — what "settled" means

`foreman send` does not reply when the bytes are written; it replies when the
target Session has produced **no new PTY output** for a quiet window — so the
snapshot you chain after it reads a settled screen, not a mid-update race.
Mechanics:

| Knob | Where |
|---|---|
| Default quiet window when the caller omits `--settle-ms` | `Settings::send_settle_ms`, src/config.rs — a **user setting** (Settings pane), ships at 120 ms, `sanitize` clamps it to 2000 |
| Hard cap on total wait | `MAX_SETTLE_MS`, src/wm.rs |
| `--settle-ms 0` | fire-and-forget, replies immediately (`handle_ctrl`'s Send arm, src/wm.rs) |
| Freshness signal | `Session::output_gen` counter, bumped per PTY chunk (src/terminal.rs) |

Settle entries are parked (`PendingSettle`) and advanced once per frame by
`WindowManager::advance_settles` (src/wm.rs, driven from `App::ui`, src/main.rs)
— the GUI never blocks. Interpretation:

- Reply came back at about the configured quiet window: the Session went quiet
  — snapshot is settled state.
- Reply took the full `MAX_SETTLE_MS`: the Session **never went quiet**
  (streaming/flooding/TUI animation) — the snapshot is a mid-stream frame.
  `--settle-ms` widens the quiet window up to the cap; beyond it is clamped.
- Because the default is a *setting*, a machine whose user raised or lowered it
  measures differently from yours. Read it back before comparing timings across
  machines; do not assume 120.
- Quiescence settle is **output** stability. It is unrelated to caret painting
  (the caret tracks the model cursor directly since the gate's retirement,
  2026-07-15, src/caret.rs). Different signals, different purposes.

### Reading a Snapshot

- Default reply: one line per **visible** grid row, **trailing spaces trimmed**,
  blank rows are empty strings — remember this when diffing. Long output that
  scrolled off the pane is not in a default snapshot; use `--tail N` for the
  last N buffer lines (scrollback + live screen, ignores current scroll).
- `--cursor` reports the **raw model cursor** from the grid
  (`term.renderable_content().cursor` in `inspect::cursor_info`). Since the Caret
  gate's retirement (2026-07-15) the painted caret is the same cell — any
  disagreement now IS a bug (paint layer, frame::overlays / show()).
- `--attrs` resolves colors through the same palette the GUI paints with
  (`inspect::snapshot_cells` → `CellData`), so attrs reads match pixels — use it
  to prove color/style claims without a screenshot.

### Send gotchas that corrupt experiments

- `--text` is written **verbatim — no escape processing** (`send_dispatch`,
  src/wm.rs, hands it to `Session::feed_text` → `text.as_bytes()`). A literal
  `\r` typed in your shell arrives as backslash+r. Send Enter as `--keys "Enter"`
  (unambiguous), or embed a real CR from PowerShell with `` "`r" ``.
  **Still-live doc drift:** docs/terminal-inspection.md's
  `--text "echo hello\r"` examples are misleading in most shells.
- `send` bypasses the Ready hold: `Session::feed` is a raw PTY write
  (src/terminal.rs), unlike chat injection which queues in `ReadyGate`
  (src/ready.rs) until the Session is Ready. Input sent while a just-spawned
  Session is still in its startup device-status scan can be swallowed. After
  `foreman open`, snapshot until you see a prompt before sending.
- `--text` then `--keys` in one call: text is written first, then keys — one
  atomic request; key names are validated *before* any write (`send_dispatch`).

## What stalls your probes (Control plane concurrency)

Full transport/timeout reference: **foreman-run-and-operate**; deadline-nesting
analysis: **foreman-proof-and-analysis-toolkit**. What matters for measurement:

- One connection carries exactly one request and one reply. A settle-send holds
  **its own** connection for the settle duration — your calling script blocks;
  since commit `15f675f` (2026-06-18, thread-per-connection server, cap
  `MAX_INFLIGHT`, src/control.rs) it does **not** block other clients'
  requests.
- Connection *establishment* still serializes: the pipe keeps a single
  listening instance at a time (interprocess 2.4.2 `PipeListener::accept`
  creates the replacement instance inside accept), so concurrent dispatchers
  queue briefly on connect, deadline-bounded at 10 s (`CONNECT_TIMEOUT`).
  Seeing `control pipe stayed busy for 10s` means the server side is wedged or
  flooded, not merely busy with one slow request.
- **Comment drift, still live:** `CONNECT_TIMEOUT`'s doc comment in
  src/control.rs says "The server handles one connection at a time". That
  predates `15f675f` and overstates serialization of request *servicing*; only
  the queue-on-connect part is still accurate.
- The GUI answers requests on its render thread; the server wakes it per
  request (`ctx.request_repaint()`), and `MAX_SETTLE_MS` is deliberately
  under `REPLY_TIMEOUT` so settles never trip "foreman did not respond".
- Practical rule: your measurement loop is serial *from the caller's side* —
  budget `settle + round-trip` per iteration; run probes sequentially unless
  you specifically want to test concurrency.

## Visual verification (screenshots)

Mechanics live in the **build-screenshot** skill (build, launch, capture
`win.png`, read it). Interpretation added here:

- The capture **pulls foreman to the foreground** and shoots screen pixels at
  the window rect — don't fight it for focus, and don't trust a capture taken
  while another window overlapped it.
- To verify multi-window/nested layouts **without hijacking the user's mouse**:
  temporarily spawn extra Projects/Sessions in `App::ui`'s one-shot startup
  block (src/main.rs), build, screenshot, then **revert the edit**. This is
  a throwaway instrumentation pattern, same discipline as the frame harness.
- Screenshot = pixels: use it only for claims a Snapshot cannot express
  (chrome, borders, icons, tab styling, caret paint).

## Fleet probes

```powershell
foreman status                 # ALL Projects + Sessions (deliberately not env-scoped)
foreman status --project p2    # one Project (unknown pN errors)
foreman chat --history 30      # last 30 room posts; NEVER joins the room
```

- `status` lines: `p1  <name>  <cwd>` then per Session
  `  t3  running  chat  <title>` — id, state (`running` | `exited(code)`),
  Chat-room membership (`chat` | `-`), title (`WindowManager::status_dispatch`,
  src/wm.rs; `no projects` when there are none). A worker that spawned and
  instantly died shows `exited(code)`: status asks the live process, not the
  title.
- `chat --history` is a pure read from any caller, and **seq gaps in the output
  are normal**: system entries (join/exit) occupy seqs but are excluded from
  history — "seqs exist to be cited, not to be dense" (`ChatRoom::history`,
  src/chat.rs). Line shape: `#14 t2: text`, targeted `#14 t2→t6 (re #9): text`.
- **Crew board** (the Chat viewer's presence panel): live Members sort
  **stalest-first** — the ones to worry about are on top — then the human seat,
  exited Members last (`ChatRoom::crew`, src/chat.rs). An age turns **amber**
  once a live Member is unheard for longer than the caller-supplied threshold,
  which is the **user setting** `Settings::crew_stale_secs` — `STALE_AFTER` in
  src/chat.rs only documents its default, so do not treat it as the live value.
  `—` means never heard; `exited` is gray, not amber. The viewer PULLS crew rows
  from the room on each draw; there is no pushed snapshot to go stale. The board
  hides entirely when the viewer window is narrower than `CHAT_BOARD_MIN_W`
  (src/chat_view.rs) — that's layout, not a bug.

## Latency measurement (frame harness)

The pattern (from docs/followups-latency-and-control.md): **temp-instrument
`App::ui` in src/main.rs, measure, REMOVE before committing.** Log two numbers —
inter-frame gap and `desktop.show()` duration — to stderr, run the **release**
exe with stderr redirected, exercise the app, read the file:

```rust
// TEMP frame harness — REMOVE before committing. Template; adapt to taste.
let t0 = std::time::Instant::now();
self.desktop.show(ui, area, true, egui::Id::new("desktop"));
eprintln!("show={:.2}ms", t0.elapsed().as_secs_f64() * 1e3);
```

```powershell
cmd /c ".\target\release\foreman.exe 2> frame.log"   # close the app to end the run
Get-Content frame.log -Tail 20
```

Dated baselines (measured 2026-06-18, release build — re-measure, don't assume):

| Load | ms/frame |
|---|---|
| Idle | ~0.13 |
| One max-rate flooding Session | ~0.8 |
| 12 simultaneous max-rate floods | ~8 avg / 11 max (still 60 fps) |

How to read deviations:

- **Render is parse-bound, not draw-bound**: cost scales ~linearly per
  *actively-outputting* Session (PTY parsing), not per visible cell. Idle cost
  rising ⇒ a draw-side regression (per-frame allocations, texture churn).
  Flood cost rising superlinearly ⇒ a parser-path regression.
- Only ~20+ *continuous simultaneous* floods threaten the 16 ms budget; the
  known lever at that point is bounded per-frame parsing (decided-deferred —
  see **foreman-change-control** before reaching for it).
- Big **inter-frame gaps while idle are normal**: the adaptive cadence runs a
  slow idle tick and a fast tick only for a short window after activity
  (`rg -n "cadence" src/main.rs` for the live intervals). Windows' ~15.6 ms
  timer granularity floors anything tighter. Don't misread the idle tick as a
  hang.
- Debug-build numbers are meaningless against these baselines; measure release.

## Panic forensics — `foreman_panic.log`

A panic inside the egui/winit callback unwinds across the platform event loop
and **aborts the process with an opaque exit code** — the window just vanishes.
`install_panic_logger` (src/main.rs) exists for exactly this: it **appends**
`=== foreman panic {ts} ===` + panic message + force-captured backtrace to
`%APPDATA%\foreman\foreman_panic.log` before the default hook runs
(`crash_log_path` → `crash_log_path_in(config::config_dir())`; a bare
CWD-relative file is only the fallback for an unresolvable `APPDATA`).

Checklist after a vanish:

1. Read the log at the config dir — **not** the repo root:

   ```powershell
   Get-Content "$env:APPDATA\foreman\foreman_panic.log" -Tail 40
   ```

   A `foreman_panic.log` sitting in a working directory is almost always a
   pre-966379b fossil that will hand you a months-old panic as if it were
   current. Entries are timestamped for exactly this reason — check the `{ts}`
   before you believe one.
2. Entry present → read message + backtrace; it's appended, so read the *last*
   entry. Release backtraces are address-only (no `[profile.release]` debug
   symbols in Cargo.toml); reproduce under the debug build for symbol names.
3. **No new entry → the crash was not a Rust panic** (native crash in a
   dependency, hard abort, or the process was killed). Different investigation.
4. Scope limit: the hook is installed only in GUI mode — in `fn main`
   (src/main.rs) the `client_main` branch exits *before* `install_panic_logger`
   runs, so a panicking `foreman send` prints to stderr instead.

Known-failure symptom dictionary: **foreman-debugging-playbook**.

## Agent/icon detection debugging

`Session::icon_kind` (src/terminal.rs) resolves a tab's icon through a ladder of
layers, first hit wins. When an icon is wrong, walk this table top-down:

| # | Layer | Fires when | Blind spots |
|---|---|---|---|
| 1 | Dispatch argv (`IconKind::from_argv`) | Session was Dispatched (`foreman open claude …`); scans every argv token for the recognized agent substrings (`rg -n "fn from_argv" -A 12 src/icons.rs` for the live set) | Hand-launched agents (no argv recorded) |
| 2 | OSC title stem (`IconKind::from_title`) | Program set a terminal title; the title's **file stem** matches | Codex titles as your *username* → misses; any program with an unhelpful title |
| 3 | Process-tree scan (`proc::agent_for`) | An agent-named process **descends from** the Session's shell PID | **WSL-blind** (sysinfo enumerates *Windows* processes only); throttled by `REFRESH_EVERY` (src/proc.rs) so the icon lags that long after start/exit |
| 4 | Shell glyph | Always (fallback) | — |

- **The stem rule** (`IconKind::from_title`, src/icons.rs): only the title's
  file stem is matched, so a shell whose title is a *path* containing a folder
  literally named `claude code` (this repo's parent, `H:\claude code\`) does not
  false-positive. That case is regression-tested in src/icons.rs. The process
  scan reuses the same discipline on exe names/args.
- Icon lag of one `REFRESH_EVERY` window after an agent starts or exits is the
  throttle, not a bug.
- The matching core `detect_agent` (src/proc.rs) is pure — reproduce a wrong
  result as a unit test with a synthetic process table instead of staring at
  live state.

## egui input debugging — the `area_rect` landmine dump

Domain background: **egui-immediate-mode-reference**. The one diagnostic that
has already paid for itself (docs/os-chrome.md): an egui `Area` registers an
invisible widget over its whole **recorded** bounding rect and blocks every
layer below it — and on its first frame, `constrain` can inflate that recorded
rect far beyond what you painted. Symptom: a whole region of the app goes
mouse-dead with zero visual difference.

**First move:** dump the recorded rects of the chrome Areas and compare with
what you expect:

```rust
// TEMP diagnostic — REMOVE after use.
for name in ["os_chrome", "os_rim_top", "os_rim_bottom", "os_rim_left", "os_rim_right"] {
    eprintln!("{name}: {:?}", ctx.memory(|m| m.area_rect(egui::Id::new(name))));
}
```

Those are foreman's desktop-level chrome Areas (`App::os_resize_rim`, called
from `App::show_os_chrome`, src/main.rs — the `os_rim_*` name array is in the
former). The `app_border` frame is
a **layer painter**, not an Area — it paints only and can never block input.
A rect spanning half the screen when the widget is a thin strip is the
landmine; the cure in-repo is `.constrain(false)` + `.default_size(...)`.

## Repo forensics one-liners

| Goal | Command |
|---|---|
| Test census, per module, densest first | see the census command below the table — derive it, never quote a remembered number. What counts as evidence: **foreman-validation-and-qa** |
| Warning baseline | `cargo build 2>&1 > build.txt; Select-String -Path build.txt -Pattern '^warning'` — expected list and the dead-code false-positive story: **foreman-build-and-env** |
| When did string X appear/vanish? | `git log --oneline -S "PendingSettle" -- src/wm.rs` (pickaxe; that example dates the settle machinery to `d3bec20`) |
| Abandoned/rewritten work | `git fsck --no-reflogs` → dangling commits; inspect with `git show <hash>`. Stories behind them: **foreman-failure-archaeology** |

Test census (the pipe cannot live in a table cell — copy it from here):

```sh
rg -c '#\[test\]' src/ | sort -t: -k2 -rn
```

## When NOT to use this skill

- **A known symptom needs a diagnosis, not a measurement** →
  **foreman-debugging-playbook** (symptom → triage).
- **Deciding whether measured evidence is *sufficient* to call a change done**
  → **foreman-validation-and-qa**.

## Provenance and maintenance

Re-verify these before leaning on them — they are both load-bearing and known
to move:

| Claim | Re-verify with |
|---|---|
| Panic log lands in `%APPDATA%\foreman\`, not the CWD | `rg -n -e crash_log_path -e PANIC_LOG_FILE src/main.rs` — must route through `config::config_dir()` |
| The settle default is a user setting, not a constant | `rg -n "send_settle_ms" src/config.rs src/wm.rs` |
| Crew staleness threshold is a user setting | `rg -n "crew_stale_secs" src/config.rs src/chat.rs` |
| The latency baselines in §Latency measurement are dated 2026-06-18 | re-run the frame harness; doc source `docs/followups-latency-and-control.md` |
