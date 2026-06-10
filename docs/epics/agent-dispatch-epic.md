# Agent Dispatch — Epic

Status: **built** (2026-06-10). v1 fire-and-watch is implemented and E2E-verified.

## What it does

A CLI session running inside a foreman terminal — Claude Code, codex, any
agent CLI — can open new, visible terminals in foreman. So an AI agent you're
talking to can dispatch worker agents that show up as real windows you can
watch, type into, and keep.

The v1 contract is **fire-and-watch**: the dispatcher spawns a worker terminal
and moves on; the human supervises it visually. No automatic result return
yet, but the protocol is shaped so round-trip (results flowing back to the
dispatcher) bolts on later without breaking changes.

## Why it exists

Foreman's whole point is "tmux built for AI" — running many agents visibly.
Claude Code's agent teams was researched first (2026-06) and can't be the
mechanism: its multi-terminal display is hard-coded to tmux/iTerm2 split
panes, has no pluggable spawn hook, and doesn't support split panes on
Windows at all. So foreman needs its own mechanism — and making it generic
means it works with any model or CLI, not just Claude.

## How it works

### 1. Control pipe (foreman side)

Foreman listens on the named pipe `\\.\pipe\foreman` from a background
thread. One JSON request per connection; the request is pushed over an
`mpsc` channel that the egui `App` drains each frame (the app already
repaints every 16ms, so latency is one frame).

v1 protocol — one verb:

```json
{"cmd":"open", "project":"p1"|null, "cwd":"H:\\repo", "title":"agent · fix-tests",
 "command":["claude","-p","<task>"]}
```

Reply:

```json
{"ok":true, "terminal":"t3", "project":"p1"}
{"ok":false, "error":"no such project: p9"}
```

`project: null` means "the focused project". Returning IDs now is what makes
round-trip possible later (`status` / `wait` verbs keyed by those IDs).

### 2. Client mode (same exe)

`foreman open [--project P] [--title T] [--cwd D] -- <command...>`

When argv has a subcommand, foreman runs as a thin pipe client and exits —
no GUI. Nothing extra to install; the dispatching agent just runs `foreman
open`.

### 3. Env injection

Every PTY foreman spawns gets:

- `FOREMAN=1` — "you are inside foreman"
- `FOREMAN_PROJECT_ID` — the project this terminal lives in
- `FOREMAN_TERMINAL_ID` — this terminal's id (kept from day one so the env
  contract is stable when round-trip arrives)
- `FOREMAN_EXE` — full path to the running foreman exe, so clients can invoke
  `foreman open` without any PATH setup

The client defaults `--project` from `FOREMAN_PROJECT_ID`, so a Claude
session inside foreman dispatches into **its own project** with zero
arguments.

### 4. Spawn semantics

The worker is a new terminal window in the target project running the
command directly (not wrapped in a shell). It snaps/tabs like any terminal
but deliberately does NOT take focus — a dispatch must never move the
keyboard out from under the user. Title: `agent · <label>`.

The pane opens with a dim `── dispatched: <command> ──` banner injected
into the emulator (not the PTY) at spawn, truncated to one 80-col line. A
`claude -p` worker prints nothing until it finishes, so without the banner
an empty pane reads as hung; with it, silence reads as "working". Injecting
in foreman (vs an `echo` wrapper in the dispatch command) avoids cmd.exe
quoting hazards — task prompts containing `&` or quotes would split or
break a `cmd /c "echo … & claude …"` wrapper.

- **Exit:** when the process ends, the terminal stays open with scrollback
  and the title gains `exited (code)`. In fire-and-watch the terminal IS the
  record — closing is manual.
- **npm shims:** `claude` on Windows is a `.cmd` shim, not an exe. No PATH
  detection — try the direct spawn, and on ANY spawn error retry once wrapped
  in `cmd /c`. Consequence: `ok:true` means "a terminal opened", not "your
  command exists" — a bogus command opens a terminal showing cmd's error.

### 5. Claude as first client

A small skill/CLAUDE.md note teaches Claude: "to dispatch a visible agent,
run `foreman open --title \"agent · <label>\" -- claude \"<task>\"`" —
interactive mode, because `claude -p` prints NOTHING until it finishes
(verified: `--verbose` does not change this; only
`--output-format stream-json` streams, as raw JSON), which reads as a hung
terminal for the whole run. Interactive workers stream live and accept
steering/permission answers in their own pane; `-p` remains the
fire-and-forget option when nobody is watching.
Model-agnostic by construction — a codex variant is the same line with a
different command.

## Gotchas

- **Terminal IDs are per-project.** `WinId` is a per-manager counter, so
  `t3` is only unique inside its project. The protocol always scopes by
  (project, terminal) pair; never promise globally unique terminal IDs.
