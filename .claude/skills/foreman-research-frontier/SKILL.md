---
name: foreman-research-frontier
description: Use when asking whether foreman work is novel or state-of-the-art, planning research-grade efforts (agent-state detection, headless agent self-verification, the chat-room A/B experiment, session persistence / daemon-client split, per-Session panic isolation, READY_GRACE), preparing any external claim, benchmark, blog post, or release, or when someone proposes reopening ConPTY resize reflow. Everything here is open or candidate — none of it is built.
---

# Foreman research frontier

Open problems where foreman could advance the state of the art, plus the
standards for claiming anything externally. **Status discipline: every item in
this skill is `open`, `candidate`, or `designed-not-built`. Nothing here is
shipped.** Verify every "still absent" claim against `src/` before you act on
it — the absences are what rot. Any work spawned from this skill still goes
through **foreman-change-control** — this document authorizes nothing.

Vocabulary is CONTEXT.md's (repo root): a **Session** is one running terminal
(shell or agent CLI plus its emulated screen); a **Snapshot** is its rendered
grid read as data; **Ready** is the latch a Session sets once it has answered
its program's startup device-status query; the **Control plane** is the local
request channel (`foreman open/chat/status/close/send/snapshot`,
src/control.rs `HELP` / `serve`) an in-terminal agent uses to drive foreman;
**Quiescence settle** is waiting for output silence before reading. A PTY is
the OS object that makes a program believe it's talking to a terminal; ConPTY
is Windows' implementation — see **terminal-emulation-reference** for the
domain pack.

## Frontier map

| # | Frontier | Status | Full detail lives in |
|---|----------|--------|----------------------|
| 1 | Agent-state detection | open (PRIMARY) | **foreman-agent-state-campaign** |
| 2 | Headless agent self-verification | candidate; inspection layer built, gaps below | this skill |
| 3 | Multi-agent coordination measurement (A/B) | designed-not-built, never executed | this skill |
| 4 | Session persistence (daemon/client split) | open, listed in HANDOFF | this skill |
| 5 | Fleet reliability floor | designed 2026-06-18, unbuilt | this skill |
| 6 | ConPTY reflow ownership | **FENCED — do not reopen** | docs/conpty-resize-reflow.md |

---

## 1. Agent-state detection (PRIMARY)

**Problem.** Know, per Session, whether the agent inside it is working, waiting
for the user, blocked at a permission prompt, or done — and surface it across a
fleet ("jump to next needs-you", HANDOFF § 5 item 2).

**Why current SOTA fails.** tmux / zellij / wezterm model *processes*
(foreground process, exit status), not *agent turns*. Foreman's own design docs
state the hard part precisely: "'Between turns' is not observable from where
foreman sits… Any gate is a heuristic, and heuristics here fail in the worst
direction" (docs/superpowers/specs/2026-06-10-chat-mentions-design.md, the
quiescence-gating warning — that spec's row 5 is design-settled with an
unsolved mechanism at its center).

