# Continuing plan orchestration

This design records the workflow discussed on 2026-10-26. It is a reviewable
implementation proposal, not evidence that these capabilities exist or
authorization to execute a plan. Exact command syntax and persistence schema
remain implementation decisions subject to the contracts below.

## Purpose and user interaction

The user starts an agent Session and asks it to orchestrate an approved plan.
Keep this entry point. Do not require a Run Plan UI, a separate coordinator,
or manual assignment setup.

The orchestrating agent executes related cards itself by default. It retains
its conversation and uses the existing project checkout on feature branches.
It may delegate independent investigation, implementation, or review when the
benefit warrants it and the user's instructions permit it. Concurrent writers
require separate checkouts; ordinary sequential work does not.

Cards represent independently acceptable outcomes. Implementation, regression
tests, and necessary documentation for one outcome belong together. Merge
artificial splits rather than designing grouped integration to accommodate them.
Keep useful checkpoints even when one agent executes several cards.

Optimize elapsed time and token use through an accepted result. Cold builds,
repeated discovery, coordinator turns, and duplicated verification are costs.
No measured speedup is claimed by this design.

## Existing seams

Use these sources when implementing, rather than treating this design as a
replacement description of current behavior:

- `src/kanban.rs`: `CardStore::start`, `claim_common`, `integrated`, and
  `dispatch_prompt` define claims, completion, and worker instructions.
- `src/wm.rs`: `dispatch_precheck` and `dispatch_spawn` connect cards to Sessions.
- `docs/kanban-board.md` describes dispatch modes, waves, and checkout holds.
- `docs/integration-queue.md` describes serialized integration and branch mode.
- `docs/agent-state.md` describes provider hook routing and identity limitations.
- `.codex/skills/foreman-kanban/orchestrating-plans.md` and its Claude counterpart
  define the existing orchestration policy that implementation must replace.

Self-service pickup is a useful seam, but a claim alone does not establish
checkout reservation, branch preparation, or safe continuation.

## Assignment and ownership

An assignment is internal bookkeeping connecting an approved plan or explicit
subset of cards to an agent Session and checkout. It is created by the
orchestration commands, not by a new user-facing setup workflow.

Persist enough information to recover the approved scope, stable assignment
identity, owner generation, repository and checkout identity, integration target,
current card, transition progress, and latest continuation note.

Only one card is actively claimed by a sequential assignment at a time. Reserve
the checkout across card transitions so another dispatch cannot take it between
integration and the next claim. Coordinate reservations across project windows
and Foreman instances that refer to the same checkout.

Ownership is a guardrail for cooperating Foreman operations, not an OS lock
against human Git commands. Revalidate actual checkout state at transitions.
Do not infer abandonment from silence, compaction, an idle badge, or an expired
timer. A dead Session makes an assignment recoverable; it does not authorize
discarding files or transferring ownership without inspection.

Keep approved execution scope separate from mutable plan membership. New cards
do not silently expand the assignment. A changed brief must be observed before
claiming the card; material scope changes require the user's decision. Do not
rewrite the task underneath an active worker.

## Continuation interface

Expose a small interface through the existing CLI/control plane. The following
operation names describe intent and are not final command spellings.

| Operation | Contract |
|---|---|
| Begin | Attach the requesting agent to approved scope, reserve the checkout, and prepare and claim eligible work. |
| Continue | Reconcile the current card and integration, then return the current task, a failure, a wait, a blocker, the next claimed task, or completion. |
| Resume | Inspect durable records and Git, then restore or explicitly recover ownership without losing unfinished work. |
| Checkpoint | Replace the concise continuation note for the current assignment and owner generation. |
| Pause | Request that execution stop before claiming another card after the current safe checkpoint. |

Use stable request identities or equivalent persisted transition identities so
retries after lost responses cannot skip a card, duplicate an integration request,
or repeat branch preparation. Old owner generations cannot mutate an assignment
after recovery by another Session.

Return structured outcomes plus concise human-readable guidance. Include the
assignment identity, current card, branch, and applicable next action. Detailed
logs are available on demand. Normal operation must not require rereading the
entire board or scraping the terminal to discover completion.

Long integration waits should occur inside tools, with bounded timeouts and
interruptibility. A timeout means waiting, not failure or permission to advance.

