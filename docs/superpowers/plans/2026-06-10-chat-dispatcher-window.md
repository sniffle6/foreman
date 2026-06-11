# Chat Window "Dispatcher's Desk" Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the bare chat tail viewer with the dispatcher's-desk window: crew board ordered by last-heard, grouped/wrapped log with system lines and a NEW divider, click-to-focus, and (slice 2) an input line with a reserved human identity.

**Architecture:** The pure model grows in `src/chat.rs` (`ChatKind`, timestamps, stamped names, `CrewRow`, `ChatView`, `build_blocks`). `Content::Chat`'s payload changes from `Rc<RefCell<ChatLog>>` to `ChatView` (log + per-window view state). The owning project `WindowManager` refreshes crew rows before each draw and drains click/post requests after it — content never mutates sibling windows mid-draw. The render arm in `Content::show` is rewritten painter-first. Spec: `docs/superpowers/specs/2026-06-10-chat-dispatcher-window-design.md` — read it first. Visual reference: `_chat_mockup.html`.

**Tech Stack:** Rust (edition 2024), egui 0.34 (`eframe 0.34.3`), `chrono` (new dep, clock only) for HH:MM rendering. Windows, GNU toolchain.

**Build gotchas (from CLAUDE.md):** kill any running foreman before building (`Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue`) or linking fails with os error 5. Tests spawn real `cmd.exe` PTYs — established repo pattern. egui 0.34: go through the painter for text (`ui.painter().layout…`); `ui.fonts(|f|…)` needs `&mut`.

**Protocol freeze:** the `chat` pipe verb, injection framing `[chat p1 #N] tX: text`, and `--history` line format are **unchanged** by this plan. If a task seems to need a protocol change, stop and re-read the spec.

**Branch:** `feature/agent-dispatch` (current).

---

## File Structure

| File | Changes |
|---|---|
| `Cargo.toml` | + `chrono` (default-features off, `clock`) |
| `src/chat.rs` | `ChatKind`; `ChatMsg` gains `name`/`at`/`to`/`kind`; `sys()`; `msgs()`/`last_seq()`/`last_activity()`; history filtering; `age_label`; `CrewRow` + `sort_crew`; `ChatView`; `ChatBlock` + `build_blocks`; `tail_rows` deleted (Task 4) |
| `src/wm.rs` | `display_name()`; name stamping + join/exit sysline emission; `Content::Chat(ChatView)` payload swap; `refresh_chat_view` pre-frame pass + title chip; render arm rewrite; `drain_chat_clicks`; slice 2: `chat_post_human`, `chat_broadcast` takes `Option<WinId>`, input drain |
| `src/terminal.rs` | none (uses existing `exited`/`exit_to_note`/`inject_input`) |
| `src/control.rs` | none (protocol frozen) |
| `docs/epics/agent-dispatch-epic.md` | chat viewer section rewritten |

Current code facts an implementer must not re-derive wrongly:

- `WinId = u64` (`src/wm.rs`). Child-manager window ids start at 1.
- `Tab { title, content, chat_member }`; `Win { id, tabs, active, … }`; `Win::title()` returns the active tab's title.
- `Session::exited(&mut self) -> Option<u32>` (note `&mut`); `Session::exit_to_note(&mut self) -> Option<u32>` fires exactly once per exit.
- Colors: `TEXT=(222,222,212)`, `DIM=(150,143,125)`, `WIN_BG=(33,30,24)`, `BORDER=(60,55,45)`.
- `Content::show(&mut self, ui, rect, active, base, win_id, resp) -> bool`.
- `Content::Chat` match sites in `src/wm.rs` (0-based lines at plan time): render arm ~283, `keepalive` ~317, `open_chat_window` ~801/809/823, `refresh_exit_titles` ~1484, tests ~3183/3229. Re-grep `Content::Chat` after each task; do not trust these numbers blindly.
- The rename flow (`src/wm.rs:1747-1772`) is the in-repo pattern for a themed `egui::TextEdit` over painter chrome — slice 2 copies it.

---

### Task 1: model rework — kinds, timestamps, stamped names (chat.rs + wm.rs)

`ChatLog::post` changes signature, so the wm caller updates in the same task or the crate doesn't compile. `tail_rows` is NOT deleted here — the old render arm still uses it until Task 4.

**Files:**
- Modify: `src/chat.rs` (whole file is small)
- Modify: `src/wm.rs` (`chat_post`, new `display_name`, existing chat tests)

- [ ] **Step 1: Write the failing tests**

Replace the body of `mod tests` in `src/chat.rs` with (keep `tail_rows_splits_multiline_messages_into_physical_rows` as-is — it dies in Task 4):

```rust
    use super::*;
    use std::time::{Duration, SystemTime};

    #[test]
    fn post_assigns_increasing_seq_from_one() {
        let mut log = ChatLog::new();
        assert_eq!(log.post("t1", "worker A", "first").seq, 1);
        assert_eq!(log.post("t3", "worker B", "second").seq, 2);
    }

    #[test]
    fn post_stamps_name_time_and_kind() {
        let before = SystemTime::now();
        let mut log = ChatLog::new();
        let m = log.post("t2", "architect", "hi");
        assert_eq!(m.name, "architect");
        assert_eq!(m.kind, ChatKind::Post);
        assert_eq!(m.to, None);
        assert!(m.at >= before && m.at <= SystemTime::now());
        // blank display name falls back to the id
        let m = log.post("t9", "  ", "hi");
        assert_eq!(m.name, "t9");
    }

    #[test]
    fn line_and_frame_formats_unchanged() {
        // PROTOCOL FREEZE: --history and injection framing keep the v1 shape.
        let mut log = ChatLog::new();
        let m = log.post("t2", "architect", "taking the parser refactor");
        assert_eq!(m.line(), "#1 t2: taking the parser refactor");
        assert_eq!(m.frame("p1"), "[chat p1 #1] t2: taking the parser refactor");
    }

    #[test]
    fn sys_entries_get_seqs_but_stay_out_of_history() {
        let mut log = ChatLog::new();
        log.post("t1", "a", "m1");
        let j = log.sys(ChatKind::Joined, "t5", "architect");
        assert_eq!(j.seq, 2);
        assert_eq!(j.kind, ChatKind::Joined);
        log.post("t5", "architect", "m2");
        assert_eq!(
            log.tail_lines(10),
            vec!["#1 t1: m1".to_string(), "#3 t5: m2".to_string()],
            "history (--history) must skip system entries"
        );
        assert_eq!(log.last_seq(), 3);
        assert_eq!(log.msgs().len(), 3, "viewer sees everything");
    }

    #[test]
    fn tail_lines_slices_the_end() {
        let mut log = ChatLog::new();
        for i in 1..=5 {
            log.post("t1", "a", &format!("m{i}"));
        }
        assert_eq!(log.tail_lines(2), vec!["#4 t1: m4", "#5 t1: m5"]);
        assert_eq!(log.tail_lines(99).len(), 5);
        assert!(ChatLog::new().tail_lines(3).is_empty());
    }

    #[test]
    fn last_activity_is_latest_entry_of_any_kind() {
        let mut log = ChatLog::new();
        assert_eq!(log.last_activity("t4"), None);
        log.sys(ChatKind::Joined, "t4", "skeptic");
        let joined_at = log.msgs().last().unwrap().at;
        assert_eq!(log.last_activity("t4"), Some(joined_at), "never-posted member uses join time");
        log.post("t4", "skeptic", "hi");
        let posted_at = log.msgs().last().unwrap().at;
        assert_eq!(log.last_activity("t4"), Some(posted_at));
    }

    #[test]
    fn age_label_boundaries() {
        assert_eq!(age_label(Duration::from_secs(0)), ("now".to_string(), false));
        assert_eq!(age_label(Duration::from_secs(59)), ("now".to_string(), false));
        assert_eq!(age_label(Duration::from_secs(60)), ("1m".to_string(), false));
        assert_eq!(age_label(Duration::from_secs(299)), ("4m".to_string(), false));
        assert_eq!(age_label(Duration::from_secs(300)), ("5m".to_string(), true));
        assert_eq!(age_label(Duration::from_secs(3600)), ("1h".to_string(), true));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test chat:: 2>&1 | Select-Object -Last 15`