**Foreman's asset.** Foreman IS the terminal, so it sees
every byte with emulator-level structure, and it already has:
- the **Ready** latch (`Session::ready`, driven by the pure `ReadyGate` in
  src/ready.rs — DSR reply flushed AND the child's first painted glyph),
- a cheap output-freshness signal (`Session::output_gen`, src/terminal.rs),
- Quiescence settle machinery (`PendingSettle`/`advance_settles`/`settle_tick`,
  src/wm.rs),
- structured Snapshots (`snapshot --attrs --cursor`, commit `ec0af05`),
- the Control plane + per-Session env identity for labeling real sessions.

**First steps.** Do not improvise: the executable, decision-gated campaign —
taxonomy, labeled real-session fixtures, classifier, declared precision/recall
gates — is **foreman-agent-state-campaign**. Start at its first gate.

**You have a result when:** a classifier meets its *pre-declared*
precision/recall targets on labeled fixtures captured from real agent Sessions
(not synthetic byte streams), and the numbers + fixtures are committed so anyone
can re-score.

## 2. Headless agent self-verification

**Problem.** Agents developing foreman *inside* foreman should verify terminal
features via `send`/`snapshot` (grid as data) instead of screenshots (pixels) —
faster, diffable, no GUI capture, machine-checkable evidence.

**Why current SOTA fails.** The common agent verification loop for terminal
software is screenshot-and-look. tmux `send-keys`/`capture-pane` and `wezterm
cli send-text`/`get-text` are real prior art for the primitives, but neither
couples send to a settled read (Quiescence settle as the reply gate), gates
injection on Ready, or gives the agent an env-injected self-target.

**Foreman's asset — the inspection layer is BUILT:**

| Piece | Evidence |
|-------|----------|
| `foreman send --text/--keys/--settle-ms/--terminal` | src/control.rs `SendRequest`; the settle path in src/wm.rs |
| Settle honored, non-blocking for the GUI | `PendingSettle` + `settle_tick` + `advance_settles` (src/wm.rs); the per-send default is `Settings::send_settle_ms` (src/config.rs, user-editable, sanitize-clamped) under the hard `MAX_SETTLE_MS` cap in src/wm.rs |
| `foreman snapshot` text / `--attrs` / `--cursor` / `--tail N` | commit `ec0af05`; `--tail` shipped `43e86d3`; src/inspect.rs |
| Zero-flag self-target | `FOREMAN`, `FOREMAN_TERMINAL_ID`, `FOREMAN_PROJECT_ID` injected at spawn (src/wm.rs `fn term_env`) |

**What's missing (re-verify with `rg -n 'wait_for|since_seq|"--region"|"--rows"' src/`
before building — the read side keeps growing, `--tail N` closed the
last-N-lines gap in `43e86d3`):** `--wait-for`/`--timeout-ms`, `--since-seq`,
`--region`, `--rows`, and the **`REPLY_TIMEOUT` exemption** — the GUI drops any
control message older than `REPLY_TIMEOUT` (src/control.rs), so a long
`--wait-for` would be dropped by design unless
inspections are exempted and honored up to their own deadline. This is the
documented ripple warning in docs/epics/terminal-inspection-epic.md ("the
`REPLY_TIMEOUT` stale-drop must exempt inspections") — read its landmines
section before building.

**First three steps in this repo:**
1. Build `snapshot --since-seq` (non-blocking poll; `output_gen` is the
   generation signal) per the epic's design.
2. Build `--wait-for` + the `REPLY_TIMEOUT` exemption using the epic's
   pending-inspections pattern — the hardest step; it touches the pipe server's
   `recv_timeout` too.
3. Dogfood: pick one real terminal feature and require its verification
   evidence to be send/snapshot only.

**You have a result when:** an agent lands a terminal feature end-to-end with
**zero screenshots** — every verification step in its evidence chain is a
`send`/`snapshot` transcript, and the chain is intact in the PR/doc. Evidence
standards live in **foreman-validation-and-qa**; measurement recipes in
**foreman-diagnostics-and-tooling**.

## 3. Multi-agent coordination measurement (the chat-room A/B)

**Problem.** Nobody has data on whether an agent chat room actually improves
result quality or token efficiency. Industry claims here are anecdote.

**Why current SOTA fails.** A "solo vs team" comparison bundles two variables
(parallelism and the channel). Foreman's designed experiment isolates the
channel: three arms (A solo; B team with contract committed upfront, no chat;
C team negotiating the contract live in the Chat room), blind grading against a
hidden acceptance suite, headline metrics **tokens-per-requirement-met** and
**bugs-per-1k-tokens**, ≥3 runs per arm with medians + spread.

**Foreman's asset.** The experiment is fully designed and planned —
docs/superpowers/specs/2026-06-15-chat-room-ab-experiment-design.md and
docs/superpowers/plans/2026-06-15-chat-room-ab-experiment.md — a complete,
ready, never-executed asset. The tell that it never ran: no
docs/chat-ab-results.md (`Test-Path docs/chat-ab-results.md`). Do not use
"there is no `scripts/` dir" as the tell — `scripts/` exists now for unrelated
reasons.

**Prerequisite: chat persistence — designed-not-built.**
docs/chat-persistence.md is the build plan, with its settled decisions
(JSONL with trailing-newline commit marker; one narrow module, no trait;
synchronous write on the post path; periodic off-path fsync; one-pass reload;
at-least-once delivery + dedup-by-seq) and **the seq/len landmine**:
`ChatLog::push` assigns `seq` from `self.msgs.len() as u64 + 1` and `last_seq()`
returns `msgs.len()` (src/chat.rs) — decouple seq from Vec
length (`next_seq`) FIRST or seqs collide/rewind across restart. Those
decisions are do-not-re-litigate per **foreman-change-control**.

**First three steps in this repo:**
1. Decouple seq from length (`next_seq` on `ChatLog`) — chat-persistence.md
   plan step 1.
2. Ship minimal JSONL append + reload (plan steps 2–4); settle the file-location
   open decision first (the plan recommends `%APPDATA%\foreman\chat\<hash>.jsonl`).
3. Run the experiment's Phase 0 de-risk: confirm token usage is harvestable
   from Claude Code session JSONLs and the chat transcript survives a run,
   before spending 9 builds.

**You have a result when:** `docs/chat-ab-results.md` exists with real per-arm
metric tables, the three comparisons (A↔C, B↔C, A↔B) on medians, and a
data-ranked reordering of docs/chat-missing-features.md.

## 4. Session persistence (daemon/client split)

**Problem.** True tmux-style survival: a Session's process and screen outlive a
GUI restart. Listed as next-phase work in docs/HANDOFF.md § 5
("Daemon/client split — move PTYs into a headless core so sessions persist
across UI restarts").

**What already ships is NOT this.** `workspace.json` (src/workspace.rs) restores
the **layout tree**, not the processes: `capture_workspace` writes a
`WorkspaceSnapshot` (built from per-level `ManagerSnap`s) of window rects, tabs
and `ContentSnap` — and
`ContentSnap::Terminal` carries only a `shell` string. On restart foreman
rebuilds the same tiling and spawns a *fresh* shell in each leaf. No child
process, scrollback, or agent conversation survives. Do not read the presence of
workspace.json as this frontier being half-done; it makes the frontier's
prerequisite *more* visible, not less (see the id-stability blocker below).

**Why it's hard here.** Sessions are owned by the GUI thread: a `Win`'s
`Content::Terminal` holds the `Session` (PTY + reader thread) inside the
`WindowManager`, and when the foreman process exits its ConPTYs close and the
child processes die with it. There is no separation between "the thing that
owns the PTY" and "the thing that paints."

**Why it's a frontier and not a solved import.** tmux solves this on Unix;
wezterm's mux-server is prior art to study. The candidate advance is the
composition: tmux-style persistence for **ConPTY-backed Sessions on Windows**
under a recursive GUI compositor, reusing an existing agent Control plane as
the client protocol. Label: candidate until a prior-art sweep says otherwise.

**Foreman's asset.** The Control plane is already message-passing (every CLI
verb over a local named pipe, serviced as `CtrlMsg` on the UI thread — operational
ground truth in **foreman-run-and-operate**), and the persistence designs
already anticipate restart: chat-persistence.md's seq-monotonic-across-restart
constraint, and its explicitly named blocker — **terminal/project ids (`tN`,
`pN`) are session-assigned and NOT stable across restart** (its "Open
decisions" section). Id stability is the shared prerequisite for both this
frontier and chat cursor persistence.

**First three steps in this repo:**
1. Inventory the exact `Session` API surface src/wm.rs consumes (pump, feed,
   snapshot, resize, ready, output_gen, …) — that seam is what must become a
   protocol. Record it against **foreman-architecture-contract**'s seam map.
2. Make Session/Project ids stable across restart (persisted allocation),
   clearing the chat-persistence blocker at the same time.
3. Spike a headless session-host process owning ONE PTY; the GUI reattaches
   over the existing Control-plane transport and proves screen restore via
   Snapshot.

**You have a result when:** you start a Session running a long-lived counter,
kill and restart the GUI, and the same Session reappears with its screen and
the **same child process PID** — verified by Snapshot before/after, no
screenshot required.

## 5. Fleet reliability floor (enabler, not glamour)

**Problem.** One Session's panic aborts the whole process (a panic unwinding
across the winit callback kills the app — that's why `install_panic_logger`
exists, src/main.rs). A fleet product cannot lose a whole desktop of Sessions to
one bad one. Every other frontier's demo dies on this.

**Designed 2026-06-18, unbuilt (probe: `rg -n 'catch_unwind' src/` — expect
nothing; do NOT probe for `READY_GRACE`, that string now appears in src/ready.rs
as a comment saying the constant is deliberately absent, so the grep passes
while the feature is missing)** — both designs are in
docs/followups-latency-and-control.md:
- **Per-Session panic isolation:** wrap each Session's `show()`/`pump()` in
  `std::panic::catch_unwind(AssertUnwindSafe(..))` in src/wm.rs; on panic,
  degrade that Win to an error tile instead of re-rendering.
- **READY_GRACE:** a Session whose program never answers the startup
  device-status query never latches Ready, so injected posts queue forever.
  Fallback: latch Ready by a generous timeout (~1.5–2 s), injectable so the
  path is deterministically testable. **Placement is already ruled:**
  src/ready.rs's module doc says READY_GRACE is intentionally *not* in the pure
  gate — the gate never reads the clock (all time is injected, same pattern as
  the Caret gate), and a grace timer needs one. It belongs in the caller that
  owns the clock, feeding the gate an injected `now`.

**Partial progress:** the *known* grid-index panic is now clamped in
the Frame plan seam (src/frame.rs, "the process-abort guard" — its module doc
carries the rationale). `catch_unwind` remains the belt-and-suspenders against
*unknown* panics. The other realized fleet-wide abort was the renderer, not a
Session: see the glow-over-wgpu battle in **foreman-failure-archaeology**.

**First three steps in this repo:** (1) the `catch_unwind` wrap per the
followups doc's approach; (2) an error-tile degraded state for a panicked Win;
(3) `READY_GRACE` with an injectable timeout + a deterministic test.

**You have a result when:** an induced panic inside one Session's `show()`
leaves every other Session running and interactive, the panicked Win shows an
error tile, and `foreman_panic.log` captured the cause.

## 6. FENCED: ConPTY reflow ownership — do not reopen

The resize+recall divergence is ConPTY's bug (microsoft/terminal #18725).
Foreman adopted #19535's cursor-only mitigation on 2026-07-09. What remains
fenced is **full buffer parity**: byte-level tracing proved all four
redraw-ownership combinations failed, and matching Windows Terminal requires
replicating conhost's `ResizeWithReflow` math byte-for-byte around
`alacritty_terminal`. That remains rejected on cost/benefit. Full evidence:
docs/conpty-resize-reflow.md; the investigation chronicle belongs to
**foreman-failure-archaeology**.

**Reopening full parity requires BOTH:** the user's explicit sign-off (via
**foreman-change-control**) AND accepting that exact byte-for-byte reflow
obligation, not another frontend redraw scheme. Cursor-sync package maintenance
does not reopen this fence.

