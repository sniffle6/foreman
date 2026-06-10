# Agent Group Chat (`foreman chat`) — Design

**Date:** 2026-06-10
**Status:** Approved
**Builds on:** agent dispatch (`docs/epics/agent-dispatch-epic.md`,
`docs/superpowers/plans/2026-06-09-agent-dispatch.md`)
**Supersedes:** the point-to-point `foreman send` design drafted earlier the
same day (never implemented; replaced before any code was written).

## Goal

Close the loop on agent dispatch: agents running inside a foreman project can
coordinate — discuss design, divide work, report results — in a shared
**project chat room**. Posts are **broadcast-injected** into every other
member's terminal as typed input, so delivery is push-based: no agent ever
has to remember to poll a mailbox. A chat subwindow inside the project shows
the room to the human.

## Why group chat instead of point-to-point send

- Peer awareness: parallel workers see each other's reports, preventing
  duplicate work and conflicting edits. Point-to-point only solved steering.
- Less machinery: no `--to` addressing, no parent/child enforcement matrix,
  no cross-project edge cases. You post to your project's room; that's it.
- The human can watch (chat window) and participate (their orchestrator
  session is a member like any other).

## Decisions (made during brainstorming)

| Decision | Choice |
|---|---|
| Scope | One room per **project**; cross-project chat unsupported in v1 |
| Delivery | Broadcast injection into members' PTYs (push); history verb for catch-up (pull) |
| Membership | Dispatched terminals auto-join; others join on first post; history reads don't join |
| Persistence | In-memory only (`Vec` on the project manager); dies with foreman |
| Human UI | Read-only chat subwindow per project (posting from the pane = v2) |
| Storm control | Prompt convention via the dispatch skill (accepted v1 risk) |
| Delivery ordering | Reply-before-inject (an injection cannot be undone) |

## 1. Protocol — new `chat` verb on the existing control pipe

Same named pipe (`\\.\pipe\foreman`), same JSON line request/reply, same
`REPLY_TIMEOUT` and stale-request dropping as `open`:

```json
{ "cmd": "chat", "project": "p1", "from": "t2", "text": "taking the parser refactor" }
{ "ok": true }

{ "cmd": "chat", "project": "p1", "from": "t2", "history": 20 }
{ "ok": true, "history": ["#12 t1: split the work by module", "#13 t3: I'll take src/wm.rs"] }
```

- CLI: `foreman chat "<message>"` to post,
  `foreman chat --history [N]` to read the last N messages (default 20),
  `--project pX` to override the env default.
- A request must carry exactly **one** of `text` / `history`; both or
  neither is a client-side parse error.
- `project` and `from` are filled **automatically** by the client from
  `FOREMAN_PROJECT_ID` / `FOREMAN_TERMINAL_ID`. Missing terminal env is an
  immediate client-side error. As with `open`, this is a **guardrail
  against confused agents, not a security boundary** — any same-user
  process can speak to the pipe and claim any `from`; nothing here is
  authentication.
- The reply type gains an optional `history` field
  (`skip_serializing_if = None`), used only for history requests.
- The server's unknown-verb rejection now accepts `open` and `chat`,
  rejects everything else. `CtrlMsg` gains a `Chat` variant drained by the
  GUI thread like `Open`.

## 2. The room and its members

- The chat log lives on the **project's** nested `WindowManager`:
  `Vec<ChatMsg>` where `ChatMsg { seq: u64, from: String, text: String }`.
  Seq starts at 1 and only grows. In-memory; restarting foreman empties
  the room (project ids are runtime-scoped anyway — cross-restart identity
  is a problem v1 refuses to take on).
