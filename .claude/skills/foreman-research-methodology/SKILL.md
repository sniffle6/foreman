---
name: foreman-research-methodology
description: Use when forming or accepting a theory about foreman behavior — proposing a root cause, designing an experiment or A/B test, recording a negative result, retiring a feature, or retrying old ideas ("try vsync off / --await-ack / a writer thread / the double-reflow fix again?"). Also when writing specs or plans under docs/superpowers, running an agent debate, or judging whether agent consensus is approved. Symptoms: "I think the bug is X", "this fix seems to work".
---

# Foreman research methodology

The discipline that turns a hunch into an accepted result in this repo. Every
principle below is grounded in a real, verifiable episode from foreman's own
history — not aspiration. Repo root: `H:/claude code/foreman`. All commands are
PowerShell 7+, read-only.

Domain terms used once here: a **PTY** is the OS pseudo-terminal a Session's
process talks through; **ConPTY** is Windows' PTY layer; **DSR** is the
device-status query a program sends at startup (a Session is **Ready** once it
is answered). Depth on all of these: **terminal-emulation-reference**.

## The evidence bar

A root-cause claim is accepted here only when it clears ALL of these:

- [ ] **One mechanism explains every observation — including the negatives**
      (the things that *don't* misbehave).
- [ ] **It survives a genuine refutation attempt** — an A/B against a system
      that doesn't show the symptom, and you can explain *why* that system is
      clean.
- [ ] **It predicts which fixes will fail**, and they do fail when tried.
- [ ] **Evidence is measured, not eyeballed** — byte traces, harness numbers,
      Snapshots (how: **foreman-diagnostics-and-tooling**).
- [ ] **It is written down where the next person will trip on it** — a doc plus
      a code comment pointing at the doc.

### Worked example: the ConPTY resize/recall corruption

Source: `docs/conpty-resize-reflow.md` (all observations reproduced);
committed as `5332757`; the code comment on `Session::resize` in
`src/terminal.rs` ends "See docs/conpty-resize-reflow.md before touching this."

| Observation | Theory A: "double reflow in `Session::resize`" | Theory B: upstream divergence (microsoft/terminal #18725) |
|---|---|---|
| Narrow past a wrapped prompt, Up-arrow recall corrupts until Ctrl+L | explains | explains |
| ConPTY's own resize repaint is clean; only the *recall* bytes land one row high — the bytes disagree with each other | unexplained | explains (ConPTY reports a cursor inconsistent with its own repaint) |
| Offset equals the prompt's wrapped-row count at every size | coincidence | predicted (conhost-vs-alacritty reflow delta) |
| Plain typing after resize is fine; only history-recall misfires | unexplained | explains (the recalling program reads the wrong cursor) |
| **Windows Terminal renders the identical scenario cleanly** (the A/B) | unexplained — WT resizes both sides too | explains: WT replicates conhost's reflow byte-for-byte (microsoft/terminal PR #4741) |
| All four "let ConPTY own the redraw" fix strategies failed | predicts they'd help | predicts they can't (the inconsistency is *inside* ConPTY; no frontend redraw-ownership reaches it) |

Theory A explained the headline symptom and was still wrong. It was disproven
by byte-level tracing of the ConPTY↔foreman stream plus the Windows Terminal
A/B. Theory B explains everything *including the four negatives* — that is the
bar. The corrupted diagnosis would have burned a rewrite of `Session::resize`
for nothing.

## Hypothesis predicts numbers BEFORE you run

State the mechanism quantitatively, derive what the measurement must show, then
measure. Two verified instances:

1. **Render latency (2026-06-18).** The claimed mechanism (commit `1accc46`):
   a fixed 16 ms repaint metronome, floored by Windows' ~15.6 ms default timer
   granularity, adds ~16–32 ms per keystroke/echo — and render work itself is
   *not* the cost. That predicts sub-millisecond frames once event-driven. The
   throwaway harness matched: idle ~0.13 ms/frame, the proxy-driven repaint
   path ~0.2 ms, one max-rate flood ~0.8 ms, 12 simultaneous floods ~8 ms avg
   (`docs/followups-latency-and-control.md`). The rival mechanism (vsync) was
   killed by A/B — see negatives below.
2. **The chat-room A/B experiment (designed 2026-06-15, still never run —
   check with `ls docs/chat-ab-results.md`).**
   `docs/superpowers/specs/2026-06-15-chat-room-ab-experiment-design.md`
   pre-registers the headline metrics (**tokens-per-requirement-met**,
   **bugs-per-1k-tokens**), fixes ≥3 runs per arm × 3 arms = 9 builds, blinds
   the grader, interleaves runs against cache bias — and gates all of it behind
   a **Phase 0 de-risk step**: prove the token-measurement chain works on one
   cheap run first, because "if not, the whole token half collapses — stop and
   rethink." Design the kill-switch before spending the budget.

Rule of thumb: if your hypothesis doesn't tell you a number the experiment must
produce, you don't have a hypothesis yet — you have a vibe.

## Negative results are recorded, not discarded

A negative result costs real time; writing it down (with its measurement and a
retry condition) is what stops the project paying that cost twice. Canonical
negatives and where they live:

| Negative result | Recorded measurement / reason | Home |
|---|---|---|
| vsync-off did NOT fix typing latency | typing feel identical off vs on; the metronome was the cause; off risks tearing + GPU spin | `docs/followups-latency-and-control.md` § "Decisions & known edges" |
| "Let ConPTY own the redraw" — all 4 strategy combinations failed | per-combination byte outcomes listed | `docs/conpty-resize-reflow.md` § "What was tried and rejected" |
| Background writer thread for chat persistence | saves zero ms (write is sub-ms on an open handle); only *adds* a crash-loss window | `docs/chat-persistence.md` decision #3 |
| Storage trait + in-memory fake | fails the deletion test — a fake "tests a Vec prod never runs" | `docs/chat-persistence.md` decision #2 |
| `--await-ack` (accepted-but-inert flag) | live skill testing showed the ack gap mitigated in practice; an inert flag is "a lying API surface" | `docs/contracts/chat-handshake-remaining-work.md` status banner |

Recording convention: the negative goes in the **same doc a future reader will
consult before retrying**, with the measurement and the condition under which a
retry becomes rational. The full chronicle of dead ends is
**foreman-failure-archaeology**; this section is only the discipline.

## Adversarial refutation is standard practice

A design here is not "reviewed"; it is *attacked*, and the attack is recorded.
Verified episodes:

| Episode | Format | Outcome worth copying |
|---|---|---|
| Chat persistence (2026-06-27) | Four-lens debate (reliability / performance / grug / deep-modules) run in the project Chat room; output committed as `fdc6d87` | The writer-thread proposal was **withdrawn by its own author** under debate (`docs/chat-persistence.md` #3). Author-withdrawal is a success signal, not an embarrassment. |
| Chat @-mentions v2 (2026-06-10) | Two Dispatched agents with opposing briefs — "Architect" (pro-mentions) vs "Skeptic" (pro-broadcast) — debated live in the p1 Chat room; full transcript in the spec appendix | Concessions recorded per side ("What each side conceded"); consensus reached — and *scoped*: row 5's rationale is flagged as load-bearing on an unsolved mechanism. |
| Chat missing-features (2026-06-11) | Three-agent opinion panel in the Chat room (`docs/chat-missing-features.md`) | Explicitly labeled the *opinion* counterpart the A/B experiment exists to replace with data. |

The standing caveat, verbatim from the mentions spec (`docs/superpowers/specs/2026-06-10-chat-mentions-design.md`):
**"Treat the consensus as a strong proposal, not a human-approved decision."**
Agent consensus never self-approves; acceptance runs through
**foreman-change-control**.

Running one: give each side a real brief with a stake in losing; require every
concession to be written down; record which conclusions are load-bearing on
unproven assumptions (the mentions spec's ⚠ warning: if quiescence-gated
injection proves unbuildable, "the rationale in the decision table is no longer
quite the one that was agreed" — that unsolved gate is now
**foreman-agent-state-campaign**'s problem).

## The idea lifecycle (pipeline with artifacts)

| Stage | Artifact | Verified example |
|---|---|---|
| 1. Hunch / observed problem | A dated, measured problem statement | "invoked the foreman-dispatch skill, then still spent 5m34s / 23.5k tokens re-deriving facts from source" (`docs/superpowers/specs/2026-06-10-foreman-skills-split-design.md`) |
| 2. Spec | `docs/superpowers/specs/<date>-<name>-design.md` — Status line, decision table, **rejected alternatives with why**, out-of-scope list | the A/B design; the mentions design |
| 3. Plan | `docs/superpowers/plans/<date>-<name>.md` — checkbox-executable ("Steps use checkbox (`- [ ]`) syntax for tracking") | `2026-06-15-chat-room-ab-experiment.md` |
| 4. Execution | Subagent-driven commit series; session state in `docs/superpowers/sessions/` | agent-group-chat: 11 commits `61b515e`→`45f4725`, each task a fresh implementer + spec review + quality review |
| 5. Consolidation | Feature doc in `docs/`, vocabulary entry in `CONTEXT.md`, tests | `docs/tiling-tree.md`; CONTEXT.md's "Outbox", "Caret gate" |
| 6. Distillation | Serena memory write for cross-session recall (location referenced in `CLAUDE.md` § Session Context) | — |

House style for stages 2–5 docs is **foreman-docs-and-writing**; what "tests"
must show is **foreman-validation-and-qa**.

### Retirement is a first-class exit, not a failure

The `--await-ack` retirement (commit `9bb3a35`) is the template. When a built
surface stops earning its keep:

1. **Remove it cleanly** — flag, wire field, and the unconsumed state machine
   all went, not just the flag.
2. **Document the recovery point** — "recover them from git at increment-1
   commit `4607001`" (`docs/contracts/chat-handshake-remaining-work.md`).
3. **Write the restart condition** — "If unattended fleets ever need
   self-healing handoffs, restart here."
4. **Name the reason honestly** — it was removed as "a lying API surface"
   (accepted on the wire, did nothing), not as "bad code".

A designed-but-unbuilt idea parks the same way: `docs/chat-persistence.md` is
labeled **"designed, not built"** and stays true. Check it with
`rg -n "std::fs|next_seq" src/chat.rs` — note that grepping for `ChatLog`
alone would **pass falsely**, because an in-memory `ChatLog` exists; only the
file I/O and the sequence numbering are the unbuilt part. A re-verification
command that can pass for the wrong reason is worse than none.

## Don't-re-litigate discipline

Settled decisions carry their rationale *at the place you will trip on them*:

- **Doc banners:** `docs/chat-persistence.md` § "The converged design (do not
  re-litigate these)"; `docs/followups-latency-and-control.md` § "Decisions &
  known edges (don't re-litigate)"; the tabbing epic's "PARTIALLY SUPERSEDED
  (2026-06-11)" header.
- **Code comments pointing at docs:** the `Session::resize` comment routes any
  future "fix" attempt to `docs/conpty-resize-reflow.md` first.
- **The registry** of settled decisions and their gates lives in
  **foreman-change-control** — cross-reference it; do not restate it.

Two asymmetries keep this honest:

1. **"Settled" is user-ownable, not model-ownable.** The tabbing epic settled
   "we are **not** building a BSP tile tree" — and the user reversed it on
   2026-06-11 into today's Layout tree + two-state model (`docs/tiling-tree.md`,
   commits `daeda90`…`31a9120`). A model re-opening a settled decision needs new
   evidence; the user needs only the decision.
2. **Supersession must reach every doc, and a "supersede the docs" commit is
   not proof that it did.** `docs/snap-tiling.md` describes the deleted 9-zone
   snap system. Commit `b42e92e` was literally titled "supersede zone-snap
   docs" — it updated HANDOFF, CLAUDE.md, the epic and foreman.md, and missed
   the one file named after the superseded feature. The banner did not land
   until `5ad4ee9` (2026-08-24), months later, and only because an audit went
   looking. The lesson is the failure mode, not the fix: **a supersession
   commit's own title is the weakest possible evidence that supersession
   happened.** Enumerate the files that mention the dead thing
   (`rg -l <dead-symbol> docs/`) and check them off; do not trust the commit
   message. The doc trust map is **foreman-docs-and-writing**.

## Where good ideas came from (observed, not theorized)

| Source | Verified episode |
|---|---|
| **Dogfooding** — foreman coordinating the agents that build foreman | The mentions-v2 spec "was produced *by* the feature it extends" — two Dispatched agents debating in the p1 Chat room; the persistence four-lens debate and the missing-features panel also ran in the room. |
| **Live failures converted to design** | Paste-burst lost submit → the deferred `\r` (`SUBMIT_DELAY`, now in `src/ready.rs`, commit `45f4725`). The 5m34s/23.5k-token re-derivation spiral → the stop-sign skill pattern ("This skill is complete. Do NOT read foreman source or docs…" — top of the shipped agent skills). A wedged client blocking all Dispatch → thread-per-connection Control plane (`15f675f`). |
| **User reversals of settled decisions** | zone snapping → Layout tree (above). |
| **Upstream bug reports as design input** | microsoft/terminal #18725 and PR #4741 set the fix/no-fix economics for the resize corruption — the "real fix" was costed from what Windows Terminal actually does, then deliberately not pursued. |

## What acceptance means

An accepted result = the evidence bar above **plus** a measurable gate with
evidence per **foreman-validation-and-qa** **plus** classification/review per
**foreman-change-control**. Nothing in this skill routes around either.

Test counts quoted in episode docs are dated facts about the day they were
written; never quote one as current. Derive it:
`rg -c '#\[test\]' src/ | sort -t: -k2 -rn`.

## When NOT to use this skill

- Triaging a known symptom right now → **foreman-debugging-playbook** (this
  skill is for forming a *new* theory; that one is theories already settled).
- The worked analysis recipes themselves → **foreman-proof-and-analysis-toolkit**.

## Provenance and maintenance

The discipline does not rot; the episodes it cites can. Re-check these before
repeating them:

| Claim | Re-verify with |
|---|---|
| ConPTY episode: doc and code comment still paired | `rg -n "conpty-resize-reflow" src/terminal.rs docs/` |
| `--await-ack` retirement: both commits still reachable | `git show -s --format=%s 4607001 9bb3a35` |
| Chat persistence still designed-not-built | `rg -n "std::fs" src/chat.rs; rg -n "next_seq" src/chat.rs` — no output from either means still unbuilt; grepping `ChatLog` instead would false-pass |
| The chat A/B experiment still has not been run | `ls docs/chat-ab-results.md scripts/ab` — both absent means still un-run |
| The standing "consensus is not approval" caveat still in the mentions spec | `rg -n "not a human-approved" docs/superpowers/specs/2026-06-10-chat-mentions-design.md` |
