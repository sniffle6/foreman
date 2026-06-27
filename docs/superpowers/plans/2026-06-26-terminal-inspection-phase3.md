# Terminal Inspection Phase 3 — Settle/Wait Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `foreman send` SETTLE (wait for terminal to go quiet) before replying, using a cross-frame pending list so the UI never blocks.

**Architecture:** Add an `output_gen: u64` counter to `Session::pump()` so any new PTY bytes increment it — a cheap, polling-friendly freshness signal. Add a `Vec<PendingSettle>` to `WindowManager` that is drained each frame: each entry watches one terminal's `output_gen`, resets its silence clock on any change, and fires the reply once quiet long enough (or deadline hit). The `CtrlMsg::Send` arm pushes to this list instead of replying immediately (unless `settle_ms == 0`). `main.rs` calls `advance_settles()` after each `show()` so sessions have already pumped this frame.

**Tech Stack:** Rust, egui 0.34, `std::sync::mpsc`, `std::time::{Instant,Duration}`.

## Global Constraints

- GNU toolchain (`stable-gnu`), w64devkit linker — NOT MSVC. Build commands use PowerShell.
- Kill running `foreman` before building (`Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue`).
- `cargo test` must stay fully green throughout.
- Do NOT touch: DSR/ready latch, `Listener`, `read_input`, `VoidListener`, `OpenReply` struct fields.
- Match the codebase borrow style: two-pass immutable-then-mutable for `session_mut`; `std::mem::take` for mutating a `Vec` while using `&mut self`.
- Comments minimal — only for non-obvious decisions.
- The build command: `$env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"; cargo build 2>&1 | Select-Object -Last 30`
- The test command: `$env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"; cargo test 2>&1 | Select-Object -Last 40`

---

### Task 1: Add `output_gen` counter to `Session` in `src/terminal.rs`

The `output_gen` counter is the cheapest possible signal that a terminal has received new PTY output since the last frame. Bumping it inside the `try_recv` loop means every newly-arrived byte batch (not every byte) increments it. The WM can poll this without touching the grid — no lock contention, no snapshot cost.

**Files:**
- Modify: `src/terminal.rs` (struct `Session`, `pump()`, add public getter)

**Interfaces:**
- Produces: `Session::output_gen() -> u64` (pub) — used by `wm.rs::session_gen`

- [ ] **Step 1: Add the field to `Session`**

  In `src/terminal.rs`, find the `Session` struct (line ~170). Add `output_gen: u64` as the last field:

  ```rust
  pub struct Session {
      // ... existing fields unchanged ...
      pending_note: Option<String>,
      pending_submit: Option<std::time::Instant>,
      ready: bool,
      output_gen: u64,   // ← add this
  }
  ```

- [ ] **Step 2: Initialize in `spawn_with`**

  Find the `spawn_with` constructor in `Session`. Add `output_gen: 0` to the initializer. (Search for `pending_submit: None, ready: false` — add after `ready: false`.)

  ```rust
  ready: false,
  output_gen: 0,
  ```

- [ ] **Step 3: Bump in `pump()`**

  In `pump()`, inside the `while let Ok(bytes) = self.rx.try_recv()` loop, add the bump after `self.parser.advance(...)`:

  ```rust
  fn pump(&mut self) {
      while let Ok(bytes) = self.rx.try_recv() {
          self.parser.advance(&mut self.term, &bytes);
          self.output_gen = self.output_gen.wrapping_add(1);  // ← add this
      }
      // ... rest unchanged
  ```

- [ ] **Step 4: Add public getter**

  After `pub fn term_mode(&self) -> ...`, add:

  ```rust
  /// Counter bumped every time new PTY bytes arrive in `pump()`.
  /// Used by the settle machinery to detect terminal activity.
  pub fn output_gen(&self) -> u64 {
      self.output_gen
  }
  ```

- [ ] **Step 5: Build to verify no compilation errors**

  ```powershell
  $env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
  Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue
  cargo build 2>&1 | Select-Object -Last 30
  ```

  Expected: `Compiling foreman ...` then `Finished ...` with no errors.

- [ ] **Step 6: Run tests to confirm still green**

  ```powershell
  $env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
  cargo test 2>&1 | Select-Object -Last 40
  ```

  Expected: all existing tests pass.

