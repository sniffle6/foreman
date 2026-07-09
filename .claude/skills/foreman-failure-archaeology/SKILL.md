---
name: foreman-failure-archaeology
description: Use when tempted to re-diagnose or re-try a settled foreman battle, or when digging in this repo's git history - resize + Up-arrow prompt corruption, "double reflow", vsync-off latency theories, --await-ack, the 9-zone snap system / compose_zone, docs/snap-tiling.md or docs/terminal-selection.md disagreeing with code, the flaky human_post_appends chat test, blame pointing at daeda90 or 31a3db4, dangling stashes, the "@" commit subject, or the never-run chat A/B experiment.
---

# Foreman failure archaeology

The chronicle of every major investigation, dead end, rejected fix, revert, and
stall in this repo — each as symptom → wrong turns → root cause → evidence →
status — so nobody re-fights a settled battle. Plus: how to do git archaeology
in this specific repo without being misled by its history hazards.

Baseline: committed HEAD `7fda1c2` on `main`, 2026-07-01. Every line-number
citation below is against that commit and is date-stamped. All commands run
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
  doc text), and `Cargo.toml:12` pins crates.io `portable-pty = "0.9.0"` with
  no `[patch]`/vendored path (as of 2026-07-01).

## 2. The 9-zone snap system — replaced in one evening (2026-06-11)

Foreman's first window model: a `Zone` enum with 9 variants
(`Max/Left/Right/Top/Bottom/Tl/Tr/Bl/Br`) snapping Wins to halves/quarters.

| When | Commit | Event |
|------|--------|-------|
| 2026-06-04 | `87464e4` | Initial commit ships snap zones (incl. hold-to-maximize) |
| 2026-06-05 | `e438a83` | `compose_zone` per-axis transition table: two perpendicular directions walk into a corner |
| 2026-06-11 | (epic banner) | User reverses the epic's §1 decision "we are NOT building a BSP tile tree" |
| 2026-06-11 21:46 | `4bbc55a` | 1433-line plan `docs/superpowers/plans/2026-06-11-tree-floating-windows.md` |
| 2026-06-11 21:59 | `daeda90` | Layout tree lands as a single squash (683-line `src/layout.rs`) — see history hazards |
| 2026-06-11 evening | `f3c76f0`..`0d714de`, `37687b5` | Tree wired into keyboard/move/split/resize; new Wins tile by default |
| 2026-06-11 22:43 | `31a9120` | "delete the 9-zone snap system" — `Zone`, `zone_rect`, `Win.snap`, `WindowManager.split` removed |
| 2026-06-11 | `b42e92e` | Docs supersession sweep: CLAUDE.md, HANDOFF.md, the tabbing epic, foreman.md updated; `docs/tiling-tree.md` added |

- **Status:** settled. The two-state (tiled/floating) Layout-tree model is the
  law; the zone system lived seven days. Do not re-propose zone snapping —
  **foreman-change-control**.
- **Trap:** the supersession sweep `b42e92e` touched 5 files but **missed
  `docs/snap-tiling.md`**, which still describes zone snapping in the present
  tense with **no supersession banner** (as of 2026-07-01). It is a trap doc —
  see the meta-failures table below.
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
  `4607001` is recorded twice — in that contract doc and at
  `docs/chat-missing-features.md:60` (as of 2026-07-01).
- **Sequel:** Part 1 of the remaining work (delivery cursor + catch-up replay)
  was built for real on 2026-06-27 (`9aeb72b`; `docs/chat-delivery.md`) — see
  battle 5. Part 2 (ack registry, timeout notice, Crew board badge) remains
  deferred (as of 2026-07-01).
- **Lesson:** an accepted-but-ignored flag is worse than no flag. Current CLI
  ground truth lives in **foreman-run-and-operate**.

## 4. Selection — the rewrite that exists only in its doc (open drift)

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
  (`docs/superpowers/plans/2026-06-11-tree-floating-windows.md:27`) — the
  rewrite lived in a working tree that was lost; only the doc was recovered.
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
  `chat_broadcast`/`chat_broadcast_in` push entirely (only test fns named
  `chat_broadcast_*` remain in `src/wm.rs`, as of 2026-07-01). This per-frame
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