- **Tab-merged projects go stale.** Resolution only sees the window ids of
  active tabs — after merging project B's tab onto project A's window, B's
  old `pN` id no longer resolves and dispatchers inside B get an error until
  they pass an explicit `--project`.
- **One foreman instance per machine** (v1). First instance owns the pipe.
- **One request in flight at a time.** The pipe thread handles connections
  serially (a dispatch can hold it for up to 5s). Concurrent clients do NOT
  error: `interprocess` waits on a busy pipe (`WaitNamedPipeW`), so they
  queue, bounded by the client's 10s connect deadline (`CONNECT_TIMEOUT`) —
  a wedged server surfaces as a clear timeout error instead of an infinite
  hang.
- **A timed-out request never executes.** If the GUI doesn't drain within 5s
  (`REPLY_TIMEOUT`), the server tells the client "foreman did not respond" —
  and the GUI drops the stale request (or closes the terminal if the timeout
  raced the spawn). The client's failure report is always true, so a
  retrying dispatcher can't create duplicate terminals.
- **The pipe is a command-execution surface.** Security boundary v1 is the
  pipe's default same-user ACL — anything running as you can open terminals.
  That's the same trust level as the shell itself.
- Workers run with the dispatcher's user/permissions; there is no sandbox.

## Group chat (`chat` verb)

Status: **built** (2026-06-10).

Agents running inside a foreman project can coordinate — discuss design,
divide work, report results — in a shared **project chat room**. Posts are
broadcast-injected into every other member's terminal as typed input (push
delivery). A read-only chat subwindow lets the human watch the room.

### Protocol

Same pipe, same `REPLY_TIMEOUT`, same stale-request dropping as `open`.
Post and history are two separate request shapes; exactly one of `text` or
`history` must be set per request:

```json
{ "cmd": "chat", "project": "p1", "from": "t2", "text": "taking the parser refactor" }
{ "ok": true }

{ "cmd": "chat", "project": "p1", "from": "t2", "history": 20 }
{ "ok": true, "history": ["#12 t1: split the work by module", "#13 t3: I'll take src/wm.rs"] }
```

`project` and `from` are filled **automatically** by the client from
`FOREMAN_PROJECT_ID` / `FOREMAN_TERMINAL_ID`. Having both or neither
`text`/`history` is a client-side parse error (exit 2, no pipe call made).
The reply's `history` field is skipped in serialization when `None`, so
normal post replies stay minimal.

CLI grammar:

```
foreman chat [--project P] [--history [N]] [--] <message words...>
```

Workers invoke the CLI via the `FOREMAN_EXE` env var since the exe
directory is not on PATH inside spawned shells (e.g. PowerShell:
`& $env:FOREMAN_EXE chat "…"`; bash: `"$FOREMAN_EXE" chat "…"`).

- `foreman chat "message"` — post to the project room.
- `foreman chat --history [N]` — read the last N messages (default: 20,
  matching `DEFAULT_HISTORY`). Omitting N uses the default.
- `--` ends flag parsing: everything after it is the message verbatim. Use
  it when the message text starts with `--` or contains flag-like words.
- The first non-flag, non-`--` positional word also ends flag parsing; the
  rest of the slice is the message as-is, so flag-like text inside a
  message body is never re-interpreted.

### Room and membership

One room per project. The log lives on the **project's** nested
`WindowManager` as an `Rc<RefCell<ChatLog>>` — an in-memory
append-only `Vec<ChatMsg>` where each message carries `seq`, `from`, and
`text`. Seq starts at 1 and only grows (seq = `len + 1`). The log dies when
foreman exits; project IDs are runtime-scoped anyway.

**Membership is a `chat_member: bool` on each `Tab`** (not `Win`):

- Terminals spawned via `open` (agent dispatch) auto-join on creation —
  they are agents by construction.
- Any other terminal (e.g. a hand-opened Claude session) joins on its
  **first post** — orchestrators enter the moment they announce anything.
- `--history` reads do **not** join; reading is free.
- The flag lives on the `Tab`, so it travels with its terminal through
  merges and untabs. Broadcast delivers to **all** member tabs (not just
  active ones); background tabs stay drained via keepalive.
- Sender identity still resolves via Win id (active tab) — the same
  staleness family as terminal-id resolution elsewhere.

### Delivery

On a post the GUI thread:

1. Drops the request unexecuted if stale (same age check as `open`).
2. Resolves the project, validates the request.
3. Appends to the log and sets the sender's `chat_member` flag on its
   active tab (join-on-first-post).
4. Sends the success reply **before** injecting. An injection cannot be
   undone; bytes only flow once the client is guaranteed to hear "ok".
   A dead reply channel means the log may have appended, but bytes must
   not flow — the post stays in history (append-only; duplicate-on-retry
   is accepted v1 behavior), but is not injected.
5. Injects the framed message into every member tab in the project
   **except the sender's active tab**, skipping exited sessions.

