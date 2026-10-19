# Handoff: fleet token efficiency (chat + kanban skills)

Status: first pass landed on `main`. This doc is the pickup point for the
next pass. It is a dated handoff, not a feature doc; the durable facts live
in `docs/chat-delivery.md`, `docs/kanban-board.md`, and
`docs/integration-queue.md`.

## The problem

The user reported that fleets driven by the foreman-chat and foreman-kanban
skills burn a lot of tokens. Reading the code showed the per-message wire
format is already lean (one framed line per post, capped length, targeted
posts reach only their targets, no echo, no backlog dump). The cost is
elsewhere:

1. **Each delivery is a full agent turn.** An injected line wakes the
   receiver, which re-reads its whole context. Broadcast is the default, so
   one post to N members costs N turns, and replies fan out again.
2. **Card workers sat in the room as full members.** Every broadcast woke
   every `kanban dispatch` worker, though their prompt is the card and they
   never take part in chat.
3. **The orchestrator re-read the whole board after every wait.** `wait
   --any` did not say which card moved, so the skill told it to run `list
   --json`, which carries every card's body.
4. **Handed-back workers read the whole board** to find their own
   integration failure.
5. **The skills are large and stay in context** for every later turn.

## What landed

Two commits on `main`, fast-forwarded (no merge commit):

- `test(git-history): widen background-work deadlines` — fixes tests that
  failed on `main` under the full parallel suite (git-history stream reads
  and wm.rs worktree drains timing out). The cause was Windows process-spawn
  cost under load, not a hang. Two full runs green after.
- `feat(chat,kanban): cut agent token use in fleets`:
  - `Reception::TargetedOnly` in `src/chat.rs`; card workers join with it
    through `add_terminal_receiving` in `src/wm.rs` (every path that spawns
    a card worker goes through `dispatch_spawn`). Broadcasts, including
    `--now` broadcasts, skip them; targeted posts deliver.
  - `kanban wait` prints the deciding card's `list` line, and an
    `integration: {...}` line on exit 3 (`wait_verdict` / `wait_report` in
    `src/kanban.rs`, printed from the client loop in `src/control.rs`).
  - `kanban list <id>` over the existing optional `id` field.
  - Dispatch prompt exit-3 text points at wait's output.
  - Chat skills: "target by default" cost rule. Kanban skill split: core
    `SKILL.md` plus `worktrees-and-integration.md`, which
    `src/skills_install.rs` now installs (new `references` field on
    `SkillBundle`). Claude and Codex copies updated together.

## Do first

1. **Ship it to the machine.** Embedded skills reach agents only when a
   rebuilt foreman launches. The installed foreman under
   `%LOCALAPPDATA%\Programs\foreman` still serves the old skill text and the
   old `wait` until a release build is installed.
2. **Measure, because nothing has been measured yet.** The first pass was
   chosen by reading code. Take one recent fleet transcript from before the
   change and one after, and count injected `[chat ...]` lines per agent and
   whole-board `list --json` calls by the orchestrator. Without this, the
   next pass is guesswork.

## Candidate next steps

Ordered by expected payoff; confirm with the measurement first.

- **Slim the chat skill the same way.** It is still about 8 KB and loads
  whole. The "dispatch a team" recipe and worker-prompt template are only
  needed by an orchestrator building a chat fleet; move them to a reference
  file beside `SKILL.md` (the installer already supports this).
- **Review foreman-dispatch** for the same pattern; it was not touched.
- **Room-side folding of low-value posts.** Not built. Idea: hold
  non-urgent broadcasts for an idle receiver and deliver them as one paste,
  or let a poster mark a post as history-only. This changes delivery
  behavior, so it needs a user decision and a spec before code.
- **Shorter dispatch prompts.** Every card worker pays for its prompt's
  close-out block on every turn. Check whether it can shrink without losing
  the integration-queue rules.

## Known caveats

- `list <id>` against an older foreman GUI: the old server ignores `id` and
  returns the whole live board, and a shipped card is missing from it. The
  CLI and GUI are normally the same build.
- `wait --any` reports one card per return. If two finish between polls, the
  second shows on the next wait (order is sorted, so deterministic).
- Card workers no longer hear `--now` broadcasts. Steer them with a targeted
  `--now`.
- Pre-existing and untouched: an unused-import warning in the test build,
  and `cargo fmt --check` is not clean on `main` (the `HELD_RETRY_*` lines in
  `src/wm.rs`).

## Cleanup

Done. The work ran in two temporary worktrees, and both have been removed
along with their branches after main was fast-forwarded. Nothing was pushed:
`main` is ahead of `origin/main` locally.

Live check, run with the new CLI against the older running GUI: `wait` on a
done card printed its line and exited 0, and `wait` on a blocked card did
the same and exited 1. `list <id>` returned the whole board, which is the
documented old-server degradation.
