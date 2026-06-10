# Session State — Agent Group Chat (2026-06-10)

Companion to:
- Spec: `docs/superpowers/specs/2026-06-10-agent-group-chat-design.md`
- Plan: `docs/superpowers/plans/2026-06-10-agent-group-chat.md`
- Epic: `docs/epics/agent-dispatch-epic.md` (Group chat section)
- Skill: `.claude/skills/foreman-dispatch/SKILL.md`

Resume hint for a future session: read this file, then the spec, then the
epic's Group chat section. The plan is execution history, not future work.

## Status

Feature **built, reviewed, live-verified, and live-debugged** on branch
`feature/agent-dispatch`. 87 tests green, clippy at 28 warnings (below the
pre-feature baseline). Not merged; no PR. The branch also carries the
earlier agent-dispatch (`open` verb) work.

## Commit series (oldest first)

| SHA | What |
|---|---|
| `61b515e` | paste-wrapped stdin injection on Session (`paste_wrap`, `inject_input`, ESC-strip) |
| `e9380ac` | in-memory `ChatLog` model (`src/chat.rs`) |
| `2333e77` | room membership (per-**Tab**), post, broadcast, history on the project manager |
| `b26f691` | `chat` verb with reply-before-inject broadcast (control pipe + `handle_ctrl`) |
| `a4feb14` | `foreman chat` CLI (flags-first parsing, `--` escape, `--history`) |
| `58ddb45` | per-project chat viewer window on leader+G (`Content::Chat`, singleton, resurfaces) |
| `8e187a3` | epic + dispatch skill docs |
| `0f5a1a7` | recorded live ConPTY bracketed-paste verification (DECSET 2004 decision) |
| `0d434e1` | viewer lays out by physical row; workers invoke via `FOREMAN_EXE` |
| `b17a248` | committed spec/plan, dropped dead `msgs()` accessor, closed epic verification note |
| `45f4725` | **deferred submit keypress** past claude's paste-burst window (+ variadic-flag trap doc) |

## How it works (one paragraph)

One room per project. `& $env:FOREMAN_EXE chat "msg"` posts via the
control pipe; foreman appends to the project's in-memory `ChatLog` and
broadcast-injects `[chat p1 #N] tX: …` into every other member tab's PTY
as a bracketed paste, then fires the submitting `\r` ~150 ms later from
the frame loop (`pending_submit` on `Session`). Dispatched terminals
auto-join; others join on first post; `--history` reads without joining.
Leader+G opens a read-only tail viewer (singleton per project).

## Decisions locked during implementation (with why)

- **Membership lives on `Tab`, not `Win`** — merges/untabs move tabs
  wholesale; per-Win let merged plain shells receive chat and background
  member tabs miss it (Task 3 review).
- **Reply-before-inject, append-only** — an injection can't be undone; a
  post whose reply channel died stays in the log, bytes never flow.
  Duplicate-on-retry accepted (spec §3).
- **ESC stripped from paste payloads** — quoted `ESC[201~` must not break
  out into live keystrokes (Task 1 review; alacritty precedent).
- **Flags-first CLI with `--` escape** — message bodies containing
  `--project p2` must not reroute the post (Task 5 review).
- **Deferred `\r` submit (`SUBMIT_DELAY` = 150 ms)** — claude's burst
  detection folds a back-to-back `\r` into the paste; the message sat
  unsubmitted in a live run. tmux send-keys/sleep/Enter problem.
- **DECSET 2004 wrap stays unconditional** — live-verified: claude honors
  the markers through ConPTY (multi-line lands as one input block).

## Live testing — what happened

Scripted verification (Task 8) passed end to end with screenshots: posts
injected into both workers, ALPHA/BRAVO replies via the pipe, multi-line
as one block, history, viewer singleton, negative checks.

The user's own live run then surfaced two real failures:
1. **Submit lost** — message visible but unsubmitted in a worker's input
   box → fixed by the deferred `\r` (`45f4725`).
2. **Workers started with no prompt** — the orchestrating agent dispatched
   `claude --allowedTools Bash Read … "<prompt>"`; the variadic flag ate
   the prompt. Not a foreman bug; documented in the skill (prompt goes
   immediately after `claude`/`claude -p`).

## Known residuals (documented, accepted)

- Deep-boot coalescing: paste + deferred `\r` both buffered through a
  member's entire boot can still merge into one read. Escalation if it
  bites: age-gate delivery to young sessions.
- Plain-shell members get chat typed at their prompt (loud errors) — by
  design; membership is supposed to keep shells out unless they post.
- Sender identity resolves Win-id → active tab (staleness family shared
  with terminal-id resolution; untab makes a terminal's own
  `FOREMAN_TERMINAL_ID` stale).
- Message storms mitigated only by prompt convention.
- In-memory log dies with foreman; restart for a new build kills the room.
- `HANDOFF.md` still lacks a dispatch+chat section (pre-existing drift).

## NEXT WORK (discussed, not yet approved/started)

The user found the live experience illegible. Diagnosis: the architecture
matches their mental model (shared room + per-terminal notification); the
presentation doesn't. Proposed, awaiting go-ahead:

1. **Real names in framing** — `[chat p1 #4] chat-auditor: …` using the
   sender window's title (id kept as suffix); same in the viewer.
2. **Auto-open the viewer** on a project's first post (singleton already
   exists; one call in the post path).
3. **Soft-wrap long lines** in the viewer (currently clipped).
4. (v2, spec out-of-scope list) input line in the viewer so the human
   posts directly.

## Operational notes

- Rebuild ritual: `Stop-Process -Name foreman -Force` first (os error 5),
  `cargo build --release`; `cargo test` touches only debug artifacts and
  can run while a release foreman is up.
- Workers must invoke the CLI via `$env:FOREMAN_EXE` (exe dir not on
  spawned shells' PATH).
- The session used subagent-driven development: every task got a fresh
  implementer + spec review + quality review; all review findings and
  their resolutions are reflected in the docs above.
