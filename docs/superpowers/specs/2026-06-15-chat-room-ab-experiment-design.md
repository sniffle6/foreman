# Chat Room A/B Experiment — Design

**Date:** 2026-06-15
**Status:** Design (pending review)
**Goal:** Produce *data* — not opinion — on whether foreman's group chat room
improves **result quality** and **token efficiency** for multi-agent builds, and
turn that data into a prioritized list of QOL / feature improvements.

This experiment is the empirical counterpart to `docs/chat-missing-features.md`
(which is an agent *opinion* panel). It re-orders that 9-item backlog from
opinion-ranked to **data-ranked**.

---

## 1. The question

> When several agents build the same app, does coordinating through the chat
> room produce a better result and/or spend fewer tokens than the
> alternatives — and where specifically does the room help or hurt?

A single "solo vs chat-team" comparison can't answer this: it bundles two
variables (parallelism *and* the chat feature). So we run **three arms** and
read the differences.

## 2. Arms

All arms build the **same app** from the **same written spec**, starting from an
**identical fresh git worktree**, with the **same model** and the **same wall-clock
time cap**. Only the *coordination harness* changes.

| Arm | Setup | What it isolates |
|-----|-------|------------------|
| **A — Solo** | 1 agent builds the whole app. No chat, no team. | Baseline: one mind, no coordination cost, no parallelism. |
| **B — Team, no chat** | An orchestrator agent decides the API contract **upfront**, dispatches a **backend** worker and a **client** worker via `foreman open` with the contract baked into each prompt, then waits and integrates. No mid-flight comms. | "Split-and-pray": parallelism with a committed-upfront contract. |
| **C — Team + chat** | Orchestrator dispatches the same backend + client workers **into the chat room**, but the contract is **deliberately left under-specified** — the two workers must **negotiate it live** in chat (the model scenario from `chat-missing-features.md`). | Live coordination through the room. |

**Key comparisons:**
- **A ↔ C** — the headline ("solo vs a chat-coordinated team").
- **B ↔ C** — chat's *pure marginal value* (same team size, contract committed-upfront vs negotiated-live). **This is the comparison that tells us what to improve.**
- **A ↔ B** — whether parallelism alone is even worth it.

**Team size:** K = 2 workers (backend, client) + 1 orchestrator, for both B and C.
Two is the minimum that creates a real contract boundary; keeping it at 2 holds
the parallelism variable constant between B and C and keeps cost down.

