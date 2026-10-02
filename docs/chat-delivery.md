# Chat room (the ChatRoom module)

## What it does

Each project owns one **ChatRoom**: the append-only message log PLUS a member
registry (who is in the room, each member's display name, and each member's
delivery cursor). The room is the one place that decides everything about chat —
who may post, who a post reaches, and which lines a member still needs. The
window-manager engine is just a thin adapter around it: it spawns terminals,
tells the room who is live each frame, and injects whatever lines the room hands
back.

A post is **appended** to the room; a **per-frame sweep** (`chat_tick`) hands the
room the current live members and injects the room's answer into each ready
terminal. Every post lands **exactly once**, and nothing is lost just because a
member had only just started up.

## Why it exists

Chat used to be smeared across the generic window manager: ~15 `chat_*` methods,
two chat-only fields bolted onto the generic `Tab`, and a `show()` whose frame
ordering silently encoded chat's delivery contract. A Member's identity was the
*window* id, which is wrong the moment two terminals share a window (tabbing) or
one is torn out (untab) — the agent kept posting under its spawn id while the
room thought it was someone else.

Pulling all of that into one `ChatRoom`:

- **Closes the leak.** The generic `Tab` no longer carries `chat_member` or
  `last_delivered_seq`; the engine no longer owns validation or delivery logic.
- **Fixes identity.** A Member's id is now its `Session`'s stable `term_id`
  (stamped at spawn = the `FOREMAN_TERMINAL_ID` the agent sees), so the room's
  view of "who" and the agent's view of "me" never disagree, even across
  tabbing/untab.
- **Makes the keystone testable.** Catch-up replay, exactly-once, self-exclusion,
  and exit reconcile are all pure logic inside the room, unit-tested with no GUI
  and no PTY.

## How it works

The room's interface is small; the behaviour behind it is not.

- **`post_now(from, text, to, re, now) -> Result<seq, err>`** — the strict path (the
  `foreman chat` CLI). Resolves `--to` flags + leading `@mentions`, validates
  them all-or-nothing (must be a member, alive, not yourself), auto-joins the
  sender on success, appends, returns the seq. A bad target is an error that
  mutates nothing.
- **`post_human(text) -> Option<seq>`** — the lenient path (the chat pane's input
  line). Same mention parsing, but a bad mention demotes the post to a plain
  broadcast instead of erroring (the input line has no error seat).
- **`join(id, name)`** — idempotent register + a `Joined` line. The engine calls
  it when it dispatches a terminal (auto-join).
- **`tick(project, live) -> Vec<Delivery>`** — the per-frame heart. `live` is the
  set of terminals the engine sees this frame (identity, name, readiness, exit,
  and agent state). The
  room (1) **reconciles presence**: a member that is exited-in-`live` or absent
  from `live` gets one `Exited` line; present members have their display name
  refreshed; (2) builds the **outbox**: for each ready, non-exited member it
  returns eligible posts addressed to it past its cursor (skipping its own),
  framed for injection. The cursor stops before a held post; urgent deliveries
  beyond it carry per-member dedup metadata until the cursor catches up.
  `project` is passed in
  per-frame so the framed line (`[chat p1 #N] …`) always reflects the live tag.
- **`crew(now)`**, **`blocks(last_seen, compact)`**, **`history(n)`**,
  **`is_member(id)`**, **`last_seq()`** — read-only views the engine/viewer pull.

The engine adapter (`src/wm.rs`):

- **`chat_tick`** walks the manager tree; in each project it builds `live` from
  the terminal sessions (Ready, paste availability, exit, and hook state), calls
  `room.tick`, then injects
  each `Delivery`'s lines into the matching terminal. Called once from `main.rs`
  after `show()`, so every session has pumped and its `ready()` is current.
- **`chat_post_now`** verifies the sender is a live terminal, then delegates to
  `room.post_now`. **`chat_post_human`** delegates to `room.post_human`.
- The **viewer pulls**: `ChatView::show` (`src/chat_view.rs`) borrows the room
  and calls `crew()` / `blocks()` directly (no pushed snapshot). A crew click
  records a member id; `drain_chat_clicks` resolves that id back to its window
  after `apply_acts` (ordering is load-bearing).

## Gotchas

- **Member id = `term_tag(session.term_id())`**, a stable `"tN"` string. NOT the
  window id. `term_id` is stamped once at spawn and never changes; the window id
  can. The human is the reserved id `"you"`, pre-registered in every room.
- **Cursor and urgent delivery metadata deduplicate** in the room registry.
  A post at-or-below the cursor is settled; urgent posts delivered beyond a
  held cursor are remembered until earlier posts settle.
