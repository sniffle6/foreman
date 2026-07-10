---
name: foreman-agent-state-campaign
description: Use when implementing or planning per-Session agent-state detection (needs-input / working / done / idle), a state badge, "jump to next needs-you", quiescence gating for injection, READY_GRACE or Ready-latch fallback work, or when a Member never latches Ready and chat posts sit queued forever. Also when tempted to sniff screen text or parse an agent's private state files for status. Signals: src/terminal.rs ready/output_gen, src/chat.rs Outbox, src/proc.rs, docs/HANDOFF.md §5.
---

# Agent-State Campaign: needs-input / working / done / idle per Session

**Status: DESIGN-STAGE. Nothing in this campaign is built (as of 2026-07-01).**
This is a decision-gated runbook for foreman's hardest live problem, not a
description of shipped code. Every phase has an exit gate; do not skip gates.
Nothing here authorizes routing around **foreman-change-control** — thresholds,
state names, and any new Control-plane verb are user sign-off items.

**Roadmap cross-link (2026-07-10):** `docs/warp-feature-candidates.md` treats
this campaign as the **product goal**, not the next shovel. Shovel first:
font fallback, snapshot `--tail`, then **Phase 0 READY_GRACE** before any
detector or badges. Task-manager panel is the future badge landing site.
OSC 133 (warp doc #1) is a Phase 1 *signal candidate*, not a parallel track.

Baseline for all citations: commit `7fda1c2` (working tree clean when audited,
2026-07-01). Re-check `git status` before building — this repo carries active
TDD work between commits.

## 1. The problem, and why it is hard

HANDOFF names this the differentiator: *"detect 'needs input' / done / idle and
surface it (badge on the terminal/project titlebar, a 'jump to next needs-you')"*
— docs/HANDOFF.md:175-177 (§5 "Next phases", item 2).

Why it is hard, in one sentence: **foreman sees PTY bytes, not turn state.**

- A *PTY* (pseudo-terminal) is the OS byte pipe a terminal program reads and
  writes; on Windows the OS layer is ConPTY. Foreman's `Session` owns one PTY
  plus an emulated screen. Full domain background: **terminal-emulation-reference**.
- From that byte stream you cannot directly see whether an agent is mid-tool-call,
  parked at a permission prompt, or idle at its input box. The chat-mentions
  design flags this exact gap as an **unsolved problem**:
  *"'Between turns' is not observable from where foreman sits… Any gate is a
  heuristic… and heuristics here fail in the worst direction: inject at the wrong
  moment and you corrupt an agent's in-flight work"* —
  docs/superpowers/specs/2026-06-10-chat-mentions-design.md:30-54 (the ⚠ section)
  and open question 1 (lines 115-120). Quiescence gating (decision-table row 5
  there) is design-settled but its core mechanism is explicitly UNSOLVED.
- The incident proving the fragility: a `\r` written back-to-back with a chat
  paste got folded into Claude Code's paste-burst detection and never submitted;
  the fix is a deferred submit `SUBMIT_DELAY = 150ms` (src/terminal.rs:300,
  commit `45f4725`). That failure took a live session to discover.

Consequence: this campaign is **passive-first**. Phases 0-2 write zero bytes
into any agent's Session for detection purposes. Anything active is fenced
(§7) or deferred to the explicit-cooperation option (§6d).

### Target states (proposal, not vocabulary yet)

| State (candidate name) | Meaning |
|---|---|
| needs-input | Agent is blocked on the human/dispatcher (permission prompt, question) |
| working | Agent is mid-turn (streaming output or mid-tool-call) |
| done | Agent finished its task; process may still be alive at its input box |
| idle | Alive, at its input box, no task in flight |

Naming is a **promotion gate** (§9): the chosen names must enter CONTEXT.md's
glossary with user sign-off. Note "done vs idle" may prove observationally
indistinguishable without option (d) — Phase 1 decides.

## 2. Terms defined once

| Term | Meaning here |
|---|---|
| DSR | Device Status Report, the escape sequence `ESC[6n` — a program asking "where is the cursor". The terminal must reply or the program hangs. Shells emit it at startup. |
| OSC title | A program setting its window title via an escape sequence; foreman captures the latest one per Session (`Session::osc_title`, src/terminal.rs:433). |
| Ready | Glossary term: the state a Session reaches once it has answered the startup DSR; injected input only lands after Ready (src/terminal.rs:271-275). |
| Quiescence settle | Glossary term: waiting until a Session has produced no new output for a short window before reading it (the `foreman send` reply mechanic). |
| Inspection | Glossary term: driving a Session with `send` and reading its screen with `snapshot` over the Control plane. |

Everything else VT/ConPTY-shaped: **terminal-emulation-reference**.

## 3. Ground truth: the observation substrate that already exists

These are the raw materials. All verified at `7fda1c2` (as of 2026-07-01).

| Signal | Where | What it tells you | Grain |
|---|---|---|---|
| Ready latch | `ready: bool`, src/terminal.rs:275; latched in `pump()` when the first DSR reply is flushed back (src/terminal.rs:718-725); getter `ready()` :566 | Startup DSR scan resolved; injection is safe to land | one-shot boolean, per Session |
| Output generation | `output_gen: u64`, src/terminal.rs:278; bumped per PTY batch in `pump()` :716; getter :687 | "New bytes arrived since I last looked" — the freshness counter the settle machinery polls | per-frame delta, per Session |
| Quiescence settle | `settle_tick` + `PendingSettle` + `advance_settles`, src/wm.rs:17-49, 938-961, 1275-1312; driven per frame from src/main.rs:396-398. `DEFAULT_SETTLE_MS = 120`, `MAX_SETTLE_MS = 4000` (src/wm.rs:17-18) | Output-silence detection already exists as a pure, unit-tested function | window over output_gen |
| Cursor position/shape | `snapshot --cursor` → `CursorInfo {row, col, shape}`; shape ∈ block/beam/underline/hollow/hidden (src/inspect.rs:33-37, 95-107) | Where the program's cursor rests; TUIs park it at their input box | per Snapshot |
| Cursor stability | Caret gate: `CURSOR_SETTLE = 50ms` (src/caret.rs:24) — cursor-position stability, explicitly distinct from output quiescence (src/caret.rs:127) | A worked example of "resting vs mid-redraw" discrimination, pure + tested | per frame (internal) |
| OSC title | `Session::osc_title` (src/terminal.rs:238, 433); consumed ONLY by `icon_kind` today (src/terminal.rs:444-462) | Which agent is running — Claude sets `claude`; **Codex sets your username** (docs/tab-icons.md, "Detection" step 2) | identity, not state |
| Process tree | `proc::agent_for` / pure `detect_agent` (src/proc.rs:69-141); refresh throttle `REFRESH_EVERY = 1500ms` (src/proc.rs:21); **WSL-blind** (src/proc.rs:10-12) | Which agent runs under the shell; running vs exited truth is `Session::exited()` (see src/wm.rs:1062-1064) | coarse, ≤1.5s lag |
| Outbox Ready gate | `ChatRoom::tick` skips non-Ready Members (`!l.ready` → cursor stays, catch-up later), src/chat.rs:598-666; `LiveMember.ready` :447 | The delivery layer already keys on the one state bit that exists | consumer of Ready |

**Drift flag (verified 2026-07-01):** src/control.rs still says `settle_ms` is
*"parsed and stored but not yet honored (settle is the next phase)"* at
control.rs:129-130, :548, and in `HELP_SEND` (:763). That is stale — settle IS
honored (src/wm.rs:938-961 parks a `PendingSettle`; src/main.rs:398 drives it).
Trust the wm.rs code, not the control.rs comments/help text, and expect those
strings to be corrected eventually.

**The one state bit that exists today is Ready.** Everything else is raw signal.
That is why Phase 0 hardens Ready before anything new is invented.

## 4. Campaign map

| Phase | Deliverable | Exit gate |
|---|---|---|
| 0 | READY_GRACE fallback latch (already-designed, unbuilt) | A Member that never answers DSR still latches Ready by timeout; deterministic test green |
| 1 | Signal audit: measured discrimination table across labeled regimes | Every signal marked keep/strike with recorded evidence |
| 2 | One solution from the ranked menu, built as a pure module + fixture tests | Numeric gates of §8 pass on fixtures |
| 3 | UI surfacing + (optionally) explicit-cooperation verb | Promotion checklist §9 complete, user signed off |

## 5. Phase 0 — harden the observation layer (READY_GRACE)

**Rationale: the Ready latch IS state-detection v0.** Today a Session that never
emits a DSR never latches Ready: `inject_input` queues forever
(src/terminal.rs:663-667) and the Outbox never delivers (src/chat.rs:638-639).
A state machine built on a latch that can wedge inherits the wedge.

The design already exists — docs/followups-latency-and-control.md §2 ("Grace
fallback for ready-gated injection", lines 45-55): add a `spawned: Instant`
field plus a generous `READY_GRACE` (1.5-2s, comfortably longer than real DSR
latency); in `pump()`, latch Ready by timeout as a fallback. **Make the grace
injectable** so the fallback is deterministically testable; otherwise it is
untestable without a contrived non-DSR program.

**Verified unbuilt (as of 2026-07-01):**

```powershell
# Both must return nothing:
Set-Location "H:/claude code/foreman"
Select-String -Path src/*.rs -Pattern "READY_GRACE"
Select-String -Path src/terminal.rs -Pattern "spawned:\s*std::time::Instant|spawned:\s*Instant"
```

### Build checklist (TDD; route the change through foreman-change-control)

1. `Session` gains `spawned: Instant` + an injectable grace (constructor
   parameter or a test-settable field — mirror how `settle_tick` was made pure
   in src/wm.rs:34-49 rather than inventing a new pattern).
2. In `pump()` (src/terminal.rs:713-740): if `!self.ready` and
   `spawned.elapsed() >= grace`, latch `ready = true`. The existing
   flush-queued-injects block at :728-732 then fires unchanged.
3. Tests, modeled on the existing recipes at src/terminal.rs:1332-1391
   (`session_latches_ready_after_dsr_is_answered`,
   `inject_before_ready_is_queued_then_flushed`):
   - **Grace path:** a child that never sends DSR, tiny injected grace
     (e.g. 50ms) → `ready()` latches by timeout and a pre-Ready
     `inject_input` flushes.
   - **Pre-check that your child really never DSRs:** with a HUGE grace,
     assert `!ready()` holds across ~1s of pumping. The existing tests use
     `cmd.exe /c pause`, which DOES emit DSR (that is the point of those
     tests) — you need a non-shell child. Candidate: a bare exe like
     `ping -n 30 127.0.0.1` via `Session::spawn_argv(&argv, None, &[], ctx)`
     (signature per src/terminal.rs:1337). **Unverified which Windows exes
     skip DSR — the pre-check is mandatory, not optional.**
   - **Non-regression:** the two existing DSR tests stay green (real DSR must
     still latch before the grace fires).

**GATE — EXPECTED:** grace-path test green; existing DSR tests green.
**If instead** `ready()` latches immediately even with a huge grace → your
child answered DSR (or ConPTY produced early output your Listener echoed);
pick a different child and re-run the pre-check.
**If instead** the queued inject flushes but the paste is eaten → you latched
before the DSR scan actually resolved on a real shell; the grace is too small
or you latched outside `pump()` — re-read docs/followups-latency-and-control.md:45-55.

## 6. Phase 1 — signal audit (pure observation, no code changes)

Goal: a measured discrimination table. **Rule: a signal that does not
discriminate two regimes is struck from the menu.** Predict first, then
measure (discipline: **foreman-research-methodology**).

### Setup

Run foreman with one Project and one real agent Session (Claude Code or
Codex). Dispatch it from inside foreman (the **foreman-dispatch** skill is the
user-facing way) or type `claude` at a shell Session by hand. Then drive the
audit from an ordinary PowerShell *outside* foreman — any built foreman.exe
acts as the Control-plane client (full verb ground truth:
**foreman-run-and-operate**):

```powershell
$fe = "H:/claude code/foreman/target/debug/foreman.exe"
& $fe status          # note your pN / tN; state column is running|exited(code)
```

### The polling loop (Inspection, read-only)

`output_gen` is not exposed over the pipe; its external proxy is "did the
screen text change between polls". Snapshot is *"a read, never a side effect"*
(CONTEXT.md) — it pumps parser state but writes nothing to the child.

```powershell
$fe = "H:/claude code/foreman/target/debug/foreman.exe"
$P = "p1"; $T = "t2"    # <- your ids from `foreman status`
$prev = ""
while ($true) {
  $r = & $fe snapshot --project $P --terminal $T --cursor | ConvertFrom-Json
  $txt  = ($r.history -join "`n")
  $hash = [BitConverter]::ToString(
            [System.Security.Cryptography.SHA1]::HashData(
              [Text.Encoding]::UTF8.GetBytes($txt))).Replace("-","").Substring(0,8)
  $flag = if ($hash -ne $prev) { "CHANGED" } else { "quiet  " }
  "{0:HH:mm:ss.fff}  screen={1} {2}  cursor=({3},{4}) {5}" -f (Get-Date),
      $hash, $flag, $r.cursor.row, $r.cursor.col, $r.cursor.shape
  $prev = $hash
  Start-Sleep -Milliseconds 250
}
```

Redirect to a file per regime (`… *> phase1-R1.log`). 4 Hz is safe: the pipe
server is thread-per-connection with a 64-inflight cap and a 10s client
connect deadline (src/control.rs:17, 256).

### Regimes (label by construction, never by eye)

| Regime | How to construct it | You know you're in it because |
|---|---|---|
| R1 working/streaming | Give the agent a long-output task: `& $fe send --project $P --terminal $T --text "explain this repo file by file"` then, in a SECOND call, `& $fe send --project $P --terminal $T --keys Enter` | You issued the prompt; output is flowing |
| R2 mid-tool-call | Prompt it to run a slow command ("run `git log -200` and summarize") | Between your Enter and the summary appearing |
| R3 needs-input | Prompt something that triggers a permission dialog | The dialog is on screen (confirm in the Snapshot text) |
| R4 resting (done/idle) | Wait for the turn to finish; poll for ≥60s after | Task provably complete |

**Why two `send` calls for prompt+Enter:** `send` writes text then keys
back-to-back in one frame (src/wm.rs:1349-1354). Claude Code folds
same-burst input into a paste — the exact failure `SUBMIT_DELAY` (150ms,
src/terminal.rs:300, commit `45f4725`) exists to dodge on the chat path.
The single-request fold is inferred, not re-measured — two calls cost
nothing and sidestep it. Also EXPECT each `send` reply to take up to
`MAX_SETTLE_MS` (4s) while the agent still streams: the settle deadline
caps the wait (src/wm.rs:47, :959).

### What to record per regime (the audit sheet)

For each signal, write a PREDICTION before running, then the measurement:

| Signal | R1 stream | R2 tool-call | R3 needs-input | R4 resting |
|---|---|---|---|---|
| screen-hash change rate (per 10s) | | | | |
| cursor resting (same cell ≥8 consecutive polls = 2s)? | | | | |
| cursor shape | | | | |
| tab icon / OSC title change? (screenshot — **build-screenshot** skill; osc_title has no pipe surface today, terminal.rs:444-462 is its only consumer) | | | | |
| `status` state column | | | | |

**GATE — EXPECTED:** at least one signal (or a pair) separates R3 from
{R1, R2}, and something separates {R1, R2} from R4.
**If a signal ties across two regimes** → strike it from §6's menu inputs and
record the evidence line in your notes.
**If R3 is indistinguishable from R4 by every passive signal** (both quiet,
cursor parked) → the composite (a) cannot deliver needs-input alone; promote
option (d) from phase-3 candidate to required, and stop to get user sign-off
(scope change).
**If nothing separates R2 from R4** (tool runs silently) → accept "working"
detection latency = tool duration, or same escalation as above. Do not invent
a text-sniffing fallback (§7).
**If Claude/Codex spinners keep the screen hash changing while parked at a
prompt** → your quiet-threshold must key on *region* or *rate*, not any-change;
note it as a composite-design obligation, don't hand-tune live.

## 7. Phase 2 — solution menu (ranked; each option's theory obligations)

### (a) RECOMMENDED: composite pure state machine over (output quiescence × cursor-rest × recent-injection)

A new pure module (e.g. `src/agentstate.rs`), Outbox-style: fed plain
observations per frame, returns a state; no egui, no PTY handles — exactly the
seam pattern of `settle_tick` (src/wm.rs:34-49), the Caret gate
(src/caret.rs), and `ChatRoom::tick` (src/chat.rs:598). Unit-tested against
**recorded fixtures** from Phase 1 logs.

Inputs (all already obtainable inside the GUI process, zero new plumbing):
`output_gen` delta, cursor cell/shape stability (Caret-gate-style, distinct
constants), time since the last `inject_input`/`feed` into that Session,
`Session::exited()`, Ready.

**Theory obligations — BEFORE any code (predict, then measure):**
1. Write the state lattice: states, and which transitions are legal
   (e.g. working→needs-input yes; done→working only via new injection).
2. Write the transition evidence table: for every edge, which observation
   combination triggers it and with what hysteresis constant — filled from the
   Phase 1 audit sheet, not from intuition.
3. Name every constant, make each injectable for tests (the `settle_tick` and
   READY_GRACE precedent).
4. State the confusion costs asymmetrically: a false "needs-you" (human
   interrupted for nothing) is the expensive error; a late one is cheap.
   The gates in §8 encode this.

### (b) OSC-title heuristics — corroborating signal only

Cheap but partial and identity-flavored: Claude sets a useful title, **Codex
sets your username** (docs/tab-icons.md "Detection" step 2; src/terminal.rs:439-442).
Also unplumbed for state: `osc_title` feeds only `icon_kind` today. Acceptable
as a corroborating input to (a); never the primary discriminator. Some agent
CLIs mutate the title per activity — if Phase 1 shows that, record it as a
bonus column, but the composite must not require it.

### (c) Process-tree signals — corroborating only

`Session::exited()` gives running-vs-exited truth (src/wm.rs:1062-1064);
`proc::agent_for` says *which* agent, ≤1.5s stale (src/proc.rs:21) and
**WSL-blind** (src/proc.rs:10-12). Coarse: it can prove "done because the
process died", never "needs input". Feed exited into (a); nothing more.

### (d) Explicit agent cooperation — strongest signal, phase-3 candidate, NOT phase 2

A new Control-plane verb (sketch: `foreman state working|blocked|done`) the
agent calls to declare its own state. Strongest possible signal; costs:
- Cross-provider: every agent must be taught to call it — updates to the
  user-facing skills in `.claude/skills/` and `.codex/skills/`, kept in sync
  and re-embedded via `src/skills_install.rs` (CLAUDE.md documents this sync
  duty). Uncooperative or crashed agents still need (a) as the floor.
- Wire compatibility: new request/reply fields must keep v1 replies
  byte-identical via the established serde skip pattern
  (src/control.rs:52-59, :146-150).
- Cousin precedent: typed chat message kinds, docs/chat-missing-features.md §4.
Build only after (a) ships as the baseline, and only through
**foreman-change-control** (new verb = wire surface = sign-off).

## 8. Fenced wrong paths — do not attempt

| Fenced path | Why | Citation |
|---|---|---|
| Keyword-sniffing screen/output text for state ("Working…", "Allow?") | Rejected for chat kinds — keying behavior off literal words is fragile; same fragility here, worse: agent UIs restyle across versions | docs/chat-missing-features.md:209-210 ("Explicitly NOT recommended") |
| Parsing another tool's private state files (Claude/Codex session files) | Agent-teams integration rejected for exactly this: another tool's private format rots under you | docs/chat-missing-features.md:211-212 |
| Active interrogation — writing bytes (cursor queries, test keys) into the agent's Session to see how it reacts | Injecting mid-turn can corrupt in-flight work; the WHEN gate is the unsolved problem, so an interrogator cannot know when it's safe — circular | 2026-06-10-chat-mentions-design.md:30-44; the `45f4725` incident |
| Re-deriving ConPTY resize/reflow behavior because state polling coincides with resize artifacts | Settled; ConPTY's bug, not ours; "let ConPTY own the redraw" already tested and failed | docs/conpty-resize-reflow.md; CLAUDE.md gotcha |

## 9. Validation protocol — success is measured, never judged by eye

1. **Fixture set:** script real sessions end-to-end over the Control plane —
   `foreman open` a claude and a codex worker, drive them with two-call `send`
   (§6), label regimes by construction, capture the timestamped
   snapshot/cursor series (the §6 loop with `*>` redirection) as fixture
   files. Minimum: ≥20 labeled transitions per agent CLI, including ≥5
   needs-input events.
2. **Numeric gates — PROPOSALS pending user sign-off (get the numbers signed
   before coding to them, per foreman-change-control):**
   - G1: **zero** false needs-input during one continuous 10-minute streaming
     run, polled at 4 Hz.
   - G2: needs-input latched within **2s** of the permission prompt appearing
     on ≥**95%** of labeled transitions in the fixture set.
   - G3: flap bound — at most **1** state change per constructed transition
     (hysteresis actually works; no strobing badge).
   - G4 acid test: one full real Claude task and one full Codex task run to
     completion with detection enabled and behave identically to a control
     run. For the passive composite this must be trivially true — the
     detector writes **zero bytes** into any Session, provable by code review
     of the module's inputs, and that invariant is the design's core selling
     point. State it in the module docs.
3. Classifier changes re-run the whole fixture set; a fixture that flakes gets
   investigated, not deleted. Evidence standards and what counts as proof:
   **foreman-validation-and-qa**. Measurement tooling (probe loops, screenshot
   verification, latency harness patterns): **foreman-diagnostics-and-tooling**.

## 10. Promotion path (in order; each step gated)

1. **Pure module + fixture tests** land first — no UI, no behavior change to
   injection or delivery. Gates G1-G3 green on fixtures.
2. **UI badge** behind the existing per-frame flow: the badge paints during
   the draw pass; any interaction it triggers ("jump to next needs-you",
   focus changes) is recorded and applied as a **Deferred action** — the draw
   pass must not mutate nested window managers mid-render. Threading model
   and seam map: **foreman-architecture-contract**.
3. **Docs + vocabulary:** a feature doc under `docs/` (per the house docs
   system — **foreman-docs-and-writing**) and a CONTEXT.md glossary entry
   naming the states with their avoid-synonyms. The state names are ubiquitous
   language from that moment on.
4. **User sign-off gates** per **foreman-change-control**: state vocabulary,
   the G1-G3 numbers, any Outbox/delivery coupling (making chat delivery wait
   on "not working" would finally solve mentions-design row 5 — that is a
   separate, explicitly gated change, not a freebie), and any new
   Control-plane verb from option (d). Commit only when asked.

## When NOT to use this skill

- Running `send`/`snapshot`/`status` day-to-day or looking up CLI flags →
  **foreman-run-and-operate**; measurement loop mechanics →
  **foreman-diagnostics-and-tooling**.
- You are an agent *inside* foreman wanting to launch workers or coordinate →
  the user-facing **foreman-dispatch** / **foreman-chat** skills, not this.
- Debugging an existing failure (black Session, swallowed input) →
  **foreman-debugging-playbook**; history of dead ends →
  **foreman-failure-archaeology**.
- PTY/VT/ConPTY concepts in depth → **terminal-emulation-reference**; egui
  paint/frame rules → **egui-immediate-mode-reference**.
- Whether this problem is externally novel / publishable →
  **foreman-research-frontier**.

## Provenance and maintenance

Written 2026-07-01 against commit `7fda1c2` (clean tree at audit time). This
campaign is design-stage: no agent-state module, no READY_GRACE, no state verb
exists at that commit. Re-verify drift-prone claims (run from
`H:/claude code/foreman`):

| Claim | Re-verify |
|---|---|
| READY_GRACE still unbuilt | `Select-String -Path src/*.rs -Pattern "READY_GRACE"` → no output |
| Ready latch lines (terminal.rs:275, :718-725, :663-667) | `Select-String -Path src/terminal.rs -Pattern "ready: bool","self.ready = true"` |
| output_gen field/getter (:278, :687, :716) | `Select-String -Path src/terminal.rs -Pattern "output_gen"` |
| Settle constants 120/4000 (wm.rs:17-18) and settle honored | `Select-String -Path src/wm.rs -Pattern "DEFAULT_SETTLE_MS","MAX_SETTLE_MS","advance_settles"` |
| control.rs "not yet honored" comments still stale | `Select-String -Path src/control.rs -Pattern "not yet honored"` — if gone, delete the drift flag in §3 |
| SUBMIT_DELAY 150ms + incident commit | `Select-String -Path src/terminal.rs -Pattern "SUBMIT_DELAY"`; `git show 45f4725 --stat` |
| Outbox Ready gate (chat.rs:598-666) | `Select-String -Path src/chat.rs -Pattern "l.ready"` |
| proc scan throttle 1500ms / WSL-blind | `Select-String -Path src/proc.rs -Pattern "REFRESH_EVERY","WSL"` |
| Codex-sets-username title quirk | `Select-String -Path docs/tab-icons.md -Pattern "username"` |
| Quiescence gating still UNSOLVED in the mentions design | `Select-String -Path "docs/superpowers/specs/2026-06-10-chat-mentions-design.md" -Pattern "unsolved"` |
| HANDOFF differentiator lines (175-177) | `Select-String -Path docs/HANDOFF.md -Pattern "needs input"` |
| Baseline commit | `git log --oneline -1` |

Line numbers above drift with any edit to the cited files; the pattern
searches are the durable re-anchors.
