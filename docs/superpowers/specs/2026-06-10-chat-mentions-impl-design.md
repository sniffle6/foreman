# Project Chat @-Mentions — v2 Implementation Design

**Date:** 2026-06-10
**Status:** Approved
**Builds on:** `docs/superpowers/specs/2026-06-10-chat-mentions-design.md` (the
agent-debate consensus — settled the WHAT), the shipped v1 group chat
(`2026-06-10-agent-group-chat-design.md`) and dispatcher window
(`2026-06-10-chat-dispatcher-window-design.md`).
**Scope guard:** quiescence gating (consensus row 5) is OUT — it needs its own
spike. This spec implements rows 1–4 and 6 plus the open questions, resolved
with the user 2026-06-10.

## TL;DR

`foreman chat --to t3 "msg"` (or a message starting `@t3 …`) delivers only to
t3's PTY. Every message — targeted or not — still lands in the one shared
transcript. Multiple targets allowed. Unknown / exited / non-member / self
targets fail the whole post loudly at send time. `--to you` / `@you` flags the
human and interrupts nobody. The human's chat-window input line gets the same
`@t3` targeting; its failures fall back to plain broadcast instead of erroring.
Untargeted framing, history lines, and wire bytes stay **byte-identical** to v1.

## Decisions (resolved with the user)

| Question (from the consensus doc) | Resolution |
|---|---|
| Syntax | Both: `--to` flag is the mechanism, leading `@tX` in the text is sugar |
| Inline rule | **Leading tokens only**; mentions stay in the text (not stripped); mid-prose `@tX` is never a target |
| Multiple targets | Yes — repeatable `--to`, multiple leading `@`s; `ChatMsg.to` becomes `Vec<String>` |
| Validation | **All-or-nothing strict**: any unknown / exited / non-member / self target fails the whole post — nothing logged, nothing joined, nothing injected |
| History + framing | Arrow in both: `#15 t1→t2,t3: text` and `[chat p1 #15] t1→t2,t3: text`; untargeted output byte-identical to v1 |
| `@you` | Valid target, pure markup: viewer highlights it, no PTY is interrupted |
| Architecture | Model-layer: pure extraction helper in `src/chat.rs`, server-side union + validation; the human input line gets targeting free |
| Human-post failures | No error UI — a human post with an invalid mention broadcasts as prose (`to` stays empty) |
| Branch | Build on `feature/agent-dispatch`; pending work committed first |

## 1. Protocol (`src/control.rs`)

- `ChatRequest` gains `#[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub to: Vec<String>` — untargeted requests are wire-identical to v1.
- `to` carries **flag-given** targets only; inline mentions travel inside
  `text` and are extracted server-side.
- Reply type, unknown-verb handling, `REPLY_TIMEOUT`, and stale-request
  dropping unchanged.
- `--to` with `--history` is a client-side parse error (exit 2, no pipe call),
  like the existing both-or-neither rule.

## 2. CLI (`parse_chat_args`)

- `--to <id>`, repeatable. Accepts `t3` or `@t3` (one leading `@` stripped).
- Client-side format check: value must match `t<digits>` or `you`; anything
  else is exit 2 with the offending value named. Existence/membership checks
  are the server's (the client cannot know terminal state).
- No grammar change for inline mentions: `@t3` is positional, so it already
  ends flag parsing and rides in the message verbatim.
- `--` still ends flag parsing only. It does **not** suppress mention
  extraction — the server sees only text, so a leading `@t3` after `--` still
  targets. (Documented in SKILL.md; a message that must start with a literal
  `@tX` without targeting it has no escape hatch. Accepted.)

## 3. Mention extraction (`src/chat.rs`, pure)

```
leading_mentions(text: &str) -> Vec<String>
```

- Whitespace-separated tokens at the **start** of the text matching `@t<digits>`
  or `@you`; extraction stops at the first non-mention token.
- Mentions are **not stripped** — the stored/injected text is exactly what was
  sent; the transcript keeps its natural reading.
- Effective targets = `--to` values in order, then inline mentions in order,
  deduplicated keeping first occurrence. Deterministic for tests and framing.

## 4. Model (`src/chat.rs`)

- `ChatMsg.to: Option<String>` → `pub to: Vec<String>` (empty = broadcast).
  `ChatLog::post` gains the targets parameter; `ChatBlock::Text.to` follows.
- `build_blocks`: header meta gains `· → t2,t3` when `to` is non-empty; the
  existing stand-alone-group rule (`m.to.is_some()`) becomes `!m.to.is_empty()`.
- `frame()` targeted: `[chat p1 #15] t1→t2,t3: text` — no spaces around `→`,
  comma-joined, target order per §3. Untargeted: byte-identical to v1.
- `line()` targeted: `#15 t1→t2,t3: text`. Untargeted: byte-identical.
- The viewer render arm (`src/wm.rs`) adapts to the `Vec`: olive left edge and
  `@word` chips are already built and light up unchanged.

## 5. Validation (`chat_post` in `src/wm.rs`)

Runs **before any mutation** — a failed post must not append to the log and
must not set join-on-first-post (today's join-then-append order is reordered
to validate → join → append). Per target, first failure wins, checked in this
order:

1. `you` → valid; resolves to no terminal.
2. No window with that id in the project → `no such terminal: t9`.
3. Target id == sender id → `cannot mention yourself`.
4. Window has no member tab → `t7 is not a chat member`.
5. Every member tab's session has exited → `t3 has exited`.