- A complete 1393-line implementation plan
  (`docs/superpowers/plans/2026-06-15-chat-room-ab-experiment.md`) plus a
  187-line design spec for a 3-arm experiment (solo / team-no-chat /
  team+chat) measuring whether the Chat room improves result quality and
  token efficiency.
- **Verified as of 2026-07-01: no part was ever built.** The prerequisites do
  not exist — no `ChatMsg::to_jsonl`, no `persist_chat_msg` anywhere in
  `src/`, no `scripts/` directory at the repo root.
- Both files are dated 2026-06-15 (inside a commit-activity gap) but were only
  committed 2026-06-29 inside the consolidation commit `31a3db4` — the plan
  carries **no status marker** saying it was abandoned.
- **Status:** abandoned-without-marker; explicitly a revival candidate — see
  **foreman-research-frontier** (ownership of the open problem) and
  **foreman-research-methodology** (evidence bar for running it).

## 8. Eaten chat posts under the passthrough ConPTY host (settled 2026-07-03)

- **Symptom:** 5 wm chat tests red, consistently (not flaky):
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
  The glyph detector is `InkScan` (src/terminal.rs), a pure cross-chunk
  scanner that skips escape/control sequences. Grid-sniffing was rejected:
  `inject_note` paints the dispatch banner into the grid without any child
  output, so "grid non-empty" false-latches in the real app. Regression
  contract test: `ready_waits_for_the_childs_first_paint`.
- **Status:** settled. If chat posts vanish again, first ask what `ready`
  observes (who answers the DSR, when the child actually paints) before
  re-litigating the outbox — the delivery model has now been proven correct
  twice. Known edge: a child that never prints anything never latches ready;
  posts queue instead of being eaten (READY_GRACE remains the designed
  remedy — **foreman-agent-state-campaign** Phase 0).

---

## Meta-failures: the docs-drift chronicle

Every entry below is a *verified* instance (2026-07-01, HEAD `7fda1c2`) where
a repo artifact asserted something the code contradicted. The single lesson of
this section: **verify against code, not docs.** The living trust map and the
fix conventions belong to **foreman-docs-and-writing**; this table is the
incident record.

| Artifact | What it says | Verified truth | Evidence |
|----------|--------------|----------------|----------|
| `docs/snap-tiling.md` | Zone snapping, present tense, no banner | Zones deleted `31a9120` (2026-06-11); the supersession sweep `b42e92e` updated 5 files and missed this one | `git show --stat b42e92e` |
| `docs/terminal-selection.md` § How to use | alacritty `Selection` API in use | Was fiction for 3 weeks (no commit had it); TRUE since `b581240` (2026-07-02) — HANDOFF's module map is now the stale one on selection | battle 4 |
| `src/control.rs:130,548,763` + `foreman send --help` | "`--settle-ms` ... not yet honored (settle is the next phase)" | Honored: `DEFAULT_SETTLE_MS = 120`, `MAX_SETTLE_MS = 4000` (`src/wm.rs:17-18`), consumed at `src/wm.rs:938`, driven by `advance_settles` — the Quiescence settle works | `rg settle src/wm.rs` |
| `docs/HANDOFF.md` § Coordinate model | "Snap/dwell use `ui.ctx().pointer_latest_pos()`..." | No `dwell`/`snap_or_tab` anywhere in `src/wm.rs` | `rg -e dwell -e snap_or_tab src/wm.rs` |
| `docs/epics/window-tabbing-split-epic.md` | "**Status:** designed, not started" | Phase 1 tab-stacks shipped (its own 2026-06-11 banner admits it); browser-style tabs shipped `d4da700`/`31a3db4` | header vs git log |
| `CLAUDE.md` § Session Context | "Currently working on `feat/browser-style-tabs`"; "check MEMORY.md in that directory" | Checkout is on `main`; that branch's tip `31a3db4` is an ancestor of `main`. The memory directory was missing until 2026-07-02, when MEMORY.md was first written there — the branch claim is still stale | `git merge-base --is-ancestor`; `Test-Path` |
| `.claude/agents/foreman-reviewer.md` (pre-2026-07-02) | "Split (`Alt+WASD`) snaps a new terminal to a zone" | Zones deleted 2026-06-11; the reviewer agent taught a dead system for 3 weeks until its 2026-07-02 rework | `rg 'snaps a new terminal to a zone' .claude/agents/` (now empty) |