- [ ] **Step 7: Commit**

  ```powershell
  git add src/terminal.rs
  git commit -m "feat(terminal): add output_gen counter bumped on each PTY batch"
  ```

---

### Task 2: Pure `settle_tick` function + unit tests in `src/wm.rs` (TDD)

`settle_tick` is a stateless pure function that makes a single settle decision based on inputs — no `&mut self`, no I/O. This makes it trivially testable. The tests use `Instant::now()` plus `Duration` offsets to construct time values — no mocking needed, just arithmetic on instants.

**Files:**
- Modify: `src/wm.rs` (add constants, `settle_tick` fn, and its `#[cfg(test)]` tests inside the existing `mod tests` block)

**Interfaces:**
- Produces: `settle_tick(last_gen, quiet_since, deadline, quiet_window, current_gen, now) -> (u64, Instant, bool)`

- [ ] **Step 1: Write the failing tests first**

  In `src/wm.rs`, inside the existing `#[cfg(test)]` `mod tests` block (near the bottom of the file), add:

  ```rust
  // --- settle_tick pure logic ---

  #[test]
  fn settle_tick_not_done_within_window() {
      let t0 = std::time::Instant::now();
      let quiet_window = std::time::Duration::from_millis(120);
      let deadline = t0 + std::time::Duration::from_millis(4000);
      // gen unchanged, 50ms elapsed < 120ms window → not done
      let (g, qs, done) = super::settle_tick(
          5, t0, deadline, quiet_window,
          5,                                   // current_gen == last_gen (no output)
          t0 + std::time::Duration::from_millis(50),
      );
      assert_eq!(g, 5, "gen unchanged");
      assert_eq!(qs, t0, "quiet_since unchanged");
      assert!(!done, "should not be done yet");
  }

  #[test]
  fn settle_tick_done_after_quiet_window() {
      let t0 = std::time::Instant::now();
      let quiet_window = std::time::Duration::from_millis(120);
      let deadline = t0 + std::time::Duration::from_millis(4000);
      // gen unchanged, 150ms elapsed > 120ms window → done
      let (g, qs, done) = super::settle_tick(
          5, t0, deadline, quiet_window,
          5,
          t0 + std::time::Duration::from_millis(150),
      );
      assert_eq!(g, 5);
      assert_eq!(qs, t0);
      assert!(done, "should be done after quiet window");
  }

  #[test]
  fn settle_tick_gen_change_resets_quiet_since() {
      let t0 = std::time::Instant::now();
      let quiet_window = std::time::Duration::from_millis(120);
      let deadline = t0 + std::time::Duration::from_millis(4000);
      // Gen changed at t0+150ms: quiet_since resets to now; not done yet
      let now = t0 + std::time::Duration::from_millis(150);
      let (g, qs, done) = super::settle_tick(
          5, t0, deadline, quiet_window,
          6,    // current_gen != last_gen → output arrived
          now,
      );
      assert_eq!(g, 6, "gen must update to current");
      assert_eq!(qs, now, "quiet_since must reset to now");
      assert!(!done, "just received output, should not be done");
  }

  #[test]
  fn settle_tick_past_deadline_always_done() {
      let t0 = std::time::Instant::now();
      let quiet_window = std::time::Duration::from_millis(120);
      // deadline in the past
      let deadline = t0 - std::time::Duration::from_millis(1);
      // Even if gen just changed, deadline overrules
      let now = t0;
      let (g, qs, done) = super::settle_tick(
          5, t0, deadline, quiet_window,
          6,
          now,
      );
      assert_eq!(g, 6);
      assert_eq!(qs, now);
      assert!(done, "past deadline must be done regardless of gen");
  }
  ```

- [ ] **Step 2: Run tests — confirm they FAIL** (function not yet defined)

  ```powershell
  $env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
  cargo test settle_tick 2>&1 | Select-Object -Last 30
  ```

  Expected: compile error `cannot find function settle_tick` or similar.