**Why the contract is handled differently in B vs C (and why that's fair, not rigged):**
Without a comms channel you *must* commit the contract upfront — that's the
honest no-chat workflow. With a channel you can negotiate. The experiment is
precisely whether live negotiation beats commit-upfront, and at what token cost.
To make this a real test, the spec (§3) is complete on *behavior* but
intentionally **leaves contract details ambiguous** (exact JSON shapes, error
format, pagination, filtering), so an upfront contract *can* be wrong and the
chat arm *has* something to negotiate.

## 3. The task / app

A small **REST API service + a CLI client** that talks to it, in **Python**
(no build step; `pytest` for the hidden acceptance suite; claude is strong here).
Final language is a minor call — Python is the default for grading simplicity.

**Domain (concrete, small, real coordination surface):** a "task list" service.

- **Server:** in-memory or SQLite store; CRUD endpoints for tasks; list with
  filtering + pagination; consistent error responses.
- **Client:** a CLI (`tasks add|list|done|rm …`) that calls the server and
  renders results.

**The spec the agents receive** (committed as a fixture; agents see this):
- ~12–15 **checkable requirements** (e.g. "client `tasks add "x"` creates a task
  and prints its id"; "`list --status open` returns only open tasks";
  "deleting a missing id exits non-zero with an error message").
- A **behavioral** description complete enough to build against.
- The **coordination surface left ambiguous on purpose**: exact request/response
  JSON shapes, HTTP status codes for errors, the error body format, the
  pagination style (offset vs cursor), and the filter query param names.

**What the agents NEVER see:** the acceptance test suite and the reviewer rubric
(§4). Held out so neither arm can teach to the test.

## 4. Metrics (the user's list, made objective)

| Requested | Operational definition | Source |
|-----------|------------------------|--------|
| **Token usage** | Sum of `input + output (+ cache_creation + cache_read)` tokens across **every** agent in the arm. Reported raw **and** as a cache-adjusted cost estimate. | Claude Code session transcripts: `~/.claude/projects/<slug>/<session>.jsonl`, `message.usage` per assistant turn. |
| **Coordination vs work split** (arm C, and any chat in others) | Tokens on turns whose user message carries the `[chat pN #…]` injection framing, plus orchestrator turns spent composing posts. Best-effort attribution. | Same transcripts, filtered by framing prefix. |
| **Bugs** | (failing acceptance tests) + (defects found by a blind reviewer agent), deduped. | `pytest` + reviewer agent. |
| **Goal completion** | % of the requirements checklist met, blind-scored. | Checklist run against the produced repo. |
| **Speed** | Wall-clock start→done, **and** total agent active time. (These trade off: a team can finish sooner in wall-clock while spending more tokens.) | Run harness timestamps. |
| **Headline (derived)** | **tokens-per-requirement-met** and **bugs-per-1k-tokens** — efficiency, not raw spend. | Computed. |

**"Done" detection:** an arm finishes when its orchestrator/agent declares done,
or when the wall-clock cap is hit (default **45 min**). We grade whatever state
the repo is in at stop time — partial work counts against goal completion.

**Blind grading:** outputs are relabeled to random ids before the reviewer/judge
sees them; the grader does not know which arm produced which repo.

## 5. Replication & reporting

- **≥ 3 runs per arm** (9 builds total for 3 arms). One run is an anecdote —
  agents vary 2–3× run to run.
- Report **median + min/max** per metric per arm, not single points.
- Compute the three comparisons (§2) on medians; show the spread.
- **Small-N caveat (stated up front):** 3 runs gives *direction and rough
  magnitude*, not statistical significance. If a comparison is borderline
  (spreads overlap), add runs before drawing a conclusion. This is a
  decision-making dogfood study, not a paper.

## 6. Instrumentation to build (the only real code)

Kept minimal — this is a measurement harness, not a feature.

1. **Token harvester** (`scripts/ab/harvest_tokens.py` or similar): given a run's
   worktree path + start/stop timestamps, locate the matching session JSONL
   file(s), sum usage, and split chat-framed turns from work turns. Outputs a
   per-run JSON metrics record.
2. **Chat-log persistence** (foreman, `src/chat.rs` + drain path): append the
   room log to a per-project JSONL on each post so arm C's transcript survives
   the run for analysis. This is `chat-missing-features.md` #2 — needed here, and
   useful permanently. Minimal append-only writer; no reload required for the
   experiment.
3. **Run harness / checklist** (`scripts/ab/run_arm.*` or a documented manual
   procedure): fresh worktree → launch the arm's agents → detect done / hit cap →
   snapshot repo → run acceptance suite → emit metrics record. Manual is
   acceptable for v1; automate only if the 9 runs are too tedious.

**Held-constant config** (recorded with each run for reproducibility): model id,
worker count K, time cap, the exact spec fixture hash, foreman build commit.

## 7. From data to improvements (the actual deliverable)

The experiment exists to drive this table. Each observed pattern maps to a
concrete, already-scoped item (numbers reference `chat-missing-features.md`).

| Observation in the data | Improvement it justifies |
|--------------------------|--------------------------|
| Arm C spends a large fraction of tokens on coordination turns | Cut broadcast amplification: targeted-delivery defaults, digest/summary posts instead of full broadcast, rate-limit. |
| Collisions (two agents edit the same file / clobber work) appear in B but **not** C | Confirms chat's core value is collision-avoidance — the room earns its keep. |
| Collisions appear in C **too** | Prioritize the claims registry (#6). |
| Targeted posts in C get no / slow replies (handshake never closes) | Prioritize the handshake + delivery-cursor backstop (#1); add the missing-ack alert. |
| Members sit idle / stale while work waits | Role/dispatch **prompt** QOL (norms), not a code feature. |
| C beats B on **quality** but loses on **tokens** | Push the "coordinate at contract boundaries only" norm + typed message kinds (#4) to cut chatter while keeping the win. |
| B matches or beats C on everything | The room isn't paying off for this task shape — investigate whether the *norms* (steering vs announcing) or the *feature* is the problem before building more. |

**Final output of the experiment:** a short results doc
(`docs/chat-ab-results.md`) with the per-arm metric tables, the three
comparisons, and a **data-ranked** re-ordering of the missing-features backlog +
any new QOL items the transcripts reveal.

## 8. Threats to validity (and mitigations)

- **Stochasticity** → replication (§5), report distributions.
- **Token attribution for the coordination split is imperfect** → state it as
  best-effort; the raw total is exact, the split is indicative.
- **Orchestrator prompts necessarily differ between arms** → minimize by sharing
  the identical task spec; vary only the harness/coordination instructions, and
  commit all prompts as fixtures.
- **Caching skews token counts** → report cache-adjusted cost, and run arms
  interleaved (A,B,C,A,B,C,…) so cache/warm-up effects don't favor one arm.
- **Single task → limited generalization** → explicitly scope the conclusion to
  "API+client builds of this size"; note that a second task shape would be the
  next step if results are decision-relevant.
- **Grader bias** → blind relabeling + objective hidden acceptance suite as the
  primary quality signal; reviewer agent is secondary.
- **`claude -p` can't be in chat** (headless workers don't read stdin) → all arms
  use interactive workers so the comparison is apples-to-apples.

## 9. Phase 0 — de-risk before spending 9 builds

Before any real run, validate the measurement chain cheaply:
1. Confirm a dispatched worker's session JSONL is locatable under its worktree
   cwd and contains `message.usage`. **If not, the whole token half collapses —
   stop and rethink (fall back to `--output-format json` headless workers for
   arms A/B and self-reported tokens for C).**
2. Confirm chat-log persistence writes a usable transcript.
3. One throwaway dry-run of each arm on a trivial spec to shake out the harness,
   "done" detection, and the grader.

Only proceed to the real 3×3 once Phase 0 is green.

## 10. Out of scope

- Statistical significance / large-N studies.
- More than one task shape (revisit only if the first result is decision-relevant).
- Building any of the *improvements* the experiment recommends — this spec only
  produces the ranked evidence; each improvement gets its own spec → plan cycle.
- Real-time token display inside foreman (the harvester is post-hoc).
