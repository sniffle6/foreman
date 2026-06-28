# Chat persistence — implementation handoff

**Status: designed, not built.** This is the build plan for
`docs/chat-missing-features.md` § Layer-1 #2 ("Persistence"). It is the output of
a four-lens design debate (reliability / performance / grug / deep-modules) run in
the project chat room on 2026-06-27; the four converged on ONE proposal, recorded
below. Read this top-to-bottom before touching code — the hard decisions are
already made, and a couple of them are non-obvious.

## What it does

Make the project chat log survive a foreman restart or crash. Today the log is an
in-memory `Vec<ChatMsg>` (`ChatLog` in `src/chat.rs`); on exit it evaporates —
every cited `#N` dangles, `--history` is empty, seq restarts at 1. After this
change the log is an **append-only JSONL file**, reloaded on startup, with seq
staying **monotonic across restarts**.

## Why it exists

It is a reliability gap and the precondition for search / jump-to-#N (gaps #9).
The driving fear is a **silently lost "done" message**: a post an agent already
acted on must never disappear because the process died. The whole design is built
around "a post is durable before anyone sees it."

## The converged design (do not re-litigate these)

The debate killed a lot of tempting complexity. These are settled:

1. **Format** — append-only **JSONL**, one post = one JSON line. The trailing
   **newline is the commit marker**: a line without it is a torn write, discarded
   on load.
2. **One narrow module, plain methods, NO trait.** Durability lives behind a
   small interface on `ChatLog` (`append → Seq`, `load(path) → Self`,
   `replay(from_seq) → posts`) so no caller carries the write/ordering/recovery
   rule. A storage *trait/port with a swappable in-memory adapter was rejected* —
   it failed the deletion test (tests just point at a temp file and exercise the
   real path; an in-memory fake is *worse*, it tests a Vec prod never runs). Add a
   trait the day a SECOND real backend (sqlite, net-sync) actually lands, not now.
3. **Synchronous write on the post path. NO writer thread, NO channel, NO async
   runtime.** Posting is a rare human/agent event, not a per-frame event. Open the
   append handle ONCE at startup; each post is a single `write()` of record+newline
   to the live handle; the seq is returned only after that `write()` returns; THEN
   the post is echoed. A background writer thread was proposed and **withdrawn by
   its own author** — it saves zero milliseconds (the write is sub-ms on an open
   handle) and only *adds* a crash-loss window (queued-but-unwritten posts) plus
   shutdown-flush plumbing.
4. **`fsync` is periodic and OFF the path.** A plain `write()` reaches the OS page
   cache in microseconds and **survives the dominant failure — the foreman process
   dying** (panic, relink, restart) — because the kernel still owns the buffer.
   `fsync` only buys power-loss / kernel-panic durability, which is the rare tail.
   So never `fsync` on the post path or the per-frame loop; an optional
   once-every-few-seconds background flush bounds a power-cut loss to seconds.
5. **Reload is one forward pass.** `load()` reads the file once, fills the resident
   Vec, drops an unterminated final line as a torn tail, and takes `next_seq` from
   the last COMPLETE record. The whole body stays resident (you need it for
   `--history` and scrollback anyway). **No lazy paging / windowing** — a per-
   project agent chat stays small for a long time; add windowing only the day a
   profile shows the startup parse actually hurts.
6. **Delivery is at-least-once + dedup-by-seq, not exactly-once.** The delivery
   cursor advances at-least-once; if a crash lands between injecting a post and
   persisting the cursor, replay re-delivers from the stale cursor and the consumer
   dedups on a seq it already saw. That fails toward a *duplicate* done message,
   never a *lost* one — the correct direction. Exactly-once would need
   transaction-grade machinery tying inject+cursor-persist atomically; that was
   explicitly rejected as the complexity demon.

## How it maps onto the actual code

The debate spoke of an idealized `ChatLog`. The real `src/chat.rs` is already most
of the way there — it has the deep module, it just doesn't write to disk:

| Debate name            | Real code today (`src/chat.rs`)                         |
|------------------------|---------------------------------------------------------|
| `append(post) → Seq`   | `ChatLog::push` / `ChatLog::post_re` (returns `seq`)    |
| `replay(from_seq)`     | **`ChatLog::deliver_after(member_id, after)`** — exists |
| delivery cursor        | `MemberState.cursor` in `ChatRoom` (in-memory)          |
| `load(path) → Self`    | **does not exist yet — this is the work**               |

`replay()` is already built: `deliver_after` scans the whole Vec from `after`, so
it already has the no-silent-truncation property the design demands. Good.

Two structural facts that make this cheap and safe:

- **Persist-then-echo is already true for free.** A `foreman chat` post is applied
  on the egui UI thread via `WindowManager::chat_post` (`src/wm.rs:1529`) →
  `ChatRoom::post`, which returns the seq synchronously. The echo/injection happens
  on a *later* frame in `ChatRoom::tick` (`src/wm.rs:1574`). So adding the
  synchronous `write()` inside `push` — before it returns the seq — guarantees
  "durable before echoed" with **zero new threading**. `tick` only *reads* the log
  to build inject lines; keep it that way (never write/fsync in `tick`, it runs
  every frame).
- **Everything is single-threaded** (the egui UI thread). The write is on the UI
  thread but only on the rare post path. That is exactly what the design wants.

