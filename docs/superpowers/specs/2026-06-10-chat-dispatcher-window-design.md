# Project Chat Window — "Dispatcher's Desk" Redesign

**Date:** 2026-06-10
**Status:** Approved
**Builds on:** `docs/superpowers/specs/2026-06-10-agent-group-chat-design.md` (v1
group chat, shipped), the NEXT WORK section of
`docs/superpowers/sessions/2026-06-10-agent-group-chat-session.md` (the
legibility complaints this answers).
**Relationship to v2 mentions:** mention *markup* here is render-only and
forward-compatible with `docs/superpowers/specs/2026-06-10-chat-mentions-design.md`;
mention *delivery* and quiescence gating stay in that spec, untouched.
**Mockup:** `_chat_mockup.html` (repo root) — the committed visual reference.
Chosen through mockup rounds: three style directions, then three concept
variations seeded from the winner; the user picked "dispatcher's desk."

## Goal

Replace the bare tail viewer (`#N tX: text`, clipped, anonymous) with a window
you can actually run a fleet from: **this window is the radio desk you run
crews from.** A crew board answers "who's stuck?" at a glance; a grouped,
wrapped log makes the room readable; clicking a crew member jumps to their
terminal. An input line (slice 2) lets the human post without a Claude session.

## Decisions

| Decision | Choice |
|---|---|
| Layout | Crew board left (~160 px) + log right; input line bottom (slice 2) |
| Adaptive | Board hides below ~480 px window width; meta trims to seq only |
| Fleet pulse | `N live` count chip in the chat window's title bar at every size |
| Crew order | Live members by last-heard, **stalest at top**; exited sink to bottom, dimmed |
| Ages | Relative (`now`/`1m`/`6m`), amber past a staleness threshold (~5 min) |
| Roster click | Focus that member's terminal. No filtering (considered, dropped) |
| Log shape | Consecutive posts from one sender group under one name header |
| Meta line | `tX · #N · HH:MM`; seq always shown (agents cite `#N`), time only at comfortable width |
| Wrapping | Soft-wrap; no clipping |
| System lines | Join/exit as dim centered log entries — membership is part of the transcript |
| New divider | Amber `NEW` rule above the first message since the window last had focus |
| Mentions | Render-only markup now: olive left edge, `→ name` in meta, `@name` chips |
| Names | Stamped into the message at post time from the sender's tab title (fallback `tX`); roster shows live titles |
| Colors | Stable per-terminal color from a small palette, assigned by terminal id |
| Scrolling | Stick-to-bottom; scrolling up pauses autoscroll until back at tail |

## Crew board

- Lists every room member (`chat_member` tabs), labeled by live tab title.
  The mockup's `you · t1` row is illustrative — the viewer cannot know which
  terminal is the human's; a terminal member is labeled by its title like any
  other. A literal `you` row is the **pane identity** and appears only with
  slice 2's input line.
- Live members sort by last-heard ascending — the quiet ones float to the top
  because they're who you check on. Last-heard derives from the log (latest
  message per sender); a member who has never posted shows since-join age.
- Exited members (session `exited()` is `Some`) sink below the live ones,
  dimmed, dot off, age replaced by `exited`. They stay listed — their `#N`
  references still resolve.
- Click a row → focus that member's terminal (existing focus cascade; resolve
  Win/tab by the member's terminal id — same staleness family as elsewhere:
  untab can orphan the resolution; acceptable, documented).

## Log

- Grouped headers: colored display name + dim meta. A new group starts when
  the sender changes or a system line intervenes.
- Join/exit system lines: `— architect (t5) joined —` / `— builder (t3)
  exited —`. Emitted when a terminal becomes a member (dispatch auto-join,
  join-on-first-post) and when a member's session exits (detected by the
  existing exit-title refresh path).
- The `NEW` divider sits above the first message whose seq is greater than the
  last seq seen while the window had focus. Focus updates the watermark;
  no unread counts, just the rule.
- Mention markup renders when a message carries a target (`to`): olive left
  border on the body, `→ name` appended to the meta, `@name` spans highlighted
  in the text. Nothing sets `to` until v2 mention delivery lands; the viewer
  code is written once.

## Model changes (`src/chat.rs`)

`ChatMsg` gains:

- `at` — wall-clock timestamp (powers meta `HH:MM` and roster ages),
- `name` — display name stamped at post time (history never rewrites when a
  window is retitled),
- `to: Option<String>` — render-only for now,
- a kind discriminator for system entries (join/exit) vs. posts.

The injection framing (`[chat p1 #N] tX: text`), `--history` output, and the
`chat` pipe verb are **unchanged** — this spec touches the window, not the
protocol. (Real names in the *framing* stay a separate, undecided idea.)
System entries get seq numbers like any message so the transcript stays
append-only and citable; they are **not** injected into member PTYs and do
not appear in `--history` output (agents asked for messages, not furniture).

`Content::Chat`'s payload grows from `Rc<RefCell<ChatLog>>` to a small view
struct: the shared log plus per-window view state (last-seen seq for the
divider, scroll/stick-to-bottom state).

## Slices

**Slice 1 — the viewer.** Everything above, read-only. Independently
verifiable: dispatch workers, watch the board reorder and ages tick, click to
focus, see system lines and the divider.

**Slice 2 — the input line.** A reserved human identity: `from` is a
non-terminal id rendered as `you` with its own color (it can never collide
with a `tX`). Posts append to the log and broadcast to **all** members — there
is no sender terminal to exclude. Framing for human posts:
`[chat p1 #N] you: text`. Plain egui text editing; Enter posts; Esc clears.
No @-completion.

## Testing

State-level (repo pattern — real `cmd.exe` PTYs where sessions are needed):

- Crew ordering: stalest-live-first, exited sink to bottom; never-posted
  member uses join age.
- Age formatting boundaries (`now`, minutes, amber threshold).
- Grouping: sender change and intervening system line both break a group.
- New-divider watermark: focus updates it; reopen shows the rule above the
  right seq.
- Name stamping: post records the tab title at post time; retitling a window
  later doesn't rewrite history; missing title falls back to `tX`.
- System entries: auto-join and exit append entries with seqs; they are not
  injected and not returned by `--history`.
- Click-focus resolution by terminal id, including the orphaned-id case.
- Compact mode threshold: board hidden + meta trimmed below the width cutoff.
- Slice 2: human post appends with the reserved id, broadcasts to all members
  (`cmd /c pause` end-to-end proof), framing bytes `[chat p1 #N] you: …`.

Live verification (build + screenshot, per working agreement): dispatch two
named workers, exchange posts, screenshot comfortable and compact sizes,
click-focus a crew row, confirm the divider after refocusing.

## Scope

Touches `src/chat.rs` (model fields, system kind), `src/wm.rs` (`Content::Chat`
view struct, crew board + log render, click-focus, join/exit emission, post
path stamps names), `src/terminal.rs` (nothing expected in slice 1),
`src/control.rs` (nothing — protocol unchanged), `_chat_mockup.html` (already
committed reference).

Out of scope: mention delivery and quiescence gating (v2 spec), real names in
the injection framing, persistence, cross-project chat, log filtering,
@-completion, read receipts, unread counts.