Expected: compile errors — `ChatKind`, `sys`, `msgs`, `last_seq`, `last_activity`, `age_label` not found; `post` arity wrong.

- [ ] **Step 3: Implement the model**

In `src/chat.rs`, above `ChatMsg` add:

```rust
use std::time::{Duration, SystemTime};

/// What a log entry is. System entries (join/exit) live in the same
/// append-only log so the transcript records membership changes, but they
/// are never injected into PTYs and never appear in `--history` output
/// (spec: chat-dispatcher-window §Model changes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatKind {
    Post,
    Joined,
    Exited,
}

/// Crew-board staleness threshold: a live member unheard for this long
/// renders its age in amber.
pub const STALE_AFTER: Duration = Duration::from_secs(300);

/// Relative age for the crew board: ("now"/"3m"/"2h", is_stale).
pub fn age_label(d: Duration) -> (String, bool) {
    let stale = d >= STALE_AFTER;
    let s = d.as_secs();
    let label = if s < 60 {
        "now".to_string()
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else {
        format!("{}h", s / 3600)
    };
    (label, stale)
}
```

Replace `ChatMsg` (struct only — `line`/`frame` keep their exact v1 bodies):

```rust
pub struct ChatMsg {
    pub seq: u64,
    pub from: String, // "t2" (or the slice-2 human id "you")
    /// Display name stamped at post time (the sender's tab title) so history
    /// never rewrites when a window is retitled. Falls back to `from`.
    pub name: String,
    pub text: String, // empty for system entries
    pub at: SystemTime,
    /// Mention target — render-only until v2 mention delivery sets it.
    pub to: Option<String>,
    pub kind: ChatKind,
}
```

In `impl ChatLog`, replace `post` and add the new methods (keep `new`, `tail_lines`, `tail_rows` where noted):

```rust
    pub fn post(&mut self, from: &str, name: &str, text: &str) -> &ChatMsg {
        self.push(from, name, text, ChatKind::Post)
    }

    /// Append a membership event (join/exit). Text stays empty; the viewer
    /// derives the display line from kind + name + id.
    pub fn sys(&mut self, kind: ChatKind, from: &str, name: &str) -> &ChatMsg {
        self.push(from, name, "", kind)
    }

    fn push(&mut self, from: &str, name: &str, text: &str, kind: ChatKind) -> &ChatMsg {
        let name = if name.trim().is_empty() { from } else { name };
        let msg = ChatMsg {
            seq: self.msgs.len() as u64 + 1,
            from: from.to_string(),
            name: name.to_string(),
            text: text.to_string(),
            at: SystemTime::now(),
            to: None,
            kind,
        };
        self.msgs.push(msg);
        self.msgs.last().expect("just pushed")
    }

    /// Every entry, system lines included — the viewer's read path.
    pub fn msgs(&self) -> &[ChatMsg] {
        &self.msgs
    }

    pub fn last_seq(&self) -> u64 {
        self.msgs.len() as u64
    }

    /// When `from` was last heard from (any entry kind) — crew-board ages.
    pub fn last_activity(&self, from: &str) -> Option<SystemTime> {
        self.msgs.iter().rev().find(|m| m.from == from).map(|m| m.at)
    }
```

Change `tail_lines` to skip system entries (history is messages, not furniture):

```rust
    /// Last `n` POSTS as display lines, oldest first — the `--history` verb.
    /// System entries are excluded: agents asked for messages. The resulting
    /// seq gaps are harmless (seqs exist to be cited, not to be dense).
    pub fn tail_lines(&self, n: usize) -> Vec<String> {
        let mut lines: Vec<String> = self
            .msgs
            .iter()
            .rev()
            .filter(|m| m.kind == ChatKind::Post)
            .take(n)
            .map(ChatMsg::line)
            .collect();
        lines.reverse();
        lines
    }
```

`tail_rows` must also skip non-posts so the interim viewer doesn't print empty lines — change its iterator line from `for m in self.msgs.iter().rev() {` to:

```rust
        'outer: for m in self.msgs.iter().rev().filter(|m| m.kind == ChatKind::Post) {
```

- [ ] **Step 4: Update the wm caller — name stamping**

In `src/wm.rs`, add next to `term_id`:

```rust
/// A tab's display name for chat purposes: the title minus the one-shot
/// exit marker `refresh_exit_titles` appends.
fn display_name(title: &str) -> &str {
    title.split("  ·  exited").next().unwrap_or(title).trim()
}
```

In `chat_post`, replace the two lines

```rust
        let mut log = self.chat.borrow_mut();
        let msg = log.post(&format!("t{from}"), text);
```

with:

```rust
        let name = display_name(sender.title()).to_string();
        let mut log = self.chat.borrow_mut();
        let msg = log.post(&format!("t{from}"), &name, text);
```