**Framing:** `[chat p1 #14] t2: <text>`. Provenance plus seq so agents can
reference earlier messages.

**Bracketed paste:** delivery wraps the payload in `ESC[200~ … ESC[201~`
so multi-line content lands as one paste block, then a **deferred `\r`**
(~150 ms, fired by the frame loop's pump) to submit. The deferral exists
because Claude Code's burst detection folds a back-to-back `\r` into the
paste — it lands as a literal newline and the message sits unsubmitted in
the input box (live failure, 2026-06-10; the tmux `send-keys; sleep;
send-keys Enter` problem). Posts inside the window merge into one
submitted turn. ESC is stripped from the payload itself (added during
implementation review, mirroring alacritty's paste hygiene) so a crafted
`ESC[201~` inside the text cannot escape the paste block into live
keystrokes. The bracketed-paste guards are applied unconditionally —
live-verified on ConPTY (2026-06-10): claude sessions honor the markers
(multi-line lands as one input block), so the unconditional wrap is the
recorded v1 decision.

### Chat viewer window

A read-only viewer any project can open to watch the room:

- **Leader+G** (default chord; data-driven like all other bindings, so the
  user file merge keeps working).
- **Singleton per project**: re-invoking the chord focuses the existing
  window rather than opening a second one. If the window was minimized or
  buried under another tab, the reopen resurfaces it (unminimizes and
  makes the chat tab active) before focusing.
- Closing the viewer window does not touch the log. The room is the log.
- The window is a **viewer, not a member** — it renders the log directly
  and is never injected into. It uses `Content::Chat(Rc<RefCell<ChatLog>>)`,
  sharing the log pointer with the project manager.
- Renders `#seq from: text` lines, newest at the bottom, auto-tailed to
  fit the visible height. Updates live: posts arrive on the GUI thread,
  which always calls `ctx.request_repaint()` after a chat dispatch.

### Gotchas

- **`from`/`project` are guardrails, not authentication.** They prevent
  confused agents from posting to the wrong room or mis-identifying
  themselves — nothing more. Any same-user process can open the pipe and
  claim any `from`. Same trust boundary as `open`.
- **DSR fresh-spawn hazard.** Bytes injected into a just-spawned member
  before its shell's startup DSR handshake resolves get eaten by the DSR
  reply scan. The GUI pumps every session every frame, so one frame of
  separation normally suffices — but a post broadcast the same instant a
  member spawns can be silently lost to that member. The message stays in
  the log and history; only that member's injection is dropped.
- **Untab makes the terminal's own id stale.** Untab moves the `Tab` struct
  intact, so membership survives — but the detached terminal lands in a
  window with a **new** id, while its `FOREMAN_TERMINAL_ID` env var (stamped
  at spawn from the original window id) still names the old one. Its next
  post resolves `from` through that stale id: join and sender-exclusion act
  on whatever terminal now sits in the old window's active tab. Same Win-id
  staleness family as the sender-identity caveat in the membership section.
- **Orphaned post on timed-out reply.** If the GUI sends the "ok" reply and
  then immediately dies before the inject, or if the log appended before a
  dead-channel abort, the message exists in history but no bytes flowed.
  A retrying client creates a duplicate log entry. Both are accepted v1.
- **Message storms** are bounded by prompt convention (§5 of the spec)
  only — every post is a user-turn for every other member, and members may
  respond to responses. This is model behavior, not a code guarantee.
  Revisit (rate limits, @-mentions) only if it bites in practice.
- **Empty injection is a no-op at the Session level.** A bare `\r` would
  submit half-typed input. Empty messages are rejected server-side at
  `chat_post` ("empty message" — `foreman chat ""` parses fine client-side
  and travels the pipe; only the no-args case errors before sending). The
  Session-level empty-injection guard is defense-in-depth behind that.
- **Tab-merged projects go stale** (same as `open`): after merging project
  B's tab onto project A, B's old `pN` id no longer resolves and chatters
  inside B get an error until they pass an explicit `--project`.

## Out of scope (v1)

- **Round-trip**: `status`/`wait` verbs and a result-file convention are
  designed-for (IDs exist, protocol is JSON-extensible) but not built.
- **Agent-teams integration**: rejected. Watching `~/.claude/teams/*` state
  files means parsing another tool's private format that can break on any
  Claude update.

## Key files

- `src/control.rs` — pipe server thread, protocol types, arg parsing, client mode.
- `src/main.rs` — argv split (subcommand → client, else GUI); drain the
  control channel each frame and route to the desktop `WindowManager`.
- `src/wm.rs` — `handle_ctrl` (drain: stale-drop → project → spawn → reply,
  with orphaned-spawn undo), `add_terminal_cmd`, project resolution, env
  injection, exited-title refresh.
- `src/terminal.rs` — command sessions (vs shell sessions), env injection,
  exited-state title.
