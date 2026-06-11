# Chat @-Mentions v2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Targeted chat delivery — `foreman chat --to t3 "msg"` (or a message starting `@t3 …`) injects only the target's PTY while every message stays in the one shared transcript.

**Architecture:** Pure mention extraction + target model live in `src/chat.rs`; the server (`chat_post` in `src/wm.rs`) unions `--to` flags with leading-`@` mentions and validates all-or-nothing before any mutation; `chat_broadcast` gains an optional target filter. The human input line reuses the same helpers with prose-fallback instead of errors. Untargeted wire bytes, framing, and history lines stay byte-identical to v1.

**Tech Stack:** Rust (GNU toolchain — see CLAUDE.md gotchas), egui 0.34, serde/serde_json, real-PTY tests via `portable-pty` (`cmd /c pause` members).

**Spec:** `docs/superpowers/specs/2026-06-10-chat-mentions-impl-design.md` — section numbers (§) below refer to it.

**Build gotchas (will bite you):**
- Kill the running app before any build that links foreman.exe:
  `Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500`
- PTY tests must pump `keepalive()` in a loop while waiting (DSR trap — see the existing `chat_broadcast_hits_members_only_excluding_sender` test for the canonical pattern).
- Serena note: line numbers below are orientation, not gospel — resolve symbols by name.

## File map

| File | Changes |
|---|---|
| `src/chat.rs` | `ChatMsg.to: Vec<String>`, `post_to`, arrow `frame`/`line`, `ChatBlock::Text.to: Vec<String>`, `build_blocks` arrow meta, NEW `valid_chat_target` / `leading_mentions` / `effective_targets` |
| `src/wm.rs` | render-arm Vec adaptation, `validate_chat_targets`, `chat_post` (validate-before-mutate + targets), `chat_post_human` (extraction + fallback), `chat_broadcast`/`chat_broadcast_in` target filter, `ChatOutcome::Posted.targets`, `chat_dispatch`, `handle_ctrl`, `drain_chat_posts` |
| `src/control.rs` | `ChatRequest.to`, `--to` parsing + format check, `--to`/`--history` exclusivity |
| `.claude/skills/foreman-dispatch/SKILL.md` | mention usage + convention |
| `docs/epics/agent-dispatch-epic.md` | mentions section; `to` no longer render-only |
| `docs/superpowers/specs/2026-06-10-chat-mentions-design.md` | status note: rows 1–4, 6 implemented |

---

### Task 1: Model — `to` becomes `Vec<String>`, arrow framing

**Files:**
- Modify: `src/chat.rs` (`ChatMsg`, `push`/`post`/`sys`, `frame`/`line`, `ChatBlock`, `build_blocks`, tests)
- Modify: `src/wm.rs` (one line in the chat render arm, ~line 477)

- [ ] **Step 1: Write the failing tests** (in `src/chat.rs` `mod tests`)

Add a new test; also update the two existing spots that touch `to` (the `msg()` helper and `build_blocks_places_divider_and_formats_meta`):

```rust
#[test]
fn targeted_frame_and_line_carry_the_arrow() {
    let mut log = ChatLog::default();
    let m = log.post_to("t1", "boss", "go", vec!["t2".into(), "t3".into()]);
    assert_eq!(m.line(), "#1 t1→t2,t3: go");
    assert_eq!(m.frame("p1"), "[chat p1 #1] t1→t2,t3: go");
    // untargeted stays byte-identical (regression — v1 agents parse this)
    let m = log.post("t2", "worker", "ok");
    assert_eq!(m.line(), "#2 t2: ok");
    assert_eq!(m.frame("p1"), "[chat p1 #2] t2: ok");
}
```

In the `msg()` test helper change `to: None,` → `to: Vec::new(),`.

In `build_blocks_places_divider_and_formats_meta` change:
```rust
m3.to = Some("skeptic".to_string());
```
→
```rust
m3.to = vec!["t4".to_string(), "you".to_string()];
```
and the two assertions:
```rust
assert!(matches!(&blocks[3], ChatBlock::Header { meta, .. } if meta == "#3 · → t4,you"));
assert!(matches!(&blocks[4], ChatBlock::Text { to, .. } if to == &["t4", "you"]));
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib chat:: 2>&1 | Select-Object -Last 15`
Expected: compile errors (`to` is `Option<String>`, `post_to` undefined) — type-change TDD fails at compile.

- [ ] **Step 3: Implement** (`src/chat.rs`)

`ChatMsg.to` field (keep the other fields untouched):
```rust
    /// Delivery targets (`t3` / the reserved `you`). Empty = broadcast.
    /// Set by v2 mention delivery; the viewer renders arrow meta + olive edge.
    pub to: Vec<String>,
```