(`sender` is already in scope; `Win::title()` is the active tab's title — the same active-tab identity resolution the rest of chat uses.)

Update any other `ChatLog::post` callers found by the compiler (tests use the 3-arg form).

- [ ] **Step 5: Run the affected suites**

Run: `cargo test chat:: wm:: control:: 2>&1 | Select-Object -Last 15`
Expected: all pass — `chat_post_validates_joins_and_frames` and the control-pipe tests prove the frozen formats survived.

- [ ] **Step 6: Commit**

```powershell
git add src/chat.rs src/wm.rs
git commit -m @'
Chat window: kinds, timestamps, stamped names on the log model

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
'@
```

---

### Task 2: join/exit system entries (wm.rs)

**Files:**
- Modify: `src/wm.rs` (`add_terminal_cmd`, `chat_post`, `refresh_exit_titles`, tests)

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `src/wm.rs` (helpers `pause_argv`, `mgr_with_project` exist):

```rust
    #[test]
    fn dispatch_emits_a_joined_entry() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        let argv = vec!["cmd.exe".to_string(), "/c".to_string(), "exit 0".to_string()];
        let t = wm
            .add_terminal_cmd(&argv, None, Some("worker A"), &ctx)
            .unwrap();
        let log = wm.chat.borrow();
        let m = log.msgs().last().expect("no joined entry");
        assert_eq!(m.kind, crate::chat::ChatKind::Joined);
        assert_eq!(m.from, format!("t{t}"));
        assert_eq!(m.name, "worker A");
    }

    #[test]
    fn first_post_emits_joined_before_the_post() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        let t = wm.add_terminal_cmd(&pause_argv(), None, None, &ctx).unwrap();
        // simulate a hand-opened terminal: not yet a member
        let w = wm.windows.iter_mut().find(|w| w.id == t).unwrap();
        w.tabs[w.active].chat_member = false;
        wm.chat_post(t, "hello").unwrap();
        let log = wm.chat.borrow();
        let kinds: Vec<_> = log.msgs().iter().map(|m| m.kind).collect();
        // dispatch auto-join from add_terminal_cmd, then the simulated
        // un-join means: Joined (dispatch), Joined (first post), Post
        assert_eq!(
            kinds.last_chunk::<2>().unwrap(),
            &[crate::chat::ChatKind::Joined, crate::chat::ChatKind::Post]
        );
        drop(log);
        // second post: member already — no second Joined
        wm.chat_post(t, "again").unwrap();
        let log = wm.chat.borrow();
        assert_eq!(log.msgs().last().unwrap().kind, crate::chat::ChatKind::Post);
        let joins = log
            .msgs()
            .iter()
            .filter(|m| m.kind == crate::chat::ChatKind::Joined && m.from == format!("t{t}"))
            .count();
        assert_eq!(joins, 2, "one from dispatch, one from first post — not three");
    }

    #[test]
    fn member_exit_emits_an_exited_entry_nonmember_does_not() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        let argv = vec!["cmd.exe".to_string(), "/c".to_string(), "exit 0".to_string()];
        let member = wm.add_terminal_cmd(&argv, None, Some("worker A"), &ctx).unwrap();
        let outsider = wm.add_terminal_cmd(&argv, None, Some("plain"), &ctx).unwrap();
        let w = wm.windows.iter_mut().find(|w| w.id == outsider).unwrap();
        w.tabs[w.active].chat_member = false;
        // wait for both `cmd /c exit 0` children to end
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let mut done = 0;
            for id in [member, outsider] {
                let w = wm.windows.iter_mut().find(|w| w.id == id).unwrap();
                let Content::Terminal(s) = &mut w.tabs[w.active].content else { panic!() };
                if s.exited().is_some() {
                    done += 1;
                }
            }
            if done == 2 {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "children never exited");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        wm.refresh_exit_titles();
        let log = wm.chat.borrow();
        let exits: Vec<_> = log
            .msgs()
            .iter()
            .filter(|m| m.kind == crate::chat::ChatKind::Exited)
            .collect();
        assert_eq!(exits.len(), 1, "only the member's exit is recorded");
        assert_eq!(exits[0].from, format!("t{member}"));
        assert_eq!(exits[0].name, "worker A", "name captured before the exit marker lands");
    }
```

Note: `last_chunk` needs a recent toolchain; if it doesn't compile, use `&kinds[kinds.len() - 2..]` against a slice literal instead.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test dispatch_emits_a_joined first_post_emits_joined member_exit_emits 2>&1 | Select-Object -Last 15`
Expected: FAIL — no `Joined`/`Exited` entries are emitted yet (the tests compile because Task 1 added the kinds).

- [ ] **Step 3: Implement**

In `add_terminal_cmd`, the auto-join block becomes (title is in scope as the computed `title` local):

```rust
        // Dispatched agents auto-join the project chat room (spec §2) — and
        // the transcript records it.
        if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
            w.tabs[w.active].chat_member = true;
            self.chat
                .borrow_mut()
                .sys(crate::chat::ChatKind::Joined, &format!("t{id}"), display_name(&title));
        } else {
            debug_assert!(false, "just-pushed window {id} missing");
        }
```

(If the borrow checker objects to `self.chat` inside the `if let` over `self.windows`, hoist `let chat = Rc::clone(&self.chat);` above the loop and use `chat.borrow_mut()`.)

In `chat_post`, replace the unconditional `sender.tabs[sender.active].chat_member = true;` with a join-edge check, keeping the sysline BEFORE the post so the transcript reads join-then-speak:

```rust
        let newly_joined = !sender.tabs[sender.active].chat_member;
        sender.tabs[sender.active].chat_member = true;
        let name = display_name(sender.title()).to_string();
        // (existing debug_assert + project lines stay here)
        let mut log = self.chat.borrow_mut();
        if newly_joined {
            log.sys(crate::chat::ChatKind::Joined, &format!("t{from}"), &name);
        }
        let msg = log.post(&format!("t{from}"), &name, text);
```

In `refresh_exit_titles`, the Terminal arm becomes (hoist `let chat = Rc::clone(&self.chat);` before the window loop, and capture `let wid = w.id;` before the tab loop):

```rust
                    Content::Terminal(s) => {
                        if let Some(code) = s.exit_to_note() {
                            if t.chat_member {
                                // Name must be read BEFORE the marker is appended.
                                chat.borrow_mut().sys(
                                    crate::chat::ChatKind::Exited,
                                    &format!("t{wid}"),
                                    display_name(&t.title),
                                );
                            }
                            t.title.push_str(&format!("  ·  exited ({code})"));
                        }
                    }
```

- [ ] **Step 4: Run the wm suite**

Run: `cargo test wm:: 2>&1 | Select-Object -Last 15`
Expected: all pass, including the pre-existing dispatch/chat tests (they now see extra Joined entries in the log — fix any that assert exact message counts by filtering on `kind == ChatKind::Post`).

- [ ] **Step 5: Commit**

```powershell
git add src/wm.rs
git commit -m @'
Chat window: join/exit system entries in the transcript

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
'@
```

---

### Task 3: ChatView payload + crew rows + pre-frame refresh (chat.rs + wm.rs)

**Files:**
- Modify: `src/chat.rs` (`CrewRow`, `sort_crew`, `ChatView`)
- Modify: `src/wm.rs` (every `Content::Chat` site, `refresh_chat_view`, call in `show`, tests)

- [ ] **Step 1: Write the failing tests**

In `src/chat.rs` tests:

```rust
    fn row(id: &str, exited: bool, last_secs_ago: Option<u64>) -> CrewRow {
        let now = SystemTime::now();
        CrewRow {
            win: 1,
            tab: 0,
            id: id.to_string(),
            name: id.to_string(),
            exited,
            last: last_secs_ago.map(|s| now - Duration::from_secs(s)),
        }
    }

    #[test]
    fn sort_crew_puts_stalest_live_first_and_exited_last() {
        let mut rows = vec![
            row("t1", false, Some(5)),
            row("t3", true, Some(10)),
            row("t5", false, Some(600)),
            row("t4", false, None), // never heard: treated as oldest
        ];
        sort_crew(&mut rows);
        let order: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(order, vec!["t4", "t5", "t1", "t3"]);
    }
```

In `src/wm.rs` tests:

```rust
    #[test]
    fn refresh_chat_view_builds_rows_and_title_chip() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        wm.last_area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let a = wm.add_terminal_cmd(&pause_argv(), None, Some("worker A"), &ctx).unwrap();
        let b = wm.add_terminal_cmd(&pause_argv(), None, Some("plain"), &ctx).unwrap();
        let w = wm.windows.iter_mut().find(|w| w.id == b).unwrap();
        w.tabs[w.active].chat_member = false;
        wm.open_chat_window();
        wm.refresh_chat_view();
        let view_win = wm
            .windows
            .iter()
            .find(|w| w.tabs.iter().any(|t| matches!(t.content, Content::Chat(_))))
            .unwrap();
        let tab = view_win
            .tabs
            .iter()
            .find(|t| matches!(t.content, Content::Chat(_)))
            .unwrap();
        assert_eq!(tab.title, "chat · 1 live");
        let Content::Chat(v) = &tab.content else { panic!() };
        assert_eq!(v.crew.len(), 1, "only members appear");
        assert_eq!(v.crew[0].id, format!("t{a}"));
        assert_eq!(v.crew[0].name, "worker A");
        assert!(!v.crew[0].exited);
        assert!(v.crew[0].last.is_some(), "joined entry counts as heard");
    }

    #[test]
    fn chat_view_watermark_moves_on_focus_loss_only() {
        let log = Rc::new(RefCell::new(crate::chat::ChatLog::new()));
        log.borrow_mut().post("t1", "a", "before-open");
        let mut v = crate::chat::ChatView::new(Rc::clone(&log));
        assert_eq!(v.last_seen, 1, "creation watermark = current tail (no NEW backlog)");
        v.on_frame(true); // focused
        log.borrow_mut().post("t1", "a", "while-focused");
        v.on_frame(true);
        assert_eq!(v.last_seen, 1, "watermark holds while focused");
        v.on_frame(false); // focus left
        assert_eq!(v.last_seen, 2, "watermark catches up on the focus-loss edge");
        log.borrow_mut().post("t1", "a", "while-unfocused");
        v.on_frame(false);
        assert_eq!(v.last_seen, 2, "unfocused arrivals stay above the watermark");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test sort_crew refresh_chat_view chat_view_watermark 2>&1 | Select-Object -Last 15`
Expected: compile errors — `CrewRow`, `sort_crew`, `ChatView`, `refresh_chat_view` don't exist.

- [ ] **Step 3: Implement — chat.rs**

```rust
/// One crew-board row, assembled by the owning project manager each frame.
/// `win`/`tab` locate the member for click-to-focus. Identity is the hosting
/// window's id — the same active-tab staleness family as the rest of chat.
pub struct CrewRow {
    pub win: crate::wm::WinId,
    pub tab: usize,
    pub id: String,   // "t4"
    pub name: String, // live tab title (exit marker stripped)
    pub exited: bool,
    pub last: Option<SystemTime>,
}

/// Crew-board order: live members stalest-first (the ones to worry about),
/// never-heard counting as oldest; exited members sink to the bottom.
pub fn sort_crew(rows: &mut [CrewRow]) {
    rows.sort_by(|a, b| {
        a.exited
            .cmp(&b.exited)
            .then_with(|| a.last.cmp(&b.last)) // None sorts before Some(_) = oldest first
    });
}

/// Per-window viewer state behind `Content::Chat`. The log is shared with
/// the project manager; everything else is this window's view of it.
pub struct ChatView {
    pub log: std::rc::Rc<std::cell::RefCell<ChatLog>>,
    /// Refreshed by the owning manager before each draw (`refresh_chat_view`).
    pub crew: Vec<CrewRow>,
    /// NEW-divider watermark: highest seq seen while this window had focus.
    pub last_seen: u64,
    was_active: bool,
    /// Scroll offset from the TOP of the laid-out log, in px. Meaningful only
    /// while `stick` is false.
    pub scroll: f32,
    /// Follow the tail (autoscroll). Scrolling up unsticks — the view then
    /// holds its content position while new messages arrive — and scrolling
    /// back to the bottom re-sticks (spec: scrolling decision row).
    pub stick: bool,
    /// Crew row clicked this frame; drained by the manager after the draw
    /// loop (content must never mutate sibling windows mid-draw).
    pub click: Option<(crate::wm::WinId, usize)>,
}

impl ChatView {
    pub fn new(log: std::rc::Rc<std::cell::RefCell<ChatLog>>) -> Self {
        // Watermark starts at the current tail: opening the window is the
        // act of looking, so the backlog is not "new".
        let last_seen = log.borrow().last_seq();
        Self {
            log,
            crew: Vec::new(),
            last_seen,
            was_active: false,
            scroll: 0.0,
            stick: true,
            click: None,
        }
    }

    /// Call once per rendered frame. The watermark advances only on the
    /// focus-LOSS edge, so everything that arrived during a focused stretch
    /// stays marked NEW until the user looks away and comes back.
    pub fn on_frame(&mut self, active: bool) {
        if self.was_active && !active {
            self.last_seen = self.log.borrow().last_seq();
        }
        self.was_active = active;
    }
}
```

- [ ] **Step 4: Implement — wm.rs payload swap + refresh**

1. `Content::Chat(Rc<RefCell<crate::chat::ChatLog>>)` becomes `Content::Chat(crate::chat::ChatView)`. Fix every match site (re-grep `Content::Chat`):
   - render arm (~283): `Content::Chat(view)` — interim body: `let rows = view.log.borrow().tail_rows(fit);` (full rewrite is Task 4).
   - `keepalive` (~317), `refresh_exit_titles` (~1484), `matches!` sites: patterns with `Content::Chat(_)` need no change.
   - `open_chat_window` (~823): `Content::Chat(crate::chat::ChatView::new(Rc::clone(&self.chat)))`.
2. Add the pre-frame pass next to `open_chat_window`:

```rust
    /// Rebuild the chat viewer's crew rows and title chip. Runs before the
    /// draw loop each frame (cheap: a handful of members). No-op when no
    /// viewer window is open.
    fn refresh_chat_view(&mut self) {
        if !self
            .windows
            .iter()
            .any(|w| w.tabs.iter().any(|t| matches!(t.content, Content::Chat(_))))
        {
            return;
        }
        let mut rows = Vec::new();
        for w in &mut self.windows {
            let wid = w.id;
            for (i, t) in w.tabs.iter_mut().enumerate() {
                if !t.chat_member {
                    continue;
                }
                let Content::Terminal(s) = &mut t.content else { continue };
                rows.push(crate::chat::CrewRow {
                    win: wid,
                    tab: i,
                    id: format!("t{wid}"),
                    name: display_name(&t.title).to_string(),
                    exited: s.exited().is_some(),
                    last: None,
                });
            }
        }
        {
            let log = self.chat.borrow();
            for r in &mut rows {
                r.last = log.last_activity(&r.id);
            }
        }
        crate::chat::sort_crew(&mut rows);
        let n_live = rows.iter().filter(|r| !r.exited).count();
        for w in &mut self.windows {
            for t in &mut w.tabs {
                if let Content::Chat(v) = &mut t.content {
                    // mem::take, not a move — the compiler can't see that the
                    // singleton makes a second loop iteration unreachable.
                    v.crew = std::mem::take(&mut rows);
                    // Clobbers a user rename each frame — accepted: the chip
                    // IS the title for the chat window.
                    t.title = format!("chat · {n_live} live");
                    return;
                }
            }
        }
    }
```

3. Call it at the top of `WindowManager::show`, before the window draw loop (next to whatever per-frame refit work opens the function). Every nested project manager's `show` runs it for its own room.

- [ ] **Step 5: Run the suites**

Run: `cargo test chat:: wm:: 2>&1 | Select-Object -Last 15`
Expected: all pass, including `open_chat_window_is_a_singleton` and `open_chat_window_resurfaces_minimized_or_buried_viewer` (they only pattern-match `Content::Chat(_)`).

- [ ] **Step 6: Commit**

```powershell
git add src/chat.rs src/wm.rs
git commit -m @'
Chat window: ChatView payload, crew rows, pre-frame refresh + title chip

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
'@
```

---

### Task 4: blocks + the dispatcher render arm (chat.rs + wm.rs + Cargo.toml)

The paintable structure (`build_blocks`) is pure and TDD'd; the painting itself is verified live in Task 8.

**Files:**
- Modify: `Cargo.toml` (chrono)
- Modify: `src/chat.rs` (`ChatBlock`, `build_blocks`; delete `tail_rows` + its test)
- Modify: `src/wm.rs` (palette consts, `chat_color`, render arm rewrite)

- [ ] **Step 1: Add the dependency**

In `Cargo.toml` under `[dependencies]`:

```toml
chrono = { version = "0.4", default-features = false, features = ["clock"] }
```

(New dependency — approved by the spec's timestamp decision; clock-only keeps it lean.)

- [ ] **Step 2: Write the failing tests**

In `src/chat.rs` tests:

```rust
    fn msg(seq: u64, from: &str, text: &str, kind: ChatKind) -> ChatMsg {
        ChatMsg {
            seq,
            from: from.to_string(),
            name: format!("name-{from}"),
            text: text.to_string(),
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(seq * 60),
            to: None,
            kind,
        }
    }

    #[test]
    fn build_blocks_groups_consecutive_senders() {
        let msgs = vec![
            msg(1, "t4", "a", ChatKind::Post),
            msg(2, "t4", "b", ChatKind::Post),
            msg(3, "t5", "c", ChatKind::Post),
        ];
        let blocks = build_blocks(&msgs, 3, true);
        let shape: Vec<&str> = blocks
            .iter()
            .map(|b| match b {
                ChatBlock::Header { .. } => "H",
                ChatBlock::Text { .. } => "T",
                ChatBlock::Sys(_) => "S",
                ChatBlock::Divider => "D",
            })
            .collect();
        assert_eq!(shape, vec!["H", "T", "T", "H", "T"]);
    }

    #[test]
    fn build_blocks_sys_lines_break_groups_and_render_labels() {
        let msgs = vec![
            msg(1, "t4", "a", ChatKind::Post),
            msg(2, "t5", "", ChatKind::Joined),
            msg(3, "t4", "b", ChatKind::Post),
        ];
        let blocks = build_blocks(&msgs, 3, true);
        assert!(matches!(&blocks[2], ChatBlock::Sys(s) if s == "— name-t5 (t5) joined —"));
        // t4 gets a fresh header after the sys line even though it also sent #1
        assert!(matches!(&blocks[3], ChatBlock::Header { .. }));
    }

    #[test]
    fn build_blocks_places_divider_and_formats_meta() {
        let mut m3 = msg(3, "t5", "c", ChatKind::Post);
        m3.to = Some("skeptic".to_string());
        let msgs = vec![msg(1, "t4", "a", ChatKind::Post), m3];
        // compact: seq only (+ arrow)
        let blocks = build_blocks(&msgs, 1, true);
        assert!(matches!(&blocks[0], ChatBlock::Header { meta, .. } if meta == "#1"));
        assert!(matches!(&blocks[2], ChatBlock::Divider), "divider above first seq > last_seen");
        assert!(matches!(&blocks[3], ChatBlock::Header { meta, .. } if meta == "#3 · → skeptic"));
        assert!(matches!(&blocks[4], ChatBlock::Text { to: Some(t), .. } if t == "skeptic"));
        // comfortable: id · seq · HH:MM (don't assert the clock digits — tz-dependent)
        let blocks = build_blocks(&msgs, 0, false);
        assert!(matches!(&blocks[0], ChatBlock::Header { meta, .. }
            if meta.starts_with("t4 · #1 · ") && meta.len() == "t4 · #1 · 00:00".len()));
        // nothing new => no divider
        let blocks = build_blocks(&msgs, 99, true);
        assert!(!blocks.iter().any(|b| matches!(b, ChatBlock::Divider)));
    }
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test build_blocks 2>&1 | Select-Object -Last 15`
Expected: compile error — `ChatBlock`, `build_blocks` not found.

- [ ] **Step 4: Implement — chat.rs**

```rust
/// What the viewer paints, in order. Pure data so grouping/divider/meta
/// logic is testable without egui.
pub enum ChatBlock {
    /// "— architect (t5) joined —"
    Sys(String),
    /// The amber NEW rule.
    Divider,
    /// Sender header for a run of consecutive messages.
    Header { name: String, id: String, meta: String },
    /// One message body under the current header.
    Text { text: String, to: Option<String> },
}

/// Flatten the log into paint order. `last_seen` is the NEW watermark;
/// `compact` trims meta to the seq (narrow windows).
pub fn build_blocks(msgs: &[ChatMsg], last_seen: u64, compact: bool) -> Vec<ChatBlock> {
    let mut out = Vec::new();
    let mut current: Option<&str> = None; // sender of the open group
    let mut divider_done = false;
    for m in msgs {
        if !divider_done && m.seq > last_seen {
            out.push(ChatBlock::Divider);
            divider_done = true;
            current = None; // a divider breaks the group like a sys line
        }
        match m.kind {
            ChatKind::Joined | ChatKind::Exited => {
                let verb = if m.kind == ChatKind::Joined { "joined" } else { "exited" };
                out.push(ChatBlock::Sys(format!("— {} ({}) {verb} —", m.name, m.from)));
                current = None;
            }
            ChatKind::Post => {
                if current != Some(m.from.as_str()) {
                    let mut meta = if compact {
                        format!("#{}", m.seq)
                    } else {
                        let t: chrono::DateTime<chrono::Local> = m.at.into();
                        format!("{} · #{} · {}", m.from, m.seq, t.format("%H:%M"))
                    };
                    if let Some(to) = &m.to {
                        meta.push_str(&format!(" · → {to}"));
                    }
                    out.push(ChatBlock::Header {
                        name: m.name.clone(),
                        id: m.from.clone(),
                        meta,
                    });
                    current = Some(m.from.as_str());
                }
                out.push(ChatBlock::Text {
                    text: m.text.clone(),
                    to: m.to.clone(),
                });
            }
        }
    }
    out
}
```

Mind one subtlety the tests pin down: a targeted message (`to` set) starts its own header even from the same sender **only if** the meta differs — simplest correct rule: treat a `to`-carrying message as a group breaker by adding `|| m.to.is_some()` to the header condition, and reset `current = None` after pushing its Text so the next plain message re-headers. Add that and extend `build_blocks_places_divider_and_formats_meta` if the simple version fails it.

Delete `tail_rows` and the `tail_rows_splits_multiline_messages_into_physical_rows` test — Step 5's render no longer uses it.

- [ ] **Step 5: Implement — wm.rs render arm**

Palette consts next to the existing color block:

```rust
// Chat viewer palette. Sender colors are assigned by terminal-id hash —
// stable for a given id, distinct enough across a small fleet.
const CHAT_COLORS: [egui::Color32; 6] = [
    egui::Color32::from_rgb(231, 169, 63),  // amber (also the human "you")
    egui::Color32::from_rgb(127, 179, 127), // green
    egui::Color32::from_rgb(111, 167, 199), // blue
    egui::Color32::from_rgb(199, 127, 174), // pink
    egui::Color32::from_rgb(180, 160, 100), // sand
    egui::Color32::from_rgb(140, 170, 160), // sage
];
const CHAT_STALE: egui::Color32 = egui::Color32::from_rgb(202, 164, 90);
const CHAT_LIVE: egui::Color32 = egui::Color32::from_rgb(127, 179, 127);
const CHAT_EDGE: egui::Color32 = egui::Color32::from_rgb(150, 107, 28);
const CHAT_MENTION_BG: egui::Color32 = egui::Color32::from_rgb(69, 64, 47);
const CHAT_BOARD_W: f32 = 160.0;
const CHAT_BOARD_MIN_W: f32 = 480.0; // window narrower than this hides the board

fn chat_color(id: &str) -> egui::Color32 {
    if id == "you" {
        return CHAT_COLORS[0];
    }
    let n: u64 = id.trim_start_matches('t').parse().unwrap_or(0);
    CHAT_COLORS[(n as usize) % CHAT_COLORS.len()]
}
```

Replace the `Content::Chat(view)` arm of `Content::show` with:

```rust
            Content::Chat(view) => {
                let p = ui.painter_at(rect);
                p.rect_filled(rect, 0.0, WIN_BG);
                let pad = 8.0;
                let meta_font = egui::FontId::proportional(11.0);
                let body_font = egui::FontId::proportional(12.5);
                let compact = rect.width() < CHAT_BOARD_MIN_W;

                // ---- crew board (comfortable widths only) ----
                let mut log_left = rect.min.x;
                if !compact {
                    let board = egui::Rect::from_min_max(
                        rect.min,
                        egui::pos2(rect.min.x + CHAT_BOARD_W, rect.max.y),
                    );
                    log_left = board.max.x;
                    p.line_segment(
                        [egui::pos2(board.max.x, rect.min.y), egui::pos2(board.max.x, rect.max.y)],
                        egui::Stroke::new(1.0, BORDER),
                    );
                    p.text(
                        egui::pos2(board.min.x + pad, board.min.y + pad),
                        egui::Align2::LEFT_TOP,
                        "CREW · BY LAST HEARD",
                        egui::FontId::proportional(9.5),
                        DIM,
                    );
                    let now = std::time::SystemTime::now();
                    let row_h = 20.0;
                    let mut y = board.min.y + pad + 16.0;
                    for r in &view.crew {
                        let row = egui::Rect::from_min_size(
                            egui::pos2(board.min.x + 4.0, y),
                            egui::vec2(board.width() - 8.0, row_h),
                        );
                        let hovered = resp.hovered()
                            && resp.hover_pos().is_some_and(|p| row.contains(p));
                        if hovered {
                            p.rect_filled(row, 3.0, TITLE_BG);
                        }
                        if hovered && resp.clicked() {
                            view.click = Some((r.win, r.tab));
                        }
                        let dot = if r.exited { BORDER } else { CHAT_LIVE };
                        p.circle_filled(egui::pos2(row.min.x + 7.0, row.center().y), 3.0, dot);
                        let name_col = if r.exited { DIM } else { chat_color(&r.id) };
                        p.text(
                            egui::pos2(row.min.x + 16.0, row.center().y),
                            egui::Align2::LEFT_CENTER,
                            format!("{} · {}", r.name, r.id),
                            egui::FontId::proportional(11.5),
                            name_col,
                        );
                        let (age, stale) = if r.exited {
                            ("exited".to_string(), false)
                        } else {
                            match r.last.and_then(|t| now.duration_since(t).ok()) {
                                Some(d) => crate::chat::age_label(d),
                                None => ("—".to_string(), false),
                            }
                        };
                        p.text(
                            egui::pos2(row.max.x - 4.0, row.center().y),
                            egui::Align2::RIGHT_CENTER,
                            age,
                            egui::FontId::proportional(10.5),
                            if stale { CHAT_STALE } else { DIM },
                        );
                        y += row_h;
                        if y + row_h > board.max.y {
                            break; // board overflow: clip; the log is the priority
                        }
                    }
                }

                // ---- log: layout pass (galleys + heights), then paint ----
                let log_rect = egui::Rect::from_min_max(
                    egui::pos2(log_left + pad, rect.min.y + pad),
                    egui::pos2(rect.max.x - pad, rect.max.y - pad),
                );
                let wrap = (log_rect.width() - 10.0).max(40.0);
                let blocks = {
                    let log = view.log.borrow();
                    crate::chat::build_blocks(log.msgs(), view.last_seen, compact)
                };
                enum Painted {
                    Galley(std::sync::Arc<egui::Galley>, egui::Color32, f32 /*indent*/, bool /*edge*/),
                    Centered(std::sync::Arc<egui::Galley>),
                    MetaPair(std::sync::Arc<egui::Galley>, std::sync::Arc<egui::Galley>, egui::Color32),
                    Rule(egui::Color32, Option<std::sync::Arc<egui::Galley>>),
                    Gap(f32),
                }
                let mut items: Vec<Painted> = Vec::new();
                let mut total = 0.0f32;
                for b in &blocks {
                    match b {
                        crate::chat::ChatBlock::Sys(s) => {
                            let g = p.layout(s.clone(), meta_font.clone(), DIM, wrap);
                            total += g.size().y + 6.0;
                            items.push(Painted::Centered(g));
                            items.push(Painted::Gap(6.0));
                        }
                        crate::chat::ChatBlock::Divider => {
                            let g = p.layout("NEW".into(), egui::FontId::proportional(9.0), CHAT_STALE, wrap);
                            total += 14.0;
                            items.push(Painted::Rule(CHAT_STALE, Some(g)));
                        }
                        crate::chat::ChatBlock::Header { name, id, meta } => {
                            let gn = p.layout_no_wrap(name.clone(), egui::FontId::proportional(12.0), chat_color(id));
                            let gm = p.layout_no_wrap(meta.clone(), meta_font.clone(), DIM);
                            total += gn.size().y + 2.0 + 4.0; // header + breathing room above
                            items.push(Painted::Gap(4.0));
                            items.push(Painted::MetaPair(gn, gm, chat_color(id)));
                        }
                        crate::chat::ChatBlock::Text { text, to } => {
                            // Mention chips: lay the body out as a LayoutJob so
                            // @tokens get their own colored sections inline.
                            let mut job = egui::text::LayoutJob::default();
                            job.wrap.max_width = wrap;
                            for (i, word) in text.split(' ').enumerate() {
                                let lead = if i == 0 { "" } else { " " };
                                let (col, bg) = if word.starts_with('@') && word.len() > 1 {
                                    (CHAT_COLORS[0], CHAT_MENTION_BG)
                                } else {
                                    (TEXT, egui::Color32::TRANSPARENT)
                                };
                                job.append(
                                    &format!("{lead}{word}"),
                                    0.0,
                                    egui::text::TextFormat {
                                        font_id: body_font.clone(),
                                        color: col,
                                        background: bg,
                                        ..Default::default()
                                    },
                                );
                            }
                            let g = p.layout_job(job);
                            total += g.size().y + 2.0;
                            items.push(Painted::Galley(g, TEXT, if to.is_some() { 10.0 } else { 0.0 }, to.is_some()));
                            items.push(Painted::Gap(2.0));
                        }
                    }
                }
                // Scroll: stick-to-bottom by default; a wheel-up unsticks and
                // the view then holds its CONTENT position while new messages
                // arrive (autoscroll paused); wheeling back to the bottom
                // re-sticks. Offset is measured from the top so an unstuck
                // view doesn't slide as `total` grows.
                let max = (total - log_rect.height()).max(0.0);
                if resp.hovered() {
                    let dy = ui.input(|i| i.raw_scroll_delta.y);
                    if dy != 0.0 {
                        let cur = if view.stick { max } else { view.scroll };
                        view.scroll = (cur - dy).clamp(0.0, max);
                        view.stick = view.scroll >= max - 1.0;
                    }
                }
                let offset = if view.stick { max } else { view.scroll.min(max) };
                let mut y = log_rect.min.y - offset;
                for it in items {
                    match it {
                        Painted::Gap(h) => y += h,
                        Painted::Centered(g) => {
                            let h = g.size().y;
                            let x = log_rect.center().x - g.size().x / 2.0;
                            p.galley(egui::pos2(x, y), g, DIM);
                            y += h;
                        }
                        Painted::Rule(col, label) => {
                            let mid = y + 7.0;
                            p.line_segment(
                                [egui::pos2(log_rect.min.x, mid), egui::pos2(log_rect.max.x, mid)],
                                egui::Stroke::new(1.0, CHAT_MENTION_BG),
                            );
                            if let Some(g) = label {
                                let w = g.size().x;
                                let lx = log_rect.center().x - w / 2.0;
                                p.rect_filled(
                                    egui::Rect::from_min_size(
                                        egui::pos2(lx - 4.0, y),
                                        egui::vec2(w + 8.0, 14.0),
                                    ),
                                    0.0,
                                    WIN_BG,
                                );
                                p.galley(egui::pos2(lx, y + 1.0), g, col);
                            }
                            y += 14.0;
                        }
                        Painted::MetaPair(gn, gm, _col) => {
                            let h = gn.size().y;
                            let nw = gn.size().x;
                            p.galley(egui::pos2(log_rect.min.x, y), gn, TEXT);
                            p.galley(egui::pos2(log_rect.min.x + nw + 6.0, y + 1.5), gm, DIM);
                            y += h + 2.0;
                        }
                        Painted::Galley(g, col, indent, edge) => {
                            let h = g.size().y;
                            if edge {
                                p.line_segment(
                                    [
                                        egui::pos2(log_rect.min.x + 2.0, y),
                                        egui::pos2(log_rect.min.x + 2.0, y + h),
                                    ],
                                    egui::Stroke::new(2.0, CHAT_EDGE),
                                );
                            }
                            p.galley(egui::pos2(log_rect.min.x + indent, y), g, col);
                            y += h;
                        }
                    }
                }
                view.on_frame(active);
                false
            }
```

Implementation notes for this step (read before typing):
- `Painter::layout_job` exists in egui 0.34; if the compiler disagrees, create the galley with `ui.ctx().fonts_mut(|f| f.layout_job(job))` instead — do NOT route through `ui.fonts` (needs `&mut Ui`, the known 0.34 trap).
- The `Gapless` arm in the snippet is explicitly marked for deletion — it exists only so a copy-paste of the snippet fails loudly if `build_blocks`'s variants drift.
- `painter_at(rect)` clips, so partially-scrolled items at the top are cut, not leaked.
- Sender colors paint the header name via `MetaPair`'s first galley: `p.galley(pos, gn, TEXT)` uses the galley's own section color (it was laid out with `chat_color(id)`), the `TEXT` argument is only the fallback.

- [ ] **Step 6: Run everything**

Run: `cargo test 2>&1 | Select-Object -Last 15`
Expected: all pass (the deleted `tail_rows` test is gone; nothing else referenced `tail_rows`).

- [ ] **Step 7: Commit**

```powershell
git add Cargo.toml Cargo.lock src/chat.rs src/wm.rs
git commit -m @'
Chat window: dispatcher render — crew board, grouped log, NEW divider

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
'@
```

---

### Task 5: click-to-focus drain (wm.rs)

**Files:**
- Modify: `src/wm.rs` (`drain_chat_clicks`, call site in `show`, tests)

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn chat_click_focuses_the_member_window_and_tab() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        wm.last_area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let t = wm.add_terminal_cmd(&pause_argv(), None, Some("worker A"), &ctx).unwrap();
        wm.open_chat_window();
        let chat_id = wm.focused.expect("open focuses the viewer");
        // simulate the render arm recording a click on worker A's row
        for w in &mut wm.windows {
            for tab in &mut w.tabs {
                if let Content::Chat(v) = &mut tab.content {
                    v.click = Some((t, 0));
                }
            }
        }
        wm.drain_chat_clicks();
        assert_eq!(wm.focused, Some(t), "click focused the member");
        assert_ne!(wm.focused, Some(chat_id));
        // stale target: must not panic or change focus
        for w in &mut wm.windows {
            for tab in &mut w.tabs {
                if let Content::Chat(v) = &mut tab.content {
                    v.click = Some((9999, 0));
                }
            }
        }
        wm.drain_chat_clicks();
        assert_eq!(wm.focused, Some(t), "stale click is a no-op");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test chat_click_focuses 2>&1 | Select-Object -Last 15`
Expected: compile error — `drain_chat_clicks` not found.

- [ ] **Step 3: Implement**

Next to `refresh_chat_view`:

```rust
    /// Apply crew-board clicks recorded during the draw (content cannot
    /// mutate sibling windows mid-loop). Stale targets (closed windows,
    /// merged-away tabs) are dropped silently — same staleness family as
    /// terminal-id resolution.
    fn drain_chat_clicks(&mut self) {
        let mut req = None;
        for w in &mut self.windows {
            for t in &mut w.tabs {
                if let Content::Chat(v) = &mut t.content {
                    if let Some(c) = v.click.take() {
                        req = Some(c);
                    }
                }
            }
        }
        if let Some((win, tab)) = req {
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == win) {
                if tab < w.tabs.len() {
                    w.active = tab;
                }
                w.minimized = false;
                self.focus(win);
            }
        }
    }
```

Call it in `WindowManager::show` immediately after the window draw loop, adjacent to where deferred `Act`s are applied (locate the `apply_acts` call inside `show`; the drain goes right before it so a click and an Act in the same frame resolve in a fixed order).

- [ ] **Step 4: Run the wm suite**

Run: `cargo test wm:: 2>&1 | Select-Object -Last 15`
Expected: all pass.

- [ ] **Step 5: Commit**

```powershell
git add src/wm.rs
git commit -m @'
Chat window: crew-board click focuses the member terminal

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
'@
```

---

### Task 6: slice 2 — input line + human identity (chat.rs + wm.rs)

**Files:**
- Modify: `src/chat.rs` (`ChatView` input fields)
- Modify: `src/wm.rs` (`chat_broadcast` signature, `chat_post_human`, `drain_chat_posts`, input strip in the render arm, tests)

- [ ] **Step 1: Write the failing tests**

In `src/wm.rs` tests:

```rust
    #[test]
    fn human_post_appends_with_reserved_id_and_broadcasts_to_all_members() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        wm.last_area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        // both members run `cmd /c pause`: ANY stdin byte makes them exit
        let a = wm.add_terminal_cmd(&pause_argv(), None, None, &ctx).unwrap();
        let b = wm.add_terminal_cmd(&pause_argv(), None, None, &ctx).unwrap();
        wm.open_chat_window();
        // simulate the input line submitting
        for w in &mut wm.windows {
            for t in &mut w.tabs {
                if let Content::Chat(v) = &mut t.content {
                    v.pending_post = Some("go".to_string());
                }
            }
        }
        wm.drain_chat_posts();
        {
            let log = wm.chat.borrow();
            let m = log
                .msgs()
                .iter()
                .rfind(|m| m.kind == crate::chat::ChatKind::Post)
                .expect("post missing");
            assert_eq!(m.from, "you");
            assert_eq!(m.name, "you");
            assert!(m.frame("p1").starts_with(&format!("[chat p1 #{}] you: go", m.seq)));
        }
        // BOTH members exit — the human excludes nobody
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let mut done = 0;
            for id in [a, b] {
                let w = wm.windows.iter_mut().find(|w| w.id == id).unwrap();
                let Content::Terminal(s) = &mut w.tabs[w.active].content else { panic!() };
                if s.exited().is_some() {
                    done += 1;
                }
            }
            if done == 2 {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "a member never got the post");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    #[test]
    fn empty_or_blank_human_post_is_a_noop() {
        let ctx = egui::Context::default();
        let mut wm = WindowManager::new();
        wm.tag = Some("p1".to_string());
        wm.last_area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        wm.open_chat_window();
        for w in &mut wm.windows {
            for t in &mut w.tabs {
                if let Content::Chat(v) = &mut t.content {
                    v.pending_post = Some("   ".to_string());
                }
            }
        }
        wm.drain_chat_posts();
        assert_eq!(wm.chat.borrow().msgs().len(), 0);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test human_post empty_or_blank_human 2>&1 | Select-Object -Last 15`
Expected: compile errors — `pending_post`, `drain_chat_posts` don't exist.

- [ ] **Step 3: Implement — chat.rs**

`ChatView` gains (and `ChatView::new` initializes them to `String::new()` / `None`):

```rust
    /// In-progress input line text (slice 2).
    pub input: String,
    /// A submitted line awaiting the manager's drain (`drain_chat_posts`).
    pub pending_post: Option<String>,
```

- [ ] **Step 4: Implement — wm.rs**

1. `chat_broadcast` takes `Option<WinId>` (`None` = the human; excludes nobody):

```rust
    fn chat_broadcast(&mut self, from: Option<WinId>, framed: &str) {
        for w in self.windows.iter_mut() {
            let active = w.active;
            let is_sender = Some(w.id) == from;
            for (i, tab) in w.tabs.iter_mut().enumerate() {
                if (is_sender && i == active) || !tab.chat_member {
                    continue;
                }
                if let Content::Terminal(s) = &mut tab.content {
                    if s.exited().is_none() {
                        s.inject_input(framed);
                    }
                }
            }
        }
    }
```

Update the existing caller chain (`chat_broadcast_in` → `chat_broadcast`) to pass `Some(from)`. Fix the existing broadcast tests' calls likewise.

2. Human post + drain, next to `chat_post`:

```rust
    /// The pane's reserved sender identity — can never collide with a "tN"
    /// terminal id (spec: chat-dispatcher-window §Slices).
    const HUMAN_ID: &'static str = "you";

    /// Append a post from the chat pane's input line. No membership games —
    /// the human is not a terminal. Returns the framed line for broadcast.
    fn chat_post_human(&mut self, text: &str) -> Option<String> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        debug_assert!(self.tag.is_some(), "human post on a tag-less manager");
        let project = self.tag.as_deref().unwrap_or("p?").to_string();
        let mut log = self.chat.borrow_mut();
        let msg = log.post(Self::HUMAN_ID, Self::HUMAN_ID, text);
        Some(msg.frame(&project))
    }

    /// Apply input-line submissions recorded during the draw. Human posts
    /// broadcast to ALL members — there is no sender terminal to exclude.
    fn drain_chat_posts(&mut self) {
        let mut pending = None;
        for w in &mut self.windows {
            for t in &mut w.tabs {
                if let Content::Chat(v) = &mut t.content {
                    if let Some(p) = v.pending_post.take() {
                        pending = Some(p);
                    }
                }
            }
        }
        if let Some(text) = pending {
            if let Some(framed) = self.chat_post_human(&text) {
                self.chat_broadcast(None, &framed);
            }
        }
    }
```

Call `self.drain_chat_posts();` right next to the `drain_chat_clicks()` call added in Task 5.

3. Input strip in the render arm. At the top of the `Content::Chat(view)` arm, reserve the strip and shrink the content rect (board and log both use the shrunk rect):

```rust
                const INPUT_H: f32 = 32.0;
                let input_rect = egui::Rect::from_min_max(
                    egui::pos2(rect.min.x, rect.max.y - INPUT_H),
                    rect.max,
                );
                let rect = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, input_rect.min.y));
