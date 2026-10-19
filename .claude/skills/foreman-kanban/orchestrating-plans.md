# Kanban reference: orchestrating a plan

Part of the foreman-kanban skill. Read it when you run or orchestrate a
plan: dispatching a plan's cards wave by wave, reviewing finished workers,
and keeping the board right as you learn.

"Run / orchestrate the plan" means every worker is a card started with
`kanban dispatch`. Never `open`, never `git worktree add` by hand — those
skip the claim, the `Card:` trailer, the integration queue, and the board.
You coordinate; you do not do the cards' work yourself.

## The loop

    find plan -> dispatch current wave -> wait --any <ids> -> review the
      worker -> act on the board -> close its terminal -> dispatch -> ...

Keep a short ledger as you go: card id, terminal id, state, and every board
change you made. The ledger replaces re-reading the board.

1. **Find the plan.** One `list --json` at the start: a card's plan is
   `planned.name` and `planned.wave`. Copy the name from a card and note each
   wave's card ids. Do not re-list the whole board later; it costs every
   card's body.
2. **Current wave** = the lowest `planned.wave` among the plan's cards that
   are not `done`. Dispatch only that wave's `backlog` cards; never start a
   later wave early.
3. **Dispatch one at a time**, recording each reply's `terminal`:

       & $env:FOREMAN_EXE kanban dispatch <id> --agent claude

   `--agent` is required and cards have no agent field: follow the user's
   instruction, else `claude`.
4. **Wait on your in-flight cards by id**:
   `kanban wait --any <id> <id> ... --timeout 600`, listing every ledger
   card you have not yet handled. It prints the line of the card that moved
   and reports at once a listed card that finished while you were busy, so
   nothing is missed. Never use bare `wait --any` here: it only sees cards
   still in progress when it starts, and it also wakes on other people's
   cards. Do not run `integrate` or `done` for a worker — its prompt already
   does.
5. **Handle the card by its state** (next sections): review, act, close.
   Then drop it from the next wait's id list.
6. **Next**: when every card of the current wave is `done`, recompute the
   current wave (your board edits count) and dispatch it. Stop when no plan
   card is left that is not `done`, then report (see the end).

## A card came back done: review the worker

The worker's terminal holds its final report. Read only the tail:

    & $env:FOREMAN_EXE snapshot --terminal tN --tail 40

First check it is finished. A finished pane ends on the agent's final
message with an empty input prompt below it. A spinner, `esc to interrupt`,
or a tool call still printing means it is still in its last turn: handle
other cards and snapshot it again later. Raise `--tail` only when the
report is cut off. Do not read its transcript or diff unless the report is
ambiguous about something you must act on.

Its dispatch prompt makes it end on four labeled lines; read those:

- **Landed:** what shipped. Check it matches the card; a gap is a
  follow-up.
- **Follow-ups:** deferred work, TODOs, bugs it found but left, checks it
  could not run.
- **Plan impact:** something that makes a later card wrong, redundant,
  already done, or dependent on new work.
- **Decisions:** what the human must decide, and assumptions to confirm.

A worker that skipped the lines still usually says these things; read its
last message for them.

## Act on the board

Turn each finding into one board change, or into a line for the human:

| Finding | Board action |
|---|---|
| Follow-up inside the plan's goal | `add` it, then `edit <new> --plan "<plan>" --wave N`: the current wave when the current wave's remaining cards need it, else the next wave |
| A later card must now wait for new work | `edit <later> --wave N` to move it behind that work; renumber later waves the same way |
| A later card is already done or no longer needed | `rm` it if it is `backlog`; note why for the report |
| A later card's body is now wrong | `list <id> --json`, then `edit <id> --body "<full corrected body>"` (the body replaces, never appends) |
| Work outside the plan's goal | `add` it with no plan; report it |
| Needs a human decision | No board change. Do not dispatch cards that depend on it; report it |

Rules:

- Act only on what the worker reported or what the board shows. Do not add
  speculative cards, and do not grow the plan past its goal.
- A card you add will be dispatched cold: its body is the worker's whole
  prompt. Give it what to do, file/doc pointers, acceptance criteria, and
  the gate commands, the way the plan's other cards do.
- Never edit, re-wave, or `rm` a card that is `in_progress`; its worker is
  already running from the old body. `rm` also refuses a card whose tree is
  dirty or ahead of its base.
- Record every change in the ledger.

## Close the worker's terminal

When the card is `done`, the pane is finished, and you have taken what you
need from it, close it before dispatching more:

    & $env:FOREMAN_EXE close tN

A finished worker's pane otherwise stays open forever, and a worktree card's
teardown waits for that pane to close. Rules:

- Always pass the id. A bare `close` closes YOUR OWN pane and kills you.
- Close only terminals from your ledger, and only for `done` cards.
- Never close a `blocked` worker's pane: the human reads it to decide.

## A card came back blocked, orphaned, or removed

- **Blocked**: snapshot the tail as above. If the blocker is a board
  problem you can fix from the report (the card needs splitting, it depends
  on a card in a later wave, its body points at the wrong file), make that
  board change and report it. Otherwise report the reason to the human.
  Leave its pane open either way. Restart is a board action — do not route
  around it with `open`.
- **Orphaned** (its terminal died) or **removed**: there is no pane to
  review. Report it.

Keep running cards that do not depend on it. Do not dispatch cards that
need the blocked card's work.

## Dispatch errors

- `foreman did not respond` or a bring-up error: worktree bring-up must
  finish inside foreman's 5 s reply window. A slow one spawns nothing; a
  reply lost after a spawn is undone and the card returns to Backlog. Check
  `list <id>`, then retry the same dispatch once.
- `held by live Session`, `integration is …`, not Backlog, cmd-shim: a real
  refusal. Report it. Do not retry blindly and do not fall back to `open`.

## Steering a running worker

A worker's card is its whole brief, and every post costs it a full turn.
Put scope, file ownership, and constraints in the card body before
dispatch, not in chat. Then:

- Post to a worker only when something changes what it must do next: a new
  owner, a changed plan, a decision it is waiting on.
- Never restate its card, the plan, or rules it already has. Never relay
  what it can read itself (the board, the repo, `chat --history`); name
  where it lives instead.
- One post per worker per change. Fold related decisions into it; do not
  follow up to refine the last post.
- Do not invent a coordination protocol (leases, grants, release notices)
  on top of the cards. If two cards touch the same file, fix it on the
  board before dispatch: re-wave one, or give the file to one card's body.
- Expect no acknowledgement. Silence means the worker is working.

## A worker that never comes back

A worker stuck on a permission prompt looks exactly like a working one —
`wait` only times out (exit 2). On a timeout, snapshot each ledger card's
pane with `--tail 30`. Report a prompt or a stall to the human; do not
answer the worker's permission prompts yourself. Worktree cards cold-build
their own `target/` and land through one queue a card at a time, so a wave
lands slowly.

## When the plan is finished

Report once, briefly: cards done, every board change from the ledger with
its reason, and every item that needs the human.
