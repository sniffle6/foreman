# Chat Persistence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make each project's chat log survive foreman restarts/crashes as an append-only JSONL file, with seq monotonic across restarts.

**Architecture:** All persistence lives inside `ChatLog` (`src/chat.rs`): an optional append handle, a synchronous `write()` on the post path, and a one-pass loader with torn-tail discard. `ChatRoom`/`WindowManager` only gain a path-plumbing call in `add_project`. The spec is `docs/chat-persistence.md` (four-lens debate, converged 2026-06-27) — its six decisions are settled; do not re-litigate them.

**Tech Stack:** Rust (edition 2024, GNU toolchain), serde + serde_json (already deps), std file IO only.

## Global Constraints

- **Settled decisions (from `docs/chat-persistence.md`, verbatim intent):** append-only JSONL, trailing newline = commit marker; one narrow module, plain methods, **NO trait**; **synchronous write on the post path, NO writer thread/channel/async**; **no `fsync` on the post path or per-frame loop** (the periodic off-path fsync polish is explicitly DEFERRED from this plan); reload is one forward pass, whole body resident, no windowing; delivery is at-least-once + dedup-by-seq.
- **Never add a per-frame disk touch.** `ChatRoom::tick` / `WindowManager::chat_tick` run every frame. (Known + accepted: `tick` already appends an `Exited` sys line when a member vanishes — a rare event, not per-frame; with persistence that append now also writes one line. That is event-driven and fine. Adding anything that writes *every* frame is a bug.)
- **No new dependencies.** serde and serde_json are in Cargo.toml. Tests use `std::env::temp_dir()`, not the `tempfile` crate.
- **Control-plane wire format v1 is untouched.** No `src/control.rs` changes.
- **Decisions made by this plan (flagged for review, see task rationale):** (1) `session_floor` join guard — new, required, the design doc missed it; (2) file key = `<leaf>-<fnv8>.jsonl` under `%APPDATA%\foreman\chat\` (spec's recommended option b, FNV-1a hand-rolled because `DefaultHasher` is not stable across Rust releases); (3) on a failed append write: disable persistence for the session and keep posting in-memory (availability over durability), `eprintln` once; (4) cursor/membership persistence DEFERRED per the spec's own open-decision (terminal ids are not stable across restart).
- **Known accepted limitation (document, don't guard):** opening the same cwd in two simultaneous project windows shares one file via two append handles — duplicate seqs become possible. Rare, documented in Task 7.
- Build loop (Windows/PowerShell): kill the app first or the link fails — `Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue`. Full test suite: `cargo test`; chat module only: `cargo test --lib chat`.
- Work on a new branch `feat/chat-persistence` off `main`. Commit messages: `type(scope): subject`, body optional, end with the standard `Co-Authored-By: Claude <model> <noreply@anthropic.com>` trailer. Stage files by name (`git add src/chat.rs …`), never `git add -A`.
- Line-number references below are approximate (baseline `7fda1c2` + `feat/quiet-project-chrome`); locate by symbol name, not line.

---

### Task 1: Decouple seq from Vec length

The single landmine called out by the spec: `ChatLog::push` assigns `seq = msgs.len() + 1` and `last_seq()` returns `msgs.len()`. That only works while the Vec is dense `1..=N`. A reloaded log (or any future windowing) breaks it — seqs collide or rewind, which is the exact monotonicity break persistence exists to prevent. Fix it before touching disk.

**Files:**
- Modify: `src/chat.rs` — `ChatLog` struct, `ChatLog::new`, `ChatLog::push`, `ChatLog::last_seq`
- Test: `src/chat.rs` `mod tests` (in-module, same file)

**Interfaces:**
- Consumes: nothing new.
- Produces: `ChatLog.next_seq: u64` private field; `last_seq()` semantics unchanged for all existing callers (still "seq of the most recent entry", 0 when empty).

- [ ] **Step 1: Write the failing test** (add to `mod tests` in `src/chat.rs`, near `post_assigns_increasing_seq_from_one`)

```rust
#[test]
fn seq_stays_monotonic_when_vec_is_not_dense() {
    let mut log = ChatLog::new();
    log.post("t1", "t1", "one"); // #1
    log.post("t1", "t1", "two"); // #2
    log.msgs.remove(0); // simulate a non-dense vec (partial/windowed load)
    let seq = log.post("t1", "t1", "three").seq;
    assert_eq!(seq, 3, "seq must come from next_seq, not msgs.len()+1");
    assert_eq!(log.last_seq(), 3);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib chat::tests::seq_stays_monotonic_when_vec_is_not_dense`
Expected: FAIL — left `2`, right `3` (old code reassigns seq 2 after the remove).

- [ ] **Step 3: Implement**

`ChatLog` struct becomes:

```rust
pub struct ChatLog {
    msgs: Vec<ChatMsg>,
    /// Next seq to assign. Decoupled from `msgs.len()` so seqs stay
    /// monotonic when the Vec is not dense 1..=N (e.g. a reloaded log).
    next_seq: u64,
}
```

`ChatLog::new`:

```rust
pub fn new() -> Self {
    Self { msgs: Vec::new(), next_seq: 1 }
}
```

In `ChatLog::push`, replace `seq: self.msgs.len() as u64 + 1,` with `seq: self.next_seq,` and add `self.next_seq += 1;` immediately before `self.msgs.push(msg);`.

`ChatLog::last_seq` (update the doc comment too — it currently claims len-equality):

```rust
/// Seq of the most recent entry (any kind); 0 when the log is empty.
pub fn last_seq(&self) -> u64 {
    self.next_seq - 1
}
```

- [ ] **Step 4: Run the chat suite**

Run: `cargo test --lib chat`
Expected: PASS, all existing tests included (`post_assigns_increasing_seq_from_one`, `tick_*`, …).

- [ ] **Step 5: Commit**

```powershell
git add src/chat.rs
git commit -m "refactor(chat): decouple seq from Vec length with explicit next_seq"
```

---

### Task 2: The on-disk record

One JSON object per line. A separate serde struct — never derive on `ChatMsg` itself, because `SystemTime`'s serialized shape is neither stable nor human-readable; `at` is stored as epoch-millis `u64`.

**Files:**
- Modify: `src/chat.rs` — add `ChatRecord` (private) after the `ChatMsg` impl; add serde derives to `ChatKind`
- Test: `src/chat.rs` `mod tests`

**Interfaces:**
- Consumes: `ChatMsg`, `ChatKind` (existing).
- Produces: `ChatRecord::from_msg(&ChatMsg) -> ChatRecord`, `ChatRecord::into_msg(self) -> ChatMsg` (private, used by Task 3). File format field names: `seq, from, name, text, at, to, re, kind` — these ARE the format; never rename.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn record_round_trips_a_msg() {
    let m = ChatMsg {
        seq: 14,
        from: "t3".into(),
        name: "api".into(),
        text: "total is cents".into(),
        at: std::time::UNIX_EPOCH + std::time::Duration::from_millis(1_750_000_000_000),
        to: vec!["t2".into()],
        re: Some(9),
        kind: ChatKind::Post,
    };
    let line = serde_json::to_string(&ChatRecord::from_msg(&m)).unwrap();
    let back = serde_json::from_str::<ChatRecord>(&line).unwrap().into_msg();
    assert_eq!(back.seq, 14);
    assert_eq!(back.from, "t3");
    assert_eq!(back.name, "api");
    assert_eq!(back.at, m.at, "epoch-millis round-trip must be exact");
    assert_eq!(back.to, m.to);
    assert_eq!(back.re, Some(9));
    assert_eq!(back.kind, ChatKind::Post);
}

#[test]
fn record_json_is_stable_and_lean() {
    let m = ChatMsg {
        seq: 1,
        from: "you".into(),
        name: "you".into(),
        text: "hi".into(),
        at: std::time::UNIX_EPOCH + std::time::Duration::from_millis(1000),
        to: Vec::new(),
        re: None,
        kind: ChatKind::Post,
    };
    // Locks the file format: field names/order, and empty to / None re omitted.
    assert_eq!(
        serde_json::to_string(&ChatRecord::from_msg(&m)).unwrap(),
        r#"{"seq":1,"from":"you","name":"you","text":"hi","at":1000,"kind":"Post"}"#
    );
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib chat::tests::record_`
Expected: FAIL to compile — `ChatRecord` not defined.

- [ ] **Step 3: Implement**

Add serde derives to `ChatKind`'s existing `#[derive(...)]` list: `serde::Serialize, serde::Deserialize`. If `ChatKind` does not already derive `Clone, Copy`, add those too (it is a fieldless three-variant enum; `ChatRecord` conversion copies it).

Add after the `ChatMsg` impl block:

```rust
/// On-disk record: one JSON object per line in the project's chat JSONL.
/// Mirrors [`ChatMsg`] with `at` as epoch-millis so the file stays stable
/// and human-readable. Field names are the file format — do not rename.
#[derive(serde::Serialize, serde::Deserialize)]
struct ChatRecord {
    seq: u64,
    from: String,
    name: String,
    text: String,
    at: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    to: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    re: Option<u64>,
    kind: ChatKind,
}

impl ChatRecord {
    fn from_msg(m: &ChatMsg) -> Self {
        Self {
            seq: m.seq,
            from: m.from.clone(),
            name: m.name.clone(),
            text: m.text.clone(),
            at: m
                .at
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            to: m.to.clone(),
            re: m.re,
            kind: m.kind,
        }
    }

    fn into_msg(self) -> ChatMsg {
        ChatMsg {
            seq: self.seq,
            from: self.from,
            name: self.name,
            text: self.text,
            at: std::time::UNIX_EPOCH + std::time::Duration::from_millis(self.at),
            to: self.to,
            re: self.re,
            kind: self.kind,
        }
    }
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test --lib chat`
Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src/chat.rs
git commit -m "feat(chat): on-disk ChatRecord with epoch-millis timestamps"
```

---

### Task 3: `ChatLog::open` — load, torn-tail discard, write-on-push

The heart of the feature. `open()` is one forward pass (spec decision 5); `push()` writes the record line to the live handle **before returning** (spec decision 3 — and because `ChatRoom::post` returns the seq synchronously while injection happens on a later `tick` frame, write-inside-push gives "durable before echoed" with zero new threading). A line without a trailing `\n` was never committed — discard it (decision 1).

**Files:**
- Modify: `src/chat.rs` — `ChatLog` struct, `ChatLog::new`, `ChatLog::push`; add `ChatLog::open`, `ChatLog::session_floor`
- Test: `src/chat.rs` `mod tests`

**Interfaces:**
- Consumes: `ChatRecord` from Task 2, `next_seq` from Task 1.
- Produces: `pub fn open(path: &std::path::Path) -> std::io::Result<ChatLog>`; `pub fn session_floor(&self) -> u64` (seq of the last loaded record, 0 for fresh/in-memory — Task 4 depends on it); `ChatLog.file: Option<std::fs::File>` private.

- [ ] **Step 1: Write the failing tests** (add the helper at the top of `mod tests`)

```rust
fn tmp_log(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir()
        .join(format!("foreman-chat-test-{}-{tag}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn open_missing_file_starts_fresh_and_appends() {
    let path = tmp_log("fresh");
    let mut log = ChatLog::open(&path).unwrap();
    assert_eq!(log.last_seq(), 0);
    assert_eq!(log.session_floor(), 0);
    log.post("t1", "t1", "hello");
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert!(on_disk.ends_with('\n'), "the trailing newline is the commit marker");
    assert!(on_disk.contains(r#""text":"hello""#));
}

#[test]
fn reopen_restores_messages_and_seq_continues() {
    let path = tmp_log("roundtrip");
    {
        let mut log = ChatLog::open(&path).unwrap();
        log.post("t1", "t1", "one");
        log.post("t2", "t2", "two");
    } // drop closes the handle; plain write()s are already in the page cache
    let mut log = ChatLog::open(&path).unwrap();
    assert_eq!(log.msgs().len(), 2);
    assert_eq!(log.last_seq(), 2);
    assert_eq!(log.session_floor(), 2);
    let seq = log.post("t3", "t3", "three").seq;
    assert_eq!(seq, 3, "seq must stay monotonic across restart");
}

#[test]
fn torn_tail_is_discarded_on_load() {
    let path = tmp_log("torn");
    {
        let mut log = ChatLog::open(&path).unwrap();
        log.post("t1", "t1", "committed");
    }
    // Simulate a crash mid-write: half a record, no newline.
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(br#"{"seq":2,"from":"t1","na"#).unwrap();
    }
    let log = ChatLog::open(&path).unwrap();
    assert_eq!(log.msgs().len(), 1, "an unterminated final line was never committed");
    assert_eq!(log.last_seq(), 1);
}

#[test]
fn bad_middle_line_is_skipped_not_fatal() {
    let path = tmp_log("badline");
    std::fs::write(
        &path,
        concat!(
            r#"{"seq":1,"from":"t1","name":"t1","text":"ok","at":0,"kind":"Post"}"#, "\n",
            "not json at all\n",
            r#"{"seq":3,"from":"t1","name":"t1","text":"also ok","at":0,"kind":"Post"}"#, "\n",
        ),
    )
    .unwrap();
    let log = ChatLog::open(&path).unwrap();
    assert_eq!(log.msgs().len(), 2);
    assert_eq!(log.last_seq(), 3, "next_seq derives from the max loaded seq");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib chat::tests::open_missing`
Expected: FAIL to compile — `open` / `session_floor` not defined.

- [ ] **Step 3: Implement**

Grow the struct (note the two new fields; if any `#[derive(Clone)]` exists on `ChatLog` or `ChatRoom`, remove it — `File` is not `Clone`, and nothing clones the room, only its `Rc`):

```rust
pub struct ChatLog {
    msgs: Vec<ChatMsg>,
    /// Next seq to assign (decoupled from msgs.len(), see Task 1).
    next_seq: u64,
    /// Seq of the last record loaded from disk; 0 for fresh/in-memory logs.
    /// Joins floor their delivery cursor here so persisted history is never
    /// re-injected into a new session's terminals (Task 4).
    session_floor: u64,
    /// Open append handle; `None` = in-memory (unit tests, the desktop
    /// manager's memberless room, or persistence disabled after an IO error).
    file: Option<std::fs::File>,
}
```

```rust
pub fn new() -> Self {
    Self { msgs: Vec::new(), next_seq: 1, session_floor: 0, file: None }
}

/// Seq of the last record loaded from disk (0 for fresh/in-memory logs).
pub fn session_floor(&self) -> u64 {
    self.session_floor
}

/// Load (or create) the JSONL log at `path` and keep an append handle.
/// One forward pass: each newline-terminated line parses into a message;
/// a final line without `\n` is a torn write and is discarded (the newline
/// is the commit marker). `next_seq` continues past the highest loaded seq.
pub fn open(path: &std::path::Path) -> std::io::Result<Self> {
    let buf = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let mut msgs = Vec::new();
    let mut max_seq = 0u64;
    for line in buf.split_inclusive('\n') {
        if !line.ends_with('\n') {
            break; // torn tail: never committed, discard
        }
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<ChatRecord>(line) {
            Ok(rec) => {
                max_seq = max_seq.max(rec.seq);
                msgs.push(rec.into_msg());
            }
            Err(e) => {
                eprintln!("foreman: chat log {}: skipping bad line: {e}", path.display());
            }
        }
    }
    let file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    Ok(Self { msgs, next_seq: max_seq + 1, session_floor: max_seq, file: Some(file) })
}
```

In `ChatLog::push`, insert between building `msg` and `self.msgs.push(msg);` (this ordering IS the durability contract — the seq escapes `push` only after the bytes reached the OS):

```rust
        if let Some(f) = self.file.as_mut() {
            use std::io::Write;
            let mut line = serde_json::to_string(&ChatRecord::from_msg(&msg))
                .expect("ChatRecord serialization cannot fail");
            line.push('\n');
            if let Err(e) = f.write_all(line.as_bytes()) {
                // Availability over durability: keep the session chatting
                // in-memory rather than dropping the post; disable further
                // writes so we don't spam or tear the file.
                eprintln!("foreman: chat log write failed, persistence disabled: {e}");
                self.file = None;
            }
        }
```

No `fsync` anywhere — spec decision 4.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test --lib chat`
Expected: PASS, including all pre-existing tests (in-memory `new()` behavior is unchanged).

- [ ] **Step 5: Commit**

```powershell
git add src/chat.rs
git commit -m "feat(chat): durable JSONL log - open/load with torn-tail discard, write-on-post"
```

---

### Task 4: `ChatRoom::open` + the join floor (restart replay guard)

**Why (this is the plan's one addition to the spec):** `ChatRoom::join` registers members with `cursor: 0`, and `deliver_after` returns every addressed `Post` with `seq > cursor`. Today that's harmless — the log is empty at session start. With a loaded log it means **every fresh terminal gets the project's entire persisted history injected as typed input** (hundreds of paste+submit turns into a booting agent). The spec's own open-decision assumes new terminals "join at head" — this task makes the code match: joins floor their cursor at `session_floor`. For in-memory logs the floor is 0, so **live-session semantics are exactly unchanged** (including catch-up of posts made earlier in the same session, which dispatch relies on).

**Files:**
- Modify: `src/chat.rs` — `ChatRoom::join` (one line), add `ChatRoom::open`
- Test: `src/chat.rs` `mod tests`

**Interfaces:**
- Consumes: `ChatLog::open`, `ChatLog::session_floor` (Task 3).
- Produces: `pub fn open(path: &std::path::Path) -> std::io::Result<ChatRoom>` — Task 6 calls this from `wm.rs`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn reloaded_history_is_never_reinjected_into_new_members() {
    let path = tmp_log("floor");
    {
        let mut log = ChatLog::open(&path).unwrap();
        log.post("t9", "t9", "ancient broadcast");
        log.post("t9", "t9", "another old one");
    }
    let mut room = ChatRoom::open(&path).unwrap();
    room.join("t2", "worker");
    let live = [LiveMember {
        id: "t2".to_string(),
        name: "worker".to_string(),
        ready: true,
        exited: false,
    }];
    assert!(
        room.tick("p1", &live).is_empty(),
        "persisted history must not replay into a fresh session"
    );
    room.post("you", "fresh post", &[], None).unwrap();
    let out = room.tick("p1", &live);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].lines.len(), 1, "only the post made after the restart delivers");
    assert!(out[0].lines[0].contains("fresh post"));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib chat::tests::reloaded_history_is_never_reinjected_into_new_members`
Expected: FAIL to compile (`ChatRoom::open` missing). After adding `open` alone it must fail the first assert — two lines deliver — proving the floor is load-bearing.

- [ ] **Step 3: Implement**

Add to the `ChatRoom` impl, next to `new`:

```rust
    /// A room whose log is durable at `path` (see [`ChatLog::open`]). Loaded
    /// history is visible to the viewer and `--history`, but never
    /// re-injected: joins floor their delivery cursor at the loaded tail.
    pub fn open(path: &std::path::Path) -> std::io::Result<Self> {
        let mut room = Self::new();
        room.log = ChatLog::open(path)?;
        Ok(room)
    }
```

In `ChatRoom::join`, change the registered state's `cursor: 0,` to:

```rust
                cursor: self.log.session_floor(),
```

- [ ] **Step 4: Run the whole suite**

Run: `cargo test --lib chat`
Expected: PASS — every existing `tick_*` / join test still passes because in-memory floors are 0.

- [ ] **Step 5: Commit**

```powershell
git add src/chat.rs
git commit -m "feat(chat): ChatRoom::open with join floor so reloaded history never re-injects"
```

---

### Task 5: Per-project file path in `config.rs`

Spec open-decision resolved as its recommended option (b): `%APPDATA%\foreman\chat\<name>-<hash8>.jsonl`, keyed by the project **cwd** — the only project identity stable across restarts (`pN` tags are session-assigned). Leaf name kept for human readability; FNV-1a over the full lowercased path for uniqueness. FNV is hand-rolled (8 lines) because `std::hash::DefaultHasher` output is not guaranteed stable across Rust releases — a compiler upgrade must not orphan every history file. Matches the `keybindings.json` / `settings.json` precedent of keeping foreman's data out of user repos.

**Files:**
- Modify: `src/config.rs` — add `chat_log_path` after `config_dir`
- Test: `src/config.rs` `mod tests`

**Interfaces:**
- Consumes: `config_dir()` (existing, `src/config.rs:29`).
- Produces: `pub fn chat_log_path(cwd: &std::path::Path) -> Option<std::path::PathBuf>` — `None` when `APPDATA` is unset or the dir can't be created (caller falls back to in-memory).

- [ ] **Step 1: Write the failing test** (in `src/config.rs` `mod tests`; `config_dir` needs `APPDATA`, which is always set on the dev machine)

```rust
#[test]
fn chat_log_path_is_stable_across_spellings_and_unique_across_dirs() {
    use std::path::Path;
    let a = chat_log_path(Path::new(r"H:\Claude Code\foreman")).unwrap();
    let b = chat_log_path(Path::new(r"h:\claude code\foreman\")).unwrap();
    assert_eq!(a, b, "case / trailing separator must not fork the history");
    let c = chat_log_path(Path::new(r"H:\somewhere else\foreman")).unwrap();
    assert_ne!(a, c, "same leaf name in a different dir must not collide");
    let name = a.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with("foreman-"), "leaf kept for readability: {name}");
    assert!(name.ends_with(".jsonl"));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib config::tests::chat_log_path_is_stable_across_spellings_and_unique_across_dirs`
Expected: FAIL to compile — `chat_log_path` not defined.

- [ ] **Step 3: Implement**

```rust
/// Per-project chat-log path: `%APPDATA%\foreman\chat\<leaf>-<hash8>.jsonl`.
/// Keyed by the project cwd (the stable cross-restart identity — `pN` tags
/// are session-assigned). Leaf name for readability, FNV-1a of the full
/// lowercased path for uniqueness. FNV is hand-rolled because
/// `DefaultHasher` is not stable across Rust releases, and a changed hash
/// would orphan every existing history file. Case and trailing separators
/// are normalized; opening the same directory through a different alias
/// (subst drive, UNC) creates a separate history — accepted.
pub fn chat_log_path(cwd: &std::path::Path) -> Option<std::path::PathBuf> {
    let dir = config_dir()?.join("chat");
    std::fs::create_dir_all(&dir).ok()?;
    let full = cwd.to_string_lossy().to_lowercase();
    let full = full.trim_end_matches(['\\', '/']);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in full.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let leaf: String = cwd
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(40)
        .collect();
    let leaf = leaf.trim_matches('-');
    let name = if leaf.is_empty() {
        format!("{:08x}.jsonl", h & 0xffff_ffff)
    } else {
        format!("{leaf}-{:08x}.jsonl", h & 0xffff_ffff)
    };
    Some(dir.join(name))
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --lib config`
Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src/config.rs
git commit -m "feat(config): stable per-project chat log path under APPDATA"
```

---

### Task 6: Wire it into `add_project` + end-to-end verification

The room is created inside `WindowManager::new()`; the cwd only becomes known in `add_project` (`src/wm.rs`, both the picker and the `foreman open` dispatch path funnel here). Replace the fresh room's contents in place — same `Rc`, and no viewers or members can exist yet at that point. Best-effort like `skills_install.rs`: any failure logs to stderr and the room stays in-memory (today's behavior).

**Files:**
- Modify: `src/wm.rs` — `WindowManager::add_project`
- Test: manual end-to-end (GUI + CLI); full `cargo test` as the regression gate

**Interfaces:**
- Consumes: `crate::config::chat_log_path` (Task 5), `crate::chat::ChatRoom::open` (Task 4).
- Produces: nothing new — behavior only.

- [ ] **Step 1: Implement** (current body shown in full; the marked lines are the only addition — note the path is resolved from `&cwd` BEFORE `cwd` moves into `child.cwd`)

```rust
    pub fn add_project(&mut self, shell: Shell, cwd: PathBuf, ctx: &egui::Context) -> WinId {
        let (id, rect) = self.next_slot(egui::vec2(720.0, 480.0));
        let title = cwd
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.to_string())
            .unwrap_or_else(|| format!("project {}", id));
        let mut child = WindowManager::new();
        child.tag = Some(format!("p{}", id));
        // Attach the durable chat log before any member can join or post.
        // Best-effort: on any failure the room stays in-memory.
        if let Some(path) = crate::config::chat_log_path(&cwd) {
            match crate::chat::ChatRoom::open(&path) {
                Ok(room) => *child.chat.borrow_mut() = room,
                Err(e) => eprintln!(
                    "foreman: chat persistence disabled for {}: {e}",
                    path.display()
                ),
            }
        }
        child.cwd = Some(cwd);
        if let Some(tid) = child.add_terminal(shell, ctx) {
            child.tile_new(tid, None);
        }
        self.push_win(id, title, rect, Content::Project(Box::new(child)));
        id
    }
```

- [ ] **Step 2: Full test suite**

Run: `cargo test`
Expected: PASS (wm, layout, chat, config, input, …).

- [ ] **Step 3: End-to-end verification (the feature's acceptance gate — do not skip)**

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 5
cargo run
```

1. Open a project (any directory) via the picker.
2. In a terminal inside it: `foreman chat "persistence smoke test"`.
3. Verify the file: `Get-Content "$env:APPDATA\foreman\chat\*.jsonl"` — exactly one JSON line containing `"persistence smoke test"`, ending in a newline.
4. Quit foreman entirely (close the last project). Restart with `cargo run`, open the **same** directory.
5. `foreman chat --history` — the smoke-test post is there with its original seq.
6. `foreman chat "post-restart"` — lands with seq N+1 (monotonic), and the terminal does **not** receive an injection of the old history.
7. Open the chat viewer — both messages render.

Expected: all seven observations hold. If step 6 injects old history, the Task 4 floor is broken — stop and fix before committing.

- [ ] **Step 4: Commit**

```powershell
git add src/wm.rs
git commit -m "feat(wm): attach durable per-project chat log in add_project"
```

---

### Task 7: Documentation

Per the project's doc rules: update the existing feature doc rather than creating a new one; one doc per system.

**Files:**
- Modify: `docs/chat-persistence.md` — status header + implementation deltas
- Modify: `docs/chat-missing-features.md` — § Layer-1 #2 status note
- Modify: `CLAUDE.md` — the `src/chat.rs` architecture bullet

**Interfaces:** none — prose only.

- [ ] **Step 1: Update `docs/chat-persistence.md`**

Change the status line at the top to:

```markdown
**Status: BUILT 2026-07-02** (plan: `docs/superpowers/plans/2026-07-02-chat-persistence.md`).
The six converged decisions below shipped unchanged. Implementation deltas:
- **Join floor (addition):** `ChatRoom::join` floors a member's delivery cursor at
  `ChatLog::session_floor()` (the last seq loaded from disk), so persisted history is
  never re-injected into a fresh session's terminals. In-memory floors are 0, so
  live-session join/catch-up semantics are byte-identical to before.
- **File key:** `%APPDATA%\foreman\chat\<leaf>-<fnv8>.jsonl` (open decision (b));
  FNV-1a hand-rolled because `DefaultHasher` is unstable across Rust releases.
- **Write-error policy:** a failed append disables persistence for the session
  (stderr note) and posting continues in-memory — availability over durability.
- **Deferred as designed:** per-member cursor/membership persistence (blocked on
  stable terminal ids) and the periodic off-path fsync polish.
- **Known limitation:** the same cwd open in two project windows at once shares one
  file through two append handles; duplicate seqs become possible. Documented, not guarded.
```

- [ ] **Step 2: Update `docs/chat-missing-features.md`**

Under `### 2. Persistence (log survives restart)`, add below the existing blockquote:

```markdown
> **Status 2026-07-02: BUILT.** Append-only JSONL under `%APPDATA%\foreman\chat\`,
> seq monotonic across restarts, torn-tail discard on load. See
> `docs/chat-persistence.md` for the shipped shape and deltas.
```

- [ ] **Step 3: Update `CLAUDE.md`**

In the `src/chat.rs` bullet, change "per-project chat room model (append-only log, pure data)." to:

```markdown
- `src/chat.rs` — per-project chat room model (append-only log; persisted as
  JSONL under `%APPDATA%\foreman\chat\`, reloaded on project open, seq
  monotonic across restarts).
```

Keep the rest of the bullet (injection/push semantics, wiring note) as-is.

- [ ] **Step 4: Commit**

```powershell
git add docs/chat-persistence.md docs/chat-missing-features.md CLAUDE.md
git commit -m "docs(chat): mark persistence built and record implementation deltas"
```

---

## Self-review notes

- **Spec coverage:** spec steps 1–5 map to Tasks 1–6; spec step 6 (periodic fsync) is explicitly deferred per its own "optional polish" label and decision 4's rationale (process-death durability is already covered by `write()`); the two open decisions are resolved (file location → Task 5) or deferred with rationale (cursors — Global Constraints).
- **Beyond-spec addition:** the Task 4 join floor. Without it, Task 6's step-6 verification fails catastrophically (full-history injection). Flagged in Global Constraints for user review.
- **Type consistency:** `ChatLog::open(&Path) -> io::Result<ChatLog>` (Tasks 3→4), `ChatRoom::open(&Path) -> io::Result<ChatRoom>` (Tasks 4→6), `chat_log_path(&Path) -> Option<PathBuf>` (Tasks 5→6), `session_floor() -> u64` (Tasks 3→4) — signatures match at every use site.