`push` gains the targets parameter; `post`/`sys` pass empty; new `post_to`:
```rust
    pub fn post(&mut self, from: &str, name: &str, text: &str) -> &ChatMsg {
        self.push(from, name, text, ChatKind::Post, Vec::new())
    }

    /// Post with delivery targets (mentions spec §4). `to` is stored verbatim
    /// (ids incl. the reserved `you`); resolution happened at the call site.
    pub fn post_to(&mut self, from: &str, name: &str, text: &str, to: Vec<String>) -> &ChatMsg {
        self.push(from, name, text, ChatKind::Post, to)
    }
```
In `sys`, the `push` call becomes `self.push(from, name, "", kind, Vec::new())`. In `push`, add the `to: Vec<String>` parameter and set `to,` in the `ChatMsg` literal (replacing `to: None,`).

`frame`/`line` share the sender tag (in `impl ChatMsg`):
```rust
    /// `t1` (broadcast) or `t1→t2,t3` (targeted) — the sender tag shared by
    /// injection framing and history lines. Untargeted output is byte-identical
    /// to v1: agents and tests parse it.
    fn from_tag(&self) -> String {
        if self.to.is_empty() {
            self.from.clone()
        } else {
            format!("{}→{}", self.from, self.to.join(","))
        }
    }
```
`frame` body becomes:
```rust
        format!("[chat {project} #{}] {}: {}", self.seq, self.from_tag(), self.text)
```
`line` body becomes:
```rust
        format!("#{} {}: {}", self.seq, self.from_tag(), self.text)
```

`ChatBlock::Text` variant: `Text { text: String, to: Vec<String> }`.

`build_blocks`, three spots inside the `ChatKind::Post` arm:
```rust
                if current != Some(m.from.as_str()) || !m.to.is_empty() {
```
```rust
                    if !m.to.is_empty() {
                        meta.push_str(&format!(" · → {}", m.to.join(",")));
                    }
```
```rust
                current = if m.to.is_empty() { Some(m.from.as_str()) } else { None };
```
(the `ChatBlock::Text { text: m.text.clone(), to: m.to.clone() }` push is unchanged in shape).

`src/wm.rs` render arm (~line 477), the `ChatBlock::Text` match: replace the `Painted::Galley` push with
```rust
items.push(Painted::Galley(g, TEXT, if to.is_empty() { 0.0 } else { 10.0 }, !to.is_empty()));
```

- [ ] **Step 4: Run the full suite**

Run: `cargo test 2>&1 | Select-Object -Last 5`
Expected: `104 passed; 0 failed` (103 + the new test).

- [ ] **Step 5: Commit**

```powershell
git add src/chat.rs src/wm.rs
git commit -m "Chat mentions: ChatMsg.to becomes Vec; arrow framing in frame/line/meta"
```

---

### Task 2: Pure mention extraction (`src/chat.rs`)

**Files:**
- Modify: `src/chat.rs` (new free functions + tests)

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn leading_mentions_take_only_the_leading_run() {
    assert_eq!(leading_mentions("@t3 take the parser"), vec!["t3"]);
    assert_eq!(leading_mentions("@t2 @you go"), vec!["t2", "you"]);
    // stops at the first non-mention token — later @s are prose
    assert_eq!(leading_mentions("@t2 hello @t3"), vec!["t2"]);
    // mid-prose mentions never target
    assert!(leading_mentions("per @t3's report, done").is_empty());
    // non-id @tokens are prose, and stop extraction
    assert!(leading_mentions("@bogus @t2 hi").is_empty());
    assert!(leading_mentions("@t hi").is_empty()); // no digits
    assert!(leading_mentions("").is_empty());
}

