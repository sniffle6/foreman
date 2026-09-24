# Integration queue

One integration turn per repository. A worker that finishes a worktree card
submits it; Foreman rebases the worktree onto the card's base, runs the
project's checks, fast-forwards the base, and marks the card Done. A conflict
goes back to the worker without holding up the cards behind it. Design
history and the acceptance cases live in
`docs/superpowers/plans/2026-09-17-worktree-integration-queue.md`; this doc
is *how*.

## Why it exists

Workers used to integrate themselves (`git rebase <base>`, then `git -C
<main> merge --ff-only card/<id>`). Two workers finishing together could each
rebase, then race the fast-forward; the loser retried, and nobody validated
the branch against what actually landed first. Retrying per worker gives no
ordering and no shared owner. The queue gives the repository one owner per
turn, a durable order, and a check run tied to the exact target it ran
against.

## What it does

- **Submission** (`foreman kanban integrate <id>`, or Submit on the detail
  page): the card must be In Progress with a worktree that is clean, on
  `card/<id>`, and idle (no rebase/merge in progress). The request records
  the repository identity, target branch, destination checkout, worktree,
  branch, commit, and worker, and is written to the queue at the tail. The
  same card and commit submitted again returns the existing request; a new
  commit for a queued card, or a resubmission after a hand-back, is a fresh
  tail entry. A request that is mid-turn cannot be replaced: cancel it first.
- **Turn**: a coordinator on every project window (running whether or not a
  board is shown, and while the window is minimized) re-reads the queue
  every couple of seconds — immediately after a submission, cancel, or
  finished turn — and, when something is queued, spawns a thread that tries
  to take the repository's turn. The turn runs queued requests in submission
  order, each one: source preflight (still at the submitted commit, clean,
  idle), destination preflight (on the target branch, idle, clean outside
  `.foreman/`), rebase onto the exact target commit, checks, final preflight,
  fast-forward, verify. The stage and the prepared commit are persisted
  before each step.
- **Checks** come from `<root>/.foreman/integrate.json` in the destination
  checkout: `{"check": ["cargo", "test", "--target-dir", "target/agent"],
  "timeout_secs": 1800}` — argv, no shell, run in the prepared worktree.
  No file (or an empty `check`) means no checks; the request records
  `none configured` rather than pretending. The record stores the command,
  result, output tail, prepared commit, and target commit together. On a
  failed check, the detail starts with up to 12 lines containing generic
  failure words (`failed`, `panicked at`, `error:`), each capped at 240
  characters, followed by the last 40 output lines. This keeps the failure
  name visible when later warnings fill the tail. Checks that pass keep just
  the tail.
- **Hand-back** (`needs resolution`): a conflict leaves the worktree exactly
  as git stopped it (mid-rebase, conflict markers in place) and records the
  conflicting files; a failed check records the failure lines and output
  tail; a moved or dirty source (`source changed`), a git operation in
  progress (`source busy`), or a missing worktree each say so. Every outcome
  carries a `next` action. The turn moves on to the next card. The worker
  resolves, commits, and resubmits.
- **Held**: a destination checkout with uncommitted tracked changes outside
  `.foreman/`, on the wrong branch, or mid-operation stops the turn — every
  card behind it would hit the same wall — and the head request stays
  queued with the hold reason. The coordinator backs off briefly and retries
  on its own; a new submission retries at once. Nothing in the destination
  is touched: git's own refusal text is preserved when the fast-forward is
  what found the overlap.
- **Moved target**: a target that advanced between preflight and
  fast-forward is re-prepared and re-checked, up to three times per turn,
  then the request goes back to the queue with a note. Git lock contention
  on the destination is retried the same bounded way.
