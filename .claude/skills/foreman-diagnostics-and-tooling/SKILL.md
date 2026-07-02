---
name: foreman-diagnostics-and-tooling
description: Use when verifying foreman behavior with evidence instead of eyeballing — proving a Session received input, reading its screen headlessly (foreman send / foreman snapshot, --settle-ms, --attrs, --cursor), measuring frame latency or a perf regression, reading foreman_panic.log after the app vanished, debugging a wrong tab icon (Claude/Codex detection), dead mouse regions, Crew board staleness, or dating a symptom in git history (git log -S, git fsck --no-reflogs, test census).
---

# Foreman diagnostics and tooling — measure, don't eyeball

Every claim about foreman behavior can be backed by a tool. This skill maps
question → instrument → how to read the result. Repo root: `H:/claude code/foreman`.
All commands are PowerShell 7+. Baseline: commit `7fda1c2` (2026-07-01).

## Tool picker

| Question | Instrument | Evidence produced |
|---|---|---|
| Did the Session receive my input? What's on its screen? | `foreman send` → `foreman snapshot` (headless loop, below) | Grid text (a Snapshot) |
| Does the UI *look* right (layout, colors, icons, chrome)? | build-screenshot skill | Pixels (`win.png`) |
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
foreman send --project p1 --terminal t3 --text "cargo test --lib layout" --keys "Enter"
foreman snapshot --project p1 --terminal t3

# Structured reads (reply becomes ONE JSON line instead of text rows):
foreman snapshot --project p1 --terminal t3 --cursor   # {row, col, shape}
foreman snapshot --project p1 --terminal t3 --attrs    # per-cell fg/bg RGB + style flags
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
Mechanics (verified in code, as of 2026-07-01):

| Constant | Value | Where |
|---|---|---|
| Default quiet window | 120 ms | `DEFAULT_SETTLE_MS`, src/wm.rs:17 |
| Hard cap on total wait | 4000 ms | `MAX_SETTLE_MS`, src/wm.rs:18 |
| `--settle-ms 0` | fire-and-forget, replies immediately | src/wm.rs:939 |
| Freshness signal | `Session::output_gen` counter, bumped per PTY chunk | src/terminal.rs:685 |

Settle entries are parked (`PendingSettle`) and advanced once per frame by
`advance_settles` (src/wm.rs:1275, driven from src/main.rs:398) — the GUI never
blocks. Interpretation:

- Reply came back fast (~120 ms): the Session went quiet — snapshot is settled state.
- Reply took ~4 s: the Session **never went quiet** (streaming/flooding/TUI
  animation) — the snapshot is a mid-stream frame. `--settle-ms` up to 4000
  widens the quiet window; beyond 4000 is clamped.
- Quiescence settle is **output** stability. It is not the Caret gate, which is
  **cursor-position** stability for painting (~50 ms window, `CURSOR_SETTLE`,
  src/caret.rs:24). Different signals, different purposes.

> **Doc drift — do not be misled (as of 2026-07-01):** `foreman send --help`
> (src/control.rs:763), the `SendRequest` doc comments (src/control.rs:129,
> 548), and docs/terminal-inspection.md all still say `--settle-ms` is "parsed
> but not yet honored". That was true before commit `d3bec20` (2026-06-26);
> the settle machinery above ships and is unit-tested (src/wm.rs:5356+).
> Trust the code. Flagged for the **foreman-docs-and-writing** trust map.

### Reading a Snapshot

- Default reply: one line per visible grid row, **trailing spaces trimmed**,
  blank rows are empty strings (src/inspect.rs:89) — remember this when diffing.
- `--cursor` reports the **raw model cursor** from the grid
  (`term.renderable_content().cursor`, src/inspect.rs:95) — never the gated
  caret that foreman paints. Diagnostics want ground truth; the painted caret
  intentionally lags/holds during redraw storms. If snapshot-cursor and the
  painted caret disagree briefly, that is the Caret gate working, not a bug.
- `--attrs` resolves colors through the same palette the GUI paints with
  (src/inspect.rs:152), so attrs reads match pixels — use it to prove color/style
  claims without a screenshot.

### Send gotchas that corrupt experiments

- `--text` is written **verbatim — no escape processing** (verified:
  `send_dispatch` writes `text.as_bytes()`, src/wm.rs:1349). A literal `\r`
  typed in your shell arrives as backslash+r. Send Enter as `--keys "Enter"`
  (unambiguous), or embed a real CR from PowerShell with `` "`r" ``.
  docs/terminal-inspection.md's `--text "echo hello\r"` examples are misleading
  in most shells — flagged drift.
- `send` bypasses the Ready hold: `Session::feed` is a raw PTY write
  (src/terminal.rs:673-677), unlike chat injection which queues until the
  Session is Ready. Input sent while a just-spawned Session is still in its
  startup device-status scan can be swallowed. After `foreman open`, snapshot
  until you see a prompt before sending.
- `--text` then `--keys` in one call: text is written first, then keys — one
  atomic request; key names are validated *before* any write (src/wm.rs:1346).