- [ ] **Step 3: Add `DEFAULT_SETTLE_MS` / `MAX_SETTLE_MS` constants and `settle_tick` implementation**

  Near the top of `impl WindowManager` (or just above it, at module scope), add:

  ```rust
  // Default quiescence window for `foreman send`. Kept under REPLY_TIMEOUT (5s)
  // so the pipe server's recv_timeout never fires before a settle reply arrives.
  const DEFAULT_SETTLE_MS: u64 = 120;
  const MAX_SETTLE_MS: u64 = 4000;
  ```

  Then add the pure function (outside `impl`, or as a free fn in the module — NOT a method, so it stays testable without `self`):

  ```rust
  /// One settle tick. If output arrived (gen changed) the quiet window restarts.
  /// Returns (updated last_gen, updated quiet_since, done).
  fn settle_tick(
      last_gen: u64,
      quiet_since: std::time::Instant,
      deadline: std::time::Instant,
      quiet_window: std::time::Duration,
      current_gen: u64,
      now: std::time::Instant,
  ) -> (u64, std::time::Instant, bool) {
      let (last_gen, quiet_since) = if current_gen != last_gen {
          (current_gen, now)
      } else {
          (last_gen, quiet_since)
      };
      let done = now.duration_since(quiet_since) >= quiet_window || now >= deadline;
      (last_gen, quiet_since, done)
  }
  ```

- [ ] **Step 4: Run tests — confirm they PASS**

  ```powershell
  $env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
  cargo test settle_tick 2>&1 | Select-Object -Last 30
  ```

  Expected: `test settle_tick_not_done_within_window ... ok` etc., all 4 green.

- [ ] **Step 5: Run full test suite to confirm nothing regressed**

  ```powershell
  $env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
  cargo test 2>&1 | Select-Object -Last 40
  ```

  Expected: all tests pass.

- [ ] **Step 6: Commit**

  ```powershell
  git add src/wm.rs
  git commit -m "test(wm): TDD settle_tick — pure quiescence decision fn + 4 tests"
  ```

---

### Task 3: `PendingSettle` struct, `WindowManager` field, `session_gen`, and `advance_settles` in `src/wm.rs`

This is the heart of the cross-frame settle machinery. The key borrow challenge: `advance_settles` needs `&mut self` to drain and refill `pending_settles` AND to call `session_gen` (also `&self`). Solve with `std::mem::take`: take the vec out, iterate over it, call `self.session_gen(...)` freely, then move the kept entries back.

**Files:**
- Modify: `src/wm.rs`

**Interfaces:**
- Consumes: `settle_tick` (Task 2), `Session::output_gen()` (Task 1)
- Produces: `WindowManager::advance_settles(&mut self, now: Instant)` (pub)

- [ ] **Step 1: Add `PendingSettle` struct**

  Near the top of `src/wm.rs` (after the `use` statements, before `pub struct WindowManager`), add:

  ```rust
  struct PendingSettle {
      pid: WinId,
      tid: WinId,
      reply: std::sync::mpsc::Sender<crate::control::OpenReply>,
      last_gen: u64,
      quiet_since: std::time::Instant,
      deadline: std::time::Instant,
      quiet_window: std::time::Duration,
  }
  ```

- [ ] **Step 2: Add `pending_settles` field to `WindowManager` struct**

  In `pub struct WindowManager`, after `drag_from_tree: Option<WinId>`, add:

  ```rust
  /// Pending `foreman send` settle entries — serviced each frame by `advance_settles`.
  pending_settles: Vec<PendingSettle>,
  ```

- [ ] **Step 3: Initialize `pending_settles` in `new()`**

  In `WindowManager::new()`, add to the initializer (alongside `drag_from_tree: None`):

  ```rust
  pending_settles: Vec::new(),
  ```

- [ ] **Step 4: Add `session_gen` read-only accessor**

  After `session_mut`, add a matching immutable version that returns the current `output_gen`:

  ```rust
  /// Read the `output_gen` of the terminal at (pid, tid). Mirrors the
  /// active-tab-preferred lookup of `session_mut` but takes `&self`.
  fn session_gen(&self, pid: WinId, tid: WinId) -> Option<u64> {
      let win = self.windows.iter().find(|w| w.id == pid)?;
      let Content::Project(child) = &win.tabs[win.active].content else {
          return None;
      };
      let tw = child.windows.iter().find(|w| w.id == tid)?;
      let active = tw.active;
      let idx = if matches!(tw.tabs[active].content, Content::Terminal(_)) {
          active
      } else {
          tw.tabs.iter().position(|t| matches!(t.content, Content::Terminal(_)))?
      };
      let Content::Terminal(s) = &tw.tabs[idx].content else {
          return None;
      };
      Some(s.output_gen())
  }
  ```