```

Then at the end of the arm (after `view.on_frame(active);`), the themed TextEdit — this is the rename pattern from `src/wm.rs:1747-1772` verbatim-adapted:

```rust
                p.line_segment(
                    [input_rect.min, egui::pos2(input_rect.max.x, input_rect.min.y)],
                    egui::Stroke::new(1.0, BORDER),
                );
                let te_rect = input_rect.shrink2(egui::vec2(8.0, 5.0));
                p.rect_filled(te_rect, egui::CornerRadius::same(3), DESK_BG);
                p.rect_stroke(
                    te_rect,
                    egui::CornerRadius::same(3),
                    egui::Stroke::new(1.0, BORDER),
                    egui::StrokeKind::Inside,
                );
                ui.visuals_mut().selection.bg_fill =
                    egui::Color32::from_rgba_unmultiplied(231, 169, 63, 90);
                let te = ui.put(
                    te_rect,
                    egui::TextEdit::singleline(&mut view.input)
                        .id(base.with((win_id, "chat-input")))
                        .font(egui::FontId::proportional(12.5))
                        .text_color(TEXT)
                        .hint_text("Message…")
                        .vertical_align(egui::Align::Center)
                        .frame(egui::Frame::NONE)
                        .margin(egui::Margin::symmetric(6, 0))
                        .desired_width(te_rect.width()),
                );
                if te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    view.pending_post = Some(std::mem::take(&mut view.input));
                    te.request_focus(); // keep typing; multi-post sessions are the norm
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) && te.has_focus() {
                    view.input.clear();
                }
