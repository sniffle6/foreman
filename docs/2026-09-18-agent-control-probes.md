# Agent control probes (2026-09-18)

## What this is

A record of what actually happens when foreman injects keys and text into a
running agent CLI — Claude Code, Codex, Grok, Gemini. Every row below came
from a live experiment against disposable Sessions, not from reading docs or
reasoning about what *should* happen.

## Why it exists

Card `ul4qs9` (GitHub issue #7, the Plan window) proposed Pause / Resume /
Stop controls that would drive worker agents from a plan UI. These probes were
run to find the interrupt mechanism those controls needed. They found instead
that there is no agent-neutral interrupt, and that the obvious key kills the
process. The controls were cut; the plan feature was abandoned and rebuilt as
a read-only view.

The probes cost real experiment time and real weekly-usage budget. They are
kept here because the next person who proposes driving an agent by injected
keys needs to read them before spending that again. The abandoned branch
`card/ul4qs9` holds the original spec if you want the surrounding design.

## Method

Disposable Sessions opened with `foreman open --cwd <temp dir>` in a scratch
project, driven only with `foreman send` / `foreman snapshot` / `foreman
status`. No user Session was touched. Screen text is quoted from snapshots;
**liveness always came from `status`, never from the screen.**

Versions: Claude Code 2.1.277, codex-cli 0.155.1, grok 1.0.34, gemini 0.50.0,
foreman installed build.

## Per-agent control table

| Case | Claude Code | Codex | Grok | Gemini |
|---|---|---|---|---|
| Esc on startup trust dialog | **exits** (0); screen still paints the dialog | **exits** (0); screen blank | no trust dialog seen | **exits** (130) |
| Esc at idle prompt | no-op | no-op | no-op | not verified |
| Ctrl+C at idle prompt | no-op (this version) | **exits immediately** (0) | no-op | "Press Ctrl+C again to exit" |
| Esc while streaming | `⎿ Interrupted · What should Claude do instead?`, alive | `■ Conversation interrupted`, alive | **ignored**, keeps streaming | not verified |
| Ctrl+C while streaming | not probed (Esc suffices) | not probed (idle Ctrl+C is fatal) | `Turn cancelled by user`, alive | not verified |
| Esc on tool permission prompt | cancels the tool, `Interrupted`, file not written, alive | not probed | not probed | not verified |
| Message submitted mid-turn | queued, acted on after the in-flight tool call | queued inline, acted on after the in-flight command | `Queued · Enter to send now`; fires after the turn ends or is cancelled | not verified |
| Cooperative pause via injected text | ack file written, count correct, no further work | ack file written, count correct, no further work | not probed | not verified |
| Resume via injected text | continued the task | not probed (weekly limit) | not probed | not verified |
| Long foreground tool | harness refused a 60 s sleep and backgrounded it | not probed | not probed | not verified |

Gemini stops on an auth dialog after the trust dialog ("This client is no
longer supported for Gemini Code Assist for individuals"). It cannot be signed
in on this machine, so every busy-state case is unverified.

## What the findings rule out

**The interrupt key is per agent, and the wrong key is fatal.** Esc interrupts
Claude and Codex. Grok ignores Esc and needs Ctrl+C. Ctrl+C at an idle Codex
prompt exits the process. Anything that stops an agent must resolve the agent
kind first (the same detection ladder as the tab icon) and must send nothing —
reporting "interrupt unsupported" — when the kind is unknown or is Gemini. A
per-kind table is the whole mechanism. There is no agent-neutral interrupt
byte.

**Any startup confirmation turns Esc into process exit** (all three verified
agents). A stop that lands before the worker's first prompt kills the worker
instead of interrupting it. Either gate on the Ready latch plus first prompt,
or accept "exited" as a stop outcome and record it as needing recovery. The
Claude permission prompt is the safe exception: Esc there cancels the tool and
interrupts.

**An interrupt is not a durable stop.** Three separate ways activity resumed
with no new user input:

- Claude restored the interrupted message into its input box; a later Enter
  resubmits it.
- A Claude background shell finished after the interrupt, and its completion
  notification started a new turn that replied DONE.
- Grok auto-submitted the queued message right after Ctrl+C cancelled the turn.

So: cancel every pending submit *before* the interrupt, never treat "interrupt
sent" as "worker idle", and call the state **interrupted**, not stopped, until
the worker is closed.

**Cooperative pause works with plain injected text on Claude and Codex.** Both
queue a message submitted mid-turn and act on it after the tool call in flight
(Claude: file 3 at 18 s, ack at 25 s; Codex: file 2 at 22 s, ack at 30 s).
Both wrote the ack with the correct count and created nothing further. Grok
only queues; it acts after the turn ends. An ack carried by a CLI verb is
realistic for Claude and Codex; on Grok, expect the ack only at turn end.

**The control plane does not report a dead target.** After `/exit`, `foreman
send` to the exited pane still replied `{"ok":true}` and `snapshot` returned
the stale screen with exit 0. Only `status` showed `exited(0)`. Liveness must
come from the process, never the grid.

**Text and CR in one write do not submit.** A prompt plus `\r` in a single
`send` landed in Claude's input box with the CR as a literal newline, cursor
on a fresh line. A separate Enter 500 ms later submitted normally. Injected
input must go through `Session::inject_input` (bracketed paste plus deferred
submit), never a raw write.

**Screen text is not a liveness signal.** Claude's exited process left the
trust dialog painted; Codex left a blank screen; both were `exited(0)`.

## Gotchas

- The table's blank cells are honest. "not verified" means nobody ran it, not
  that it works. Codex rows stop where the weekly usage limit stopped them.
- Versions matter and these are dated. Ctrl+C at an idle Claude prompt was a
  no-op *in 2.1.277*; that is the kind of thing a point release changes.
- The harness appends to a log file rather than printing, so evidence survives
  the Session. Set `FOREMAN_PROBE_LOG` to steer it out of the repo.

## How to re-run

Dot-source the harness inside a foreman terminal, then call the functions:

```powershell
$env:FOREMAN_PROBE_LOG = "$env:TEMP\probe-log.md"
. .\scripts\agent-probe.ps1
$t = Probe-Open "claude" @("claude") "$env:TEMP\probe-claude"
Probe-Wait $t "trust" 30 "startup"
Probe-Send $t "" "escape" 1500 "esc on trust dialog"
Probe-Status $t
Probe-Close $t
```

`Probe-Open` reads `FOREMAN_EXE` and `FOREMAN_PROJECT_ID` from the
environment, so it only works from inside a foreman Session.

## Key files

- `docs/2026-09-18-agent-control-probes.md` — this record.
- `scripts/agent-probe.ps1` — the probe harness.
- `.claude/skills/foreman-failure-archaeology/SKILL.md` — entry 10 points here.
- Branch `card/ul4qs9` (unmerged, kept) —
  `docs/superpowers/specs/2026-09-18-plan-window-design.md` holds the original
  design these probes were run for.