- **Membership** is a `chat_member: bool` on each `Tab` (revised from
  `Win` during Task 3 review — per-Win membership let a plain shell tab
  merged onto a member window receive injected chat at its prompt, and
  silently skipped member agents sitting in background tabs):
  - Terminals spawned via `open` (agent dispatch) are members
    automatically — they are agents by construction, and injecting chat
    into a *plain* shell would type garbage at a prompt.
  - Any other terminal (e.g. the user's hand-opened Claude session)
    becomes a member on its **first post** — orchestrators join the moment
    they announce anything.
  - `--history` does **not** join; reading is free.
  - Broadcast delivers to member tabs in **all** tab positions (background
    tabs stay drained via keepalive); the flag travels with its terminal
    through merge/untab. Sender identity still resolves by Win id (active
    tab) — that id-resolution staleness gotcha remains, as elsewhere.
- `FOREMAN_TERMINAL_ID` and `FOREMAN_PROJECT_ID` already exist in every
  PTY's env (`term_env`, src/wm.rs — tested). **No new env vars in this
  feature.**

## 3. Delivery

On a post, the GUI thread:

1. drops the request unexecuted if stale (same age check as `open`);
2. resolves the project (existing `resolve_project`), validates the
   request (`text` non-empty);
3. appends to the log (this also sets the sender's `chat_member` flag);
4. sends the success reply **first**; injects **only if the reply was
   accepted** — an injection cannot be undone, so the bytes flow only once
   the client is guaranteed to hear "ok" (the duplicate-delivery window a
   retrying client could otherwise hit collapses to near zero);
5. injects the framed message into every member terminal in the project
   **except the sender**, skipping exited sessions:
   - **Framing:** `[chat p1 #14] t2: <text>` — provenance plus seq so
     agents can reference earlier messages.
   - **Bracketed paste** (`ESC[200~ … ESC[201~`) so multi-line content
     lands as one paste block, then a **deferred `\r`** (~150 ms) to
     submit — revised 2026-06-10 after a live failure: a `\r` written
     back-to-back with the paste is folded into it by Claude Code's
     burst detection and lands as a literal newline, leaving the message
     unsubmitted in the input box (the tmux `send-keys; sleep;
     send-keys Enter` problem). Posts landing inside the window merge
     into one submitted turn (accepted).
   - Injection uses the existing `Session::send` path via a new
     `inject_input` method.

Accepted risks (explicit decisions):

- If a member's input box holds half-typed text, the injected message
  submits together with it; a member sitting at a permission prompt may
  have it answered by the injected bytes. Bounded by membership (agents
  opted in by being dispatched or by posting).
- **Message storms:** every post is a user-turn for every other member,
  and members may respond to responses. Mitigation is a prompt convention
  (§5), which is model behavior, not a guarantee. Bounded in practice by
  typical 2–4 member rooms. Revisit (rate limits, @-mentions) only if it
  bites.

A post into a room with no other live members still succeeds (log append,
no injection) — the room is the log, not the audience.

## 4. The chat window

A read-only viewer any project can open to watch the room:

- New `Content::Chat` variant in the project-level `WindowManager` —
  a normal `Win`: drags, snaps, tabs with terminals at its level, closes.
  Closing it does not touch the log.
- **Singleton per project:** the open-chat command focuses the existing
  chat window if one is open, else creates one.
- Opened via a leader-key command (`chat`) — data-driven `Keymap::default`
  assigns the default chord, so the user file merge keeps working.
- Renders the log (`#seq from: text`), newest at the bottom, auto-scrolled
  to the tail; updates live (posts arrive on the GUI thread, which
  requests a repaint).
- The window is a **viewer, not a member** — it renders the log directly
  and is never injected into.
- Posting from the pane (an input line for the human) is the natural v2;
  v1 humans post through their own Claude session.

## 5. Worker dispatch modes and the chat convention

The `foreman-dispatch` skill documents:

- **Fire-and-forget:** `claude -p "<prompt>"` — cannot *receive* chat
  mid-run (print mode doesn't read stdin), but **can post** (`foreman
  chat` is a process spawn). Pattern: prompt ends with "post your result
  to the project chat with `foreman chat \"<summary>\"` before exiting."
- **Steerable/collaborative:** interactive `claude "<prompt>"` — receives
  every post as input. Does not auto-exit; end it by posting an
  instruction addressed to it, or instruct exit-when-done in its prompt.
- **Standing convention injected into dispatched prompts:** "You are in a
  project chat. Messages arrive as `[chat p1 #N] tX: …`. Only respond when
  a message is relevant to your task — most messages need no reply. Post
  with `foreman chat \"…\"`. Check `foreman chat --history` after long
  heads-down stretches."

## 6. Testing

Unit/integration (alongside existing control/wm/terminal tests, which all
spawn real `cmd.exe` PTYs — established repo pattern):

- `chat` request/reply JSON roundtrip incl. `history` reply field omitted
  when `None`; exactly-one-of `text`/`history` parse rule; `from`/`project`
  auto-fill; missing terminal env errors client-side.
- Pipe roundtrip for the `chat` verb; unknown verbs still rejected; `open`
  behavior unchanged.
- Membership: dispatched terminal auto-joins; plain terminal not a member
  until it posts; history read does not join.
- Broadcast: post injects into other members only — not the sender, not
  non-members, not exited sessions (end-to-end proof via `cmd /c pause`
  members exiting when injected bytes hit their stdin).
- Ordering: stale request dropped without reply; reply sent before
  injection (dead reply channel ⇒ log may append, bytes must not flow).
- History: last-N slicing, formatting, empty room.
- Framing bytes: `[chat p1 #N] from:` prefix, bracketed-paste wrap,
  deferred trailing `\r` (fired by pump after the delay, never written
  with the paste).
- Chat window: open command creates the singleton, reopen focuses it,
  renders log lines (state-level assertions; pixel checks are the live
  verify's job).

Live verification (build + screenshot, per working agreement):

1. Dispatch two interactive workers, post from the orchestrator session,
   screenshot both workers receiving the framed post.
2. Worker posts back; screenshot the exchange landing in the other
   members' panes and the chat window showing the log.
3. Negative: `foreman chat --history` from a non-member works and does not
   make it start receiving posts.

## Scope

Touches `src/control.rs` (chat verb, history reply field, client),
`src/wm.rs` (chat log, membership, broadcast, `Content::Chat` window,
open-chat command), `src/terminal.rs` (paste-wrapped `inject_input`),
`src/main.rs` (none expected — drain already passes `CtrlMsg` through),
`src/keymap.rs` (default chord for the chat window command),
`.claude/skills/foreman-dispatch/SKILL.md`, and
`docs/epics/agent-dispatch-epic.md`.

Out of scope for v1: posting from the chat pane, disk persistence, named
channels, cross-project chat, @-mentions/rate limiting, message editing or
deletion, read receipts.
