# Foreman Agent Skills Split — dispatch vs chat

**Date:** 2026-06-10
**Status:** Approved
**Problem (observed live, screenshot 2026-06-10):** an orchestrator session
invoked the foreman-dispatch skill, then still spent 5m34s / 23.5k tokens
re-deriving facts from source before dispatching — reading chat internals,
spawn code, and probing how `claude` resolves because the skill gave no
authoritative answer on quoting, and had no recipe for the composite job
("dispatch a team that discusses X in the chat room"). Discovery is fine;
the skill content doesn't stop the investigation spiral.

## Decision

Split by responsibility (user's call):

- **`foreman-dispatch`** — ONLY how to launch a terminal with an agent and a
  prompt. Slimmed: all chat material moves out.
- **`foreman-chat`** (new) — what the group chat is, how to use it, how to
  set it up, how to launch agents into it, best practices.

Both live in `.claude/skills/<name>/SKILL.md`. Each opens with a stop-sign:
*"This skill is complete — do not read foreman source or docs to do this."*

## `foreman-dispatch` (rewrite)

Keeps, in order:

1. Stop-sign; FOREMAN=1 precondition.
2. The two dispatch commands (watchable interactive / fire-and-forget `-p`),
   invoked via `& $env:FOREMAN_EXE` (PowerShell) / `"$FOREMAN_EXE"` (bash).
3. Reply JSON = terminal id + project; fire-and-watch — do not poll.
4. `--project` / `--cwd` / `--title "agent · <label>"` conventions.
5. **Quoting truth (new, authoritative — verified in `Session::spawn_argv`):**
   arguments pass per-argument even on the npm-shim `cmd /c` retry, so
   spaces, `&`, `|`, backticks, `$` in prompts are safe; the one risky
   character is a literal `"` inside a prompt — rephrase or use single
   quotes. Variadic-flag trap stays (prompt immediately after `claude`).
6. `-p` silence semantics (banner, nothing until the final answer,
   transcript recovery path).
7. Closing pointer: "Coordinating multiple workers? → foreman-chat skill."

Cut (moves to foreman-chat): chat usage section, mention rules, the standing
convention paragraph, fire-and-forget chat reporting pattern.

Frontmatter description: unchanged trigger (dispatch/spawn an agent in a new
visible terminal while inside foreman).

## `foreman-chat` (new)

Sections, in order:

1. Stop-sign; FOREMAN=1 precondition.
2. **What it is** (3 lines): one room per project; posts are injected into
   every member's PTY as typed input (push — members never poll); the
   transcript is the roster; the human watches/posts via the chat window.
3. **Recipe: post / read / target** — command block: post, `--history [N]`,
   `--to t3` (repeatable, `@` tolerated), leading-`@tX` sugar (leading run
   only; mid-sentence is prose), `@you` (flags the human, interrupts no
   agent), `--` semantics, exit codes (2 client parse, 1 server error).
4. **Recipe: dispatch a chatting team** — the composite job: N ×
   `foreman open` lines plus a ready-made worker prompt template with a
   `<task>` placeholder and the standing convention paragraph baked in.
   Membership facts inline: dispatched workers auto-join; anything else
   joins on first post; `--history` never joins; interactive workers receive
   posts, `-p` workers can only send.
5. **Worker prompt template** (verbatim copy block) — task slot + the
   standing convention (framing format, silence by default, no
   acknowledgements, 1–3 actionable sentences, no report-pasting, targeting
   guidance, `FOREMAN_EXE` invocation, history catch-up) + optional
   turn-taking lines for debate-style fleets.
6. **Best practices:** target when only some members must act; `@you` for
   human-only flags; done-signals; claims before touching shared files;
   end-condition language for interactive workers (they don't auto-exit).
7. **Traps:** DSR fresh-spawn (a post the instant a member spawns can be
   silently eaten for that member — history still has it; wait a beat or
   repost); stale ids error loudly → re-read `--history` (transcript is the
   roster); one foreman instance per machine; seq gaps in history are
   normal (syslines consume seqs).

Frontmatter description triggers on: group chat / chat room / coordinate
multiple agents / fleet discussion / have agents talk to each other, while
inside foreman (FOREMAN=1).

## Decisions

| Question | Resolution |
|---|---|
| One skill or two? | Two, split by responsibility (user) — dispatch = spawn mechanics; chat = the room end-to-end |
| Where does the composite "team that discusses in chat" recipe live? | foreman-chat (it's "launching agents into the group chat") |
| How do workers learn the convention? | Injected verbatim via the prompt template — works for `-p` workers and non-Claude CLIs, no skill-discovery dependency. Workers are NOT told to invoke skills. |
| Stop the investigation spiral how? | Stop-sign line at the top of both skills + authoritative quoting/membership facts so there is nothing left to verify |
| Duplication between the skills? | None: the convention paragraph and all chat commands exist only in foreman-chat; dispatch ends with a pointer |

## Out of scope

- No code changes; two SKILL.md files plus this spec.
- No worker-side skill (`-p` workers can't use skills; interactive non-Claude
  workers don't have them).
- The epic doc keeps the system documentation; skills stay agent-operational.

## Key files

- `.claude/skills/foreman-dispatch/SKILL.md` — rewrite.
- `.claude/skills/foreman-chat/SKILL.md` — new.
- `src/terminal.rs` (`Session::spawn_argv`) — source of the quoting truth;
  unchanged.
