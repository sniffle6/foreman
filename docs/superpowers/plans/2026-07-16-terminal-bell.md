# Terminal Bell (visual attention pulse) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When a Session's program rings BEL (`\a`), pulse that Session's chrome (win border + its tab chip, or a bare-pane inset ring) caret-amber for ~300 ms so you can find the ringing pane, per `docs/terminal-bell.md`.

**Architecture:** alacritty's `Event::Bell` lands in `Listener` (src/terminal.rs), which records a pulse **deadline** on the Session via a shared `Arc<Mutex<Option<Instant>>>` (same pattern as the OSC `title` slot). Each frame the wm reads `Session::bell_active(now)` to recolor the win border / tab chip / bare content ring; keyboard focus **landing** on the session clears the deadline. A `bell: bool` master switch in `settings.json` gates all painting, plumbed through egui ctx data exactly like `font_size`.

**Tech Stack:** Rust, egui 0.34, alacritty_terminal 0.26, serde. Windows / PowerShell / GNU toolchain.

**Spec:** `docs/terminal-bell.md` (the grill output — implement against it, NOT the epic's combined title+bell "done when").

## Global Constraints

- Toolchain is **stable-gnu** (never MSVC); linker is w64devkit. See `CLAUDE.md` for the `libgcc_eh.a` stub fix if linking breaks.
- Before every build: kill only the **target-built** foreman by exe path (`Get-Process foreman -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$PWD\target\*" } | Stop-Process -Force; Start-Sleep -Milliseconds 500`). If `$env:FOREMAN` is `1` you are INSIDE foreman: never Stop-Process; build with `cargo build --target-dir target/agent` instead.
- **Never** route Bell through the `Event::PtyWrite` flush path (it latches Ready). Bell is a sibling arm of `Event::Title` / `Event::ColorRequest`. Never introduce `VoidListener` anywhere.
- Pulse color is the **caret amber family** RGB `(231, 169, 63)` — never the near-white focus-ladder colors (`BORDER_FOCUS`/`TEXT`).
- Pulse duration: **300 ms** from the **last** ring. A BEL mid-pulse **restarts** the deadline (one continuous pulse). Cancel only on the unfocused→focused **transition**; hover never cancels; BEL on an already-focused Session still pulses.
- Settings key is exactly `"bell"`, default `true` via the existing `#[serde(default)]` on `Settings` (missing key = on). File-only — **no** settings-UI checkbox, **no** leader chord.
- Out of scope (do not build): sound, OS toasts, task-manager-panel highlight, project-level bubble-up, sticky unread badges, OSC tab titles.
- Commit per task, message style `type(scope): subject` (see `git log`), ending with:
  `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`

## File Structure

| File | Change |
|------|--------|
| `src/config.rs` | `Settings.bell: bool` (+ default `true`, + test) |
| `src/theme.rs` | `pub const BELL` — caret amber at full alpha (CARET's 130 alpha is a block-fill tuning, too weak for a 1 px stroke) |
| `src/terminal.rs` | `BELL_PULSE` const; `Listener.bell` slot + `Event::Bell` arm; `Session.bell` / `was_active` fields; `bell_active` / `clear_bell` / `ring_bell_for_test`; focus-gain cancel in `show`/`keepalive`; `bell_enabled`/`set_bell_enabled` ctx accessors (mirrors `font_size`) |
| `src/wm.rs` | `Content::bell_active` + `Win::bell_active` helpers; border recolor; ringing tab-chip stroke; bare-pane inset ring; `request_repaint_after` while pulsing |
| `src/main.rs` | one line: publish `settings.bell` into ctx each frame next to `set_font_size` |
| `docs/terminal-bell.md` | flip status to built, tick acceptance |

---

### Task 1: `Settings.bell` master switch

**Files:**
- Modify: `src/config.rs` (struct `Settings` ~line 93, `impl Default` ~line 105, `mod tests` at bottom)

**Interfaces:**
- Consumes: nothing.
- Produces: `config::Settings.bell: bool` (public field, default `true`) — read by Task 4's main.rs wiring.

- [ ] **Step 1: Write the failing test** — append inside `mod tests` in `src/config.rs`:

```rust
    #[test]
    fn bell_defaults_on_and_round_trips() {
        // Missing key = on (the #[serde(default)] contract for new fields).
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert!(s.bell, "missing bell key must mean on");
        // Explicit false parses and survives a round trip.
        let s: Settings = serde_json::from_str(r#"{"bell": false}"#).unwrap();
        assert!(!s.bell);
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert!(!back.bell);
    }
```

- [ ] **Step 2: Run it — expect failure**

Run: `cargo test --lib config 2>&1 | Select-Object -Last 15`
Expected: compile error `no field `bell` on type `Settings``.

- [ ] **Step 3: Implement** — in `struct Settings`, after the `panel_dock` field:

```rust
    /// Master switch for Bell attention (the visual pulse; any later sound or
    /// push notification must honor the same key). File-only in v1 — no
    /// settings UI, no leader chord. Missing key = on.
    pub bell: bool,
```

and in `impl Default for Settings`, after `panel_dock: crate::wm::Dir::Right,`:

```rust
            bell: true,
```

- [ ] **Step 4: Run test — expect pass**

Run: `cargo test --lib config 2>&1 | Select-Object -Last 15`
Expected: all config tests PASS (including the three pre-existing ones).

- [ ] **Step 5: Commit**

```powershell
git add src/config.rs
git commit -m "feat(config): bell master switch in settings.json (default on)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: BEL → pulse deadline on the Session

**Files:**
- Modify: `src/terminal.rs` — `Listener` struct (~line 214), its `send_event` match (~line 222), `Session` struct (~line 525), the spawn wiring (~lines 851–900), the `Listener` literal in the existing test `listener_answers_color_request_into_the_pty_buffer` (~line 3059), plus new methods near `set_osc_title_for_test` (~line 763).

**Interfaces:**
- Consumes: nothing.
- Produces (used by Tasks 3–4):
  - `pub const BELL_PULSE: std::time::Duration` (300 ms)
  - `Session::bell_active(&self, now: std::time::Instant) -> bool`
  - `Session::clear_bell(&self)`
  - `#[cfg(test)] Session::ring_bell_for_test(&self, deadline: std::time::Instant)`

- [ ] **Step 1: Write the failing test** — append inside `mod tests` in `src/terminal.rs`, next to `listener_answers_color_request_into_the_pty_buffer`:

```rust
    #[test]
    fn listener_bell_sets_and_restarts_the_pulse_deadline() {
        let bell = Arc::new(Mutex::new(None));
        let l = Listener {
            out: Arc::new(Mutex::new(Vec::new())),
            title: Arc::new(Mutex::new(None)),
            bell: bell.clone(),
        };
        let before = std::time::Instant::now();
        l.send_event(Event::Bell);
        let first = bell.lock().unwrap().expect("BEL must set a pulse deadline");
        assert!(first > before, "deadline must be in the future");
        assert!(
            first <= std::time::Instant::now() + BELL_PULSE,
            "deadline must be ~now + BELL_PULSE, not unbounded"
        );
        // Spam restarts the window (one continuous pulse) — it never drops a ring.
        std::thread::sleep(std::time::Duration::from_millis(5));
        l.send_event(Event::Bell);
        let second = bell.lock().unwrap().expect("still pulsing");
        assert!(second >= first, "a mid-pulse BEL must extend, never shorten");
    }
```

- [ ] **Step 2: Run it — expect failure**

Run: `cargo test --lib terminal::tests::listener_bell 2>&1 | Select-Object -Last 15`
Expected: compile error — `Listener` has no field `bell`, `BELL_PULSE` not found.

- [ ] **Step 3: Implement**

(a) Near the top of `src/terminal.rs` (beside other pub consts):

```rust
/// How long a Bell pulse lasts after the most recent ring. A new BEL while
/// pulsing restarts this window — one continuous pulse, not a disco.
pub const BELL_PULSE: std::time::Duration = std::time::Duration::from_millis(300);
```

(b) Add the field to `Listener` (after `title`):

```rust
    /// Visual Bell (`\a`) pulse deadline: `Some(t)` = pulse until `t`. Shared
    /// with the Session, which paints it and cancels it on focus gain.
    bell: Arc<Mutex<Option<std::time::Instant>>>,
```

(c) Add the arm in `send_event`, after the `Event::ColorRequest` arm and before `_ => {}` (sibling of title/color — NOT the PtyWrite/Ready path):

```rust
            // BEL: record an attention-pulse deadline on the Session. Restart
            // (not drop) on re-ring so spam reads as one continuous pulse.
            Event::Bell => {
                if let Ok(mut b) = self.bell.lock() {
                    *b = Some(std::time::Instant::now() + BELL_PULSE);
                }
            }
```

(d) `Session` struct — after the `osc_title` field:

```rust
    /// Visual Bell pulse deadline (shared with the `Listener`).
    bell: Arc<Mutex<Option<std::time::Instant>>>,
```

(e) Spawn wiring (~line 851): create the slot beside `osc_title`, hand it to the `Listener`, store it on the `Session`:

```rust
        let resp = Arc::new(Mutex::new(Vec::new()));
        let osc_title = Arc::new(Mutex::new(None));
        let bell = Arc::new(Mutex::new(None));
        let term = Term::new(
            Config::default(),
            &Size { cols, rows },
            Listener {
                out: resp.clone(),
                title: osc_title.clone(),
                bell: bell.clone(),
            },
        );
```

and in the `Ok(Session { … })` literal, after `osc_title,`: add `bell,`.

(f) Methods on `Session`, next to `set_osc_title_for_test` (~line 763):

```rust
    /// Whether the Bell pulse is live at `now` (deadline still in the future).
    pub fn bell_active(&self, now: std::time::Instant) -> bool {
        self.bell.lock().ok().and_then(|b| *b).is_some_and(|d| d > now)
    }

    /// End the Bell pulse early (keyboard focus landed on this session).
    pub fn clear_bell(&self) {
        if let Ok(mut b) = self.bell.lock() {
            *b = None;
        }
    }

    /// Test hook: force a pulse deadline without parsing a real BEL (wm paint
    /// helpers). Production rings only via the `Listener`.
    #[cfg(test)]
    pub fn ring_bell_for_test(&self, deadline: std::time::Instant) {
        if let Ok(mut b) = self.bell.lock() {
            *b = Some(deadline);
        }
    }
```

(g) Fix the pre-existing `Listener` literal in `listener_answers_color_request_into_the_pty_buffer` (~line 3059) — add `bell: Arc::new(Mutex::new(None)),`. Grep for any other `Listener {` literal and do the same.

- [ ] **Step 4: Run tests — expect pass**

Run: `cargo test --lib terminal::tests::listener 2>&1 | Select-Object -Last 15`
Expected: both listener tests PASS. Then `cargo build 2>&1 | Select-Object -Last 5` — clean (catches any missed `Listener`/`Session` literal).

- [ ] **Step 5: Commit**

```powershell
git add src/terminal.rs
git commit -m "feat(terminal): record BEL as a 300ms session pulse deadline

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: cancel the pulse when keyboard focus lands

**Files:**
- Modify: `src/terminal.rs` — `Session` struct (add `was_active`), `Session::show` (~line 1636), `Session::keepalive` (~line 1625), spawn literal, new pure fn + test.

**Interfaces:**
- Consumes: `Session::clear_bell()` (Task 2).
- Produces: behavior only — `Session::show(ui, rect, active, resp)` clears the pulse exactly on the `active` false→true transition. (`fn bell_cancelled_by_focus(was_active: bool, active: bool) -> bool` is private.)

Background: in `wm.rs`, `is_focus` ANDs down the project tree so exactly one leaf terminal gets `show(active=true)` per frame; hidden tabs get `keepalive()` instead of `show`. Cancel must fire only when focus **lands** — a BEL on an already-focused session must still pulse (spec "Cancel" rule).

- [ ] **Step 1: Write the failing test** — in `src/terminal.rs` `mod tests`:

```rust
    #[test]
    fn bell_cancels_only_on_gaining_focus() {
        // unfocused → focused: the user found the pane; kill the pulse.
        assert!(bell_cancelled_by_focus(false, true));
        // already focused: a BEL must still do its short pulse.
        assert!(!bell_cancelled_by_focus(true, true));
        // staying (or going) unfocused never cancels.
        assert!(!bell_cancelled_by_focus(false, false));
        assert!(!bell_cancelled_by_focus(true, false));
    }
```

- [ ] **Step 2: Run it — expect failure**

Run: `cargo test --lib terminal::tests::bell_cancels 2>&1 | Select-Object -Last 15`
Expected: compile error `cannot find function `bell_cancelled_by_focus``.

- [ ] **Step 3: Implement**

(a) Pure helper (free fn near `BELL_PULSE`):

```rust
/// Focus-transition rule for Bell cancel (pure, unit-tested): the pulse dies
/// when keyboard focus *lands* on the session. Holding focus never cancels —
/// a BEL on an already-focused session still pulses. Hover is not focus.
fn bell_cancelled_by_focus(was_active: bool, active: bool) -> bool {
    active && !was_active
}
```

(b) `Session` field, after `bell`:

```rust
    /// Keyboard focus last frame — Bell cancels on the false→true transition.
    was_active: bool,
```

and `was_active: false,` in the spawn `Ok(Session { … })` literal after `bell,`.

(c) Top of `Session::show`, before the `let font_px = font_size(ui.ctx());` line:

```rust
        if bell_cancelled_by_focus(self.was_active, active) {
            self.clear_bell();
        }
        self.was_active = active;
```

(d) In `Session::keepalive` (hidden/minimized tabs never run `show`, so they must read as unfocused), after `self.cancel_all_mouse_captures();`:

```rust
        self.was_active = false;
```

- [ ] **Step 4: Run tests — expect pass**

Run: `cargo test --lib terminal::tests::bell 2>&1 | Select-Object -Last 15`
Expected: `listener_bell_sets_and_restarts_the_pulse_deadline` and `bell_cancels_only_on_gaining_focus` PASS.

- [ ] **Step 5: Commit**

```powershell
git add src/terminal.rs
git commit -m "feat(terminal): cancel bell pulse when keyboard focus lands

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: paint the pulse (border, tab chips, bare ring) behind the settings gate

**Files:**
- Modify: `src/theme.rs` (~line 55, beside `CARET`)
- Modify: `src/terminal.rs` (beside `font_size`/`set_font_size`, ~line 630)
- Modify: `src/main.rs` (~line 537, beside `set_font_size`)
- Modify: `src/wm.rs` — `impl Content` (~line 127), `impl Win`, bare branch (~line 3391), chips branch (~line 3737), border paint (~line 4165), `mod tests`

**Interfaces:**
- Consumes: `Session::bell_active(now)`, `ring_bell_for_test`, `clear_bell` (Tasks 2–3); `Settings.bell` (Task 1).
- Produces:
  - `theme::BELL: egui::Color32`
  - `terminal::bell_enabled(ctx: &egui::Context) -> bool` / `terminal::set_bell_enabled(ctx: &egui::Context, on: bool)`
  - `Content::bell_active(&self, now: std::time::Instant) -> bool` and `Win::bell_active(&self, now: std::time::Instant) -> bool` (private to wm.rs)

- [ ] **Step 1: Write the failing wm test** — in `src/wm.rs` `mod tests` (same spawn pattern as `terminal_shells_lists_one_pair_per_terminal_tab`; state-only, no pump needed — bell is only set by the parser or the test hook):

```rust
    #[test]
    fn bell_pulses_the_stack_until_expiry_or_clear() {
        let ctx = egui::Context::default();
        let mut m = WindowManager::new();
        let env: Vec<(String, String)> = vec![];
        let s1 = Session::spawn(Shell::Cmd, None, &env, ctx.clone()).unwrap();
        let s2 = Session::spawn(Shell::Cmd, None, &env, ctx.clone()).unwrap();
        let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0));
        m.push_win(1, Tab::fixed("front", Content::Terminal(s1)), r);
        m.windows[0].tabs.push(Tab::fixed("back", Content::Terminal(s2)));

        let now = std::time::Instant::now();
        assert!(!m.windows[0].bell_active(now), "fresh sessions must not pulse");

        // Ring the background tab: the whole stack (border rule) pulses, but
        // only that tab's content does (chip rule).
        let Content::Terminal(s) = &m.windows[0].tabs[1].content else {
            panic!("expected terminal");
        };
        s.ring_bell_for_test(now + std::time::Duration::from_millis(300));
        assert!(m.windows[0].bell_active(now));
        assert!(m.windows[0].tabs[1].content.bell_active(now));
        assert!(!m.windows[0].tabs[0].content.bell_active(now));

        // Past the deadline the pulse is over — no sticky badge.
        let later = now + std::time::Duration::from_millis(301);
        assert!(!m.windows[0].bell_active(later));

        // Clearing (focus landed) kills it immediately.
        s.ring_bell_for_test(now + std::time::Duration::from_millis(300));
        s.clear_bell();
        assert!(!m.windows[0].bell_active(now));
    }
```

- [ ] **Step 2: Run it — expect failure**

Run: `cargo test --lib wm::tests::bell_pulses 2>&1 | Select-Object -Last 15`
Expected: compile error — no method `bell_active` on `Win` / `Content`.

- [ ] **Step 3: Implement the wm helpers** — in `impl Content` (beside `icon_kind`):

```rust
    /// Whether this content is a terminal with a live Bell pulse. Projects do
    /// not bubble up (project/panel Bell chrome is explicitly out of v1 scope).
    fn bell_active(&self, now: std::time::Instant) -> bool {
        matches!(self, Content::Terminal(s) if s.bell_active(now))
    }
```

and in `impl Win`:

```rust
    /// Border pulse rule: the whole stack pulses while ANY of its tabs rings.
    fn bell_active(&self, now: std::time::Instant) -> bool {
        self.tabs.iter().any(|t| t.content.bell_active(now))
    }
```

Run: `cargo test --lib wm::tests::bell_pulses 2>&1 | Select-Object -Last 15` — expect PASS.

- [ ] **Step 4: Theme token** — in `src/theme.rs`, directly under `CARET`:

```rust
/// Bell attention pulse — the caret amber family at full alpha (CARET's 130
/// alpha is tuned for a block fill; a 1px border stroke needs full strength).
/// Never the focus ladder: Bell is amber and temporary, focus is near-white.
pub const BELL: egui::Color32 = egui::Color32::from_rgb(231, 169, 63);
```

- [ ] **Step 5: Settings gate plumbing** — in `src/terminal.rs`, mirror the `font_size` ctx-data accessors (same shape as `font_size_id`/`FontSizeState` directly above):

```rust
#[derive(Clone, Copy)]
struct BellEnabledState(bool);

fn bell_enabled_id() -> egui::Id {
    egui::Id::new("foreman-bell-enabled")
}

/// Master Bell switch (settings.json `bell`), published per-frame by App.
/// Unset (tests, headless) = on — matching the settings default.
pub fn bell_enabled(ctx: &egui::Context) -> bool {
    ctx.data_mut(|d| d.get_temp::<BellEnabledState>(bell_enabled_id()))
        .map(|s| s.0)
        .unwrap_or(true)
}

/// Publish the persisted Bell switch as the live value paint paths read.
pub fn set_bell_enabled(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(bell_enabled_id(), BellEnabledState(on)));
}
```

In `src/main.rs`, directly after `terminal::set_font_size(&ctx, self.settings.font_size);` (~line 537):

```rust
        terminal::set_bell_enabled(&ctx, self.settings.bell);
```

- [ ] **Step 6: Border pulse** — in `src/wm.rs` at the `// --- border + resize ---` block (~line 4165), replace:

```rust
            let border_col = if is_focus {
                if is_project {
                    PROJ_BORDER_FOCUS
                } else {
                    BORDER_FOCUS
                }
            } else {
                BORDER
            };
```

with:

```rust
            // Bell: while ANY tab in this stack rings, the border flashes caret
            // amber — temporary attention routing that outranks the focus color
            // for the pulse (a focused session that rings still pulses). The
            // repaint_after keeps the tail of the pulse from outliving its
            // deadline on the idle 100ms cadence.
            let bell_on = crate::terminal::bell_enabled(ui.ctx())
                && self.windows[i].bell_active(std::time::Instant::now());
            let border_col = if bell_on {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(30));
                BELL
            } else if is_focus {
                if is_project {
                    PROJ_BORDER_FOCUS
                } else {
                    BORDER_FOCUS
                }
            } else {
                BORDER
            };
```

- [ ] **Step 7: Tab-chip pulse** — in the `HeaderContentLayout::Tabs { chips }` branch (~line 3737), add just inside it, before `for ch in chips {`:

```rust
                    let bell_now = std::time::Instant::now();
                    let bell_gate = crate::terminal::bell_enabled(ui.ctx());
```

then inside the loop, directly after the `if is_active_tab && is_project { … }` border block (after its closing brace, before `let txt_col = …`):

```rust
                        // Bell: only the ringing session's chip pulses (the
                        // whole-stack border pulse is painted at the frame).
                        if bell_gate && self.windows[i].tabs[ti].content.bell_active(bell_now) {
                            p.rect_stroke(
                                chip,
                                radius,
                                egui::Stroke::new(BORDER_W, BELL),
                                egui::StrokeKind::Inside,
                            );
                        }
```

- [ ] **Step 8: Bare-pane inset ring** — a bare sole pane draws no chrome, so it needs the content-rect fallback. In the `if bare { … }` branch (~line 3391), after the `if child_interacted { … }` block and before `continue;`:

```rust
                // Bare sole pane has no border or chips — the Bell falls back
                // to an inset ring on the content rect (the only surface that
                // doesn't invent chrome).
                if crate::terminal::bell_enabled(ui.ctx())
                    && self.windows[i].bell_active(std::time::Instant::now())
                {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(30));
                    ui.painter_at(scr.intersect(area)).rect_stroke(
                        scr.shrink(1.0),
                        egui::CornerRadius::ZERO,
                        egui::Stroke::new(2.0, BELL),
                        egui::StrokeKind::Inside,
                    );
                }
```

- [ ] **Step 9: Build + full test gate**

```powershell
Get-Process foreman -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$PWD\target\*" } | Stop-Process -Force; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 20
cargo test 2>&1 | Select-Object -Last 20
```

Expected: clean build (no new warnings), all tests PASS — the pre-existing Ready/DSR/title/color-request tests are the regression fence for the Listener change.

- [ ] **Step 10: Commit**

```powershell
git add src/theme.rs src/terminal.rs src/main.rs src/wm.rs
git commit -m "feat(wm): paint bell pulse on border, tab chips, and bare panes

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: visual acceptance, regression sweep, docs flip

**Files:**
- Modify: `docs/terminal-bell.md` (status line + acceptance checkboxes)
- Modify: `docs/epics/terminal-completeness-epic.md` (Phase 6 note: bell half shipped, title half still open)

**Interfaces:**
- Consumes: the built feature (Tasks 1–4). Produces: evidence + updated docs.

- [ ] **Step 1: Launch the built app** (skip the kill if `$env:FOREMAN` is `1` — then ask the user to run it instead):

```powershell
Get-Process foreman -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$PWD\target\*" } | Stop-Process -Force; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 5
Start-Process .\target\debug\foreman.exe
```

- [ ] **Step 2: Drive the acceptance checklist from `docs/terminal-bell.md`** — verify each with a screenshot (script in `docs/HANDOFF.md` § 3; `Read` the PNG). To ring an **unfocused** pane without touching the user's keyboard, use the headless control plane: `foreman send <terminal-id> "printf \"`a\"" --enter` from inside a foreman terminal, or open two terminals and type `printf "`a"` in one, then focus the other before it fires (e.g. `sleep 2; printf "`a"`).

  - [ ] `printf "`a"` in an unfocused Session pulses its border amber (tab chip too if stacked)
  - [ ] Background tab rings → that chip + the border pulse; other chips do not
  - [ ] Bare lone tile (single terminal, no chrome) shows the inset ring
  - [ ] Pulse lasts ~300 ms; `printf "`a`a"` with a pause mid-pulse restarts it (one continuous pulse)
  - [ ] Clicking the ringing Session cancels the pulse early; hovering does not
  - [ ] A focused Session that rings still pulses briefly
  - [ ] `"bell": false` in `%APPDATA%\foreman\settings.json` (edit, restart) → no pulse anywhere; delete the key → on again
  - [ ] Ring a minimized window's session → no crash; restore within 300 ms shows the tail, after → nothing
  - [ ] Startup shells still prompt (Ready/DSR untouched), tab icons still detect agents (title path untouched)

- [ ] **Step 3: Full regression sweep**

Run: `cargo test 2>&1 | Select-Object -Last 10`
Expected: PASS across layout/wm/chat/terminal.

- [ ] **Step 4: Flip the docs.** In `docs/terminal-bell.md`: change the status line to `**Status (2026-07-16): built.**` (keep the design-grill provenance sentence), tick the acceptance boxes verified in Step 2, and correct the "Key files (planned)" heading to "Key files". In `docs/epics/terminal-completeness-epic.md` Phase 6: note Bell shipped per `docs/terminal-bell.md`; OSC titles remain open. No CONTEXT.md change (the Bell glossary entry already exists).

- [ ] **Step 5: Commit**

```powershell
git add docs/terminal-bell.md docs/epics/terminal-completeness-epic.md
git commit -m "docs(bell): mark terminal bell built; record acceptance

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```
