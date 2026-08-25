# Context layout — where foreman's knowledge lives

## What it does

Decides which file an agent reads to learn a given thing, so nothing is stored
twice. `CLAUDE.md` is a router, not a library.

## Why it exists

Foreman accumulated a project skill library, a long `HANDOFF.md`, a
`CONTEXT.md` glossary, ADRs, and a folder of feature docs — and `CLAUDE.md` still
carried a copy of the build loop, the gotcha list, and a per-file module map.
Skill descriptions are injected into every session automatically, so those
copies were paying context rent on every single turn to say things a skill
already said better. Worse, the copies drifted: `CLAUDE.md`'s module map was
newer than `HANDOFF.md`'s, and its "active work" note pointed at a branch that
had merged months earlier.

Anthropic's Claude 5 context-engineering guidance names this directly: keep
`CLAUDE.md` lightweight, spend its tokens on repo-specific gotchas, don't state
what an agent can see by reading the repo, and use progressive disclosure —
route to a skill instead of inlining it.

## How to use

| Question | File |
|---|---|
| What is this repo, and what will hurt me in the first 5 minutes? | `CLAUDE.md` |
| Vision, architecture narrative, module map, next phases | `docs/HANDOFF.md` |
| What does this word mean here? | `CONTEXT.md` |
| Why was this decided? | `docs/adr/` |
| How does feature X work? | `docs/<feature>.md` |
| How do I do task Y? | a project skill in `.claude/skills/` |

`CLAUDE.md` keeps only what fails *before* an agent would think to load a skill:
the destructive gotchas (running inside foreman, killing by name, DSR /
`VoidListener`), the structural invariants, the working agreement, and a
routing table.

## Gotchas

- **Don't re-fatten `CLAUDE.md`.** The instinct when a bug bites is to add a
  warning at the top. Add it to the matching skill instead and let the routing
  table do its job. Over ~110 lines means something belongs elsewhere.
- **Watch the plan-template ratchet.** The old implementation plans under
  `docs/superpowers/plans/` each ended with a step like "add one line to the
  `CLAUDE.md` architecture bullet". One line per plan, plan after plan, and
  that is literally how the module map got there. New plans update
  `docs/HANDOFF.md` §2 instead.
- **One home per fact.** If a skill and `CLAUDE.md` both say it, `CLAUDE.md`
  loses. Duplicates drift and the stale copy wins about half the time.
- **`AGENTS.md` routes by file path, `CLAUDE.md` routes by skill name.** Codex
  does not get the `.claude/skills/` descriptions auto-injected, so its table
  points at `SKILL.md` paths to read. Same library, one home, two access
  methods. If you add a project skill, add a row to both tables.
- **`epic-manager setup` will re-add its block.** It injects a block of MCP
  tool-usage instructions between `<!-- BEGIN/END EPIC MANAGER -->` markers.
  That was removed on 2026-08-24 — tool instructions belong in the tool
  descriptions, not in every turn's context. If it reappears, delete it again.

## Key files

- `CLAUDE.md` — the router.
- `docs/HANDOFF.md` — authoritative deep doc; wins on any conflict.
- `CONTEXT.md` — ubiquitous language.
- `.claude/skills/` — the project skill library (`ls .claude/skills/`).
- `AGENTS.md` — Codex counterpart; thinned the same way (2026-08-24).