- [ ] **Step 5: Add `pub fn advance_settles`**

  After `session_gen`, add:

  ```rust
  /// Drive all pending settle entries. Called each frame after `show()` so
  /// sessions have already pumped new PTY output this frame.
  pub fn advance_settles(&mut self, now: std::time::Instant) {
      if self.pending_settles.is_empty() {
          return;
      }
      use crate::control::OpenReply;
      let ok_reply = || OpenReply {
          ok: true,
          terminal: None,
          project: None,
          error: None,
          history: None,
          seq: None,
      };
      let mut settles = std::mem::take(&mut self.pending_settles);
      settles.retain_mut(|ps| {
          let current_gen = match self.session_gen(ps.pid, ps.tid) {
              None => {
                  // Terminal gone — reply ok and drop.
                  let _ = ps.reply.send(ok_reply());
                  return false;
              }
              Some(g) => g,
          };
          let (new_gen, new_qs, done) = settle_tick(
              ps.last_gen,
              ps.quiet_since,
              ps.deadline,
              ps.quiet_window,
              current_gen,
              now,
          );
          ps.last_gen = new_gen;
          ps.quiet_since = new_qs;
          if done {
              let _ = ps.reply.send(ok_reply());
              false  // drop entry
          } else {
              true   // keep
          }
      });
      self.pending_settles = settles;
  }
  ```

- [ ] **Step 6: Build to verify**

  ```powershell
  $env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
  Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue
  cargo build 2>&1 | Select-Object -Last 30
  ```

  Expected: `Finished` with no errors.

- [ ] **Step 7: Run tests**

  ```powershell
  $env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
  cargo test 2>&1 | Select-Object -Last 40
  ```

  Expected: all tests pass (settle is not yet wired to `handle_ctrl`, so existing send tests still get an immediate reply).

- [ ] **Step 8: Commit**

  ```powershell
  git add src/wm.rs
  git commit -m "feat(wm): PendingSettle struct + advance_settles cross-frame settle machinery"
  ```

---

### Task 4: Wire `handle_ctrl` Send arm to use settle, fix existing tests, add settle-path test

This changes `CtrlMsg::Send` handling from immediate-reply to push-to-pending (when `settle_ms != 0`). Two existing tests assume an immediate reply — they must pass `settle_ms: Some(0)` to stay on the fast path. A new test validates the settle path end-to-end (no real PTY needed — just advance time past the deadline).

Also changes `send_dispatch` signature to return `Result<(WinId, WinId), String>` so the caller has the ids to push into `pending_settles`.

**Files:**
- Modify: `src/wm.rs`

**Interfaces:**
- Consumes: `advance_settles` (Task 3), `session_gen` (Task 3), `DEFAULT_SETTLE_MS`/`MAX_SETTLE_MS` (Task 2)

- [ ] **Step 1: Change `send_dispatch` return type to return `(pid, tid)`**

  Find `fn send_dispatch` in `src/wm.rs`. Change signature and return:

  ```rust
  fn send_dispatch(
      &mut self,
      req: &crate::control::SendRequest,
  ) -> Result<(WinId, WinId), String> {
      let terminal = req.terminal.as_deref().ok_or("send: missing terminal")?;
      let (pid, tid) = self.resolve_terminal(req.project.as_deref(), terminal)?;
      // Read mode with an immutable borrow BEFORE taking the mutable session borrow.
      let mode = {
          let win = self.windows.iter().find(|w| w.id == pid).expect("resolved");
          let Content::Project(child) = &win.tabs[win.active].content else {
              return Err("not a project".into());
          };
          let tw = child.windows.iter().find(|w| w.id == tid).expect("resolved");
          let active = tw.active;
          let idx = if matches!(tw.tabs[active].content, Content::Terminal(_)) {
              active
          } else {
              tw.tabs
                  .iter()
                  .position(|t| matches!(t.content, Content::Terminal(_)))
                  .ok_or_else(|| format!("no terminal tab in t{tid}"))?
          };
          let Content::Terminal(s) = &tw.tabs[idx].content else {
              return Err("not a terminal tab".into());
          };
          s.term_mode()
      };
      // Validate key names BEFORE any write (atomic — errors before side effects).
      let key_bytes = crate::inspect::parse_keys(&req.keys, mode)?;
      let session = self.session_mut(pid, tid)?;
      if let Some(text) = &req.text {
          session.feed(text.as_bytes());
      }
      if !key_bytes.is_empty() {
          session.feed(&key_bytes);
      }
      Ok((pid, tid))
  }
  ```