- **Branch-mode cards** (spec: dispatch-branch) submit with `in_place`:
  the source is the project checkout itself, on `card/<id>`. Their turn
  skips the rebase — a target that is no longer an ancestor is handed back
  as `base moved` — runs the checks in the checkout (uncommitted changes
  and all; submission does not require a clean checkout, because the
  changes may be the human's), then moves the target with `git update-ref
  <target> <commit> <old>` and HEAD with `git symbolic-ref`. No file in the
  checkout changes. A target checked out in some worktree holds the
  request. After any hold, the rest of the turn still tries branch-mode
  requests (each once): the worktree card held because the checkout is on
  `card/<id>` must not keep that card from landing. Recovery: commit on
  target → integrated (and HEAD moved if still on the branch at that
  commit); otherwise queued again.
- **Done follows integration**: the turn writes `integrated` with the
  prepared commit; the coordinator of any window whose board has the card
  then marks it Done, queues the ordinary held teardown, and removes the
  request. `done` by hand on a worktree card is refused while the queue owns
  it and while the branch has commits not on its base; a branch already on
  base (integrated by hand) passes on ancestry alone.
- **Ownership guards**: while a request is queued or integrating the queue
  owns the worktree — teardown waits, and `rm`, Discard, and re-dispatch
  are refused with the phase and the cancel command.
- **Cancel** (`integrate <id> --cancel`, or the detail page): a queued or
  handed-back request is removed at once; an integrating one is flagged and
  the owner stops at its next checkpoint (between stages, or by killing a
  running check), leaving the worktree at whatever the rebase produced and
  reporting that in a toast. An integrated request cannot be cancelled.
- **Progress**: `kanban list` appends `[integrate queued #2]`,
  `[integrate integrating · check]`, `[integrate needs resolution ·
  conflict: conflicts in: f.txt]`; `--json` adds an `integration` object
  (`phase`, `commit`, `position`, `stage`, `reason`, `detail`, `next`,
  `hold`, `prepared`, `checks`, `note`, `cancel_requested`). `wait <id>`
  exits `3` on needs resolution; `wait --any` leaves that to the live
  worker. The board paints the substate as the card's status line and the
  detail page's Integration section, with Submit / Resubmit / Cancel.

## Recovery and ownership

The turn is an OS file lock (`turn.lock`) held by the owning process for the
whole turn and released by the OS when that process dies — process-lifetime
ownership, never an expiring lease that could grant a second turn while the
first still runs. Queue writes take a second, short lock (`meta.lock`);
request files are written atomically, so readers need no lock. Two Foreman
processes on the same clone, or two project windows in one process, share
the same locks; different repositories are independent.

A request still `integrating` when a new owner takes the turn belonged to a
dead owner. Recovery decides from git, never from a missing reply: a
prepared commit already on the target is `integrated` (bookkeeping
completes); a rebase left in progress in the worktree is handed back as
`interrupted` with the abort/continue instruction; a worktree still at the
submitted commit is queued again; one at the prepared commit is queued
again from that commit; anything else is `source changed`.

Git and check subprocesses run under a kill-on-close Job, so an owner that
dies takes its children with it — a half-finished rebase or a `cargo test`
tree cannot overlap the next owner. Long git operations and checks have
deadlines; a check past its timeout is killed and reported as failed. A
check that exits while a descendant still holds its output pipes gets one
5 s grace, shared by stdout and stderr, then whatever arrived is reported.

## How to use it

Worker (the dispatch prompt says exactly this):

```
foreman kanban integrate <id>
foreman kanban wait <id> --timeout 1800
```

Exit `0`: Done, finished. Exit `3`: read `foreman kanban list --json`, fix
in the worktree, `integrate` and `wait` again. Exit `2`: wait again. Never
run `done` on a worktree card; never merge into the main checkout.

Project: add `.foreman/integrate.json` at the repository root to run checks
on every integration. Without it, integrations land unchecked and the card
says `checks: none configured`.

Human: the card's status line shows the phase; the detail page shows the
reason and next action; Cancel withdraws; Resubmit re-queues a handed-back
card once the worktree is clean.

## Gotchas

- **Stop editing after `integrate`.** The queue rebases your worktree in
  place. A commit made after submitting is refused as `source changed`
  (and kept); a dirty tree is refused at submission.
- **A conflict is handed back mid-rebase.** Your worktree sits in a stopped
  rebase with markers. Resolve, `git add`, `git rebase --continue`, then
  resubmit. Resubmitting while the rebase is still in progress is refused.
- **A dirty main checkout holds everyone.** The dirty check ignores
  `.foreman/` (the app writes card files there constantly) but any other
  tracked edit in the main checkout stops the queue until it is committed
  or stashed. The head card shows `queued · held: destination dirty: …`.
- **Queue files live in `.git`**, at `<common-dir>/foreman/integration/`:
  shared by every linked worktree of the clone, never tracked, gone with
  the clone. Delete the directory to reset a wedged queue only when no
  Foreman is running.
- **Done arrives from disk, not from the thread.** The card flips Done on
  the tick after `integrated` is written, so a crash between the merge and
  the flip still lands the same message after restart. Two instances both
  trying is harmless: the second `done` is a no-op.
- **`wait --any` never returns 3.** An orchestrator waiting on any card
  sees a hand-back as still in progress; the worker's own `wait <id>` sees
  it at once.
- **A card with no new commits integrates as a no-op** and goes Done.
- **The check runs in the worker's worktree**, with that worktree's own
  `target/`. First runs cold-build.

## Key files

- `src/integrate.rs` — the GUI-free queue: `Request` / `Phase` / `Reason`
  / `Attempt` / `CheckRecord` (the file schema), `Queue` (`submit`,
  `cancel`, `remove`, `try_turn`, `requests`, `views`), `prepare_submission`
  + `repo_identity` (the submission preflight and repository identity),
  `run_turn` (turn loop + `recover`), `integrate_one` (the stages),
  `dest_preflight` / `source_preflight`, `rebase` / `fast_forward` (git
  refusal classification), `load_policy` + `run_check` + `run_child`
  (bounded, cancellable, Job-owned subprocesses), `IntegrationView` /
  `views` (the wire projection), `TurnHooks` (test barriers).
- `src/wm.rs` — `IntegrationDriver` (per-project coordinator state),
  `integration_tick` (re-read, bookkeeping, turn thread),
  `kanban_integrate` (the verb and the board's Submit/Cancel),
  `done_guard`, `integration_owns` (teardown / rm / discard / dispatch
  guards), `report_integration` (toasts).
- `src/kanban.rs` — `CardStore::integration` / `set_integration`,
  `CardLine::integration`, `wait_verdict` + `WAIT_NEEDS_RESOLUTION`,
  `dispatch_prompt` (the worker close-out).
- `src/control.rs` — `KanbanRequest::cancel`, `parse_kanban_integrate`,
  `HELP_KANBAN`.
- `src/board.rs` — the card status line and the detail page's Integration
  section (`BoardAct::Integrate` / `CancelIntegration`).
- `.claude/skills/foreman-kanban/SKILL.md`, `.codex/skills/foreman-kanban/
  SKILL.md` — the worker-facing instructions (embedded; rebuild to
  propagate).