Meta-lesson: HANDOFF.md is *authoritative by declaration* (CLAUDE.md says it
wins conflicts) yet has drifted in places itself — while on selection it is
the doc that's *right*. Authority labels don't track freshness; only code does.

## History hazards: blame and bisect traps

| Hazard | What happened | Why it misleads |
|--------|---------------|-----------------|
| `daeda90` squash | Exact squash of a discarded 5-commit worktree chain (`9e9418c`→`902ae38`→`6f24d0d`→`47d0d75`→`37a01dc`, now dangling). Proof: `git rev-parse daeda90^{tree} 37a01dc^{tree}` → both `e8aa459...` | `git blame` on the first 683 lines of `src/layout.rs` points at one squash; the real step-by-step authorship exists only in the dangling chain |
| `31a3db4` consolidation | 12 files, +2080/−164 across caret/chat/config/control/icons/input/inspect/proc/terminal/wm **plus** two 06-15-dated docs, under a `feat(tabs)` subject; body admits "Also consolidates in-progress work across ... modules" | ~2 weeks of multi-module WIP under one tabs-flavored subject — blame or bisect landing here tells you almost nothing about intent |
| `37687b5` | First message line is literally `@` (PowerShell here-string leak); the real subject is line 2: "feat(wm): new terminals and projects tile by default" | Subject greps and changelog tools misread it. Commit-message conventions: **foreman-docs-and-writing** |
| Only 2 merge commits | `970e8c9`, `3588634` (as of 2026-07-01) | History is otherwise linear; `git log --first-parent main` ≈ full mainline |
| Zero file deletions | `git log --diff-filter=D --oneline --all` returns **nothing** (as of 2026-07-01) | Every file ever added still exists; dead code dies by in-file edits — hunt lifetimes with `-S`, not `--follow` on deleted paths |
| Commit-activity gaps | No commits (any ref) 06-06..06-08, 06-13..06-17, 06-19..06-25 | Gaps ≠ idle: the A/B plan is dated 06-15, mid-gap, committed 06-29. Look for later consolidation commits and dangling objects |
| Plan dates ≠ commit dates | `docs/superpowers/plans/*` filenames carry authoring dates; several were committed much later | Date a decision by the plan filename *and* `git log -- <path>` |

**Dangling-object inventory** (`git fsck --no-reflogs`, as of 2026-07-01 — 4
dangling commits):

| Hash | What it is | Status |
|------|-----------|--------|
| `37a01dc` | Tip of the discarded layout-tree worktree chain | Fully recovered by the `daeda90` squash (tree-identical) |
| `ad12875` | Orphan twin of plan commit `4bbc55a` (same subject + author timestamp 2026-06-11 21:46:52) | Superseded duplicate |
| `880fc35` | Stash-form "WIP on feature/tiling-tree" (2026-06-11, `src/wm.rs`) | Dropped stash; likely superseded same evening |
| `30346e9` | Stash-form "On feat/terminal-input-and-inspection: chat-cursor-wip" (2026-06-27 15:53; chat.rs/main.rs/wm.rs, +141/−103) | Dropped stash; the ChatRoom/delivery work landed 17:45 the same day (`9aeb72b`), but **whether the stash was fully superseded is unverified — possibly lost work** |

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

# When did a doc last change (drift dating):
git log --oneline -- docs/snap-tiling.md

# Deleted files (returns NOTHING in this repo as of 2026-07-01 — that's the finding):
git log --diff-filter=D --oneline --all