## What stalls your probes (Control plane concurrency)

Full transport/timeout reference: **foreman-run-and-operate**; deadline-nesting
analysis: **foreman-proof-and-analysis-toolkit**. What matters for measurement,
verified at HEAD (2026-07-01):

- One connection carries exactly one request and one reply. A settle-send holds
  **its own** connection for the settle duration — your calling script blocks;
  since commit `15f675f` (2026-06-18, thread-per-connection server, cap
  `MAX_INFLIGHT = 64`, src/control.rs:256) it does **not** block other clients'
  requests.
- Connection *establishment* still serializes: the pipe keeps a single
  listening instance at a time (interprocess 2.4.2 `PipeListener::accept`
  creates the replacement instance inside accept), so concurrent dispatchers
  queue briefly on connect, deadline-bounded at 10 s (`CONNECT_TIMEOUT`).
  Seeing `control pipe stayed busy for 10s` means the server side is wedged or
  flooded, not merely busy with one slow request.
- **Comment drift:** src/control.rs:12-14 ("The server handles one connection
  at a time") predates `15f675f` and overstates serialization of request
  *servicing*. The queue-on-connect part is still accurate.
- The GUI answers requests on its render thread; the server wakes it per
  request (`ctx.request_repaint()`), and `MAX_SETTLE_MS` (4 s) is deliberately
  under `REPLY_TIMEOUT` (5 s) so settles never trip "foreman did not respond".
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
  temporarily spawn extra Projects/Sessions in the `if !self.started` startup
  block (src/main.rs:341), build, screenshot, then **revert the edit**. This is
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
  Chat-room membership (`chat` | `-`), title (src/wm.rs:1090-1110). A worker
  that spawned and instantly died shows `exited(code)`: status asks the live
  process, not the title.
- `chat --history` is a pure read from any caller, and **seq gaps in the output
  are normal**: system entries (join/exit) occupy seqs but are excluded from
  history — "seqs exist to be cited, not to be dense" (src/chat.rs:219-233).
  Line shape: `#14 t2: text`, targeted `#14 t2→t6 (re #9): text`.
- **Crew board** (the Chat viewer's presence panel): live Members sort
  **stalest-first** — the ones to worry about are on top — then the human seat,
  exited Members last (src/chat.rs:668-694). An age turns **amber** once a live
  Member is unheard for ≥ 300 s (`STALE_AFTER`, src/chat.rs:20); `—` means never
  heard; `exited` is gray, not amber. The board hides when the viewer window is
  narrower than 480 px (`CHAT_BOARD_MIN_W`, src/wm.rs:83) — that's layout, not
  a bug.

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
- Big **inter-frame gaps while idle are normal**: the adaptive cadence idles at
  a 100 ms tick and runs a 4 ms tick only for 250 ms after activity
  (src/main.rs:410-419). Windows' ~15.6 ms timer granularity floors anything
  tighter. Don't misread the idle tick as a hang.
- Debug-build numbers are meaningless against these baselines; measure release.

## Panic forensics — `foreman_panic.log`

A panic inside the egui/winit callback unwinds across the platform event loop
and **aborts the process with an opaque exit code** — the window just vanishes.
`install_panic_logger` (src/main.rs:427) exists for exactly this: it **appends**
`=== foreman panic ===` + panic message + force-captured backtrace to
`foreman_panic.log` in the **process CWD** before the default hook runs.

Checklist after a vanish:

1. Look for `foreman_panic.log` **in the directory foreman was launched from**
   (relative path — a foreman started from another CWD logs there).
2. Entry present → read message + backtrace; it's appended, so read the *last*
   entry. Release backtraces are address-only (no `[profile.release]` debug
   symbols in Cargo.toml); reproduce under the debug build for symbol names.
3. **No new entry → the crash was not a Rust panic** (native crash in a
   dependency, hard abort, or the process was killed). Different investigation.
4. Scope limit: the hook is installed only in GUI mode — CLI subcommands exit
   via `client_main` *before* the hook installs (src/main.rs:448-451), so a
   panicking `foreman send` prints to stderr instead.

Known-failure symptom dictionary: **foreman-debugging-playbook**.

## Agent/icon detection debugging

`Session::icon_kind` (src/terminal.rs:444) resolves a tab's icon through four
layers, first hit wins. When an icon is wrong, walk this table top-down:

| # | Layer | Fires when | Blind spots |
|---|---|---|---|
| 1 | Dispatch argv (`IconKind::from_argv`) | Session was Dispatched (`foreman open claude …`); scans every argv token for `claude`/`codex` substrings | Hand-launched agents (no argv recorded) |
| 2 | OSC title stem (`IconKind::from_title`) | Program set a terminal title; the title's **file stem** matches | Codex titles as your *username* → misses; any program with an unhelpful title |
| 3 | Process-tree scan (`proc::agent_for`) | An agent-named process **descends from** the Session's shell PID | **WSL-blind** (sysinfo enumerates *Windows* processes only); throttled ~1.5 s (`REFRESH_EVERY`, src/proc.rs:21) so the icon lags up to that after start/exit |
| 4 | Shell glyph | Always (fallback) | — |

- **The stem rule** (src/icons.rs:81-94): only the title's file stem is
  matched, so a shell whose title is a *path* containing a folder literally
  named `claude code` (this repo's parent, `H:\claude code\`) does not
  false-positive. Regression-tested with `H:\claude code\foreman`
  (src/icons.rs:206). The process scan reuses the same discipline on exe
  names/args.
- Icon lag ≤ ~1.5 s after an agent starts or exits is the throttle, not a bug.
- The matching core `detect_agent` is pure — reproduce a wrong result as a unit
  test with a synthetic process table (src/proc.rs:69) instead of staring at
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

Those five are foreman's desktop-level chrome Areas (src/main.rs:138, 245).
The `app_border` frame is a **layer painter**, not an Area — it paints only and
can never block input (src/main.rs:94). A rect spanning half the screen when
the widget is a thin strip is the landmine; the cure in-repo is
`.constrain(false)` + `.default_size(...)` (src/main.rs:257-258).

## Repo forensics one-liners

| Goal | Command |
|---|---|
| Test census (attribute count) | `(Select-String -Path src\*.rs -Pattern '#\[test\]').Count` — 353 as of 2026-07-01 @ `7fda1c2`; per-module counts and what counts as evidence: **foreman-validation-and-qa** |
| Warning baseline | `cargo build 2>&1 > build.txt; Select-String -Path build.txt -Pattern '^warning'` — expected list and the dead-code false-positive story: **foreman-build-and-env** |
| When did string X appear/vanish? | `git log --oneline -S "PendingSettle" -- src/wm.rs` (pickaxe; that example dates the settle machinery to `d3bec20`) |
| Abandoned/rewritten work | `git fsck --no-reflogs` → dangling commits; inspect with `git show <hash>`. Several dangle as of 2026-07-01. Stories behind them: **foreman-failure-archaeology** |

## When NOT to use this skill

- **Running the app / CLI verb+flag+timeout reference** → **foreman-run-and-operate**.
- **You're an agent inside foreman wanting to dispatch or chat** → the
  user-facing **foreman-dispatch** / **foreman-chat** skills (do not read
  source for operational mechanics).
- **A known symptom needs a diagnosis, not a measurement** → **foreman-debugging-playbook** (symptom → triage), **foreman-failure-archaeology** (history of investigations).
- **Screenshot mechanics** → **build-screenshot**.
- **Deciding whether measured evidence is *sufficient* to call a change done** → **foreman-validation-and-qa**.
- **Designing a new experiment/theory of behavior** → **foreman-research-methodology**; deep analysis recipes → **foreman-proof-and-analysis-toolkit**.
- **Build environment problems** (`-lgcc_eh`, `os error 5`) → **foreman-build-and-env**.

## Provenance and maintenance

Written 2026-07-01 against commit `7fda1c2` (clean working tree; that commit
landed the src/geom.rs + src/frame.rs paint/input seams that were in-flight TDD
earlier the same day — line numbers cited here can drift as that area evolves).
Every claim verified by reading code or running the read-only command that day. Drift-prone
claims and their re-verification commands:

| Claim | Re-verify with |
|---|---|
| Settle constants 120/4000 ms | `Select-String -Path src\wm.rs -Pattern 'SETTLE_MS: u64'` |
| `--settle-ms` honored (stale "not yet honored" docs) | `Select-String -Path src\wm.rs -Pattern 'PendingSettle'` and re-read src/control.rs:763 |
| Snapshot cursor = raw model cursor | `Select-String -Path src\inspect.rs -Pattern 'renderable_content'` |
| `--text` verbatim, no unescaping | `Select-String -Path src\wm.rs -Pattern 'text.as_bytes'` |
| `send` bypasses Ready | `Select-String -Path src\terminal.rs -Pattern 'fn feed'` (read comment above it) |
| Thread-per-connection, `MAX_INFLIGHT = 64` | `Select-String -Path src\control.rs -Pattern 'MAX_INFLIGHT'` |
| Timeouts 5 s / 10 s | `Select-String -Path src\control.rs -Pattern '_TIMEOUT'` |
| `STALE_AFTER` 300 s, stalest-first Crew board | `Select-String -Path src\chat.rs -Pattern 'STALE_AFTER','sort_crew'` |
| Icon layers + 1.5 s throttle | `Select-String -Path src\terminal.rs -Pattern 'fn icon_kind'`; `Select-String -Path src\proc.rs -Pattern 'REFRESH_EVERY'` |
| Panic log name/CWD/append | `Select-String -Path src\main.rs -Pattern 'foreman_panic'` |
| Chrome Area names | `Select-String -Path src\main.rs -Pattern 'os_rim','os_chrome'` |
| Adaptive cadence 4/100 ms, hot 250 ms | `Select-String -Path src\main.rs -Pattern 'cadence'` |
| Latency baselines (2026-06-18) | re-run the frame harness; doc source `docs/followups-latency-and-control.md` |
| Test census 353 | the census one-liner above |
| Baseline commit | `git log --oneline -1` |