- [ ] **Step 2: Rewrite the `CtrlMsg::Send` arm in `handle_ctrl`**

  Find the `CtrlMsg::Send(req, reply, sent) => {` block in `handle_ctrl` and replace it entirely:

  ```rust
  CtrlMsg::Send(req, reply, sent) => {
      if sent.elapsed() >= REPLY_TIMEOUT {
          return;
      }
      match self.send_dispatch(&req) {
          Err(e) => {
              let _ = reply.send(OpenReply::err(e));
          }
          Ok((pid, tid)) => {
              let settle = req.settle_ms.unwrap_or(DEFAULT_SETTLE_MS);
              if settle == 0 {
                  let _ = reply.send(OpenReply {
                      ok: true,
                      terminal: None,
                      project: None,
                      error: None,
                      history: None,
                      seq: None,
                  });
              } else {
                  let now = std::time::Instant::now();
                  let quiet_window =
                      std::time::Duration::from_millis(settle.min(MAX_SETTLE_MS));
                  let gen = self.session_gen(pid, tid).unwrap_or(0);
                  self.pending_settles.push(PendingSettle {
                      pid,
                      tid,
                      reply,
                      last_gen: gen,
                      quiet_since: now,
                      deadline: now
                          + std::time::Duration::from_millis(MAX_SETTLE_MS),
                      quiet_window,
                  });
              }
              ctx.request_repaint();
          }
      }
  }
  ```

- [ ] **Step 3: Fix the two existing send tests to use `settle_ms: Some(0)`**

  Find `fn send_msg` in the test module (around line 5109). The existing helper has `settle_ms: None`. Add a new `settle_ms` parameter to the helper, or simply add a separate `send_msg_settle0` helper. The cleanest fix: add a `settle_ms` parameter to `send_msg`:

  ```rust
  fn send_msg(
      project: Option<&str>,
      terminal: &str,
      text: &str,
      sent: std::time::Instant,
      settle_ms: Option<u64>,
  ) -> (
      crate::control::CtrlMsg,
      std::sync::mpsc::Receiver<crate::control::OpenReply>,
  ) {
      let (rtx, rrx) = std::sync::mpsc::channel();
      let req = crate::control::SendRequest {
          cmd: "send".into(),
          project: project.map(str::to_string),
          terminal: Some(terminal.to_string()),
          text: Some(text.to_string()),
          keys: vec![],
          settle_ms,
      };
      (crate::control::CtrlMsg::Send(req, rtx, sent), rrx)
  }
  ```

  Then update every call site of `send_msg` to pass `Some(0)` as the last argument so they still exercise the immediate path. Search for `send_msg(` in the test module and add the extra arg:

  - `send_replies_ok_for_valid_terminal`: change `send_msg(Some("p1"), &ta, "hello", std::time::Instant::now())` → `send_msg(Some("p1"), &ta, "hello", std::time::Instant::now(), Some(0))`
  - `send_unknown_terminal_errors`: change `send_msg(Some("p1"), "t99", "x", std::time::Instant::now())` → `send_msg(Some("p1"), "t99", "x", std::time::Instant::now(), Some(0))`
  - `stale_send_is_dropped`: change `send_msg(Some("p1"), &ta, "x", stale)` → `send_msg(Some("p1"), &ta, "x", stale, Some(0))`

- [ ] **Step 4: Add settle-path test**

  In the test module, after `stale_snapshot_is_dropped`, add:

  ```rust
  #[test]
  fn send_with_settle_pushes_pending_no_immediate_reply() {
      let ctx = egui::Context::default();
      let (mut d, a, _b) = chat_fixture(&ctx);
      let ta = format!("t{a}");
      // Default settle (None → DEFAULT_SETTLE_MS = 120ms): no immediate reply.
      let (msg, rrx) = send_msg(Some("p1"), &ta, "x", std::time::Instant::now(), None);
      d.handle_ctrl(msg, &ctx);
      assert!(
          rrx.try_recv().is_err(),
          "settle send must NOT reply immediately"
      );
      // Advance to past the MAX_SETTLE_MS deadline → settle fires.
      let future = std::time::Instant::now() + std::time::Duration::from_millis(5000);
      d.advance_settles(future);
      let r = rrx.try_recv().expect("settle must reply after deadline");
      assert!(r.ok, "{:?}", r.error);
  }

  #[test]
  fn send_with_settle_ms_zero_replies_immediately() {
      let ctx = egui::Context::default();
      let (mut d, a, _b) = chat_fixture(&ctx);
      let ta = format!("t{a}");
      let (msg, rrx) = send_msg(Some("p1"), &ta, "x", std::time::Instant::now(), Some(0));
      d.handle_ctrl(msg, &ctx);
      let r = rrx.try_recv().expect("settle_ms=0 must reply immediately");
      assert!(r.ok, "{:?}", r.error);
  }
  ```

