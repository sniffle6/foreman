---
name: foreman-failure-archaeology
description: Use when tempted to re-diagnose or re-try a settled foreman battle, or when digging in this repo's git history - resize + Up-arrow prompt corruption, "double reflow", vsync-off latency theories, wgpu vs glow / GPU device loss, --await-ack, the 9-zone snap system / compose_zone, docs/snap-tiling.md describing a deleted zone system, the flaky human_post_appends chat test, blame pointing at daeda90 or 31a3db4, dangling stashes, the "@" commit subject, or the never-run chat A/B experiment.
---

# Foreman failure archaeology

The chronicle of every major investigation, dead end, rejected fix, revert, and
stall in this repo — each as symptom → wrong turns → root cause → evidence →
status — so nobody re-fights a settled battle. Plus: how to do git archaeology
in this specific repo without being misled by its history hazards.

Entries are cited by commit hash and symbol name, never by line number: the
hashes are immutable and the reasoning is what survives. All commands run
from the repo root `H:/claude code/foreman` in PowerShell 7+.

**Rule: before proposing a root cause or a fix for anything in the index
below, read its entry.** For live triage of a symptom you have *right now*,
start with **foreman-debugging-playbook** instead; this skill is the history
behind those triage rows. Settled verdicts are enforced by
**foreman-change-control** — this skill is the evidence record.

## Index of settled battles

| # | Battle | Verdict | Settled |
|---|--------|---------|---------|
| 1 | ConPTY resize/recall corruption | Cursor mitigation shipped 2026-07-09; full reflow still wontfix; `Ctrl+L` heals residuals | 2026-06-29 / 2026-07-09 |
| 2 | 9-zone snap system | Deleted; Layout tree replaced it | 2026-06-11 |
| 3 | `--await-ack` | Removed same day as built ("lying API surface") | 2026-06-11 |
| 4 | Selection rewrite | Doc described code that didn't exist for 3 weeks — until `b581240` built it | resolved 2026-07-02 |
| 5 | Flaky chat broadcast test | Pre-Ready swallow; fixed in 3 stages | 2026-06-27 |
| 6 | Input latency | 16 ms metronome, NOT vsync | 2026-06-18 |
| 7 | Chat-room A/B experiment | Planned in full, never executed | abandoned |
| 8 | Eaten chat posts under the passthrough ConPTY host | `ready` redefined: DSR answered AND first child paint | 2026-07-03 |
| 9 | GPU device loss aborting the whole app | Renderer switched wgpu → glow; wgpu FENCED | 2026-08-25 |

---

## 1. ConPTY resize/recall corruption — full reflow wontfix; cursor mitigated

ConPTY is Windows' pseudo-console layer: it hosts the child shell and
translates its screen into VT escape bytes that foreman's emulator
(`alacritty_terminal`) consumes. Domain mechanics: **terminal-emulation-reference**.

- **Symptom:** narrow a Session past a wrapped prompt, press Up (history
  recall) — the recalled line renders into the old wrapped prompt and stays
  corrupted until `Ctrl+L`. Triage row: **foreman-debugging-playbook**.
- **Wrong first diagnosis:** a "double reflow" in `Session::resize`
  (`term.resize()` + `master.resize()`). Explicitly disproven — do not
  re-derive it.
- **Disproof:** byte-level tracing of the ConPTY↔foreman stream plus an A/B
  against Windows Terminal. ConPTY's own resize repaint is clean, then its
  recall bytes place the cursor a wrapped-row-count too high (`ESC[23;22H`
  where the repaint put row 24). Foreman renders both faithfully — *the bytes
  disagree with each other*. Windows Terminal renders the identical scenario
  cleanly only because it bundles a newer ConPTY **and** replicates conhost's
  reflow math byte-for-byte.