---

## External positioning: novel vs known

| Cluster | Position | Precedents to beat |
|---------|----------|--------------------|
| Recursive same-engine compositor (one `WindowManager` at desktop level and nested as every Project's Content) | candidate-novel | i3/tmux nest *different* layers; same-engine recursion is the claim |
| In-terminal agent Control plane + Chat-room coordination (env-injected self-identity, Ready-gated injection, Outbox delivery) | candidate-novel | tmux control mode (`-CC`); no known agent-turn-aware equivalent |
| Headless inspection with settle semantics (send → Quiescence-settled Snapshot) | candidate-novel *in composition only* | tmux `send-keys`/`capture-pane`, `wezterm cli send-text`/`get-text` own the primitives |
| Tiling / tabs / Zoom / floating | table stakes | tmux, zellij, wezterm — claim nothing |

**Before claiming "first/only/novel" anywhere public:** run a written prior-art
sweep of at least tmux (control mode, capture-pane), wezterm (cli, mux-server),
zellij (actions/plugins), and current agent-orchestration tools, and file the
result in docs/. A novelty claim without that document is not permitted.

### Claim standards (binding)

- **Perf/behavior claims need dated, measured numbers + the harness to
  reproduce them.** Model: the 2026-06-18 render-latency measurements (idle
  ~0.13 ms/frame; 12 simultaneous max-rate floods ~8 ms avg — 
  docs/followups-latency-and-control.md, with its throwaway-harness recipe).
  Quote such numbers WITH their date; re-measure before any external use.
  Harnesses: **foreman-diagnostics-and-tooling**.
- **Reproducibility floor** (from the A/B spec § 6, generalized): every
  external claim ships with (a) the exact foreman commit, (b) held-constant
  config (model id, caps, fixture hashes), (c) the raw per-run records, and
  (d) ≥3 runs with medians + spread and the small-N caveat stated up front.
- **Agent-authored consensus docs are proposals, not decisions.** The mentions
  spec says it itself: "Treat the consensus as a strong proposal, not a
  human-approved decision"
  (docs/superpowers/specs/2026-06-10-chat-mentions-design.md). The same caveat
  covers docs/chat-missing-features.md and the chat-persistence debate output
  when positioning externally — cite them as design *inputs* unless the user
  has ratified them. (Internal do-not-re-litigate status is
  **foreman-change-control**'s domain.)

### Release readiness

Licensing is settled and shipped: dual MIT / Apache-2.0, `LICENSE-MIT` +
`LICENSE-APACHE` at the repo root, `license = "MIT OR Apache-2.0"` in
Cargo.toml, badge and License section in the README. This is no longer a
blocker; do not talk anyone out of shipping over it.

One caveat still bites external "tests pass" claims: `.github/workflows/release.yml`
runs `cargo test` on **tag pushes** (and on PRs that touch the workflow or
install.ps1), gated on the tag matching the Cargo.toml version — there is no
test gate on ordinary commits or ordinary PRs. So "CI is green" is a statement
about the last release tag, not about `main`. Quote it that way, or run the
suite yourself and say when. Adding a push/PR gate is a change — route through
**foreman-change-control**. Evidence-bar and result-lifecycle
discipline for research work is **foreman-research-methodology**; this skill
only says *what* is worth researching and *what* may be claimed.

## When NOT to use this skill

- Executing agent-state detection work → **foreman-agent-state-campaign** (the
  step-by-step campaign; this skill only frames the frontier).
- Turning a hunch into an accepted result, or recording a negative one →
  **foreman-research-methodology**.
- Why a dead end died → **foreman-failure-archaeology**.

## Provenance and maintenance

Every "still absent" claim in this skill is the perishable kind — a frontier
closes when someone builds it, and nothing here notices. Re-verify before
acting:

| Claim | Re-verify |
|-------|-----------|
| Inspection gaps still open | `rg -n -e '"--wait-for"' -e '"--since-seq"' -e '"--region"' -e '"--rows"' src/control.rs` (expect nothing) |
| seq/len landmine still present | `rg -n 'msgs.len\(\) as u64 \+ 1' src/chat.rs` |
| Chat persistence unbuilt | `rg -n -e 'std::fs' -e 'File::' src/chat.rs` (expect nothing) |
| Reliability floor unbuilt | `rg -n 'catch_unwind' src/` (expect nothing — `READY_GRACE` is a FALSE probe, it matches a comment in src/ready.rs) |
