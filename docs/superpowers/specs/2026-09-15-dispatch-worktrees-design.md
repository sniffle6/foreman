# Dispatch into per-card git worktrees — design spec

Settled with the user 2026-09-15. Extends the kanban board
(`2026-08-28-kanban-board-design.md`): card dispatch gains a git worktree per
worker, the card shows the worktree's state, and close-out integrates and
tears it down. Nothing in the terminal, chat, or sessions panel changes.

## Problem

Card-dispatched workers all run in the project cwd — one shared checkout.
Two workers editing the same file at once means one of them commits the
other's half-finished hunks. It happened on 2026-09-15: commit `86e95db`
(header title fade) staged `src/wm.rs` whole and swept in four hunks of an
unrelated card's work, leaving two commits that do not build standalone and
a commit message that lies about what it contains. Nothing downstream can
undo that; only isolation at dispatch time prevents it.

## Decisions (from brainstorm)

- **One worktree per dispatched card**, created by the app at dispatch time,
  passed as the Session's cwd. Skill-driven `foreman open` is unchanged — it
  already takes `--cwd`; callers that want a worktree make one.
- **Teardown model 1: the worker integrates before close-out.** The dispatch
  prompt tells it to rebase onto the base branch and fast-forward the base
  from the worktree. `done` removes the worktree and deletes the branch, and
  both git commands refuse if anything is dirty or unmerged, so the app never
  destroys work. *Rejected:* human-merges-from-the-card (Done cards
  accumulate live worktrees, exactly the "nobody tore it down" state this is
  meant to avoid); app-auto-merges-on-done (fast-forward only, so it silently
  fails whenever the base moved, which with parallel workers is most of the
  time).
- **The card is the state surface, not chat.** A worktree is 1:1 with a card
  and has a lifecycle; chat is a transcript. Branch, dirty flag, and
  ahead/behind render on the card face and in `foreman kanban list`. No
  separate `foreman worktree` verb: `git worktree list` already exists.
- **On by default**, gated by one persisted setting, and silently skipped
  when the project cwd is not inside a git repository.

## Goals and non-goals

Goals: no two workers share a checkout; the human can see every live
worktree and whether it is integrated without leaving the board; a finished
card leaves no worktree behind; a failed or refused step never loses work.

Non-goals (v1): worktrees for `foreman open`; warming the new worktree's
build cache; any merge-conflict UI; nested or non-git projects; a settings
axis for the worktree location or branch naming.

## Card schema

`Card` gains one optional field. It is absent on the wire and in files when
`None`, so v1 card files and `list --json` output are byte-identical for
cards without a worktree.

```json
"worktree": {
  "path": "H:/claude code/foreman/.foreman/worktrees/etxvs5",
  "branch": "card/etxvs5",
  "base": "main"
}
```

- `path` — absolute, the worktree root.
- `branch` — the branch the worktree has checked out; always `card/<id>`.
- `base` — the branch that was checked out in the project's main checkout
  at dispatch time. The worker rebases onto it and fast-forwards it. Recorded
  rather than assumed so a project on a feature branch integrates there,
  not into `main`.

The field lives on the card, not on the claim: `block` clears the claim but
must keep the worktree for the human and for Restart. `done` clears both.

Status (dirty, ahead, behind, missing) is **derived, never stored** — the
same rule as orphaned-ness. It is recomputed by a poll while a board is
visible and exists in no file.

## Bring-up (dispatch)

Runs inside `drain_board_acts` for `BoardAct::Dispatch`, before the spawn,
when `Settings::dispatch_worktrees` is on. Every step is a git subprocess
with `CREATE_NO_WINDOW` (the pattern in `terminal_titles.rs`).

1. `git -C <project cwd> rev-parse --show-toplevel` → `root`. Not a repo →
   dispatch in place, as today, with no toast (the setting is a preference,
   not a promise).