- **Root cause:** ConPTY's internal reflow diverges from the hosting terminal's.
  The old build returned a cursor inconsistent with the host grid through
  `GetConsoleScreenBufferInfo(Ex)` — upstream
  [microsoft/terminal #18725](https://github.com/microsoft/terminal/issues/18725).
- **Four failed experiments** ("let ConPTY own the redraw"), built on a
  vendored `portable-pty` with `PSEUDOCONSOLE_RESIZE_QUIRK` dropped, an
  experimental grid-reset on resize, and a sideloaded NuGet ConPTY
  (`Microsoft.Windows.Console.ConPTY` 1.24.260512001):

  | # | Combination | Result |
  |---|-------------|--------|
  | 1 | ConPTY 1.24 stable + quirk on | no resize repaint; alacritty reflow diverges → gap |
  | 2 | ConPTY 1.24 stable + quirk off | still no resize repaint → same gap |
  | 3 | in-box ConPTY + quirk off + reflow | clean repaint, recall still misplaced → mashup |
  | 4 | in-box ConPTY + quirk off + grid-reset | recall still misplaced → mashup |

  No frontend redraw-ownership strategy can fix a cursor report that is
  inconsistent with ConPTY's own repaint.
- **2026-07-09 addendum:** #18725 is closed by #19535. Foreman now bundles
  official ConPTY 1.25.260512002-preview, which lazily requests the host cursor
  before a later screen-buffer query. Byte traces prove Foreman's existing
  `Event::PtyWrite` path returns the matching CPR. This improves cursor placement
  but cannot reconstruct dropped rows or erase stale PSReadLine text.
- **Status:** the full conhost-parity reflow remains settled wontfix on
  cost/benefit. `Ctrl+L` remains the residual repair. Reopening full buffer
  parity is a research-frontier question, not a routine bugfix:
  **foreman-research-frontier**.
- **Evidence trail:** `docs/conpty-resize-reflow.md` (full record), commit
  `5332757` (2026-06-29). The remote branch `origin/fix/terminal-reflow-on-resize`
  tips at `5332757`, which despite the branch name changes **no behavior**: a
  73-line doc, a 6-line pointer comment at `Session::resize`, a 5-line
  CLAUDE.md gotcha. The four experiments were **never committed anywhere**:
  `git log --all -S "PSEUDOCONSOLE_RESIZE_QUIRK"` matches only `5332757` (the
  doc text), and `Cargo.toml` pins crates.io `portable-pty` with
  no `[patch]`/vendored path (`rg -n portable-pty Cargo.toml`).

## 2. The 9-zone snap system — replaced in one evening (2026-06-11)

Foreman's first window model: a `Zone` enum with 9 variants
(`Max/Left/Right/Top/Bottom/Tl/Tr/Bl/Br`) snapping Wins to halves/quarters.

| When | Commit | Event |
|------|--------|-------|
| 2026-06-04 | `87464e4` | Initial commit ships snap zones (incl. hold-to-maximize) |
| 2026-06-05 | `e438a83` | `compose_zone` per-axis transition table: two perpendicular directions walk into a corner |
| 2026-06-11 | (epic banner) | User reverses the epic's §1 decision "we are NOT building a BSP tile tree" |
| 2026-06-11 21:46 | `4bbc55a` | The tree + floating-windows plan lands (deleted once it shipped; `git show 4bbc55a`) |
| 2026-06-11 21:59 | `daeda90` | Layout tree lands as a single squash (683-line `src/layout.rs`) — see history hazards |
| 2026-06-11 evening | `f3c76f0`..`0d714de`, `37687b5` | Tree wired into keyboard/move/split/resize; new Wins tile by default |
| 2026-06-11 22:43 | `31a9120` | "delete the 9-zone snap system" — `Zone`, `zone_rect`, `Win.snap`, `WindowManager.split` removed |
| 2026-06-11 | `b42e92e` | Docs supersession sweep: CLAUDE.md, HANDOFF.md, the tabbing epic, foreman.md updated; `docs/tiling-tree.md` added |

- **Status:** settled. The two-state (tiled/floating) Layout-tree model is the
  law; the zone system lived seven days. Do not re-propose zone snapping —
  **foreman-change-control**.
- **Trap, since defused:** the supersession sweep `b42e92e` **missed
  `docs/snap-tiling.md`**, which went on describing zone snapping in the
  present tense for over two months. `5ad4ee9` (2026-08-24) finally added the
  SUPERSEDED banner. The doc is still zone-era content — read it as decision
  history, never as a spec — but it now says so itself. The lesson survives the
  fix: a supersession sweep that updates *some* docs leaves the missed ones
  looking authoritative, and nothing in the repo notices.
- The reversal is recorded in the banner of
  `docs/epics/window-tabbing-split-epic.md` and referenced from
  `docs/tiling-tree.md` ("earlier floating-only decision recorded in the
  tabbing epic").

## 3. `--await-ack` — built and deleted the same day (2026-06-11)

The chat-handshake feature: a poster could pass `--await-ack` so the Control
plane would confirm a Member acknowledged a post.

| Commit | Date | Event |
|--------|------|-------|
| `4607001` | 2026-06-11 | Increment 1: wire protocol (`--re`/`--await-ack`), `(re #N)` rendering, `resolve_ack`/`AckState` state machine (tested, **not consumed by any server path**) |
| `a250e37` | 2026-06-11 | Increment 2 foundation: `Session.ready` latch on first DSR reply flushed (chose first-reply-flush over "DSR + output idle" — a strict idle never fires for a streaming agent) |
| `9bb3a35` | 2026-06-11 | Removal: `--await-ack` flag, `ChatRequest.expect_ack`, `AckState`/`resolve_ack` deleted |

- **Why removed:** the flag was accepted by the CLI but nothing consumed it —
  in the contract doc's words, "an accepted-but-inert `--await-ack` flag was a
  lying API surface." Live skill testing showed the eaten-post window is
  mitigated by the dispatch-then-pause rule plus a watching dispatcher.
- **Kept and still live:** `--re N` threading, `OpenReply.seq`, the
  `Session.ready` latch.
- **Restart point** (if unattended fleets ever need self-healing handoffs):
  `docs/contracts/chat-handshake-remaining-work.md`. The recovery commit
  `4607001` is recorded twice — in that contract doc and in
  `docs/chat-missing-features.md`.
- **Sequel:** Part 1 of the remaining work (delivery cursor + catch-up replay)
  was built for real on 2026-06-27 (`9aeb72b`; `docs/chat-delivery.md`) — see
  battle 5. Part 2 (ack registry, timeout notice, Crew board badge) was still
  deferred as of 2026-07-01; check the contract doc before assuming that holds.
- **Lesson:** an accepted-but-ignored flag is worse than no flag. Current CLI
  ground truth lives in **foreman-run-and-operate**.

## 4. Selection — the rewrite that existed only in its doc (resolved 2026-07-02)

- **v1 (initial commit `87464e4`):** hand-rolled selection storing two
  `(row, col)` tuples in screen/viewport coordinates. Recorded failure mode:
  scroll the scrollback and the highlight stays pinned to fixed screen rows
  while the text moves, and the copy path used the *current* scroll offset, so
  highlighted cells and copied cells disagreed.
- **The documented rewrite:** `docs/terminal-selection.md` (committed
  `64a80b9`, 2026-06-12) describes replacing v1 with `alacritty_terminal`'s
  `Selection` module in absolute buffer coordinates, with the lesson "Never
  store screen rows — that was the original bug."
- **The drift window (2026-06-12 → 2026-07-02):** for three weeks that code
  existed **on no branch**. `git log --all -S "Selection::new"` matched only
  the doc commit itself. The tree plan records why: the doc was "untracked
  pending the selection-rewrite recovery"
  (the tree + floating-windows plan) — the
  rewrite lived in a working tree that was lost; only the doc was recovered.
  (That plan has since been deleted — read it back with `git show 4bbc55a`.)
  Throughout the window the real code stayed hand-rolled
  `sel_anchor`/`sel_head: Option<(usize, usize)>` viewport cells, copied by a
  `selection_text` that converted with the *current* `display_offset`.
- **Resolution (`b581240`, 2026-07-02):** the rewrite finally landed —
  selection migrated onto `alacritty_terminal`'s buffer-coordinate `Selection`
  (`Selection::new`/`update`, `to_range`, `selection_to_string`), multi-click
  Semantic/Lines and CJK handled; `sel_anchor`/`sel_head`/`selection_text`
  deleted. The authority flip is the lesson: `docs/terminal-selection.md` went
  from fiction to accurate, while `docs/HANDOFF.md`'s module map
  (`sel_anchor`/`sel_head`/`selection_text`/`cell_at`) went from right to
  stale on the same day. Authority labels don't track freshness; only code does.
- **Status:** resolved. Current selection questions belong to
  **terminal-emulation-reference** (updated with the migration).

## 5. Flaky chat broadcast test → the pre-Ready swallow (2026-06-11 → 2026-06-27)

Background: at startup a shell/agent sends a DSR (Device Status Report,
`ESC[6n`) and blocks until the terminal replies; a Session is **Ready** only
after foreman's reply is flushed. Bytes injected before Ready get eaten by the
child's device-status scan. Details: **terminal-emulation-reference**.

- **Symptom:** `human_post_appends_with_reserved_id_and_broadcasts_to_all_members`
  failed in nearly every full parallel `cargo test` run, passed in isolation.
- **Root cause** (diagnosed in `docs/plans/2026-06-11-fix-flaky-chat-broadcast-test.md`):
  the test injected the post **once**, immediately after spawn — pre-Ready. In
  isolation the deferred 150 ms submit `\r` landed post-Ready and rescued it;
  under full-suite load (dozens of concurrent conhost spawns) DSR resolved
  late, *both* writes were eaten, and nothing was ever re-sent. Serializing
  the suite would only have hidden the race.
- **Fix, stage 1 (test-only):** `23446e5` (2026-06-11) — pump every Session
  and re-send the broadcast every iteration until seen ("re-send-until-seen").
- **Fix, stage 2 (production twin):** the same swallow existed live — a post
  to a just-Dispatched Member could be eaten. `6ad7f64` (2026-06-18) holds
  injected input until the Session is past startup (queue until Ready).
- **Fix, stage 3 (structural):** the 2026-06-27 delivery-cursor sweep
  (`9aeb72b`; `docs/chat-delivery.md`) — per-Member delivery cursors +
  catch-up replay, gated on `Session::ready()`. It **deleted** the production
  `chat_broadcast`/`chat_broadcast_in` push entirely (`rg -n 'fn chat_broadcast'
  src/wm.rs` should still find test fns only). This per-frame
  delivery decision is today's Outbox seam.
- **Status:** settled. If a chat delivery test flakes now, suspect a new bug,
  not this one. Flaky policy: **foreman-validation-and-qa**.

## 6. Latency investigation — vsync was innocent (2026-06-18)

- **Symptom:** typing/echo felt slow; suspicion initially fell on vsync.
- **Negative result (recorded, don't re-try):** turning vsync off did **not**
  help — typing feel identical off vs on. Left at default (on) to avoid
  tearing and heavy-output GPU spin
  (`docs/followups-latency-and-control.md`, "Decisions & known edges").
  Negative-result discipline: **foreman-research-methodology**.
- **Real cause:** the app repainted on a fixed `request_repaint_after(16ms)`
  metronome, and **Windows' ~15.6 ms default timer granularity floors any
  shorter request** — every keystroke waited ~16–32 ms for a tick.
- **Fix:** event-driven wakes (PTY reader threads, the Control plane's
  `serve()`, winit input all wake the loop) + adaptive cadence (hot tick for
  250 ms after activity, 100 ms idle backstop). Shipped 2026-06-18 as
  `1accc46`, `f3e64ba`, `ce61f02`, `15f675f` (+ `6ad7f64`, battle 5).
- **Measured numbers (dated facts, 2026-06-18):** idle ~0.13 ms/frame; one
  max-rate flood ~0.8 ms; 12 simultaneous max-rate floods ~8 ms avg / 11 ms
  max — still 60 fps. Render is **parse-bound, not draw-bound**; cost scales
  per actively-outputting Session, not per visible cell. Re-measure with the
  harness pattern in **foreman-diagnostics-and-tooling** before citing these
  as current.
- **Status:** settled. The pre-identified next lever (only if ~20+
  continuously-noisy agents become a target) is bounded per-frame parsing —
  designed-not-built.

## 7. Never executed: the chat-room A/B experiment (abandoned without marker)

- A complete implementation plan
  (`docs/superpowers/plans/2026-06-15-chat-room-ab-experiment.md`) plus a
  design spec for a 3-arm experiment (solo / team-no-chat /
  team+chat) measuring whether the Chat room improves result quality and
  token efficiency.
- **No part was ever built.** The prerequisites do not exist —
  `rg -e to_jsonl -e persist_chat_msg src/` finds nothing, and there is no
  `docs/chat-ab-results.md`. (Do not use "no `scripts/` dir" as the tell any
  more: `scripts/` exists now, for unrelated reasons.)
- Both files are dated 2026-06-15 (inside a commit-activity gap) but were only
  committed 2026-06-29 inside the consolidation commit `31a3db4` — the plan
  carries **no status marker** saying it was abandoned.
- **Status:** abandoned-without-marker; explicitly a revival candidate — see
  **foreman-research-frontier** (ownership of the open problem) and
  **foreman-research-methodology** (evidence bar for running it).

## 8. Eaten chat posts under the passthrough ConPTY host (settled 2026-07-03)

- **Symptom:** the wm chat tests red, consistently (not flaky):
  `chat_broadcast_hits_members_only_excluding_sender`,
  `chat_broadcast_reaches_background_member_tab_not_foreground_shell`,
  `chat_post_replies_ok_then_broadcasts`,
  `chat_targeted_broadcast_hits_only_the_target`,
  `human_post_appends_with_reserved_id_and_broadcasts_to_all_members`. A
  broadcast to a `ready()` member never reached its PTY (`cmd /c pause`
  members never exited). The live app was equally affected: posts to a
  freshly-dispatched member silently vanished.
- **Wrong turns (exonerated by experiment — do not re-test):** conpty pair in
  deps, orphan swarm, ConPTY slowness, the session-tree-kill branch changes,
  cmd AutoRun, test parallelism, and the prime-suspect commit `9aeb72b` (the
  delivery-cursor sweep) — the whole chat model was proven innocent by
  instrumentation, no bisect needed. A prior session's "410 green at
  `29fd6bb`" claim could not be reproduced; the mechanism, not the exact
  first-red date, is the settled fact.
- **Root cause:** since `405dc55` (vendored OpenConsole passthrough host for
  kitty graphics), the startup DSR that latches `Session.ready` comes from
  the HOST, microseconds after spawn — seconds before the child's input path
  opens. `ready` stopped meaning "injection is safe": the boot window ate the
  chat paste + deferred `\r`, and `ChatRoom::tick` had already advanced the
  delivery cursor (it advances even when nothing is addressed), so the post
  was never re-sent. Battle 5's pre-Ready swallow, resurrected by the host
  redefining what the signal observes.
- **Evidence (instrumented single-test runs, 2026-07-03):** tick returned the
  delivery, the wm matched the terminal, the paste was written with
  `ready=true` — yet the child's prompt first rendered **2.57s after** the
  post. rx timeline: host chrome (`ESC[1t`, `ESC[6n ESC[c ESC[?1004h
  ESC[?9001h`, `ESC[1;1H`) all inside 11ms, ready latched at +373µs; first
  child ink ("Press any key…") at +3.0s. Single-variable confirmation: delay
  only the post until both children painted → the identical delivery path
  went green in 3.1s.
- **Fix:** `ready` = DSR answered AND first visible glyph in the PTY output.
  The glyph detector is `InkScan` (now in src/ready.rs alongside `ReadyGate`),
  a pure cross-chunk scanner that skips escape/control sequences.
  Grid-sniffing was rejected:
  `inject_note` paints the dispatch banner into the grid without any child
  output, so "grid non-empty" false-latches in the real app. Regression
  contract test: `ready_waits_for_the_childs_first_paint`.
- **Status:** settled. If chat posts vanish again, first ask what `ready`
  observes (who answers the DSR, when the child actually paints) before
  re-litigating the outbox — the delivery model has now been proven correct
  twice. Known edge: a child that never prints anything never latches ready;
  posts queue instead of being eaten (READY_GRACE remains the designed
  remedy — **foreman-agent-state-campaign** Phase 0).

## 9. GPU device loss killed the app — the renderer, not the geometry (2026-08-25)

- **Symptom:** foreman vanished with no window, no dialog, repeatedly, mostly
  after sleep or a display power transition. `%APPDATA%\foreman\foreman_panic.log`
  held the evidence (GH #2). Triage row: **foreman-debugging-playbook**.
- **Wrong first diagnosis (do not re-derive):** "too much terminal geometry per
  frame" — the panic message quotes an index-buffer size, so it reads like a
  capacity problem. The 11th recorded panic killed that theory outright: a
  **173 KB** allocation failed against a **1.9 MB** buffer with 11× headroom, on
  a frame carrying a *quarter* of the geometry of the smallest previous crash.
  Size was never the variable. (foreman only meshes the viewport anyway —
  `frame.rs::plan_paint` bounds rows to the screen — so scrollback depth cannot
  move the index count.)
- **Root cause:** `egui-wgpu`'s `update_buffers` responds to device loss with an
  unconditional `panic!`. `Queue::write_buffer_with` returns `None` *only* on
  device loss (every other error class goes to wgpu's own error handler), so the
  `None` is unambiguous — and the panic unwinds across the winit callback and
  aborts the process, taking every PTY with it. `egui_glow`'s entire reaction to
  a lost context, `GL_CONTEXT_LOST` included, is a `log::error!`.
- **Ruled out:** upgrading is not a fix — the same panic is present in
  `egui-wgpu` 0.36.1, the current release. "Just restart on device loss" was
  rejected too: one recorded crash had no Power-Troubleshooter or Kernel-Power
  event at all, so brief GPU power transitions would fire the restart at
  unpredictable moments during normal use.
- **Disproof method — a concurrent side-by-side A/B.** Both builds were launched
  **at the same time on the same laptop** and put through one device-loss event:
  the wgpu build took its 11th panic, the glow build kept rendering terminals
  correctly. Concurrency is what makes it a result rather than an anecdote — the
  crash only fires on roughly two thirds of transitions, so a lone glow survival
  would have been indistinguishable from luck. The wgpu process is its own
  positive control.
- **Fix (`ba803ef`):** one dependency line. `eframe` with
  `default-features = false` plus the `glow` feature. `default-features = false`
  is load-bearing, not cosmetic — eframe prefers wgpu whenever both backends are
  enabled. foreman is renderer-agnostic (every texture goes through
  `egui::ColorImage` + `ctx.load_texture`), so the only source change was
  `App::on_exit`, whose signature eframe `cfg`s on the renderer feature.
- **The road not taken:** branch `wip/wgpu-device-loss-fix` holds a working
  alternative — a `[patch.crates-io]` fork of `egui-wgpu` swapping the panic for
  a sticky flag, plus `src/gpu.rs` with a crash-loop guard and an ordered
  save-and-respawn. Rejected because **it never saved the agents**: every
  `Session` owns a `KILL_ON_JOB_CLOSE` job (`src/job.rs`), so PTY children die
  with the process whether it exits cleanly or panics — an ordered restart is
  still a restart. It also pinned a vendored fork to one `egui-wgpu` version,
  taxing every future egui bump. Kept in case glow ever proves worse.
- **Status:** settled; the fence is in CLAUDE.md ("do not *modernize* the
  `eframe` line back to wgpu"). Reopening requires re-running the concurrent
  side-by-side, not an argument. Note glow is panic-*rarer*, not panic-free:
  eframe's `change_gl_context` unwraps `make_current()` every frame on Windows,
  and eframe requests a non-robust GL context so a reset raises no
  `GL_CONTEXT_LOST` at all — if foreman ever wakes up visibly *corrupt* rather
  than crashed, start there. `NativeOptions::vsync` also changes meaning: a
  no-op under wgpu, live under glow (see battle 6 before re-testing it).
- **Evidence trail:** `docs/gpu-device-loss.md` (full record incl. the A/B and
  the rejected branch), commit `ba803ef`, GH #2.

---

## Meta-failures: the docs-drift chronicle

**The lesson: verify against code, not docs.** Authority labels do not track
freshness — HANDOFF.md is *authoritative by declaration* (CLAUDE.md says it wins
conflicts) and has still drifted in places, while on selection it was the doc
that was right and HANDOFF that was stale. Only code is current.

A tabulated inventory of *which* artifacts were drifted used to live here. It
was deleted on purpose: a drift table rots faster than the drift it records, and
a stale entry pointing at a doc that has since been fixed sends people to argue
with a corrected file. Date the drift yourself (`git log --oneline -- <path>`)
and check the code. The living trust map and the fix conventions belong to
**foreman-docs-and-writing**.

One row is kept because it is the incident that taught the lesson:

- **`--settle-ms` "parsed but not yet honored" (drift window ~2026-06-26 →
  2026-07-xx; resolved).** `src/control.rs`'s doc comments and `foreman send
  --help` announced the flag as inert while `src/wm.rs` was already consuming
  it — a settle path driven by `advance_settles`/`settle_tick` off a
  `PendingSettle`, under a `MAX_SETTLE_MS` hard cap, with the per-send default
  now coming from `Settings::send_settle_ms` in `src/config.rs`. Both the code
  comments and `docs/terminal-inspection.md` have since been corrected. The bite
  was not the wrong doc; it was that a *self-deprecating* doc is trusted harder
  than a boastful one — nobody re-checks a feature the docs say isn't finished.
  The frozen constant names in the original entry (`DEFAULT_SETTLE_MS`, since
  deleted) are exactly the kind of citation this skill no longer writes.

## History hazards: blame and bisect traps

| Hazard | What happened | Why it misleads |
|--------|---------------|-----------------|
| `daeda90` squash | Exact squash of a discarded 5-commit worktree chain (`9e9418c`→`902ae38`→`6f24d0d`→`47d0d75`→`37a01dc`, now dangling). Proof: `git rev-parse daeda90^{tree} 37a01dc^{tree}` → both `e8aa459...` | `git blame` on the first 683 lines of `src/layout.rs` points at one squash; the real step-by-step authorship exists only in the dangling chain |
| `31a3db4` consolidation | 12 files, +2080/−164 across caret/chat/config/control/icons/input/inspect/proc/terminal/wm **plus** two 06-15-dated docs, under a `feat(tabs)` subject; body admits "Also consolidates in-progress work across ... modules" | ~2 weeks of multi-module WIP under one tabs-flavored subject — blame or bisect landing here tells you almost nothing about intent |
| `37687b5` | First message line is literally `@` (PowerShell here-string leak); the real subject is line 2: "feat(wm): new terminals and projects tile by default" | Subject greps and changelog tools misread it. Commit-message conventions: **foreman-docs-and-writing** |
| Few merge commits | Run `git log --oneline --merges` first — the count grows, but history stays overwhelmingly linear | `git log --first-parent main` ≈ the full mainline; first-parent reading rarely hides anything |
| Almost no file deletions | `git log --diff-filter=D --oneline --all` — re-run it, it is a one-line check. The 2026-08-25 plan purge under `docs/superpowers/plans/` is the bulk of what it returns; **no `src/` file has ever been deleted** (`git log --diff-filter=D --oneline --all -- 'src/*'` is the check that matters for code) | Dead code dies by in-file edits, not by removing files — hunt lifetimes with `-S`, not `--follow` on deleted paths. For a plan that vanished, `--diff-filter=D --name-only` names the removing commit and `git show <commit>^:<path>` reads it back |
| Commit-activity gaps | No commits (any ref) 06-06..06-08, 06-13..06-17, 06-19..06-25 | Gaps ≠ idle: the A/B plan is dated 06-15, mid-gap, committed 06-29. Look for later consolidation commits and dangling objects |
| Plan/spec dates ≠ commit dates | `docs/superpowers/` filenames carry authoring dates; several were committed much later | Date a decision by the filename *and* `git log --all -- <path>` (use `--all` and keep the path even if the file is gone — deleted plans still answer) |

**Notable dangling objects.** Enumerate the current set yourself —
`git fsck --no-reflogs 2>$null | Select-String "dangling commit"` — the list
grows with every discarded worktree and dropped stash. Two carry a judgment
worth not re-deriving:

| Hash | What it is | Status |
|------|-----------|--------|
| `37a01dc` | Tip of the discarded layout-tree worktree chain | Fully recovered by the `daeda90` squash — **proven** tree-identical (`git rev-parse daeda90^{tree} 37a01dc^{tree}` → same hash), so blame is recoverable there |
| `30346e9` | Stash-form "On feat/terminal-input-and-inspection: chat-cursor-wip" (2026-06-27 15:53; chat.rs/main.rs/wm.rs) | Dropped stash; the ChatRoom/delivery work landed 17:45 the same day (`9aeb72b`), but **whether the stash was fully superseded is unverified — possibly lost work** |

Dangling objects are unreachable and get pruned by `git gc` eventually; if you
need one, inspect it soon (`git show --stat <hash>`) or tag it.

## Git archaeology in this repo: how-to

All read-only; PowerShell 7+ from the repo root.

```powershell
# Lifetime of a symbol (birth/death commits). Worked example — compose_zone:
git log --all -S "compose_zone" --format="%h %ad %s" --date=short
#  e438a83 2026-06-05  birth (corner state machine)
#  4bbc55a 2026-06-11  plan mentions it
#  f3c76f0 2026-06-11  code death (tree commands replace it)
#  b42e92e 2026-06-11  doc-text mention

# When did a doc last change (drift dating). Worked example: this is how you
# find that snap-tiling.md's SUPERSEDED banner arrived long after the code did.
git log --oneline -- docs/snap-tiling.md

# Deleted files (returns NOTHING in this repo — that's the finding):
git log --diff-filter=D --oneline --all

# Dangling commits (discarded chains, dropped stashes):
git fsck --no-reflogs 2>$null | Select-String "dangling commit"
git show --stat <hash>                 # what it touched
git diff <hash>^1 <hash>               # stash-form commits are merges; diff vs first parent
git show <hash>:src/chat.rs            # read a file as it was in the lost commit

# Prove/refute "commit A is a squash of chain tip B":
git rev-parse <A>^{tree} <B>^{tree}    # identical hashes = identical content

# Mainline reading (history is near-linear; check with `git log --oneline --merges`):
git log --first-parent --oneline main

# Line-range history when blame dead-ends at daeda90/31a3db4:
git log -L 1,40:src/layout.rs          # follows the range through history
git log --all -S "exact code string"   # or pivot to string lifetime
```

**Blame discipline:** when `git blame` answers `daeda90` or `31a3db4`, you
have learned nothing — pivot to `-S`/`-L`, the dated spec under
`docs/superpowers/specs/` (or the plan of that era, which lives in history now:
`git log --diff-filter=D --name-only -- docs/superpowers/plans/`), and (for
`daeda90`) the dangling chain.

**Adding an entry:** when a battle settles (fix rejected, decision reversed,
investigation closed), append it here in the same symptom → wrong turns →
root cause → evidence → status shape, with hashes and dates. That is what
makes the verdict enforceable by **foreman-change-control**.

## When NOT to use this skill

- **You have a live symptom to triage now** → **foreman-debugging-playbook**
  (symptom → discriminating experiment tables). This skill is the history
  behind those rows, not the triage itself.
- **You want to change or re-litigate a settled decision** →
  **foreman-change-control** (the gate; this skill is its evidence annex).

## Provenance and maintenance

Verdicts and commit hashes here are permanent. What perishes is the "still
true today" half of an entry, so re-check only those:

| Claim | Re-verify with |
|-------|----------------|
| Battle 1: the four ConPTY experiments were never committed anywhere | `git log --all -S "PSEUDOCONSOLE_RESIZE_QUIRK"` → only `5332757`; `rg -n portable-pty Cargo.toml` (crates.io pin, no `[patch]`) |
| Battle 9: the renderer is still glow | `rg -n -A6 '^eframe = ' Cargo.toml` → `default-features = false` **and** the `glow` feature. Losing either silently restores the aborting wgpu path |
| Dangling objects (gc prunes them; `30346e9` may be real lost work) | the `git fsck` command below the table |
| Latency numbers (idle ~0.13 ms/frame etc.) | dated 2026-06-18; re-measure via **foreman-diagnostics-and-tooling** before quoting |

Dangling-object sweep (the pipe cannot live in a table cell — copy it from
here; PowerShell):

```powershell
git fsck --no-reflogs 2>$null | Select-String "dangling commit"
```

**A note on this table's own history.** It used to be three times this long, and
several of its rows had rotted into *false passes* — greps for symbols that had
since been deleted, which therefore "confirmed" a claim by matching nothing, or
`Test-Path` probes whose premise the repo had already outgrown. A self-check
that cannot fail is worse than no self-check. If you add a row, make sure a
change in the code can actually make it report red.