- **`live` must be the full set of a project's terminals every frame.** The room
  marks a registered member exited if it is *absent* from `live`. If the engine
  ever passed a partial set, the room would wrongly exit members. (`chat_tick`
  passes every terminal, ready or not.)
- **Delivery only into a `ready()` session.** Ready = the startup DSR
  (`ESC[6n`) has been answered AND the child has painted its first visible
  output; bytes injected before both get eaten by the boot window. The DSR
  alone stopped being proof when the passthrough ConPTY host arrived — the
  host answers the DSR itself microseconds after spawn, seconds before the
  child's input path opens (the 2026-07-03 eaten-post regression). A not-ready
  member keeps its cursor and catches up the first frame it is ready. (This is
  the original silent-drop fix, now inside the room's outbox.)
- **A member never gets its own post** (`from == id` is skipped in the outbox).
- **`post_now` is strict, `post_human` is lenient.** Two methods, two policies, one
  owner. Don't fold them — the CLI agent needs the error; the pane input can't
  show one.
- **One post = one turn.** `ReadyGate` queues every post and writes the next
  paste only after the previous post's deferred Enter has fired (~150ms each).
  Posts never fold into one paste, so a receiver never reads two instructions
  as one line.
- **Stale posts get an age stamp.** At delivery, a post older than
  `STAMP_AFTER` (60s) is framed `[chat p6 #12 · 14m ago] t2: ...` so a replayed
  handoff can't pass for a fresh one. Fresh posts keep the v1 frame byte-for-byte.
- **Strict `post_now` guards the sender's mistakes.** It rejects (exit 1) a post
  over `MAX_POST_CHARS` (1200) and a post whose `@tX` sits in the first few
  tokens but not at the start (it would parse as prose and broadcast). `--to`
  covering that id lifts the mention check. `post_human` is exempt from both.
- **Idle-aware delivery uses the state setting.** With agent state enabled,
  normal posts wait through Working and Needs you, and deliver at Idle. With
  state disabled, no accepted hook yet, or a provider/icon mismatch, delivery
  falls back to Ready-only behavior. Naming-only hooks cannot gate delivery:
  they send Working without the Stop event needed to release it.
- **`--now` bypasses Working, including a stale Working after Claude Esc.**
  It can overtake earlier held normal posts and is delivered once. It never
  bypasses Ready, a pending paste/Enter, or Needs you: a paste at a permission
  prompt can answer the approval UI. It writes the usual paste and deferred
  Enter; it does not send Esc or cancel the receiver's turn. Older servers
  ignore the optional field and retain their existing delivery behavior.
- **Held posts stay in the log.** For a member with known state, the outbox
  hands off one post only when the paste lane is free, then checks state again
  for the next post. No second queue holds state-gated messages. Their age stamp
  is computed at handoff. State remains advisory: late hooks, same-provider
  children, and invisible interrupts retain the limits in `docs/agent-state.md`.
- **Not yet persistent.** The log is still in memory; a restart wipes it and the
  cursors. That's the next transport piece (feature #2). Independent of this —
  on restart there are no live members to deliver to anyway (PTYs don't survive).

## Key files

- `src/chat.rs` — `ChatRoom` (registry + `post_now`/`post_human`/`join`/`tick`/
  `crew`/`is_member`/`history`/`blocks`), `LiveMember`, `Delivery`, and the inner
  `ChatLog` it composes (`deliver_after`, `frame`, seq). All unit-tested here.
- `src/wm.rs` — the adapter: `chat_tick` (per-frame reconcile + inject),
  `chat_post_now` / `chat_post_human` (thin), auto-join in `add_terminal_cmd`,
  membership in `status_dispatch`, the `Content::Chat` viewer pull,
  `drain_chat_clicks` (resolve click → window by id).
- `src/chat_view.rs` — the dispatcher's-desk window: `ChatView::show` renders
  the crew board (ordered by last-heard, amber when stale), the grouped/wrapped
  log with system lines and the NEW divider, and the human input line. It
  borrows the room and pulls `crew()` / `blocks()` rather than being handed a
  snapshot. Design: `docs/superpowers/specs/2026-06-10-chat-dispatcher-window-design.md`.
- `src/terminal.rs` — `Session::term_id()` / `set_term_id()` (the stable Member
  id) and `ready()` (the DSR-answered + first-paint latch the outbox gates on;
  `InkScan` is the paint detector).
- `src/main.rs` — calls `chat_tick()` once per frame after `show()`.
- Design: `CONTEXT.md` (Member / Member id / Outbox), and
  `docs/contracts/chat-handshake-contract.md` (the pinned delivery contract).
