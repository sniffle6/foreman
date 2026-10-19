# Kanban reference: worktrees, integration, and shipping

Part of the foreman-kanban skill. Read it when your prompt has a
`# Workspace` section, `wait` exited 3, the queue is held, or you are
inspecting worktrees, strays, or a Cut.

## More verbs

```powershell
& $env:FOREMAN_EXE kanban worktrees --stray --json
& $env:FOREMAN_EXE kanban integrate a3f8k2
& $env:FOREMAN_EXE kanban list --shipped v0.5.0 --json
& $env:FOREMAN_EXE kanban cut v0.5.0
& $env:FOREMAN_EXE kanban uncut v0.5.0
```

- `worktrees` — every worktree foreman made for the project, probed live:
  `<name> <state> <title> [wt …]` for a card's tree, `<name> no card [wt …]`
  for a stray (a tree no card owns). `--stray` keeps only strays; `--json`
  emits one object per line (name, path, branch, base, card, status).
- `integrate` — submit a worktree card's committed work to the repository's
  integration queue, or `--cancel` to withdraw it; `--json` prints the
  integration object.
- `cut NAME` / `uncut NAME` — the ship ritual, run by a human or a release
  script after tagging: `cut` moves every ungrouped Done card into Version
  NAME; `uncut` puts them back. Codex is not expected to Cut.

## Worktrees: where a dispatched card runs

A card started from the board (or `kanban dispatch`) may run in its own git
worktree — `<repo>/.foreman/worktrees/<id>` on branch `card/<id>` — so no
two workers share a checkout. The choice is made at dispatch time: the
board's `wt` / `branch` / `here` chip, or `dispatch --worktree` / `--branch`
/ `--no-worktree`; both default to the app setting. A card that already has
a worktree or a branch always restarts in it (a conflicting flag is
refused). If you were dispatched into one, your prompt has a `# Workspace`
section saying so; if it does not, you are in the project cwd.

**Branch mode** (`branch`) puts `card/<id>` in the project checkout itself,
no worktree. Your `# Workspace` says "There is no worktree". The checkout may
hold the human's uncommitted changes, so: stage only your own files by name
(never `git add -A` / `git add .`), and never stash, reset, restore, clean,
or switch branches. Integration is the same `integrate` + `wait`, but the
queue never rebases the shared checkout: if the base moved, `wait` exits 3
with reason `base moved` and you rebase yourself (block if the human's
changes stop the rebase). On success the checkout is back on the base with
the human's changes untouched. Only one branch-mode card can hold the
checkout at a time; while it does, worktree cards' integrations hold.

What that changes for you:

- **Never merge into the main checkout yourself, and never run `done` on a
  worktree card.** Commit everything, then hand the card to the integration
  queue (`integrate` + `wait`, both lines are in your prompt); foreman
  rebases, checks, fast-forwards, and marks the card Done. `done` on a
  branch with commits not yet on its base is refused and points at
  `integrate`. **Queued is not Done.**
- **Never stage `.foreman/`.** The worktree carries a stale copy of the
  board's card files; `git add -A` would commit them.
- **`done` queues a non-forcing teardown** that waits until your terminal
  pane is closed. A Done card still showing its branch until then is normal.
  A dirty tree or an unmerged branch is kept, never deleted — only a human
  can discard.
- **`rm` refuses** a card whose tree is dirty or ahead of base.
- **`list`** appends `[wt card/<id> +ahead -behind dirty|missing]` to a
  worktree card's line; `--json` adds `worktree` (path, branch, base) and a
  derived `worktree_status`.
- **`worktrees`** is the whole picture: a tree whose card was removed,
  deleted by hand, or left on another branch is a stray only this verb (and
  the board's Worktrees page) can see. Cleanup of a stray is human-only.

## Integration queue: how a worktree card lands

Foreman owns one integration turn per repository at a time, so two workers
never race onto the shared checkout. The whole close-out for a worktree card
is two commands, run from anywhere inside your Session:

```powershell
& $env:FOREMAN_EXE kanban integrate <id>
& $env:FOREMAN_EXE kanban wait <id> --timeout 1800
```

`integrate` needs a clean worktree on `card/<id>` with no rebase or merge in
progress; it replies with the request's state (`queued #N`) and returns at
once. **Stop touching the worktree while it is queued or integrating**:
foreman rebases it onto the card's base, runs the project's checks
(`.foreman/integrate.json`: `{"check": ["cargo","test"], "timeout_secs": 1800}`;
no file means no checks), fast-forwards the base, and marks the card Done.

Then branch on `wait`'s exit code:

- `0` — integrated and Done. You are finished; do not run `done`.
- `3` — handed back: the rebase conflicted or a check failed. `wait` prints
  an `integration: {...}` line with `reason`, `detail`, and `next`
  (`list <id> --json` shows the same object). Fix it in your worktree — a
  conflict is left mid-rebase for you: resolve, `git add`,
  `git rebase --continue` — commit, then run `integrate` and `wait` again.
  A resubmission joins the tail of the queue.
- `2` — timed out (still queued, or a long check): run `wait` again.
- `1` — blocked, orphaned, or removed: a human is involved; stop.

Resubmitting the same commit is harmless. Committing more after submitting
makes the submission stale: it comes back as `needs resolution · source
changed`; resubmit. A dirty main checkout or one on the wrong branch holds
the whole queue (`queued · held: …`) until the human fixes it. **Held is not
blocked — do not `block` the card.** The queue retries by itself; keep
running `wait`. `integrate <id> --cancel` withdraws your request. The board
shows the same substates and offers Submit / Resubmit / Cancel.
