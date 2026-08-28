# Kanban board — brainstorm record (pre-spec)

Decisions settled in conversation with the user on 2026-08-28, ahead of a
proper spec session. Records the *why* and the rejected alternatives. Not a
plan; nothing here is built.

## What it is

A per-project panel showing work as **cards** in columns by task state, with a
button on each card that spawns an agent in a new Session with a prompt built
from the card. Closes the loop both ways: human clicks card → agent starts;
agent finishes → agent moves its own card via CLI → board updates live.

## Decisions and their reasons

**Native board for work-in-flight; GitHub Issues stay the durable tracker.**
A full native issue tracker (comments, labels, dependency graphs) would
rebuild GitHub Issues badly and lose its free web/mobile UI, and the wayfinder
workflow leans on GitHub-native sub-issues and dependencies. Routing rule:
matters after the session ends → GitHub issue; work-in-flight coordination →
board card. A card is promoted with one `gh issue create`.
*Rejected:* native-only tracker; gh-only (no live in-app surface, dead for
projects with no GitHub remote, network latency and auth flake per call).

**Storage: in-repo, one file per card — `.foreman/tasks/<id>.json`.**
The board travels with the clone; opting out is that repo's `.gitignore`
choice, not foreman's. File-per-card because branch merges then conflict only
when two branches touched the *same* card, not on every neighboring line of a
single array file.
*Rejected:* `%APPDATA%` (clean tree but machine-local and invisible);
single `tasks.json` array (every insert dirties neighbors → constant merge
conflicts); JSONL (better than an array, worse than file-per-card).
*Accepted cost:* card moves dirty the working tree; branch switches rewrite
cards under the running app.

**All writes go through the pipe (`foreman kanban` verbs); files are durable
output, not the API.** Single writer lets the app validate transitions and
reject garbage with a clean error, repaints the panel on its own write instead
of a file-watcher tick, and matches how chat/send/snapshot already work.
Reads may hit the files directly. The app must still tolerate the files
changing under it (pulls, branch switches, a rogue direct edit) — re-read and
treat the file as authoritative over its in-memory copy.
*Rejected:* agents editing card files directly (races the GUI; "mostly safe"
concurrency).

**Dispatch-from-card is a thin wrapper over existing dispatch plumbing** (the
same path `foreman open` drives — see the foreman-dispatch skill). The button
serializes the card into a prompt template and spawns the Session.

**Card ↔ Session linkage is recorded at dispatch.** The card stores the
spawned terminal's id. Pays off twice: clicking a card jumps to its Session
(reuse `surface_target` in `src/wm.rs`), and when agent-state badges land
(see foreman-agent-state-campaign) the badge renders on the card.
Reconciliation rule required: Session gone while card is in progress → card
flags itself, never squats silently.

**Teaching agents, three tiers, cheapest first** — chosen to keep ambient
context near zero:

1. The dispatch prompt embeds the exact close-out commands (`foreman kanban
   done <id>` / `block <id>`), so a card-spawned agent needs zero discovery.
   (A card-spawned agent never runs `start` — the dispatch itself claims the
   card and records the Session id.)
2. A `foreman-kanban` skill (the foreman-chat/foreman-dispatch pattern:
   embedded via `src/skills_install.rs`, installed globally, trigger gated on
   `FOREMAN=1`) covers agents not spawned from a card. Costs one description
   line until invoked.
3. `foreman kanban --help` is ground truth for flags; the skill defers to it.

*Rejected:* explaining the board in CLAUDE.md (every session pays, whether or
not it touches the board; duplicates tiers 1–2).

**Orchestration: workers are dispatched Sessions, and the board is the
coordination channel — not chat, not subagents.** An orchestrator (human or
agent) claims/dispatches cards, one worker Session per card, sequential or
parallel as it chooses; each worker's close-out write (`done`/`block`) is the
completion signal. Card state is durable and needs no live handshake, so the
chat system's delivery reliability is not a dependency of this workflow.
*Rejected:* workers as Claude Code subagents — invisible (no Session to link
the card to, no badge, no jump-to-terminal, no human intervention on a stuck
worker), and they die with the orchestrator. Subagents remain fine for small
fan-out (research, verification) below the card threshold: if work deserves a
card, it deserves a Session.
*Follow-on verb:* `foreman kanban wait <id>` (and/or `wait --any`) — block
until a card leaves in-progress, with a timeout — so an orchestrator gets a
synchronous primitive instead of polling `list`.

## v1 scope fences

- Fixed columns: Backlog / In Progress / Blocked / Done. Configurable columns
  are config + persistence + migration surface for no v1 value.
- No drag-and-drop between columns; cards move because state changes (button,
  CLI, agent). The sessions-panel reorder work showed what drag costs.
- One fixed prompt template with card fields interpolated. No template system.
- Card creation is mostly CLI/agent-side; the panel gets at most a title-only
  quick-add. No modal card editor.

## Open questions for the spec session

- Card schema: id scheme, fields (title, body, state, claimed-by Session id,
  blocked reason, created/updated), and what the reconciliation flag looks
  like.
- Verb surface: exact `foreman kanban` subcommands and their pipe request
  shapes (wire-compat v1 gate applies — see foreman-change-control). The set
  includes `start <id>` for self-service pickup: an agent not spawned from a
  card claims one, and the single-writer validation rejects `start` on an
  already-claimed card — that rejection IS the two-agents-one-card guard.
- Prompt template contents, including the close-out instructions.
- Panel placement and interaction: its own tiled window like the sessions
  panel, or a view inside the project?
- Vocabulary: claim **board** and **card** in CONTEXT.md with the seam commit;
  "task manager" is already taken by the window-switcher panel and "task" is
  ambiguous with it.
- Whether `.foreman/` gets a README or schema-version marker for forward
  compat.