2. `git -C <root> symbolic-ref --short HEAD` → `base`. Detached HEAD →
   dispatch in place with a warning toast ("no branch checked out; dispatched
   without a worktree").
3. `path = <root>/.foreman/worktrees/<id>`, `branch = card/<id>`.
4. Ignore the directory locally: if `git check-ignore -q <path>` fails, append
   `.foreman/worktrees/` to the file `git rev-parse --git-path info/exclude`
   names. This is per-clone and never committed — the app does not edit the
   user's `.gitignore`.
5. Reuse or create:
   - The card already has a `worktree` and `git worktree list --porcelain`
     lists its path → reuse (Restart on a Blocked or orphaned card resumes in
     the same tree, with the worker's uncommitted state intact).
   - The branch exists but no worktree does (a previous teardown removed the
     tree but the branch was unmerged) → `git worktree add <path> <branch>`.
   - Otherwise → `git worktree add -b <branch> <path> HEAD`.
6. Spawn via the existing `add_terminal_cmd` with `cwd = Some(path)`. The
   environment is unchanged: `FOREMAN_PROJECT_ID` still names the project, so
   `foreman kanban done` reaches the project's board in the main checkout,
   not the stale `.foreman/tasks/` copy inside the worktree.
7. Record `worktree` on the card in the same `claim_for_dispatch` write that
   records the claim.

Any git failure aborts before the spawn: the card is unchanged, the error
toast carries git's stderr first line, and nothing is half-created except
possibly a directory git already cleaned up. Step 5 failing after step 4
succeeded leaves a harmless exclude line.

The bring-up is synchronous on the UI thread. `git worktree add` on this
repo is well under a second; the cost that is not is the worker's first
`cargo build`, which is the worker's problem (see Gotchas).

## The prompt template

The existing template gains a `# Workspace` section and two integration
lines in close-out, only when the card has a worktree. Cards dispatched in
place render exactly today's text. `closeout_style()` still selects
`foreman` versus `& $env:FOREMAN_EXE`; the git lines are style-independent.

```
You are a worker Session dispatched from card {id} on this project's board.

# Task: {title}

{body}

# Workspace
You are in a git worktree at {path}, on branch {branch}, based on {base}.
The main checkout at {root} is shared with other workers: never edit files there.
Leave .foreman/ untouched and never stage it.

# Close-out (required)
Integrate first, from inside your worktree:
    git rebase {base}
    git -C "{root}" merge --ff-only {branch}
Resolve rebase conflicts yourself. If the fast-forward is refused, rebase again and retry.
If git refuses because the main checkout has uncommitted changes in files you touched, block instead of forcing.
When the work is complete, run:    foreman kanban done {id}
If you are stuck and need a human: foreman kanban block {id} --reason "<one line>"
Do not end the session without running one of these.
```

`{root}` is quoted because this repo's own path has a space in it.

## Teardown

Teardown is one function, `Store::teardown_worktree(id) -> TeardownOutcome`,
run on a **background thread** (removing a worktree that holds a `target/`
directory deletes gigabytes and takes seconds; the UI must not stall). It
never forces:

1. `git -C <root> worktree remove <path>` — refuses on a dirty tree.
2. `git -C <root> branch -d <branch>` — refuses on an unmerged branch.
3. `git -C <root> worktree prune` — housekeeping, always safe.

| Outcome | Card `worktree` field | Board |
|---|---|---|
| Both removed | cleared | nothing to show |
| Tree dirty | kept | badge `dirty`, toast "worktree kept: uncommitted changes" |
| Branch unmerged | kept (path may be gone) | badge `unmerged`, toast "branch kept: N commits not on {base}" |
| git missing / errored | kept | error toast with stderr |

The card keeps its `worktree` field until the thread reports success; the
wm clears it on the next frame through a channel, with a store write. A Done
card with a leftover worktree is therefore visible as such in the Done
column — that is the point.

**Retry-safe (amended 2026-09-17).** `git worktree remove` is not atomic: on
Windows a directory that is some process's cwd survives the final rmdir
after git has deleted its contents and its registration. Each step judges
what is actually left rather than the previous step's assumed state:

- Step 1 checks registration (`git worktree list --porcelain`) separately
  from directory existence. Registered + present → `worktree remove`;
  registered + gone → `prune`; unregistered → remove the directory only if
  it is empty and its parent is `<root>/.foreman/worktrees/`. A nonempty
  unregistered directory is kept and reported (git no longer tracks it, so
  nothing inside is provably ours) — even under Discard.
- (Amended 2026-09-24.) Before `worktree remove`, a registered tree must
  pass a hold probe: rename it to a sibling and back. A hold below the top
  directory (a running exe from the tree's `target/`, a subdirectory cwd)
  otherwise makes git delete part of the tree, stop, and drop the
  registration regardless — a nonempty leftover the rule above keeps
  forever. A failed probe is the errored row with the tree still registered
  and whole, so a retry after the process exits completes.
- Step 2 treats an already-missing branch as done; an existing branch keeps
  git's own `-d` / `-D` protection.
- Nothing left at all is `Both removed`, so a repeated teardown clears the
  field instead of erroring.

Which transitions tear down:

- `done` → teardown.
- Board **Release** (In Progress or Blocked → Backlog) → teardown.
- `rm` → a synchronous pre-check first (the status-poll commands, ~100 ms):
  dirty or ahead of base → `rm` **errors** ("card has unmerged work in its
  worktree; discard it first") and the card file stays. Clean and merged →
  the card file is deleted and teardown runs as for `done`. Deleting a card
  must not orphan a branch nobody can find, and teardown itself is
  asynchronous, so the verdict cannot wait on it.
- `block` → keep everything. The human or a Restart needs the tree.
- Orphaned (worker died) → keep everything; Restart reuses it.

**Discard** is the one forcing action and it is human-only: a Done, Blocked,
or orphaned card's detail page offers "Discard worktree", which runs
`worktree remove --force` then `branch -D`, after the app's standard
confirm. It is deliberately not a wire verb: an agent should never be able
to delete another agent's unmerged work.

## Status poll

While a board is shown, alongside the existing 2 s staleness poll, every
card with a `worktree` field is checked every 5 s on a background thread:

- `git -C <path> status --porcelain --untracked-files=no` → `dirty` (any
  output). Path missing → `missing: true`, skip the rest.
- `git -C <root> rev-list --left-right --count <base>...<branch>` →
  `behind`, `ahead`.

Results land in a `HashMap<card id, WorktreeStatus>` on the store, replaced
wholesale each round; a card whose worktree field was cleared drops out. A
hidden board polls nothing, same as staleness today. The first render after
opening a board may show the badge without numbers for one round.

## Surfaces

**Card face** (all columns): one compact line under the title when the
card has a worktree — branch, then `+N` ahead, `-N` behind, and a dot when
dirty. Theme tokens only; `dirty` and `unmerged` use the existing attention
colour, `missing` uses dim. Nothing on cards without a worktree, so the
board's height for those is unchanged.

**Detail page**: the full path, base branch, the same status, and the
Discard action (Done, Blocked, or orphaned only).

**`foreman kanban list`**: the human line appends
`[wt card/etxvs5 +3 -1 dirty]` after the claim; `--json` adds the stored
`worktree` object plus a derived `worktree_status` object, both skipped when
absent.

**`foreman kanban wait`** is unchanged: it watches card state, not the
worktree.

## Settings

One new persisted axis in `Settings` (`config.rs`): `dispatch_worktrees:
bool`, default `true`, with a row in the settings menu. Off means dispatch
behaves exactly as before this spec. Follow the foreman-config-and-flags
checklist for a persisted setting (serde default, settings row, doc).

## Testing

Pure seams, no git required:

- `worktree_layout(root, id, base) -> (path, branch)` — path and branch
  naming.
- `dispatch_prompt` with a worktree renders the template above verbatim,
  in both closeout styles; without one renders today's text verbatim.
- `teardown_verdict(remove_result, branch_result) -> TeardownOutcome` —
  the four-row table above as an exhaustive test.
- `parse_status(porcelain, rev_list) -> WorktreeStatus`.
- `human_line` / `json_line` with and without the worktree fields;
  `list --json` byte-identical for cards without one.
- Card file round-trip: a v1 file without `worktree` deserializes and
  re-serializes unchanged.

Git-backed integration tests use a temp repo built with `git init` and
`git commit --allow-empty`, and are skipped (not failed) when `git` is not
on PATH: bring-up creates a listed worktree on `card/<id>`; reuse on
Restart; teardown after a merged branch leaves nothing; teardown with an
uncommitted file keeps the tree and reports `dirty`; teardown with an
unmerged commit keeps the branch and reports `unmerged`.

No GUI test for the badge; the build-screenshot flow verifies it.

## Gotchas

- **Cold build per worktree.** Each worktree has its own `target/`; the
  first build costs minutes. `docs/dev-launcher.md` forbids sharing a target
  dir across worktrees (hazard 4) and that still stands. Accepted: minutes
  of compile beat corrupted commits.
- **The worktree carries a stale `.foreman/tasks/`.** Close-out routes
  through the pipe to the project's checkout, so the board is unaffected,
  but a worker that runs `git add -A` commits stale card files and the
  fast-forward then carries them into the base. The prompt says never to
  stage `.foreman/`; a reviewer should still watch for it.
- **The fast-forward rewrites the user's main checkout.** That is the design:
  integration is a ref update plus a checkout of the touched files. Git
  refuses if the user has uncommitted edits in those files, and the prompt
  tells the worker to block rather than force in that case.
- **Card-id worktree names are only unique per repo.** Two projects on the
  same repo (unusual) would collide on `card/<id>`; ids are random, so the
  chance is negligible, and `git worktree add` fails loudly rather than
  reusing.
- **The installed foreman sees the same repo.** A dev-fleet foreman and the
  installed one share `.git`; worktrees created by either are visible to
  both, and teardown from either is safe because it never forces.

## Vocabulary

- **Worktree** — a card's private checkout under `.foreman/worktrees/<id>`,
  on branch `card/<id>`.
- **Base** — the branch the main checkout had at dispatch; the integration
  target.
- **Integrate** — the worker's rebase onto base plus fast-forward of base.
- **Teardown** — the non-forcing remove-and-delete `done`/release/`rm` run.
- **Discard** — the human-only forcing teardown.

## Key files (expected)

- `src/kanban.rs` — `Worktree`, `WorktreeStatus`, `teardown_worktree`,
  status poll, prompt template, list lines.
- `src/wm.rs` — bring-up in `drain_board_acts`, teardown dispatch on the
  `done`/release/`rm` verbs, channel drain for teardown results.
- `src/board.rs` — card-face line, detail-page fields, Discard action.
- `src/config.rs`, `src/settings_menu.rs` — `dispatch_worktrees`.
- `docs/kanban-board.md` — user doc update (one section, not a new file).