# Dangling commits (discarded chains, dropped stashes):
git fsck --no-reflogs 2>$null | Select-String "dangling commit"
git show --stat <hash>                 # what it touched
git diff <hash>^1 <hash>               # stash-form commits are merges; diff vs first parent
git show <hash>:src/chat.rs            # read a file as it was in the lost commit

# Prove/refute "commit A is a squash of chain tip B":
git rev-parse <A>^{tree} <B>^{tree}    # identical hashes = identical content

# Mainline reading (only 2 merges exist):
git log --first-parent --oneline main

# Line-range history when blame dead-ends at daeda90/31a3db4:
git log -L 1,40:src/layout.rs          # follows the range through history
git log --all -S "exact code string"   # or pivot to string lifetime
```

**Blame discipline:** when `git blame` answers `daeda90` or `31a3db4`, you
have learned nothing — pivot to `-S`/`-L`, the dated plan under
`docs/superpowers/plans/`, and (for `daeda90`) the dangling chain.

**Adding an entry:** when a battle settles (fix rejected, decision reversed,
investigation closed), append it here in the same symptom → wrong turns →
root cause → evidence → status shape, with hashes and dates. That is what
makes the verdict enforceable by **foreman-change-control**.

## When NOT to use this skill

- **You have a live symptom to triage now** → **foreman-debugging-playbook**
  (symptom → discriminating experiment tables).
- **You're deciding which doc to trust or fixing drift** →
  **foreman-docs-and-writing** (trust map, house style); this skill only
  records the incidents.
- **You want to change or re-litigate a settled decision** →
  **foreman-change-control** (the gate; this skill is its evidence annex).
- **You want to revive the A/B experiment or open new research** →
  **foreman-research-frontier** / **foreman-research-methodology**.
- **You're measuring performance or driving the app headlessly** →
  **foreman-diagnostics-and-tooling**.
- **You're an agent running inside foreman needing dispatch/chat usage** →
  the user-facing **foreman-dispatch** / **foreman-chat** skills.

## Provenance and maintenance

Written 2026-07-01 against HEAD `7fda1c2` (`main`). Re-verify drift-prone
claims before trusting them:

| Claim | Re-verify with |
|-------|----------------|
| Experiments never committed (battle 1) | `git log --all -S "PSEUDOCONSOLE_RESIZE_QUIRK"` → only `5332757`; `rg portable-pty Cargo.toml` |
| `origin/fix/terminal-reflow-on-resize` tips at `5332757` | `git log --oneline origin/fix/terminal-reflow-on-resize -1` |
| `docs/snap-tiling.md` still bannerless | `Get-Content docs/snap-tiling.md -TotalCount 5` |
| Selection rewrite landed (battle 4 resolved) | `git log --all -S "Selection::new"` → `64a80b9` (doc) + `b581240` (code); `rg "sel_anchor" src/terminal.rs` (expect empty) |
| `--settle-ms` honored but help still lies | `rg "not yet honored" src/control.rs`; `rg "DEFAULT_SETTLE_MS" src/wm.rs` |
| Production `chat_broadcast` still deleted | `rg "fn chat_broadcast" src/wm.rs` → test fns only |
| A/B prerequisites still unbuilt | `rg -e to_jsonl -e persist_chat_msg src/`; `Test-Path scripts` |
| Dangling inventory (gc may prune) | `git fsck --no-reflogs 2>$null` — look for "dangling commit" lines |
| Zero deletions / 2 merges / date gaps | `git log --diff-filter=D --oneline --all`; `git log --merges --oneline`; `git log --format=%ad --date=short --all` |
| Epic/CLAUDE.md/HANDOFF drift rows | commands in the meta-failures table |
| Latency numbers (idle ~0.13 ms/frame etc.) | dated 2026-06-18; re-measure via **foreman-diagnostics-and-tooling** |
| Line numbers cited (`control.rs:130,548,763`, `wm.rs:17-18,938`, `terminal.rs:260`) | `rg -n` the quoted strings; treat numbers as 2026-07-01 snapshots |
