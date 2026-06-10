# Agent Group Chat (`foreman chat`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One chat room per project — agents post via a `chat` verb on the existing control pipe; foreman broadcast-injects each post into every other member's terminal, and a read-only chat subwindow shows the room.

**Architecture:** Pure chat model in a new `src/chat.rs` (`ChatLog`/`ChatMsg`, shared via `Rc<RefCell<…>>` between the project manager and its viewer window). Membership is a `chat_member` flag on `Win` (dispatched terminals auto-join; others join on first post). Delivery is reply-before-inject. Spec: `docs/superpowers/specs/2026-06-10-agent-group-chat-design.md` — read it first.

**Tech Stack:** Rust, egui 0.34 (painter-based text — `ui.fonts(|f|…)` needs `&mut`, go through the painter), `interprocess` named pipe, `portable-pty`/ConPTY, serde_json. Windows, GNU toolchain.

**Build gotchas (from CLAUDE.md):** kill any running foreman before building (`Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue`) or linking fails with os error 5. Tests spawn real `cmd.exe` PTYs — established repo pattern, not a smell.

**Branch:** `feature/agent-dispatch` (current).

---

## File Structure

| File | Changes |
|---|---|
| `src/terminal.rs` | `paste_wrap()` pure fn; `Session::inject_input()` (bracketed paste + `\r` submit) |
| `src/chat.rs` | **New.** `ChatMsg`, `ChatLog` — pure model: post/seq, line + frame formatting, tail slicing |
| `src/wm.rs` | `chat` field on `WindowManager`; `Win.chat_member`; `term_id()`; `chat_post`/`chat_broadcast`/`chat_history`; `Content::Chat` variant + render; `open_chat_window`; `handle_ctrl` becomes a match; `Command::OpenChat` dispatch |
| `src/control.rs` | `ChatRequest`; `OpenReply.history`; `CtrlMsg::Chat`; two-verb `serve()`; generic `request()`; `client_main` split into `open_main`/`chat_main` + shared `report()`; `parse_chat_args` |
| `src/main.rs` | `mod chat;` declaration only |
| `src/keymap.rs` | `Command::OpenChat` (+ ALL/group/label/default-chord arms — existing exhaustive tests enforce all of these) |
| `.claude/skills/foreman-dispatch/SKILL.md` | chat usage, worker modes, standing chat convention |
| `docs/epics/agent-dispatch-epic.md` | group chat section |

---

### Task 1: `paste_wrap` + `Session::inject_input` (terminal.rs)

**Files:**
- Modify: `src/terminal.rs` (impl Session ~line 160–460; tests module at bottom)

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module at the bottom of `src/terminal.rs`:

```rust
#[test]
fn paste_wrap_brackets_text_without_submitting() {
    let b = paste_wrap("line1\nline2");
    assert_eq!(b, b"\x1b[200~line1\nline2\x1b[201~".to_vec());
    assert!(!b.ends_with(b"\r"), "submit must be a separate write");
}

#[test]
fn inject_input_reaches_child_stdin() {
    let ctx = egui::Context::default();
    let argv = vec!["cmd.exe".to_string(), "/c".to_string(), "pause".to_string()];
    let mut s = Session::spawn_argv(&argv, None, &[], ctx).expect("spawn failed");
    // `cmd /c pause` blocks until any key arrives on stdin. If the injected
    // bytes reach the child, pause consumes one and the process exits.
    s.inject_input("hello room");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while s.exited().is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "pause never saw the injected input"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test paste_wrap inject_input 2>&1 | Select-Object -Last 15`
Expected: compile error — `paste_wrap` and `inject_input` not found.

- [ ] **Step 3: Implement**

Free function near `read_clipboard` in `src/terminal.rs`:

```rust
/// Bracketed-paste wrapper (`ESC[200~ … ESC[201~`): multi-line text lands in
/// the target's input box as one paste block instead of submitting per line
/// (spec: agent-group-chat §3).
pub fn paste_wrap(text: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(text.len() + 12);
    v.extend_from_slice(b"\x1b[200~");
    v.extend_from_slice(text.as_bytes());
    v.extend_from_slice(b"\x1b[201~");
    v
}
```

Method on `impl Session` (next to `inject_note`, ~line 362; `Session::send` already exists in the same impl):

```rust
/// Deliver chat text into this session's stdin: bracketed paste, then a
/// separate `\r` to submit (spec: agent-group-chat §3).
pub fn inject_input(&mut self, text: &str) {
    self.send(&paste_wrap(text));
    self.send(b"\r");
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test paste_wrap inject_input 2>&1 | Select-Object -Last 15`
Expected: `2 passed`.

- [ ] **Step 5: Commit**

```powershell
git add src/terminal.rs
git commit -m @'
Group chat: paste-wrapped stdin injection on Session

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
'@
```

---

### Task 2: chat model (new `src/chat.rs`)

**Files:**
- Create: `src/chat.rs`
- Modify: `src/main.rs` (add `mod chat;` next to the existing `mod` declarations)

- [ ] **Step 1: Write the failing tests**

Create `src/chat.rs` with the tests first (module skeleton so it compiles as a test target):

```rust
//! Project chat room model: an in-memory, append-only message log.
//! Pure data — the pipe/wm wiring lives in control.rs / wm.rs.
//! Spec: docs/superpowers/specs/2026-06-10-agent-group-chat-design.md §2.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_assigns_increasing_seq_from_one() {
        let mut log = ChatLog::new();
        assert_eq!(log.post("t1", "first").seq, 1);
        assert_eq!(log.post("t3", "second").seq, 2);
    }

    #[test]
    fn line_and_frame_formats() {
        let mut log = ChatLog::new();
        let m = log.post("t2", "taking the parser refactor");
        assert_eq!(m.line(), "#1 t2: taking the parser refactor");
        assert_eq!(
            m.frame("p1"),
            "[chat p1 #1] t2: taking the parser refactor"
        );
    }

    #[test]
    fn tail_lines_slices_the_end() {
        let mut log = ChatLog::new();
        for i in 1..=5 {
            log.post("t1", &format!("m{i}"));
        }
        assert_eq!(log.tail_lines(2), vec!["#4 t1: m4", "#5 t1: m5"]);
        assert_eq!(log.tail_lines(99).len(), 5);
        assert!(ChatLog::new().tail_lines(3).is_empty());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test chat:: 2>&1 | Select-Object -Last 15`
Expected: compile error — `ChatLog` not found (add `mod chat;` to `src/main.rs` first or the module won't build at all).

- [ ] **Step 3: Implement**

Above the tests in `src/chat.rs`:

```rust
pub struct ChatMsg {
    pub seq: u64,
    pub from: String, // "t2"
    pub text: String,
}

impl ChatMsg {
    /// History/window line: `#14 t2: text`.
    pub fn line(&self) -> String {
        format!("#{} {}: {}", self.seq, self.from, self.text)
    }

    /// Injection framing with provenance: `[chat p1 #14] t2: text` —
    /// receivers can tell agent chat from their human, and the seq lets
    /// them reference earlier messages.
    pub fn frame(&self, project: &str) -> String {
        format!("[chat {project} #{}] {}: {}", self.seq, self.from, self.text)
    }
}

/// Append-only room log. Seq is `len + 1` — messages are never removed in v1,
/// so no separate counter to drift.
pub struct ChatLog {
    msgs: Vec<ChatMsg>,
}

impl ChatLog {
    pub fn new() -> Self {
        Self { msgs: Vec::new() }
    }

    pub fn post(&mut self, from: &str, text: &str) -> &ChatMsg {
        let msg = ChatMsg {
            seq: self.msgs.len() as u64 + 1,
            from: from.to_string(),
            text: text.to_string(),
        };
        self.msgs.push(msg);
        self.msgs.last().expect("just pushed")
    }

    /// Last `n` messages as display lines, oldest first.
    pub fn tail_lines(&self, n: usize) -> Vec<String> {
        let start = self.msgs.len().saturating_sub(n);
        self.msgs[start..].iter().map(ChatMsg::line).collect()
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test chat:: 2>&1 | Select-Object -Last 15`
Expected: `3 passed`.

- [ ] **Step 5: Commit**

```powershell
git add src/chat.rs src/main.rs
git commit -m @'
Group chat: in-memory ChatLog model

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
'@
```

---

### Task 3: membership, post, broadcast, history (wm.rs)

**Files:**
- Modify: `src/wm.rs:296-313` (`Win`), `:430-453` (`WindowManager::new`), `:475-487` (`push_win`), `:639-661` (`add_terminal_cmd`), tests

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `src/wm.rs`:

```rust
fn pause_argv() -> Vec<String> {
    // stays alive until stdin sees a key; exits cleanly when the PTY drops
    vec!["cmd.exe".into(), "/c".into(), "pause".into()]
}

#[test]
fn dispatched_terminals_auto_join_chat() {
    let ctx = egui::Context::default();
    let mut wm = WindowManager::new();
    let argv = vec!["cmd.exe".to_string(), "/c".to_string(), "exit 0".to_string()];
    let t = wm.add_terminal_cmd(&argv, None, None, &ctx).unwrap();
    assert!(wm.windows.iter().find(|w| w.id == t).unwrap().chat_member);
}

#[test]
fn chat_post_validates_joins_and_frames() {
    let ctx = egui::Context::default();
    let mut wm = WindowManager::new();
    wm.tag = Some("p1".to_string());
    let t = wm.add_terminal_cmd(&pause_argv(), None, None, &ctx).unwrap();
    // simulate a hand-opened (non-dispatched) terminal
    wm.windows.iter_mut().find(|w| w.id == t).unwrap().chat_member = false;

    assert!(wm.chat_post(t, "").is_err(), "empty message rejected");
    assert!(wm.chat_post(999, "hi").is_err(), "unknown sender rejected");
    let framed = wm.chat_post(t, "hello room").unwrap();
    assert_eq!(framed, format!("[chat p1 #1] t{t}: hello room"));
    assert!(
        wm.windows.iter().find(|w| w.id == t).unwrap().chat_member,
        "posting joins the sender"
    );
    assert_eq!(wm.chat_history(10), vec![format!("#1 t{t}: hello room")]);
}

#[test]
fn chat_broadcast_hits_members_only_excluding_sender() {
    let ctx = egui::Context::default();
    let mut wm = WindowManager::new();
    wm.tag = Some("p1".to_string());
    // all three run `cmd /c pause`: receiving ANY stdin byte makes them exit
    let sender = wm.add_terminal_cmd(&pause_argv(), None, None, &ctx).unwrap();
    let member = wm.add_terminal_cmd(&pause_argv(), None, None, &ctx).unwrap();
    let outsider = wm.add_terminal_cmd(&pause_argv(), None, None, &ctx).unwrap();
    wm.windows.iter_mut().find(|w| w.id == outsider).unwrap().chat_member = false;

    let framed = wm.chat_post(sender, "go").unwrap();
    wm.chat_broadcast(sender, &framed);

    // positive signal: the member exits because bytes hit its stdin
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let w = wm.windows.iter_mut().find(|w| w.id == member).unwrap();
        let Content::Terminal(s) = &mut w.tabs[w.active].content else { panic!() };
        if s.exited().is_some() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "member never received the broadcast"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // sender and non-member saw nothing: still alive after the member exited
    std::thread::sleep(std::time::Duration::from_millis(300));
    for (id, who) in [(sender, "sender"), (outsider, "non-member")] {
        let w = wm.windows.iter_mut().find(|w| w.id == id).unwrap();
        let Content::Terminal(s) = &mut w.tabs[w.active].content else { panic!() };
        assert!(s.exited().is_none(), "{who} must not be injected");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test dispatched_terminals_auto_join chat_post_validates chat_broadcast_hits 2>&1 | Select-Object -Last 15`
Expected: compile errors — `chat_member`, `chat_post`, `chat_broadcast`, `chat_history` don't exist.

- [ ] **Step 3: Implement**

Top of `src/wm.rs`, add imports:

```rust
use std::cell::RefCell;
use std::rc::Rc;
```

`Win` gains a field (after `prev`):

```rust
    /// Member of this project's chat room (spec: agent-group-chat §2).
    /// Dispatched terminals auto-join; others join on first post. Same gotcha
    /// family as terminal-id resolution: tab-merge/untab churn can orphan it.
    pub chat_member: bool,
```

`push_win`'s `Win` literal gains `chat_member: false,`. `Grep` for every other `Win {` struct literal in `src/wm.rs` (e.g. in `untab`) and add `chat_member: false,` to each — an untabbed member re-joins on its next post; accepted, documented on the field.

`WindowManager` gains a field (in the struct and in `new()`):

```rust
    /// This project's chat room (unused at desktop level). Shared with the
    /// viewer window (`Content::Chat`), hence the Rc<RefCell<…>>.
    pub chat: Rc<RefCell<crate::chat::ChatLog>>,
```

```rust
    // in WindowManager::new()
    chat: Rc::new(RefCell::new(crate::chat::ChatLog::new())),
```

In `add_terminal_cmd`, after the existing `push_win` call (dispatched terminals are agents by construction — auto-join, spec §2):

```rust
        self.push_win(id, title, rect, Content::Terminal(s));
        self.windows.last_mut().expect("just pushed").chat_member = true;
```

Free function next to `dispatch_banner` (mirrors `resolve_project`'s `'p'` parsing):

```rust
/// Parse a "t4"-style terminal id.
fn term_id(spec: &str) -> Result<WinId, String> {
    spec.strip_prefix('t')
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| format!("bad terminal id: {spec}"))
}
```

Three methods on `impl WindowManager` (project level, near `add_terminal_cmd`):

```rust
    /// Post into this project's chat: validate the sender, append, join the
    /// sender (spec §2: join-on-first-post). Returns the framed injection line.
    /// Injection itself is `chat_broadcast` — kept separate because the reply
    /// must be sent BEFORE bytes flow (spec §3: reply-before-inject).
    fn chat_post(&mut self, from: WinId, text: &str) -> Result<String, String> {
        if text.is_empty() {
            return Err("empty message".into());
        }
        let sender = self
            .windows
            .iter_mut()
            .find(|w| w.id == from)
            .ok_or_else(|| format!("no such terminal: t{from}"))?;
        sender.chat_member = true;
        let project = self.tag.as_deref().unwrap_or("p?");
        let mut log = self.chat.borrow_mut();
        let msg = log.post(&format!("t{from}"), text);
        Ok(msg.frame(project))
    }

    /// Inject a framed chat line into every member except the sender, skipping
    /// exited sessions and non-terminal content (the chat viewer renders the
    /// log directly — it is never injected into).
    fn chat_broadcast(&mut self, from: WinId, framed: &str) {
        for w in self.windows.iter_mut() {
            if w.id == from || !w.chat_member {
                continue;
            }
            if let Content::Terminal(s) = &mut w.tabs[w.active].content {
                if s.exited().is_none() {
                    s.inject_input(framed);
                }
            }
        }
    }

    /// Last `n` chat lines (the `--history` verb; reading does not join).
    fn chat_history(&self, n: usize) -> Vec<String> {
        self.chat.borrow().tail_lines(n)
    }
```

- [ ] **Step 4: Run the wm test suite**

Run: `cargo test wm:: 2>&1 | Select-Object -Last 15`
Expected: all pass (new tests plus all pre-existing wm tests).

- [ ] **Step 5: Commit**

```powershell
git add src/wm.rs
git commit -m @'
Group chat: room membership, post, broadcast, history on the project manager

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
'@
```

---

### Task 4: `chat` verb end to end — protocol + GUI wiring (control.rs + wm.rs)

Cross-cutting: adding `CtrlMsg::Chat` forces `handle_ctrl` to become a match in the same compile unit.

**Files:**
- Modify: `src/control.rs:19-51` (types), `:105-155` (`CtrlMsg`, `serve`), `:161-174` (`request`), tests
- Modify: `src/wm.rs:561-596` (`handle_ctrl`), new `chat_dispatch`, tests

- [ ] **Step 1: Write the failing tests**

In `src/control.rs` tests:

```rust
#[test]
fn chat_request_roundtrips_and_reply_omits_empty_history() {
    let req: ChatRequest = serde_json::from_str(
        r#"{"cmd":"chat","project":"p1","from":"t2","text":"taking the parser"}"#,
    )
    .unwrap();
    assert_eq!(req.from, "t2");
    assert_eq!(req.text.as_deref(), Some("taking the parser"));
    assert_eq!(req.history, None);
    let s = serde_json::to_string(&req).unwrap();
    assert_eq!(serde_json::from_str::<ChatRequest>(&s).unwrap(), req);

    // ok-reply without history must not serialize the field
    let ok = OpenReply {
        ok: true,
        terminal: None,
        project: None,
        error: None,
        history: None,
    };
    assert!(!serde_json::to_string(&ok).unwrap().contains("history"));
    // history reply roundtrips
    let h = OpenReply {
        ok: true,
        terminal: None,
        project: None,
        error: None,
        history: Some(vec!["#1 t2: hi".into()]),
    };
    let s = serde_json::to_string(&h).unwrap();
    assert_eq!(serde_json::from_str::<OpenReply>(&s).unwrap(), h);
}

#[test]
fn chat_pipe_roundtrip() {
    let pipe = format!("foreman-test-chat-{}", std::process::id());
    let (tx, rx) = std::sync::mpsc::channel();
    let p2 = pipe.clone();
    std::thread::spawn(move || serve(&p2, tx));
    std::thread::spawn(move || {
        match rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap() {
            CtrlMsg::Chat(req, reply, _) => {
                assert_eq!(req.from, "t2");
                assert_eq!(req.text.as_deref(), Some("hello"));
                let _ = reply.send(OpenReply {
                    ok: true,
                    terminal: None,
                    project: None,
                    error: None,
                    history: None,
                });
            }
            _ => panic!("expected CtrlMsg::Chat"),
        }
    });
    let req = ChatRequest {
        cmd: "chat".into(),
        project: Some("p1".into()),
        from: "t2".into(),
        text: Some("hello".into()),
        history: None,
    };
    let mut reply = None;
    for _ in 0..100 {
        match request(&pipe, &req) {
            Ok(r) => {
                reply = Some(r);
                break;
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
    assert!(reply.expect("no reply").ok);
}
```

In `src/wm.rs` tests (uses `pause_argv` from Task 3):

```rust
/// Desktop with one project (p1) containing two member terminals.
fn chat_fixture(ctx: &egui::Context) -> (WindowManager, WinId, WinId) {
    let mut child = WindowManager::new();
    child.tag = Some("p1".to_string());
    let a = child.add_terminal_cmd(&pause_argv(), None, None, ctx).unwrap();
    let b = child.add_terminal_cmd(&pause_argv(), None, None, ctx).unwrap();
    let mut d = WindowManager::new().as_desktop();
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
    d.push_win(1, "proj".into(), rect, Content::Project(Box::new(child)));
    (d, a, b)
}

fn chat_req(from: WinId, text: Option<&str>, history: Option<usize>) -> crate::control::ChatRequest {
    crate::control::ChatRequest {
        cmd: "chat".into(),
        project: Some("p1".into()),
        from: format!("t{from}"),
        text: text.map(str::to_string),
        history,
    }
}

#[test]
fn chat_post_replies_ok_then_broadcasts() {
    let ctx = egui::Context::default();
    let (mut d, a, b) = chat_fixture(&ctx);
    let (rtx, rrx) = std::sync::mpsc::channel();
    d.handle_ctrl(
        crate::control::CtrlMsg::Chat(chat_req(a, Some("go"), None), rtx, std::time::Instant::now()),
        &ctx,
    );
    assert!(rrx.try_recv().expect("no reply").ok);
    // end-to-end: member b runs `cmd /c pause` and exits when bytes arrive
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let win = d.windows.iter_mut().find(|w| w.id == 1).unwrap();
        let Content::Project(child) = &mut win.tabs[win.active].content else { panic!() };
        let w = child.windows.iter_mut().find(|w| w.id == b).unwrap();
        let Content::Terminal(s) = &mut w.tabs[w.active].content else { panic!() };
        if s.exited().is_some() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "member never received the post"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn chat_history_replies_lines_and_does_not_join() {
    let ctx = egui::Context::default();
    let (mut d, a, _b) = chat_fixture(&ctx);
    // seed one message
    let (rtx, rrx) = std::sync::mpsc::channel();
    d.handle_ctrl(
        crate::control::CtrlMsg::Chat(chat_req(a, Some("hi"), None), rtx, std::time::Instant::now()),
        &ctx,
    );
    rrx.try_recv().expect("post reply");
    // history from an unknown-to-the-room id: replies, does not error, does not join
    let (rtx, rrx) = std::sync::mpsc::channel();
    d.handle_ctrl(
        crate::control::CtrlMsg::Chat(chat_req(999, None, Some(10)), rtx, std::time::Instant::now()),
        &ctx,
    );
    let r = rrx.try_recv().expect("no history reply");
    assert!(r.ok);
    assert_eq!(r.history.as_deref().map(|h| h.len()), Some(1));
}

#[test]
fn stale_chat_request_is_dropped_without_reply() {
    let ctx = egui::Context::default();
    let (mut d, a, _b) = chat_fixture(&ctx);
    let (rtx, rrx) = std::sync::mpsc::channel();
    let stale = std::time::Instant::now() - crate::control::REPLY_TIMEOUT;
    d.handle_ctrl(crate::control::CtrlMsg::Chat(chat_req(a, Some("late"), None), rtx, stale), &ctx);
    assert!(
        rrx.try_recv().is_err(),
        "stale request must be dropped unanswered (client already saw a timeout)"
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test chat_request_roundtrips chat_pipe_roundtrip chat_post_replies chat_history_replies stale_chat 2>&1 | Select-Object -Last 15`
Expected: compile errors — `ChatRequest`, `CtrlMsg::Chat`, `OpenReply.history` don't exist.

- [ ] **Step 3: Implement — control.rs**

Add after `OpenReply`'s impl:

```rust
/// Project chat post or history read (spec: agent-group-chat §1). Exactly one
/// of `text` (post) / `history` (read last N) must be set — the client
/// enforces this; the server treats `history` as the discriminator. `from` is
/// the sender's own terminal id from its env. As with `open`, this is a
/// guardrail against confused agents, NOT a security boundary — any local
/// process can speak to the pipe and claim any `from`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChatRequest {
    pub cmd: String, // always "chat"
    #[serde(default)]
    pub project: Option<String>, // "p1"; None = focused project
    pub from: String, // "t2"
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub history: Option<usize>,
}
```

`OpenReply` gains a field (after `error`); every existing `OpenReply { … }` literal in src + tests gains `history: None,`:

```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<Vec<String>>, // chat --history results
```

(`OpenReply::err` also gains `history: None`.)

Extend `CtrlMsg`:

```rust
pub enum CtrlMsg {
    Open(OpenRequest, mpsc::Sender<OpenReply>, std::time::Instant),
    Chat(ChatRequest, mpsc::Sender<OpenReply>, std::time::Instant),
}
```

Rewrite the per-connection body of `serve` (the `let reply = match …` block; keep the listener loop and read/write lines as they are). Put `struct Verb` above the function if clippy minds items-in-statements:

```rust
        #[derive(serde::Deserialize)]
        struct Verb {
            cmd: String,
        }
        let now = std::time::Instant::now();
        let (rtx, rrx) = mpsc::channel();
        let msg = match serde_json::from_str::<Verb>(&line) {
            Err(e) => Err(format!("bad request: {e}")),
            Ok(v) => match v.cmd.as_str() {
                "open" => serde_json::from_str::<OpenRequest>(&line)
                    .map(|r| CtrlMsg::Open(r, rtx, now))
                    .map_err(|e| format!("bad request: {e}")),
                "chat" => serde_json::from_str::<ChatRequest>(&line)
                    .map(|r| CtrlMsg::Chat(r, rtx, now))
                    .map_err(|e| format!("bad request: {e}")),
                other => Err(format!("unknown cmd: {other}")),
            },
        };
        let reply = match msg {
            Err(e) => OpenReply::err(e),
            Ok(m) => {
                if tx.send(m).is_err() {
                    return; // GUI gone; stop serving
                }
                rrx.recv_timeout(REPLY_TIMEOUT)
                    .unwrap_or_else(|_| OpenReply::err("foreman did not respond"))
            }
        };
```

Make `request` accept either request type (body unchanged — it already just serializes):

```rust
pub fn request(pipe: &str, req: &impl serde::Serialize) -> std::io::Result<OpenReply> {
```

- [ ] **Step 4: Implement — wm.rs**

Rewrite `handle_ctrl` as a match (the Open arm is byte-for-byte the current body):

```rust
    /// Drain-side handler for one control message (desktop manager only).
    /// Both verbs honor the reply-timeout contract (drop stale requests
    /// unexecuted). `open` additionally undoes orphaned spawns; chat posts
    /// instead reply BEFORE injecting — an injection cannot be undone, so the
    /// bytes only flow once the client is guaranteed to hear "ok" (spec §3).
    pub fn handle_ctrl(&mut self, msg: crate::control::CtrlMsg, ctx: &egui::Context) {
        use crate::control::{CtrlMsg, OpenReply, REPLY_TIMEOUT};
        match msg {
            CtrlMsg::Open(req, reply, sent) => {
                if sent.elapsed() >= REPLY_TIMEOUT {
                    return;
                }
                let res = self.open_dispatch(req, ctx);
                let undo = res.as_ref().ok().copied();
                if reply.send(Self::open_reply(res)).is_err() {
                    if let Some((pid, tid)) = undo {
                        self.close_terminal(pid, tid);
                    }
                }
            }
            CtrlMsg::Chat(req, reply, sent) => {
                if sent.elapsed() >= REPLY_TIMEOUT {
                    return;
                }
                match self.chat_dispatch(&req) {
                    Err(e) => {
                        let _ = reply.send(OpenReply::err(e));
                    }
                    Ok(ChatOutcome::History(lines)) => {
                        let _ = reply.send(OpenReply {
                            ok: true,
                            terminal: None,
                            project: None,
                            error: None,
                            history: Some(lines),
                        });
                    }
                    Ok(ChatOutcome::Posted { pid, from, framed }) => {
                        let ok = OpenReply {
                            ok: true,
                            terminal: None,
                            project: None,
                            error: None,
                            history: None,
                        };
                        if reply.send(ok).is_ok() {
                            self.chat_broadcast_in(pid, from, &framed);
                            ctx.request_repaint(); // viewer windows update now
                        }
                    }
                }
            }
        }
    }
```

Add next to `open_dispatch` (desktop level):

```rust
/// What a validated chat request resolved to. Posting is split from injection
/// so the reply can be sent between the two (spec §3).
enum ChatOutcome {
    Posted { pid: WinId, from: WinId, framed: String },
    History(Vec<String>),
}
```

```rust
    /// Resolve + execute the room-side half of a chat request: history reads
    /// answer immediately; posts append/join and return the framed line for
    /// the post-reply broadcast.
    fn chat_dispatch(&mut self, req: &crate::control::ChatRequest) -> Result<ChatOutcome, String> {
        let pid = self.resolve_project(req.project.as_deref())?;
        let win = self
            .windows
            .iter_mut()
            .find(|w| w.id == pid)
            .expect("resolved");
        let Content::Project(child) = &mut win.tabs[win.active].content else {
            return Err("not a project".into()); // unreachable after resolve
        };
        match (&req.text, req.history) {
            (None, Some(n)) => Ok(ChatOutcome::History(child.chat_history(n))),
            (Some(text), None) => {
                let from = term_id(&req.from)?;
                let framed = child.chat_post(from, text)?;
                Ok(ChatOutcome::Posted { pid, from, framed })
            }
            _ => Err("chat needs exactly one of text/history".into()),
        }
    }

    /// Broadcast a framed post inside project `pid` (the after-reply half).
    fn chat_broadcast_in(&mut self, pid: WinId, from: WinId, framed: &str) {
        if let Some(win) = self.windows.iter_mut().find(|w| w.id == pid) {
            if let Content::Project(child) = &mut win.tabs[win.active].content {
                child.chat_broadcast(from, framed);
            }
        }
    }
```

- [ ] **Step 5: Run the new tests, then the whole suite**

Run: `cargo test chat_request_roundtrips chat_pipe_roundtrip chat_post_replies chat_history_replies stale_chat 2>&1 | Select-Object -Last 15`
Expected: `5 passed`.

Run: `cargo test 2>&1 | Select-Object -Last 15`
Expected: all pass — in particular `unknown_verb_is_rejected` and `pipe_roundtrip` (the serve rewrite preserves both error strings and Open behavior).

- [ ] **Step 6: Commit**

```powershell
git add src/control.rs src/wm.rs
git commit -m @'
Group chat: chat verb with reply-before-inject broadcast

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
'@
```

---

### Task 5: `foreman chat` CLI (control.rs)

**Files:**
- Modify: `src/control.rs:177-210` (`client_main`), tests

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn parse_chat_args_builds_post_and_history() {
    // post: trailing words join into one message
    let req = parse_chat_args(
        &s(&["taking", "the", "parser"]),
        Some("p1".into()),
        Some("t2".into()),
    )
    .unwrap();
    assert_eq!(req.project.as_deref(), Some("p1"));
    assert_eq!(req.from, "t2");
    assert_eq!(req.text.as_deref(), Some("taking the parser"));
    assert_eq!(req.history, None);
    // history with explicit N
    let req = parse_chat_args(&s(&["--history", "5"]), None, Some("t2".into())).unwrap();
    assert_eq!(req.history, Some(5));
    assert_eq!(req.text, None);
    // history default N
    let req = parse_chat_args(&s(&["--history"]), None, Some("t2".into())).unwrap();
    assert_eq!(req.history, Some(20));
    // explicit --project beats env default
    let req = parse_chat_args(
        &s(&["--project", "p2", "hi"]),
        Some("p1".into()),
        Some("t2".into()),
    )
    .unwrap();
    assert_eq!(req.project.as_deref(), Some("p2"));
}

#[test]
fn parse_chat_args_rejects_bad_input() {
    // not inside a foreman terminal
    assert!(parse_chat_args(&s(&["hi"]), None, None).is_err());
    // nothing to do
    assert!(parse_chat_args(&s(&[]), None, Some("t2".into())).is_err());
    // both post and history
    assert!(parse_chat_args(&s(&["--history", "5", "hi"]), None, Some("t2".into())).is_err());
    // flag without value
    assert!(parse_chat_args(&s(&["--project"]), None, Some("t2".into())).is_err());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test parse_chat_args 2>&1 | Select-Object -Last 15`
Expected: compile error — `parse_chat_args` not found.

- [ ] **Step 3: Implement**

Add next to `parse_open_args`:

```rust
/// Parse `foreman chat` args: `[--project P] <message words...>` to post, or
/// `[--project P] --history [N]` to read (default 20). `default_project` /
/// `self_terminal` come from the caller's FOREMAN_* env; a caller outside a
/// foreman terminal cannot use chat.
pub fn parse_chat_args(
    args: &[String],
    default_project: Option<String>,
    self_terminal: Option<String>,
) -> Result<ChatRequest, String> {
    let from =
        self_terminal.ok_or("not inside a foreman terminal (FOREMAN_TERMINAL_ID unset)")?;
    let mut project = default_project;
    let mut history: Option<usize> = None;
    let mut words: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--project" => {
                project = Some(
                    args.get(i + 1)
                        .ok_or("--project needs a value")?
                        .clone(),
                );
                i += 2;
            }
            "--history" => {
                // optional count: `--history 5` or bare `--history`
                match args.get(i + 1).and_then(|v| v.parse::<usize>().ok()) {
                    Some(n) => {
                        history = Some(n);
                        i += 2;
                    }
                    None => {
                        history = Some(20);
                        i += 1;
                    }
                }
            }
            _ => {
                words.push(args[i].clone());
                i += 1;
            }
        }
    }
    match (words.is_empty(), history) {
        (false, None) => Ok(ChatRequest {
            cmd: "chat".into(),
            project,
            from,
            text: Some(words.join(" ")),
            history: None,
        }),
        (true, Some(n)) => Ok(ChatRequest {
            cmd: "chat".into(),
            project,
            from,
            text: None,
            history: Some(n),
        }),
        (true, None) => Err("nothing to do: give a message or --history".into()),
        (false, Some(_)) => Err("--history and a message are mutually exclusive".into()),
    }
}
```

Restructure `client_main` into a dispatcher plus shared reporting (the `open_main`/`report` bodies are the existing `client_main` code, split):

```rust
/// Subcommand entry point (no GUI). Returns the process exit code.
pub fn client_main(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("open") => open_main(&args[1..]),
        Some("chat") => chat_main(&args[1..]),
        _ => {
            eprintln!("usage: foreman open [--project P] [--title T] [--cwd D] -- <command...>");
            eprintln!("       foreman chat [--project P] <message...>");
            eprintln!("       foreman chat [--project P] --history [N]");
            2
        }
    }
}

fn open_main(args: &[String]) -> i32 {
    let req = match parse_open_args(args, std::env::var("FOREMAN_PROJECT_ID").ok()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("foreman open: {e}");
            return 2;
        }
    };
    report("foreman open", request(PIPE, &req))
}

fn chat_main(args: &[String]) -> i32 {
    let req = match parse_chat_args(
        args,
        std::env::var("FOREMAN_PROJECT_ID").ok(),
        std::env::var("FOREMAN_TERMINAL_ID").ok(),
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("foreman chat: {e}");
            return 2;
        }
    };
    report("foreman chat", request(PIPE, &req))
}

/// Print the pipe reply (or the connection failure) the way all subcommands do.
/// History replies print line-per-line for agent readability; other ok replies
/// print as JSON (the open reply carries terminal/project ids the caller needs).
fn report(label: &str, res: std::io::Result<OpenReply>) -> i32 {
    match res {
        Ok(r) if r.ok => {
            if let Some(lines) = &r.history {
                for l in lines {
                    println!("{l}");
                }
            } else {
                println!("{}", serde_json::to_string(&r).unwrap_or_default());
            }
            0
        }
        Ok(r) => {
            eprintln!("{label}: {}", r.error.unwrap_or_default());
            1
        }
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
            eprintln!(
                "{label}: foreman is running but its control pipe stayed busy for {}s — retry, or check for a wedged dispatch",
                CONNECT_TIMEOUT.as_secs()
            );
            1
        }
        Err(e) => {
            eprintln!("{label}: cannot reach foreman ({e}) — is it running?");
            1
        }
    }
}
```

- [ ] **Step 4: Run the full suite**

Run: `cargo test 2>&1 | Select-Object -Last 15`
Expected: all pass.

- [ ] **Step 5: Commit**

```powershell
git add src/control.rs
git commit -m @'
Group chat: foreman chat subcommand (post + history)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
'@
```

---

### Task 6: chat viewer window + leader-key command (wm.rs + keymap.rs)

**Files:**
- Modify: `src/wm.rs:249-291` (`Content` enum + impl), `:794-851` (`dispatch`), new `open_chat_window`, tests
- Modify: `src/keymap.rs:19-46` (`Command`), `:53-94` (`ALL`), `:97-107` (`group`), `:110+` (`label`), default-chord match (~line 600)

- [ ] **Step 1: Write the failing test**

In `src/wm.rs` tests:

```rust
#[test]
fn open_chat_window_is_a_singleton() {
    let mut wm = WindowManager::new();
    wm.last_area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
    wm.open_chat_window();
    let chat_wins = |wm: &WindowManager| {
        wm.windows
            .iter()
            .filter(|w| w.tabs.iter().any(|t| matches!(t.content, Content::Chat(_))))
            .count()
    };
    assert_eq!(chat_wins(&wm), 1);
    let first = wm.windows.last().unwrap().id;
    // focus something else, then reopen: focuses, does not duplicate
    wm.focused = None;
    wm.open_chat_window();
    assert_eq!(chat_wins(&wm), 1);
    assert_eq!(wm.focused, Some(first));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test open_chat_window_is_a_singleton 2>&1 | Select-Object -Last 15`
Expected: compile error — `Content::Chat`, `open_chat_window` don't exist.

- [ ] **Step 3: Implement — wm.rs**

`Content` gains a variant:

```rust
pub enum Content {
    Terminal(Session),
    /// A project window is a sandbox hosting its own nested WindowManager.
    Project(Box<WindowManager>),
    /// Read-only viewer of the owning project's chat room. Shares the log via
    /// Rc — a viewer, not a member: never injected into (spec §4).
    Chat(Rc<RefCell<crate::chat::ChatLog>>),
}
```

`Content::show` gains an arm (egui 0.34: go through the painter; `painter_at` clips to the rect):

```rust
            Content::Chat(log) => {
                let p = ui.painter_at(rect);
                p.rect_filled(rect, 0.0, WIN_BG);
                let font = egui::FontId::monospace(13.0);
                let line_h = 17.0;
                let pad = 6.0;
                let fit = (((rect.height() - 2.0 * pad) / line_h).floor() as usize).max(1);
                let lines = log.borrow().tail_lines(fit);
                for (i, line) in lines.iter().enumerate() {
                    p.text(
                        egui::pos2(rect.min.x + pad, rect.min.y + pad + i as f32 * line_h),
                        egui::Align2::LEFT_TOP,
                        line,
                        font.clone(),
                        TEXT,
                    );
                }
                false
            }
```

`Content::keepalive` gains a no-op arm:

```rust
            Content::Chat(_) => {}
```

Fix remaining exhaustive-match fallout: `Grep` for `match` sites over `Content` (e.g. `focused_child`/`active_content` at `src/wm.rs:669/867`, exit-title refresh at `:1262`) — each gets a `Content::Chat(_)` arm with the do-nothing/None behavior of a non-terminal, non-project window.

Method on `impl WindowManager` (project level, near `add_terminal_cmd`):

```rust
    /// Open (or focus) this project's chat viewer — singleton per project
    /// (spec §4). Closing it later doesn't touch the log; the room is the log.
    fn open_chat_window(&mut self) {
        if let Some(w) = self
            .windows
            .iter()
            .find(|w| w.tabs.iter().any(|t| matches!(t.content, Content::Chat(_))))
        {
            let id = w.id;
            self.focus(id);
            return;
        }
        let (id, rect) = self.next_slot(egui::vec2(420.0, 320.0));
        self.push_win(id, "chat".into(), rect, Content::Chat(Rc::clone(&self.chat)));
    }
```

In `dispatch` (`src/wm.rs:794`), add to the **inner-level** match (next to `Command::NewTerm`):

```rust
                        Command::OpenChat => child.open_chat_window(),
```

- [ ] **Step 4: Implement — keymap.rs**

`Command` enum gains `OpenChat,` (in the terminal-level group, after `TabPrev`). Then fix the exhaustive arms — the existing keymap tests enforce all of them:

- `ALL`: add `OpenChat,` to the Terminals block (after `TabPrev`).
- `group()`: add `OpenChat` to the `Group::Terminals` arm.
- `label()`: `OpenChat => "Open project chat",`
- Default-chord match (~line 600, where `Help`/`NewProject` have explicit chords): add `Command::OpenChat => Some(Chord::new(K::G, false, false, false)),` — if the existing "no duplicate default chords" test fails because `G` is taken, pick another free letter and update this plan's live-verify chord below.

- [ ] **Step 5: Run keymap + wm suites, then everything**

Run: `cargo test keymap:: wm:: 2>&1 | Select-Object -Last 15`
Expected: all pass — the keymap completeness/no-dup tests are the real check here.

Run: `cargo test 2>&1 | Select-Object -Last 15`
Expected: all pass.

- [ ] **Step 6: Commit**

```powershell
git add src/wm.rs src/keymap.rs
git commit -m @'
Group chat: per-project chat viewer window on leader+G

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
'@
```

---

### Task 7: docs + dispatch skill

**Files:**
- Modify: `docs/epics/agent-dispatch-epic.md`
- Modify: `.claude/skills/foreman-dispatch/SKILL.md`

- [ ] **Step 1: Update the epic**

Add a "Group chat (`chat` verb)" section to `docs/epics/agent-dispatch-epic.md` covering, in the epic's existing voice: the post/history JSON shapes; per-project room + in-memory log; membership rules (dispatched auto-join, join-on-post, history doesn't join); reply-before-inject ordering and why (no undo for an injection; a timed-out post may still appear in history but its bytes never flow); `[chat p1 #N] t2: …` framing + bracketed paste + separate `\r`; the chat viewer window (singleton, viewer-not-member); and the gotchas: `from`/`project` are guardrails not authentication; tab-merge/untab can orphan membership; message storms are mitigated by prompt convention only.

- [ ] **Step 2: Update the dispatch skill**

Extend `.claude/skills/foreman-dispatch/SKILL.md` with:

1. **Worker mode choice (spec §5):** `claude -p "<prompt>"` for fire-and-forget — cannot *receive* chat mid-run, but CAN post (`foreman chat` is a process spawn, not stdin); interactive `claude "<prompt>"` for collaborative workers that receive every post. Interactive workers don't auto-exit — instruct exit-when-done in the prompt or post an instruction addressed to them.
2. **Chat usage:** `foreman chat "<message>"` to post; `foreman chat --history [N]` to catch up. Same-project only.
3. **Standing convention to inject into every dispatched prompt** (verbatim, spec §5): "You are in a project chat. Messages arrive as `[chat p1 #N] tX: …`. Only respond when a message is relevant to your task — most messages need no reply. Post with `foreman chat \"…\"`. Check `foreman chat --history` after long heads-down stretches."
4. **Fire-and-forget reporting pattern:** end the worker's prompt with "post your result with `foreman chat \"<summary>\"` before exiting."

- [ ] **Step 3: Commit**

```powershell
git add docs/epics/agent-dispatch-epic.md .claude/skills/foreman-dispatch/SKILL.md
git commit -m @'
Group chat: epic + dispatch skill docs

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
'@
```

---

### Task 8: full verification (superpowers:verification-before-completion)

- [ ] **Step 1: Clean build + full test run**

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 5
cargo test 2>&1 | Select-Object -Last 10
```

Expected: build clean, all tests pass.

- [ ] **Step 2: Live three-way exchange**

Run `cargo run --release` (background). From a Claude session inside a foreman project, dispatch two interactive workers:

```powershell
& $env:FOREMAN_EXE open --title "agent · worker A" -- claude "You are in a project chat. Messages arrive as [chat p1 #N] tX: ... Wait for instructions in the chat; only respond to messages relevant to you. Post with: foreman chat ..."
& $env:FOREMAN_EXE open --title "agent · worker B" -- claude "<same convention prompt>"
& $env:FOREMAN_EXE chat "worker A: reply in chat with the word ALPHA. worker B: reply with BRAVO."
```

Screenshot the window (script in `docs/HANDOFF.md` § 3), `Read` the PNG, confirm: both worker panes show the framed post as submitted input, and their `foreman chat` replies arrive back in the orchestrator's pane. **Do not claim success without the screenshot.**

- [ ] **Step 3: Chat window**

Press the leader chord then `G` in the project; screenshot: the chat window shows the full exchange as `#N tX: …` lines. Re-invoke the chord: no second chat window appears.

- [ ] **Step 4: History + negative checks**

```powershell
& $env:FOREMAN_EXE chat --history 10    # prints the exchange line-per-line, exit 0
& $env:FOREMAN_EXE chat ""              # exit 1, "empty message" (server-side; bare `chat` with no args is the client-side exit-2 case)
```

Also confirm a hand-opened plain pwsh terminal in the project received **no** injected chat (it never posted — not a member).

- [ ] **Step 5: Final commit if verification produced fixes**

Commit any fixes from verification as their own commit. Do not amend.

---

## Out of Scope (spec)

Posting from the chat pane, disk persistence, named channels, cross-project chat, @-mentions/rate limiting, message editing/deletion, read receipts. If a task seems to need one of these, stop and re-read the spec — it doesn't.