- [ ] **Step 5: Build**

  ```powershell
  $env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
  Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue
  cargo build 2>&1 | Select-Object -Last 30
  ```

  Expected: `Finished` with no errors.

- [ ] **Step 6: Run all tests**

  ```powershell
  $env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
  cargo test 2>&1 | Select-Object -Last 40
  ```

  Expected: all tests pass, including the two new settle tests and the three updated send tests.

- [ ] **Step 7: Commit**

  ```powershell
  git add src/wm.rs
  git commit -m "feat(wm): wire handle_ctrl::Send to push PendingSettle; settle_ms=0 fast path"
  ```

---

### Task 5: Drive `advance_settles` each frame from `src/main.rs`

The pending list exists and is wired to `handle_ctrl`, but it never drains unless something calls `advance_settles`. The right place is immediately after `self.desktop.show(...)` — by then all sessions have pumped their PTY output for this frame.

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `WindowManager::advance_settles` (Task 3)

- [ ] **Step 1: Add the `advance_settles` call after `show`**

  In `src/main.rs`, inside `impl eframe::App for App`, in the `ui` method, find:

  ```rust
  self.desktop.show(ui, area, true, egui::Id::new("desktop"));
  ```

  Add one line immediately after:

  ```rust
  self.desktop.show(ui, area, true, egui::Id::new("desktop"));
  self.desktop.advance_settles(std::time::Instant::now());
  ```

- [ ] **Step 2: Build**

  ```powershell
  $env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
  Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue
  cargo build 2>&1 | Select-Object -Last 30
  ```

  Expected: `Finished` with no errors.

- [ ] **Step 3: Run all tests (final gate)**

  ```powershell
  $env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
  cargo test 2>&1 | Select-Object -Last 40
  ```

  Expected: all tests pass.

- [ ] **Step 4: Commit**

  ```powershell
  git add src/main.rs
  git commit -m "feat(main): call advance_settles each frame after show()"
  ```

---

### Task 6: Update `docs/terminal-inspection.md`

**Files:**
- Modify: `docs/terminal-inspection.md` (check if exists; update or create)

- [ ] **Step 1: Check if the doc exists**

  ```powershell
  ls "H:\claude code\foreman\docs\terminal-inspection.md" 2>$null
  ```