```

4. The crew board also shows the pane identity once input exists — in `refresh_chat_view`, after the member loop and before `sort_crew`:

```rust
        rows.push(crate::chat::CrewRow {
            win: 0, // no window: click is a no-op (id 0 never matches; ids start at 1)
            tab: 0,
            id: Self::HUMAN_ID.to_string(),
            name: Self::HUMAN_ID.to_string(),
            exited: false,
            last: self.chat.borrow().last_activity(Self::HUMAN_ID),
        });
```

(Adjust the Task 3 test's `v.crew.len()` assertion from 1 to 2, and its `n_live`-chip assertion from `"chat · 1 live"` to `"chat · 2 live"` — the human counts as live crew.)

- [ ] **Step 5: Run everything**

Run: `cargo test 2>&1 | Select-Object -Last 15`
Expected: all pass — in particular `chat_broadcast_hits_members_only_excluding_sender` (now via `Some(from)`) and `chat_broadcast_reaches_background_member_tab_not_foreground_shell` still hold.

- [ ] **Step 6: Commit**

```powershell
git add src/chat.rs src/wm.rs
git commit -m @'
Chat window: input line posts as the reserved human identity

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
'@
```

---

### Task 7: epic doc update

**Files:**
- Modify: `docs/epics/agent-dispatch-epic.md` (the "Chat viewer window" subsection of the Group chat section)

- [ ] **Step 1: Rewrite the viewer subsection**

Replace the existing "Chat viewer window" content with, in the epic's voice: the dispatcher's-desk layout (crew board by last-heard / grouped log / input line); the `ChatView` payload and why crew rows are refreshed pre-frame and clicks/posts drained post-frame (no sibling mutation mid-draw); system entries (join/exit) live in the log with seqs but are excluded from `--history` and never injected; the NEW-divider watermark rule (advances on focus loss); the reserved `you` identity and that human posts broadcast to all members; the title chip clobbering renames (accepted); and the staleness gotchas (crew identity is the hosting window id; click targets can go stale — dropped silently). Point at the spec and `_chat_mockup.html`.

- [ ] **Step 2: Commit**

```powershell
git add docs/epics/agent-dispatch-epic.md
git commit -m @'
Chat window: epic doc — dispatcher viewer section

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
cargo clippy 2>&1 | Select-Object -Last 5
```

Expected: build clean, all tests pass, clippy at or below the pre-task warning count.

- [ ] **Step 2: Live exchange + viewer**

Run `cargo run --release` (background). Inside a foreman project, dispatch two named interactive workers and exchange posts (worker invocation via `$env:FOREMAN_EXE`, prompt immediately after `claude` — the variadic-flag trap). Open the viewer (leader, then the OpenChat chord). Screenshot (script in `docs/HANDOFF.md` § 3), `Read` the PNG, confirm: crew board with names/ages ordered stalest-first, "N live" title chip, grouped messages with `tX · #N · HH:MM` meta, join system lines. **Do not claim success without the screenshot.**

- [ ] **Step 3: Adaptive + divider + click**

Resize the viewer below ~480 px: board hides, meta trims to `#N`. Restore width. Focus a worker terminal, have a worker post (or post from the input line), refocus the viewer: the NEW divider sits above the new message. Click a crew row: that worker's window takes focus. Screenshot each state.

- [ ] **Step 4: Input line + exit entry**

Type into the input line, Enter: both workers receive `[chat p1 #N] you: …` as a submitted turn; the log shows `you` in amber. Let one worker exit (post it an instruction to exit): crew row dims to `exited` and the log gains the exit system line. `& $env:FOREMAN_EXE chat --history 10` still prints plain `#N tX: text` lines — no names, no system lines (protocol frozen).

- [ ] **Step 5: Commit any verification fixes**

As their own commits. Do not amend.

---

## Out of Scope (spec)

Mention *delivery*/quiescence gating (v2 spec owns it), real names in the injection framing, persistence, cross-project chat, log filtering, @-completion, read receipts, unread counts. If a task seems to need one of these, stop and re-read the spec.