## Execution and integration

The normal sequence is:

```text
begin/resume -> prepare branch and claim card -> implement and validate
             -> submit integration -> wait and reconcile -> checkpoint
             -> prepare and claim next card, or pause/finish
```

Retain per-card feature branches and existing serialized integration. Reuse
the checkout and build directory; do not create a fresh worktree per card.
Retaining build artifacts does not guarantee no rebuild when inputs change.

Foreman prepares the next feature branch before returning permission to work.
The agent must never start the next card on the default branch. Branch creation
uses `--no-track` and verifies tracking. Preparing a branch and recording a claim
need recoverable intermediate states because filesystem and Git operations are
not one atomic transaction.

Keep the current wave eligibility rule. Within an eligible wave, choose a stable
order and document the tie-breaker. Blocked work is not automatically retried
because Continue was called. Distinguish assignment completion from a plan that
still has blocked or unassigned work.

A card becomes Done only after verified integration. Check failure, base movement,
or a held integration keeps the current card and provides recovery guidance.
The agent does not implement the next card while its predecessor is integrating.

Permission to orchestrate must be interpreted with applicable repository rules.
Do not infer permission to commit, integrate, or publish where the user's
authorization or repository policy does not provide it. Surface such a missing
permission before beginning dependent execution, rather than discovering it at
every card boundary. This design itself authorizes none of those actions.

## Context and token use

At assignment start, supply the goal, constraints, acceptance criteria, compact
card outline, current full brief, and relevant document pointers. Do not load
every future card body or the entire reference library.

At transitions, supply the next brief and relevant changes. Reuse the existing
conversation rather than reinjecting completed reports and unchanged policies.
Do not require acknowledgement messages or a second agent to relay routine
integration results.

Maintain one concise continuation note, replacing stale instructions rather than
appending a transcript. Include current intent, consequential decisions and
reasons, unfinished steps, relevant discoveries, and verification gaps. Update
at meaningful milestones, including during long cards. Do not rely on a warning
immediately before compaction. Git and integration records establish what landed;
the note supplies reasoning those records cannot express.

Completion evidence remains durable per card. Replacing the continuation note
must not erase landed commits, acceptance results, or unresolved follow-ups.

## Compaction and recovery hooks

Let the provider CLI compact its own conversation. Compaction does not release
ownership, change cards, restart the worker, or imply completion.

Use `SessionStart` with source `compact` in supported Claude Code and Codex
versions to return a compact recovery brief before execution continues. The brief
contains approved scope, current card, observed checkout/integration state, the
latest continuation note, and guidance to reconcile through Continue or Resume.

The hook is read-only: it does not claim work, change ownership, submit integration,
or modify Git. It must be fast, bounded, and safe to invoke repeatedly. It emits
no assignment context for unrelated sessions. Stale notes cannot override current
Git or integration evidence; report disagreements rather than choosing silently.

Route by validated provider session identity as well as Foreman instance, project,
and terminal identity. Inherited environment variables alone are insufficient:
child agents can inherit terminal identifiers, and provider daemon behavior can
misroute events. A fork or replacement session must not inherit execution authority
just because it received the old conversation. Recovery explicitly reconciles
ownership and records a new owner generation.

Reuse appropriate hook installation conventions without coupling this feature to
optional naming or badge settings. Preserve unrelated hooks and existing trust
positions. Detect unsupported versions and disclose untrusted or unavailable
hooks; installation is not proof that a hook ran.

Continue and Resume remain safe without hooks. Provider hooks restore context
automatically; they are not the sole source of correctness or recovery.

Official references consulted on 2026-10-26:

- [Claude Code hooks](https://code.claude.com/docs/en/hooks#sessionstart):
  `SessionStart` includes `compact`; the reference also describes `PreCompact`
  and `PostCompact`. Use the context-loading event for the recovery brief.
- [Codex hooks](https://learn.chatgpt.com/docs/hooks#sessionstart): matching
  `SessionStart` hooks run after root-session compaction before the next model
  request, including a mid-turn continuation, and can provide additional context.

Verify installed CLI support and actual delivery during implementation. The
documentation lookup does not establish behavior in the user's installed versions.

## Validation and shared-checkout safety

Preserve repository test-first requirements and witnessed bug reproduction.
Run focused tests while developing and required gates at acceptance checkpoints.
Record the command, result, commit, and relevant environment or working-tree
conditions. Do not repeat identical checks merely because another role appeared;
rerun when relevant inputs changed or evidence is insufficient.

Branch-mode validation can observe unrelated dirty files. Do not label a result
as validating only the submitted commit when those files affected execution.
The acceptance path must either establish that the validated inputs correspond
to the integrated result, verify in an appropriately isolated environment, or
hold integration with a concrete explanation. Do not silently weaken evidence
to retain speed. The exact mechanism must be settled before changing integration.

Preserve unrelated tracked and untracked work. Never automatically stash, reset,
clean, or discard it. Unexpected branch movement or incompatible checkout changes
stop automatic transitions. A reservation is not permission to overwrite edits.

Independent review is selected by risk and applicable policy. Review required for
acceptance occurs before integration, with findings returned to the same worker.
Routine cards do not require a permanent coordinating agent merely to read reports.

## Pause, interruption, and completion

A requested checkpoint pause prevents the next claim after the current safe
checkpoint. It does not cancel an in-flight Git operation. Immediate interruption
uses existing Session controls and leaves recoverable work.

On restart, reconcile persisted transition state against Git, card state, and the
integration queue before issuing instructions. In particular, recognize integration
that completed before a response or checkpoint was recorded.

Releasing a checkout reservation requires an explicit safe transition. A paused
or dead owner must not free a dirty checkout for another writer automatically.
Completion releases ownership only after integration is reconciled and no
assignment-owned transition is pending.

## Implementation scope and acceptance

Deliver the lifecycle, CLI support, recovery hooks, and paired orchestration
instructions together. Implement and verify the lifecycle before its adapters.
Use existing board status where sufficient; new Run Plan controls and a separate
assignment-management UI are not required. Preserve individual-card dispatch
and worktree mode for independent work.

Before implementation, settle the persistence location and schema, command and
wire formats, request identity strategy, cross-instance reservation mechanism,
provider identity verification, and validation treatment of dirty inputs. These
are required design details, not permission to silently relax this contract.

Acceptance must cover:

- One Session completing related cards in one checkout without another agent
  launch or worktree creation, preserving useful build artifacts.
- Claims and writes occurring only on the prepared feature branch.
- Duplicate and concurrent continuation requests, including lost responses.
- Competing Sessions and project windows attempting to reserve the same checkout.
- Failure or restart around every persisted branch, claim, and integration transition.
- Integration failure, held requests, moved targets, and completion during a wait.
- Unexpected checkout changes and preservation of unrelated files.
- Pause preventing subsequent claims; blocked work distinguished from completion.
- Replacement owner recovery rejecting stale commands from the previous owner.
- Compaction briefs delivered to the correct root provider session, including
  mid-card compaction; children and unrelated sessions receive no assignment brief.
- Missing, untrusted, failed, timed-out, and duplicate hook invocations.
- Existing dispatch modes, wire compatibility, and integration safeguards.

Use test-first checks through supported interfaces for assertable production
behavior. Exercise actual CLI hooks and control-plane behavior for delivery
claims. Run repository-required checks and update the existing feature docs when
implementation lands. Keep Claude and Codex orchestration copies semantically
aligned, adapting only harness-specific details.

Measure representative plans through accepted integration: elapsed time, model
usage where available, initial/incremental builds, tests, integration waits,
reviews, retries, defects, and human interventions. Compare against the existing
workflow without claiming equivalent tasks or guaranteed performance gains where
the evidence does not establish them.

## Alternatives not selected

- A fresh worker per card repeats discovery and discards useful context.
- Larger cards everywhere sacrifice independently useful acceptance checkpoints.
- A persistent worktree pool adds lifecycle complexity without an established
  need for concurrent writers in the default workflow.
- A mandatory coordinator adds handoffs to a sequential stream of work.
- Compaction detection alone cannot cover crashes, missing hooks, or lost replies.
- Automatic fresh conversations after each card defeat the intended continuity;
  explicit recovery remains available when a conversation should be replaced.
- Grouped integration and a new dependency scheduler are unnecessary for the
  agreed per-card checkpoints and existing wave ordering.