#[test]
fn effective_targets_union_flags_then_inline_deduped() {
    let flags = vec!["t3".to_string()];
    assert_eq!(effective_targets(&flags, "@t2 @t3 go"), vec!["t3", "t2"]);
    assert!(effective_targets(&[], "plain broadcast").is_empty());
    assert_eq!(effective_targets(&[], "@you need eyes"), vec!["you"]);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib chat:: 2>&1 | Select-Object -Last 10`
Expected: compile error — `leading_mentions` not found.

- [ ] **Step 3: Implement** (free functions in `src/chat.rs`, next to `build_blocks`)

```rust
/// Is `id` a well-formed mention target — `t<digits>` or the reserved `you`?
/// Format only; existence/membership is the server's check (spec §5).
pub fn valid_chat_target(id: &str) -> bool {
    id == "you"
        || id
            .strip_prefix('t')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Leading-mention extraction (mentions spec §3): whitespace-separated tokens
/// at the START of the text matching `@t<digits>` or `@you`; stops at the
/// first non-mention token. Mentions stay in the text — this is a pure read.
pub fn leading_mentions(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map_while(|tok| {
            tok.strip_prefix('@')
                .filter(|id| valid_chat_target(id))
                .map(str::to_string)
        })
        .collect()
}

/// Flag targets first, then inline mentions, deduped keeping first occurrence
/// (spec §3) — the order is what framing renders and tests assert.
pub fn effective_targets(to_flags: &[String], text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in to_flags.iter().cloned().chain(leading_mentions(text)) {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib chat:: 2>&1 | Select-Object -Last 5`
Expected: all pass (incl. the two new ones).

- [ ] **Step 5: Commit**

```powershell
git add src/chat.rs
git commit -m "Chat mentions: leading-mention extraction + flag/inline union"
```

---

### Task 3: Protocol + CLI — `ChatRequest.to`, `--to` flag

**Files:**
- Modify: `src/control.rs` (`ChatRequest`, `parse_chat_args`, tests)

- [ ] **Step 1: Write the failing tests** (in `src/control.rs` `mod tests`)

```rust
#[test]
fn parse_chat_args_collects_to_targets() {
    // repeatable, leading @ stripped, you allowed
    let req = parse_chat_args(
        &s(&["--to", "t3", "--to", "@you", "go"]),
        Some("p1".into()),
        Some("t2".into()),
    )
    .unwrap();
    assert_eq!(req.to, vec!["t3", "you"]);
    assert_eq!(req.text.as_deref(), Some("go"));
    // bad format is a client-side error naming the value
    let e = parse_chat_args(&s(&["--to", "bogus", "hi"]), None, Some("t2".into())).unwrap_err();
    assert!(e.contains("bogus"), "{e}");
    let e = parse_chat_args(&s(&["--to", "t", "hi"]), None, Some("t2".into())).unwrap_err();
    assert!(e.contains("t"), "{e}");
    // --to needs a value
    assert!(parse_chat_args(&s(&["--to"]), None, Some("t2".into())).is_err());
    // mutually exclusive with --history
    let e = parse_chat_args(&s(&["--to", "t3", "--history"]), None, Some("t2".into())).unwrap_err();
    assert!(e.contains("mutually exclusive"), "{e}");
    // a plain post carries no targets
    let req = parse_chat_args(&s(&["hi"]), None, Some("t2".into())).unwrap();
    assert!(req.to.is_empty());
}

#[test]
fn chat_request_to_is_wire_compatible_with_v1() {
    // empty to serializes away — untargeted requests are byte-identical to v1
    let req = parse_chat_args(&s(&["hi"]), Some("p1".into()), Some("t2".into())).unwrap();
    let json = serde_json::to_string(&req).unwrap();
    assert!(!json.contains("\"to\""), "{json}");
    // a v1 request (no `to` key) still parses
    let v1 = r#"{"cmd":"chat","project":"p1","from":"t2","text":"hi"}"#;
    let req: ChatRequest = serde_json::from_str(v1).unwrap();
    assert!(req.to.is_empty());
    // targets roundtrip
    let req = parse_chat_args(&s(&["--to", "t3", "go"]), None, Some("t2".into())).unwrap();
    let back: ChatRequest = serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
    assert_eq!(back.to, vec!["t3"]);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib control:: 2>&1 | Select-Object -Last 10`
Expected: compile error — no `to` field on `ChatRequest`.

- [ ] **Step 3: Implement**

`ChatRequest` gains (after `from`):
```rust
    /// Delivery targets from `--to` flags (mentions spec §1). Inline leading-@
    /// mentions are NOT carried here — they ride in `text` and the server
    /// extracts them. Empty = no explicit targets; skipped on the wire so
    /// untargeted requests stay byte-identical to v1.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub to: Vec<String>,
```

`parse_chat_args`: declare `let mut to: Vec<String> = Vec::new();` beside `history`, add a match arm before the catch-all `--` arm:
```rust
            "--to" => {
                let v = args.get(i + 1).ok_or("--to needs a value (tN or you)")?;
                let id = v.strip_prefix('@').unwrap_or(v);
                if !crate::chat::valid_chat_target(id) {
                    return Err(format!("bad --to target: {v} (expected tN or you)"));
                }
                to.push(id.to_string());
                i += 2;
            }
```
In the final `match`, the post arm `(false, None)` adds `to,` to the literal; the history arm `(true, Some(n))` becomes:
```rust
        (true, Some(n)) => {
            if !to.is_empty() {
                return Err("--to and --history are mutually exclusive".into());
            }
            Ok(ChatRequest {
                cmd: "chat".into(),
                project,
                from,
                to: Vec::new(),
                text: None,
                history: Some(n),
            })
        }
```
Update the other two `ChatRequest` literals in this function and any in existing tests (`chat_request_roundtrips_and_reply_omits_empty_history`, `chat_pipe_roundtrip`, …) with `to: Vec::new(),`.

- [ ] **Step 4: Run the full suite**

Run: `cargo test 2>&1 | Select-Object -Last 5`
Expected: all pass (106 by now).

- [ ] **Step 5: Commit**

```powershell
git add src/control.rs
git commit -m "Chat mentions: --to flag and wire-compatible ChatRequest.to"
```

---

### Task 4: Targeted delivery — broadcast filter + server validation (one task, one commit)

**Files:**
- Modify: `src/wm.rs` (`chat_broadcast`, `chat_broadcast_in`, `validate_chat_targets` NEW, `chat_post`, `ChatOutcome`, `chat_dispatch`, `handle_ctrl`, `drain_chat_posts`, tests)

The broadcast filter and the validation that produces targets are one behavior —
splitting them would leave a dead parameter and signature churn between commits
(grug-review finding, 2026-06-10).

- [ ] **Step 1: Write the failing tests** (in `src/wm.rs` `mod tests`; the first is modeled on `chat_broadcast_hits_members_only_excluding_sender` — reuse its pump/deadline pattern exactly; the DSR trap is real)

```rust
#[test]
fn chat_targeted_broadcast_hits_only_the_target() {
    let ctx = egui::Context::default();
    let mut wm = WindowManager::new();
    wm.tag = Some("p1".to_string());
    // all run `cmd /c pause`: any stdin byte makes them exit
    let sender = wm.add_terminal_cmd(&pause_argv(), None, None, &ctx).unwrap();
    let target = wm.add_terminal_cmd(&pause_argv(), None, None, &ctx).unwrap();
    let bystander = wm.add_terminal_cmd(&pause_argv(), None, None, &ctx).unwrap();
    // bystander IS a member — only the target filter may exclude it
    let framed = wm.chat_post(sender, "go", &[]).unwrap().0;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        for w in wm.windows.iter_mut() {
            if let Content::Terminal(s) = &mut w.tabs[w.active].content {
                s.keepalive();
            }
        }
        wm.chat_broadcast(Some(sender), &framed, Some(&[target]));
        let w = wm.windows.iter_mut().find(|w| w.id == target).unwrap();
        let Content::Terminal(s) = &mut w.tabs[w.active].content else { panic!() };
        if s.exited().is_some() {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "target never received the bytes");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // member bystander + sender saw nothing (kept pumped so a wrongful
    // injection would surface), and Some(&[]) injects nobody at all
    wm.chat_broadcast(Some(sender), &framed, Some(&[]));
    let grace = std::time::Instant::now() + std::time::Duration::from_millis(300);
    while std::time::Instant::now() < grace {
        for w in wm.windows.iter_mut() {
            if let Content::Terminal(s) = &mut w.tabs[w.active].content {
                s.keepalive();
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    for (id, who) in [(sender, "sender"), (bystander, "member bystander")] {
        let w = wm.windows.iter_mut().find(|w| w.id == id).unwrap();
        let Content::Terminal(s) = &mut w.tabs[w.active].content else { panic!() };
        assert!(s.exited().is_none(), "{who} must not be injected");
    }
}
```

(This test is written against the NEW `chat_post` signature implemented in
Step 3 below — it compiles once the whole task's implementation lands, which is
the point of the merged task.)

More failing tests, same step:
```rust
#[test]
fn targeted_post_validates_all_or_nothing_before_any_mutation() {
    let ctx = egui::Context::default();
    let mut wm = WindowManager::new();
    wm.tag = Some("p1".to_string());
    let sender = wm.add_terminal_cmd(&pause_argv(), None, None, &ctx).unwrap();
    let member = wm.add_terminal_cmd(&pause_argv(), None, None, &ctx).unwrap();
    let outsider = wm.add_terminal_cmd(&pause_argv(), None, None, &ctx).unwrap();
    {
        let w = wm.windows.iter_mut().find(|w| w.id == outsider).unwrap();
        w.tabs[w.active].chat_member = false;
    }
    let len_before = wm.chat.borrow().msgs().len();

    // unknown id — names it; one bad target fails a multi-target post entirely
    let e = wm.chat_post(sender, "go", &[term_tag(member), "t99".into()]).unwrap_err();
    assert!(e.contains("no such terminal: t99"), "{e}");
    // self-mention
    let e = wm.chat_post(sender, "go", &[term_tag(sender)]).unwrap_err();
    assert!(e.contains("cannot mention yourself"), "{e}");
    // non-member
    let e = wm.chat_post(sender, "go", &[term_tag(outsider)]).unwrap_err();
    assert!(e.contains("is not a chat member"), "{e}");
    // nothing appended by any failed post
    assert_eq!(wm.chat.borrow().msgs().len(), len_before);
    // inline mentions count too: a leading @ with a bad id fails the post
    let e = wm.chat_post(sender, "@t99 go", &[]).unwrap_err();
    assert!(e.contains("no such terminal: t99"), "{e}");
}

#[test]
fn failed_targeted_post_does_not_join_the_sender() {
    let ctx = egui::Context::default();
    let mut wm = WindowManager::new();
    wm.tag = Some("p1".to_string());
    let sender = wm.add_terminal_cmd(&pause_argv(), None, None, &ctx).unwrap();
    {
        // make the sender a NON-member so a successful post would join it
        let w = wm.windows.iter_mut().find(|w| w.id == sender).unwrap();
        w.tabs[w.active].chat_member = false;
    }
    let _ = wm.chat_post(sender, "go", &["t99".into()]).unwrap_err();
    let w = wm.windows.iter().find(|w| w.id == sender).unwrap();
    assert!(!w.tabs[w.active].chat_member, "failed post must not join");
    assert!(wm.chat.borrow().msgs().is_empty(), "no Joined sysline either");
}

#[test]
fn targeted_post_resolves_targets_and_frames_the_arrow() {
    let ctx = egui::Context::default();
    let mut wm = WindowManager::new();
    wm.tag = Some("p1".to_string());
    let sender = wm.add_terminal_cmd(&pause_argv(), None, None, &ctx).unwrap();
    let member = wm.add_terminal_cmd(&pause_argv(), None, None, &ctx).unwrap();

    // flags first, then inline, deduped; `you` resolves to no terminal
    let (framed, targets) = wm.chat_post(sender, "@you go", &[term_tag(member)]).unwrap();
    let mtag = term_tag(member);
    let stag = term_tag(sender);
    assert!(framed.contains(&format!("{stag}→{mtag},you: @you go")), "{framed}");
    assert_eq!(targets, Some(vec![member]));
    // pure-@you: Some(empty) — targeted, deliver to nobody
    let (framed, targets) = wm.chat_post(sender, "@you need eyes", &[]).unwrap();
    assert!(framed.contains(&format!("{stag}→you: @you need eyes")), "{framed}");
    assert_eq!(targets, Some(vec![]));
    // untargeted: None — broadcast
    let (_, targets) = wm.chat_post(sender, "plain", &[]).unwrap();
    assert_eq!(targets, None);
}

#[test]
fn targeting_an_exited_member_errors() {
    let ctx = egui::Context::default();
    let mut wm = WindowManager::new();
    wm.tag = Some("p1".to_string());
    let sender = wm.add_terminal_cmd(&pause_argv(), None, None, &ctx).unwrap();
    let victim = wm.add_terminal_cmd(&pause_argv(), None, None, &ctx).unwrap();
    // kill the victim by injecting a byte (pause exits on any stdin), pumping
    // through the DSR window like the broadcast tests
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        for w in wm.windows.iter_mut() {
            if let Content::Terminal(s) = &mut w.tabs[w.active].content {
                s.keepalive();
            }
        }
        {
            let w = wm.windows.iter_mut().find(|w| w.id == victim).unwrap();
            let Content::Terminal(s) = &mut w.tabs[w.active].content else { panic!() };
            s.inject_input("x");
            if s.exited().is_some() {
                break;
            }
        }
        assert!(std::time::Instant::now() < deadline, "victim never exited");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let e = wm.chat_post(sender, "go", &[term_tag(victim)]).unwrap_err();
    assert!(e.contains("has exited"), "{e}");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib wm::tests::targeted 2>&1 | Select-Object -Last 10`
Expected: compile errors (`chat_broadcast` takes 2 args; `chat_post` takes 2 args and returns `String`).

- [ ] **Step 3: Implement**

`chat_broadcast` — add the parameter and the filter at the top of the window loop (doc comment gains one line: `targets: None = broadcast; Some(ids) = only those windows' member tabs; Some(&[]) injects nobody (a pure @you post)`):
```rust
    fn chat_broadcast(&mut self, from: Option<WinId>, framed: &str, targets: Option<&[WinId]>) {
        for w in self.windows.iter_mut() {
            if let Some(t) = targets
                && !t.contains(&w.id)
            {
                continue;
            }
            // …existing body unchanged…
```

`chat_broadcast_in` passes through:
```rust
    fn chat_broadcast_in(&mut self, pid: WinId, from: WinId, framed: &str, targets: Option<&[WinId]>) {
        if let Some(win) = self.windows.iter_mut().find(|w| w.id == pid)
            && let Content::Project(child) = &mut win.tabs[win.active].content
        {
            child.chat_broadcast(Some(from), framed, targets);
        }
    }
```

`drain_chat_posts` passes `None` for now (real human targets arrive in Task 5):
`self.chat_broadcast(None, &framed, None);` — and existing tests calling
`chat_broadcast(...)` append `, None`.

New helper in `impl WindowManager` (near `chat_post`; `&mut self` because `Session::exited` polls):
```rust
    /// Resolve + validate mention targets against this project's members
    /// (mentions spec §5) — call BEFORE any mutation: a failed post must not
    /// append and must not join-on-first-post. `sender` None = the human
    /// (`you` then counts as self-mention). Returns the terminal WinIds to
    /// deliver to; `you` is valid markup but resolves to no terminal.
    fn validate_chat_targets(
        &mut self,
        sender: Option<WinId>,
        targets: &[String],
    ) -> Result<Vec<WinId>, String> {
        let mut ids = Vec::new();
        for t in targets {
            if t == "you" {
                if sender.is_none() {
                    return Err("cannot mention yourself".into());
                }
                continue;
            }
            let id = term_id(t)?;
            let win = self
                .windows
                .iter_mut()
                .find(|w| w.id == id)
                .ok_or_else(|| format!("no such terminal: {t}"))?;
            if Some(id) == sender {
                return Err("cannot mention yourself".into());
            }
            let (mut member, mut alive) = (false, false);
            for tab in &mut win.tabs {
                if !tab.chat_member {
                    continue;
                }
                member = true;
                if let Content::Terminal(s) = &mut tab.content
                    && s.exited().is_none()
                {
                    alive = true;
                }
            }
            if !member {
                return Err(format!("{t} is not a chat member"));
            }
            if !alive {
                return Err(format!("{t} has exited"));
            }
            ids.push(id);
        }
        Ok(ids)
    }
```

`chat_post` — new signature and validate-first body (doc comment: add `Targets validate all-or-nothing before the join/append (spec §5); returns the framed line plus the resolved delivery filter for chat_broadcast`):
```rust
    fn chat_post(
        &mut self,
        from: WinId,
        text: &str,
        to_flags: &[String],
    ) -> Result<(String, Option<Vec<WinId>>), String> {
        if text.is_empty() {
            return Err("empty message".into());
        }
        let targets = crate::chat::effective_targets(to_flags, text);
        let resolved = if targets.is_empty() {
            None
        } else {
            Some(self.validate_chat_targets(Some(from), &targets)?)
        };
        // …existing body from `let sender = …` through `log.sys(…)` unchanged…
        let msg = log.post_to(&from_tag, &name, text, targets);
        Ok((msg.frame(project), resolved))
    }
```

`ChatOutcome`:
```rust
    Posted {
        pid: WinId,
        from: WinId,
        framed: String,
        /// `None` = broadcast; `Some` = deliver only to these windows
        /// (`you` already filtered out — a pure-@you post is `Some(vec![])`).
        targets: Option<Vec<WinId>>,
    },
```

`chat_dispatch` post arm:
```rust
            (Some(text), None) => {
                let from = term_id(&req.from)?;
                let (framed, targets) = child.chat_post(from, text, &req.to)?;
                Ok(ChatOutcome::Posted { pid, from, framed, targets })
            }
```

`handle_ctrl` Posted arm: destructure `targets` and pass `targets.as_deref()` to `chat_broadcast_in` (whose signature gained the param above).

Update existing `chat_post(` test callers (e.g. `chat_broadcast_hits_members_only_excluding_sender`, `first_post_emits_joined_before_the_post`, `chat_post_replies_ok_then_broadcasts` if it calls directly): add `, &[]` and take `.0` where they want the framed string.

- [ ] **Step 4: Run the full suite**

Run: `cargo test 2>&1 | Select-Object -Last 5`
Expected: all pass.

- [ ] **Step 5: Commit**

```powershell
git add src/wm.rs
git commit -m "Chat mentions: targeted delivery — broadcast filter + all-or-nothing validation"
```

---

### Task 5: Human input line — extraction with prose fallback

**Files:**
- Modify: `src/wm.rs` (`chat_post_human`, `drain_chat_posts`, tests)

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn human_mention_narrows_delivery_or_falls_back_to_prose() {
    let ctx = egui::Context::default();
    let mut wm = WindowManager::new();
    wm.tag = Some("p1".to_string());
    let member = wm.add_terminal_cmd(&pause_argv(), None, None, &ctx).unwrap();
    let mtag = term_tag(member);

    // valid mention: targeted, arrow-framed under the reserved sender
    let (framed, targets) = wm.chat_post_human(&format!("@{mtag} check the diff")).unwrap();
    assert!(framed.contains(&format!("you→{mtag}: @{mtag} check the diff")), "{framed}");
    assert_eq!(targets, Some(vec![member]));
    assert_eq!(wm.chat.borrow().msgs().last().unwrap().to, vec![mtag.clone()]);

    // unknown id: prose fallback — broadcast, text intact, no error (spec §7)
    let (framed, targets) = wm.chat_post_human("@t99 anyone?").unwrap();
    assert!(framed.contains("you: @t99 anyone?"), "{framed}");
    assert_eq!(targets, None);
    assert!(wm.chat.borrow().msgs().last().unwrap().to.is_empty());

    // @you from the human is a self-mention: same fallback
    let (framed, targets) = wm.chat_post_human("@you hello").unwrap();
    assert!(framed.contains("you: @you hello"), "{framed}");
    assert_eq!(targets, None);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib wm::tests::human_mention 2>&1 | Select-Object -Last 10`
Expected: compile error — `chat_post_human` returns `Option<String>`.

- [ ] **Step 3: Implement**

`chat_post_human` (doc comment gains: `Leading mentions narrow delivery like CLI posts, but a bad mention demotes the post to plain broadcast instead of erroring — the input line has no error seat (spec §7)`):
```rust
    fn chat_post_human(&mut self, text: &str) -> Option<(String, Option<Vec<WinId>>)> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        // effective_targets (not raw leading_mentions) for dedup parity with
        // CLI posts — `@t2 @t2 go` must not frame `you→t2,t2`
        let mentions = crate::chat::effective_targets(&[], text);
        let (to, resolved) = if mentions.is_empty() {
            (Vec::new(), None)
        } else {
            match self.validate_chat_targets(None, &mentions) {
                Ok(ids) => (mentions, Some(ids)),
                Err(_) => (Vec::new(), None), // prose fallback
            }
        };
        debug_assert!(self.tag.is_some(), "human post on a tag-less manager");
        let project = self.tag.as_deref().unwrap_or("p?").to_string();
        let mut log = self.chat.borrow_mut();
        let msg = log.post_to(Self::HUMAN_ID, Self::HUMAN_ID, text, to);
        Some((msg.frame(&project), resolved))
    }
```

`drain_chat_posts` tail:
```rust
        if let Some(text) = pending {
            if let Some((framed, targets)) = self.chat_post_human(&text) {
                self.chat_broadcast(None, &framed, targets.as_deref());
            }
        }
```

Update the existing human-post tests that call `chat_post_human` (e.g. `human_post_appends_with_reserved_id_and_broadcasts_to_all_members`, the frame assertion near `you: go`) to destructure the tuple (`.0` for the framed line).

- [ ] **Step 4: Run the full suite**

Run: `cargo test 2>&1 | Select-Object -Last 5`
Expected: all pass.

- [ ] **Step 5: Commit**

```powershell
git add src/wm.rs
git commit -m "Chat mentions: human input line targets via leading @, prose fallback"
```

---

### Task 6: Docs — SKILL.md convention + epic

**Files:**
- Modify: `.claude/skills/foreman-dispatch/SKILL.md`
- Modify: `docs/epics/agent-dispatch-epic.md`
- Modify: `docs/superpowers/specs/2026-06-10-chat-mentions-design.md` (status line only)

- [ ] **Step 1: SKILL.md — extend the Chat usage section**

After the existing `chat` example block, add:

```markdown
Target a message so it interrupts ONLY the named terminals (everyone still
sees it in history and the chat window — mentions filter delivery, not
visibility):

    & $env:FOREMAN_EXE chat --to t3 "rebase first, then rerun"   # flag form
    & $env:FOREMAN_EXE chat "@t3 rebase first, then rerun"       # leading-@ sugar
    & $env:FOREMAN_EXE chat --to t2 --to t3 "you two own src/wm.rs"  # multi-target
    & $env:FOREMAN_EXE chat "@you tests are red, need a decision"    # flag the human

Mention rules:

- Only a LEADING run of `@tX` / `@you` tokens targets — `@t3` mid-sentence is
  prose and never narrows delivery. `--` does not suppress a leading mention.
- Targeted frames read `[chat p1 #N] t1→t2,t3: text`. If your id is right of
  the arrow, the message was addressed to you specifically — act on it.
- Bad targets fail the WHOLE post loudly (unknown id, exited terminal,
  non-member, yourself). On a stale-id error, re-read `--history` — the
  transcript is the roster; a respawned worker has a new id.
- `@you` reaches the human through the chat window without interrupting any
  agent — the cheapest way to flag something for the fleet runner.
```

- [ ] **Step 2: SKILL.md — one sentence in the standing convention paragraph**

In the blockquote paragraph, after "…post the one-line conclusion and where the detail lives.", insert:

```markdown
> When only some members need to act, target them (`chat --to t3 "…"` or a
> leading `@t3`) — a broadcast wakes every member and costs their attention.
```

- [ ] **Step 3: Epic doc — mentions subsection**

In `docs/epics/agent-dispatch-epic.md`, under the `## Group chat (chat verb)` section after the Delivery subsection, add:

```markdown
### Mentions (v2 — delivery filter)

Status: **built** (2026-06-10). Spec:
`docs/superpowers/specs/2026-06-10-chat-mentions-impl-design.md`.

- `--to tX` (repeatable, `@`-prefix tolerated) or a leading run of `@tX` /
  `@you` tokens in the text narrow PTY delivery to those terminals. Every
  message still lands in the one shared log — mentions never gate visibility.
- Targets validate all-or-nothing at send time, before the log append and
  before join-on-first-post: unknown id, exited member, non-member, or
  self-mention fails the whole post (`ok:false`, CLI exit 1). The request's
  `to` field carries flag targets only; the server extracts inline mentions.
- Targeted framing/history: `[chat p1 #15] t1→t2,t3: text` / `#15 t1→t2,t3:
  text`; untargeted output is byte-identical to v1.
- `@you` is valid markup that delivers to no PTY — flags the human via the
  chat window without waking an agent.
- The chat window's input line targets with leading `@tX` too; its failures
  fall back to plain broadcast (prose) instead of erroring.
- ChatMsg's `to` is now `Vec<String>` and set by delivery — no longer
  render-only.
```

- [ ] **Step 4: Consensus spec status line**

In `docs/superpowers/specs/2026-06-10-chat-mentions-design.md`, change the `**Status:**` line to:

```markdown
**Status:** Drafted — consensus reached; rows 1–4 and 6 implemented 2026-06-10
(see `2026-06-10-chat-mentions-impl-design.md`). Row 5 (quiescence gating)
still needs its own spike.
```

- [ ] **Step 5: Commit**

```powershell
git add .claude/skills/foreman-dispatch/SKILL.md docs/epics/agent-dispatch-epic.md docs/superpowers/specs/2026-06-10-chat-mentions-design.md
git commit -m "Chat mentions: teach the dispatch skill; epic + spec status"
```

---

### Task 7: Full verification (main session — needs the GUI)

- [ ] **Step 1: Full suite**

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo test 2>&1 | Select-Object -Last 5
```
Expected: ~110+ passed, 0 failed.

- [ ] **Step 2: Live verify** (build + screenshot, per working agreement; script in `docs/HANDOFF.md` § 3)

1. `cargo build`, run foreman, create a project, dispatch two interactive
   workers (`claude`) + keep the orchestrator session.
2. From the orchestrator: `& $env:FOREMAN_EXE chat --to t2 "targeted ping"` —
   screenshot: t2's pane shows `[chat p1 #N] t1→t2: targeted ping`, the other
   worker's pane shows nothing new, the chat window shows the olive edge +
   `→ t2` meta.
3. From a worker: `& $env:FOREMAN_EXE chat "@you need a decision"` —
   screenshot: NO pane receives bytes; the viewer highlights the mention.
4. Negative: `& $env:FOREMAN_EXE chat --to t99 "x"` → loud error, nonzero
   exit, nothing in the viewer.
5. Type `@t2 from the desk` into the chat window's input line — only t2
   receives it, framed `you→t2`.

- [ ] **Step 3: Update the dispatcher-window/live findings** — if any gotcha
  surfaced during live verify, record it in the epic doc's gotchas before
  closing out.

---

## Self-review notes (already applied)

- Spec coverage: §1→Task 3, §2→Task 3, §3→Task 2, §4→Task 1, §5→Task 4,
  §6→Task 4, §7→Task 5, §8→Task 6, §9→Tasks 1–5 tests + Task 7 live verify.
- Untargeted byte-identity is regression-locked by Task 1 Step 1 (and v1's
  existing frame/line tests, which stay untouched and passing).
- Grug-reviewed 2026-06-10 (Fable 5): merged the original Tasks 4+5 (the
  broadcast filter and its only producer are one behavior — the split left a
  dead parameter and cross-task signature churn); human path uses
  `effective_targets` for dedup parity with CLI posts.