- [ ] **Step 2: Update or create the doc**

  If it exists, update the "Phase 3" section. If not, create it at `docs/terminal-inspection.md`:

  ```markdown
  # Terminal Inspection

  `foreman send` drives input into a terminal; `foreman snapshot` reads its rendered screen as plain text. Together they close the feedback loop: an agent can send keys, wait for the terminal to settle, then snapshot what's on screen.

  ## What it does

  - `foreman send --text "echo hi\r"` writes raw UTF-8 into a terminal's PTY.
  - `foreman send --keys "F5"` encodes named keys through the same path as live keyboard input (`input::encode_key`).
  - By default, `send` SETTLES before replying: it waits until the terminal has been quiet for ~120ms (no new PTY bytes), then replies `{ok:true}`. This means a following `snapshot` reads settled state.
  - `--settle-ms 0` disables settling (immediate reply). `--settle-ms N` sets a custom window up to 4000ms.
  - `foreman snapshot` reads the rendered viewport as plain text rows (one string per visible row, trailing spaces trimmed) in the `history` field.

  ## Why it exists

  Without this, verifying a terminal change requires screenshotting the GUI window and reading the PNG. There was no closed feedback loop for agents or automated tests. `send` + `snapshot` make the terminal scriptable and inspectable without the GUI.

  ## How settle/wait works (cross-frame, non-blocking)

  The GUI is single-threaded egui; blocking the frame for 120ms would freeze the UI. Instead:

  1. `handle_ctrl` receives a `CtrlMsg::Send`, feeds the bytes into the PTY, then pushes a `PendingSettle` entry onto `WindowManager::pending_settles` and returns immediately.
  2. Each frame, after `show()` (which pumps every session's PTY output), `App::ui` calls `desktop.advance_settles(now)`.
  3. `advance_settles` checks each entry's terminal `output_gen` (a counter bumped every time new PTY bytes arrive in `Session::pump()`). If the gen changed, the silence clock resets. Once the terminal has been quiet for `quiet_window` ms, or the deadline (`MAX_SETTLE_MS = 4000ms`) passes, the reply is sent.

  ## How to use

  ```sh
  # Send text and wait for settle (default ~120ms quiet):
  foreman send --project p1 --terminal t2 --text "echo hi\r"

  # Send a key sequence (F5) then snapshot:
  foreman send --project p1 --terminal t2 --keys "F5"
  foreman snapshot --project p1 --terminal t2

  # Skip settle (fire-and-forget):
  foreman send --project p1 --terminal t2 --text "x" --settle-ms 0
  ```

  ## Non-obvious gotchas

  - `settle_ms` default is 120ms, max is capped at 4000ms (well under the 5s pipe `REPLY_TIMEOUT`). A long settle does NOT block the GUI — it runs across frames.
  - If `settle_ms` is 0, the reply is immediate (sync path, no pending entry).
  - If a terminal closes while settling, the reply is sent `ok:true` and the entry drops.

  ## Key files

  - `src/terminal.rs` — `Session::output_gen()` — the PTY freshness counter
  - `src/wm.rs` — `PendingSettle`, `advance_settles`, `session_gen`, `settle_tick`, `handle_ctrl` Send arm
  - `src/main.rs` — `advance_settles` call after `show()`
  - `src/inspect.rs` — `snapshot_text`, `parse_keys` (pure seam; Phase 1/2)
  - `src/control.rs` — `SendRequest` with `settle_ms` field, `CtrlMsg::Send`
  ```

- [ ] **Step 3: Commit**

  ```powershell
  git add docs/terminal-inspection.md
  git commit -m "docs: terminal-inspection phase 3 settle/wait"
  ```

---

## Self-Review Against Spec

**Spec coverage check:**

| Requirement | Task |
|---|---|
| `output_gen` counter in `Session`, bumped in `pump()` | Task 1 |
| `pub fn output_gen(&self) -> u64` | Task 1 |
| `DEFAULT_SETTLE_MS = 120`, `MAX_SETTLE_MS = 4000` constants | Task 2 |
| `PendingSettle` struct with all required fields | Task 3 |
| `pending_settles: Vec<PendingSettle>` in `WindowManager`, init empty | Task 3 |
| `settle_tick` pure fn with correct logic | Task 2 |
| TDD: 4 `settle_tick` tests (within window, past window, gen reset, past deadline) | Task 2 |
| `session_gen` read-only accessor | Task 3 |
| `advance_settles` with `mem::take` borrow pattern | Task 3 |
| `send_dispatch` returns `Result<(WinId, WinId), String>` | Task 4 |
| `handle_ctrl` Send arm: stale drop → dispatch → settle/immediate branch | Task 4 |
| Existing send tests updated to `settle_ms: Some(0)` | Task 4 |
| New settle-path test: no immediate reply, replies after deadline | Task 4 |
| New settle-ms-zero test: immediate reply | Task 4 |
| `advance_settles` called from `main.rs` after `show()` | Task 5 |
| Doc update | Task 6 |

**Placeholder scan:** No TBDs, no "add validation later", no missing code blocks found.

**Type consistency check:**
- `settle_tick` signature in Task 2 tests uses `super::settle_tick` — the function must be at module level (not inside `impl`), which is specified in Task 2 Step 3. Consistent throughout.
- `session_gen` returns `Option<u64>` — consumed as `Option<u64>` in `advance_settles` Task 3 and in the `handle_ctrl` arm Task 4. Consistent.
- `PendingSettle` field names match across struct definition (Task 3 Step 1), push in `handle_ctrl` (Task 4 Step 2), and use in `advance_settles` (Task 3 Step 5). Consistent.
- `send_msg` helper gains a `settle_ms: Option<u64>` parameter in Task 4 Step 3 — all call sites updated in the same step.