## ⚠ The one non-obvious gotcha: seq is currently coupled to Vec length

`ChatLog::push` assigns `seq = self.msgs.len() as u64 + 1` and `last_seq()` returns
`msgs.len()`. **This silently breaks the design unless you decouple it.** It works
across restart *only if* the reloaded Vec is dense (every entry, including the
`Joined`/`Exited` sys lines, present in order). The moment `load()` is allowed to
"MAY window" (decision #5's own future) or you choose not to persist sys entries,
`len ≠ last_seq` and seqs collide or rewind — the exact monotonicity break the
whole feature exists to prevent.

**Fix it first:** add an explicit `next_seq: u64` to `ChatLog`, assign from it in
`push`, bump after. Derive it on `load()` from the last complete record's seq + 1.
This is the debate's "next_seq from the last complete line" and it is the single
change that makes the seq invariant survive the design's own future. Small, pure,
covered by the existing tests once the `len` assumption is removed.

## Implementation plan (ordered)

1. **Decouple seq from length.** Add `next_seq` to `ChatLog`; assign+bump in
   `push`; `last_seq()` returns the last assigned seq. Pure refactor. Add a test:
   seqs stay monotonic after a simulated load whose Vec is not `1..=N` dense.
2. **Define the on-disk record.** A small `#[derive(Serialize, Deserialize)]`
   struct mirroring `ChatMsg` (`seq`, `from`, `name`, `text`, `to`, `re`, `kind`),
   with `at` stored as **epoch-millis `u64`** (don't serialize `SystemTime`
   directly — keep the JSON stable and human-readable). serde + serde_json are
   already deps, so this is *not* a "schema framework" — it's one derive. One JSON
   object per line.
3. **`ChatLog::open(path) -> io::Result<Self>`** — one forward pass: read the file
   line by line, parse each into a `ChatMsg`, push into the Vec; if the final line
   has no trailing newline, drop it (torn tail); set `next_seq = last_complete +
   1`; then open the file in append mode and **keep the handle** on the struct. One
   op = load + recover + ready-to-append.
4. **Write on append.** In `push`, after building the `ChatMsg`, `write_all` its
   JSON line + `\n` to the open handle **before returning the seq**. No `fsync`.
5. **Wire it through `ChatRoom` / `WindowManager`.** Resolve a per-project path and
   pass it to `ChatRoom::new(path)` → `ChatLog::open(path)`. The room is constructed
   at `src/wm.rs:662` and a project's `cwd` is set in `add_project`
   (`src/wm.rs:756`). Plumb the path in there.
6. **(Optional polish) periodic off-path `fsync`.** Flush on an idle / few-second
   cadence, never on the per-frame `tick` and never on the post path.

## Open decisions (settle before coding)

- **Where the file lives.** A project has a stable `cwd` (`src/wm.rs:599`); the
  project id `pN` is **session-assigned and NOT stable across restart**, so it
  cannot key the file. Two options:
  - (a) `<cwd>/.foreman/chat.jsonl` — literally "beside the project", but it
    **litters every external repo foreman opens** with a chat file. Grug objects.
  - (b) **(recommended)** `%APPDATA%\foreman\chat\<hash-of-cwd>.jsonl` — keyed by a
    hash of the project cwd (the stable cross-restart key), keeping foreman's data
    out of the user's repos. Matches the existing `%APPDATA%\foreman\
    keybindings.json` precedent.
- **Cursor / membership persistence — likely DEFER.** The design's membership story
  ("a terminal respawning under its old `FOREMAN_TERMINAL_ID` calls
  `replay(cursor)`") assumes terminal ids are stable across restart. **They are
  not** — foreman assigns `tN` per session, so after a restart the old terminals
  are gone and new ones get fresh ids; no member re-attaches under its old id. So
  on reload there are zero live members, nothing delivers, and a brand-new terminal
  joins-on-first-post at head. **The headline win is the message log surviving** (so
  `#N` citations and `--history` work across restart); persisting per-member
  cursors only pays off once foreman can restore a terminal under its old id, which
  it can't today. Ship the log first; treat cursor persistence as a later, separate
  step gated on id-restoration. Persisted membership/cursor records are otherwise
  harmless dead weight.

## Key files

- `src/chat.rs` — the module. `ChatLog` (add `next_seq`, the handle, `open`, the
  write-on-`push`), `ChatMsg` (the serde record), `deliver_after` (already
  `replay`). All persistence logic lives here.
- `src/wm.rs` — `WindowManager` owns `chat: Rc<RefCell<ChatRoom>>` (`:607`/`:662`);
  `chat_post` (`:1529`) is the write trigger, `tick` (`:1574`) the read-only
  per-frame echo. Path resolution hooks into `add_project` (`:756`). **Do not add a
  disk touch to `tick`.**
- `src/control.rs` — the `foreman chat` CLI/IPC client; no change needed (it posts
  through the same server path).
- `docs/chat-missing-features.md` — § Layer-1 #2 is the parent item this builds.
- `docs/chat-delivery.md` — the in-memory delivery-cursor mechanism being made
  durable.
- `docs/contracts/chat-handshake-contract.md` — the cursor/`#N`/dedup contract the
  at-least-once delivery semantic must keep honoring.
