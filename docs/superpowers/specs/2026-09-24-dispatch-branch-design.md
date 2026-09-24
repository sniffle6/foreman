# Dispatch onto a feature branch without a worktree — design spec

Card d0ixyd, 2026-09-24. Extends `2026-09-15-dispatch-worktrees-design.md`
and the integration queue (`docs/integration-queue.md`). How-to lives in
`docs/kanban-board.md` (the **Branch mode** bullet); this file is the *why*.

## Problem

A worktree costs a cold build (minutes, for this repo) and a second copy of
the tree. For a small card, or a project where the human wants to watch the
work land in their own editor, that is the wrong trade. The only other
choice was "in place": no branch at all, so the worker commits straight onto
whatever the checkout has, and nothing integrates or checks it.

Branch mode is the middle: the card gets its own branch, `card/<id>`, in
the project checkout, and lands through the same integration queue.

## Decisions

- **A third dispatch mode, chosen per dispatch on the board.** The mode chip
  cycles `wt` → `branch` → `here`; the detail page shows three radio
  buttons; `kanban dispatch` takes `--branch` beside `--worktree` /
  `--no-worktree` (one of the three). The global setting still seeds only
  `wt` or `here`: branch mode shares the human's checkout, so it is always
  an explicit pick.
- **Stored as the existing `worktree` record with `in_place: true`**, `path`
  = the checkout root. Everything that already follows the record — Restart
  lock, status poll, `done` guard, `rm` and Cut hold-back, integration
  ownership, teardown on done/release/rm — works unchanged or with a small
  branch. The flag is skipped when false, so worktree card files are
  byte-identical. *Rejected:* a separate `branch` field (every consumer
  would need a second arm, and forgetting one would silently skip a guard).
  The cost is that every consumer that *removes* something must check the
  flag: teardown, Discard, the Worktrees page, and strays do.
- **Branch creation never touches files.** `git switch -c card/<id>` from
  HEAD leaves every uncommitted change in place. Restart uses plain
  `git switch card/<id>`, which refuses when it would overwrite a local
  change; that refusal aborts the dispatch with the card unchanged. Nothing
  ever stashes, resets, or forces. *Rejected:* refusing dispatch on a dirty
  checkout (the human's edits are exactly why they chose the shared
  checkout); auto-stash (the stash stack is shared across every worktree
  and session, and a stash that fails to reapply is lost work in disguise).
- **One branch card per checkout.** Dispatch refuses while the checkout is
  on another `card/…` branch, in either mode — a worktree dispatched then
  would record the other card's branch as its base.
- **The worker is told the checkout is shared.** The prompt forbids
  `git add -A`/`.`, stash, reset, restore, clean, and switching branches,
  and says to block if the human's changes stop a rebase.
- **Integration never rebases the shared checkout.** A rebase with the
  human's uncommitted edits either refuses or needs an autostash, and a
  conflict would leave the human's checkout mid-rebase. So the in-place
  turn requires the target to be an ancestor of the branch, and hands a
  moved base back as `base moved` for the worker to rebase. In practice the
  base rarely moves: the checkout is on the card's branch, so nothing lands
  on base through the queue meanwhile.
- **Landing is two ref writes, no checkout.** `git update-ref <target>
  <commit> <old>` (compare-and-swap: fails if the target moved), then
  `git symbolic-ref HEAD <target>`. Both refs name the same commit at that
  point, so the index and working tree — including the human's uncommitted
  edits — are already correct. *Rejected:* `git switch <base>` +
  `merge --ff-only` (two working-tree rewrites, each able to refuse over an
  overlapping edit halfway through).
- **Checks run in the checkout as it stands.** Uncommitted changes are in
  the tree the check sees. Accepted: the alternative is refusing to
  integrate whenever the human has an edit open, which makes branch mode
  useless for its main case. Submission therefore does not require a clean
  checkout either (the worker cannot tell its leftovers from the human's).
- **A held worktree card must not starve the branch card.** While the
  checkout is on `card/<id>`, every worktree request holds on "destination
  is on card/<id>". A hold used to end the turn; now, after a hold, the
  turn still tries each queued branch-mode request once. Landing it puts
  the checkout back and the held cards go on the next turn.
- **Teardown deletes the branch, never the checkout.** If the checkout is
  still on the branch, it switches back to base first — only when the
  branch is merged (or under Discard), and with a non-forcing
  `git switch`, whose refusal is reported as `Dirty`. An unmerged branch
  that is checked out is kept (`Unmerged`), so Release never yanks the
  human off unfinished work.

## Recovery

A dead owner's in-place request: if the commit is already on the target,
it is integrated, and a checkout still on the branch at that commit is moved
onto the target the way the turn would have. Otherwise it is queued again;
the next turn's preflight judges whatever changed.

## Non-goals

More than one branch card per checkout; the queue
rebasing a branch card; hiding the human's uncommitted edits from the
check.

## Testing

Git-backed, skipped without git: bring-up keeps tracked and untracked
changes; Restart reuses or switches, and git's overwrite refusal aborts
with the edit intact; dispatch refuses to stack on another card's branch;
status counts the checkout only while it is on the branch; teardown keeps
an unmerged checked-out branch, switches back and deletes a merged one,
and Discard drops unmerged commits; the in-place turn lands and leaves the
human's edit uncommitted; a moved base and a failed check hand back
without moving anything; a held worktree card does not block the branch
card; recovery finishes a half-done landing; the wm dispatch records the
branch, warns about the edit, and Release switches back.