Any failure fails the whole post (all-or-nothing): reply `ok:false` with the
message above; the CLI prints it and exits nonzero. With multiple bad targets
the first (in §3 order) is reported.

Membership note: a freshly dispatched worker is a member from spawn (auto-join)
and is targetable before it ever posts. A hand-opened session that never posted
is not — one broadcast post from it (or a dispatch) is the entry ticket.

## 6. Delivery (`chat_broadcast` in `src/wm.rs`)

- Signature gains `targets: Option<Vec<WinId>>`:
  - `None` → broadcast to all member tabs (v1 behavior, untouched).
  - `Some(ids)` → inject only member tabs of the listed windows. Merged member
    tabs in a target window all receive — crew identity is the hosting window
    id, consistent with the viewer.
  - `Some(vec![])` is real: a pure `@you` post injects **nobody** — the
    cheapest message in the system.
- `ChatOutcome::Posted` carries `targets: Option<Vec<WinId>>` with the same
  meaning — `None` untargeted, `Some` with `you` filtered out (it resolves to
  no terminal), so a pure-`@you` post arrives as `Some(vec![])`, not `None`.
- Reply-before-inject ordering, exited-session skip, bracketed paste +
  deferred `\r`, and the DSR fresh-spawn hazard are all unchanged and apply to
  targeted delivery identically.
- Sender exclusion is unchanged (and a target can never be the sender — §5.3).

## 7. Human input line (`drain_chat_posts` path)

- The `you` post path runs the same extraction + validation. Typing
  `@t3 check the diff` into the chat window interrupts only t3.
- On any invalid target (including `@you` — self-mention), the post goes
  through as a **plain broadcast with `to` empty**; the mention text stays as
  prose. No error UI. (CLI posts get loud errors; the human gets forgiveness —
  deliberate asymmetry, chosen over an input-line error affordance.)
- Valid targeted human posts frame as `[chat p1 #N] you→t3: text` and deliver
  to the targets only (no sender terminal to exclude, as in v1).

## 8. SKILL.md (`.claude/skills/foreman-dispatch/SKILL.md`)

Teach agents:

- `& $env:FOREMAN_EXE chat --to t3 "msg"` and the leading-`@t3` sugar;
  multi-target via repeated `--to` or `@t2 @t3 …`; leading-only rule
  (mid-prose `@tX` never targets); `--` does not suppress a leading mention.
- Targeted frame variant `[chat p1 #N] t1→t2,t3: text` — if your id is on the
  right of the arrow, the message was addressed to you specifically.
- All-or-nothing loud errors; on a stale-id error, re-read `--history` to find
  the live ids (the transcript is the roster).
- `--to you` / `@you` to flag the human without waking a single peer.
- Convention nudge in the standing paragraph: target when only some members
  need to act; broadcast wakes everyone.

## 9. Tests (repo pattern — state-level, real `cmd.exe` PTYs where needed)

- **Parse:** repeated `--to`; `@`-strip; bad format exits client-side;
  `--to`+`--history` rejected; JSON roundtrip with `to` omitted when empty
  (wire-compat with v1 requests both directions).
- **Extraction:** leading tokens; stop at first non-mention; mid-text `@`
  ignored; `@you`; flag+inline union order and dedup.
- **Validation:** each §5 case errors; log length unchanged; sender not
  joined by the failed post; no bytes injected. Multi-target with one bad id
  fails all.
- **Delivery:** targeted post injects only the target's member tabs
  (end-to-end `cmd /c pause` proof: target exits, bystander member doesn't);
  multi-target hits each listed window; `@you`-only injects nobody but
  appends; merged-tab target window delivers to all its member tabs.
- **Framing regression:** untargeted `frame()`/`line()` byte-identical to v1
  expectations; targeted formats exact.
- **Viewer:** `build_blocks` multi-target meta `→ t2,t3`; targeted message
  still breaks grouping.
- **Human path:** valid `@t3` narrows delivery and frames `you→t3`; invalid
  mention falls back to broadcast with `to` empty and the text intact.

Live verification (build + screenshot, per working agreement): dispatch two
interactive workers, target one (`--to`), screenshot the target receiving the
arrow frame while the bystander's pane is untouched and the viewer shows the
olive/→ markup; post `@you` from a worker and confirm zero injections with the
viewer highlighting it.

## Out of scope

- **Quiescence-gated injection** — consensus row 5; mechanism unsolved
  (foreman sees PTY bytes, not turn state). Own spike/spec. Nothing in this
  design blocks it: gating slots into `chat_broadcast`'s injection loop later.
- Threads, DMs / private visibility, read-state (scope frozen by consensus).
- Rate limiting; @-completion in the input line; name-based targeting (ids
  only — the transcript is the roster).

## Key files

- `src/control.rs` — `ChatRequest.to`, `--to` parsing, format validation.
- `src/chat.rs` — `leading_mentions`, `ChatMsg.to: Vec<String>`, `frame`/
  `line`/`build_blocks` target forms.
- `src/wm.rs` — `chat_post` validation + reorder, `chat_broadcast` targets,
  `ChatOutcome::Posted` targets, human post path, viewer `Vec` adaptation.
- `.claude/skills/foreman-dispatch/SKILL.md` — agent-facing conventions.
- `src/terminal.rs` — untouched (quiescence is the future spike).
