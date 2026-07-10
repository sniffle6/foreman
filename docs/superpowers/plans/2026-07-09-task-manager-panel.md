# Task-Manager Panel Implementation Plan

> **For agentic workers:** Execute this plan with the **hybrid-plan-execution** protocol — see the "Execution Protocol" section below, which is self-contained in case this session lacks the skill. Mechanics: superpowers:subagent-driven-development for dispatch + two-stage review, superpowers:executing-plans for inline tasks, superpowers:verification-before-completion before claiming done. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A desktop-level side-panel window listing every project and its terminal/chat tabs with open/minimized state, click-to-focus, and hover minimize/close — fully replacing the minimize chip taskbars.

**Architecture:** Two deep modules behind small seams, then a shallow view. Read seam: `WindowManager::panel_model()` returns a plain-data snapshot of the whole tree. Write seam: `surface_target(TargetPath)` hides the restore/tab-switch/raise/focus dance behind one method, exposed via new path-carrying `Act` variants in the existing deferred-action queue (one apply loop). The panel itself is `Content::TaskManager(PanelView)` — a real window in the desktop tiling tree (right-edge root split, rail collapse), non-closable/non-minimizable/non-tabbable. The chat crew board's click is retrofitted onto the same write seam so it has two adapters on day one.

**Tech Stack:** Rust, egui 0.34 (immediate mode), existing `wm.rs` window engine + `layout.rs` tiling tree. No new dependencies (adding one is a change-control gate — don't).

**Spec:** `docs/superpowers/specs/2026-07-09-task-manager-panel-design.md` — read it first.

## Global Constraints

- Windows/PowerShell, GNU toolchain. Kill the app before building: `Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500` (else `Access is denied (os error 5)`).
- Build/test: `cargo build 2>&1 | Select-Object -Last 20`; `cargo test --lib wm` / `--lib panel` / `--lib layout` / `--lib keymap`.
- Never use `VoidListener` for a real `Session`; tests here never spawn Sessions at all — use the existing wm test helpers (`push`, `mgr_with_project`) which build windows without PTYs.
- All mutation from panel rows flows through the existing deferred-`Act` queue. Do NOT mutate `self.windows` while iterating content draws.
- Commit style: `type(scope): subject`, trailer `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`. Commit after each task.
- egui 0.34: `App::ui(&mut Ui, ...)`; paint via `ui.painter()`; interactions via `ui.interact(rect, id, Sense::click())` (see `paint_taskbar` wm.rs:3577 for the pattern being replaced).
- Colors come from `src/theme.rs` consts (glob-imported in wm.rs: `WIN_BG`, `BORDER`, `DIM`, etc.). No new hex literals in wm.rs/panel.rs — add tokens to theme.rs if needed.

---

## Execution Protocol (hybrid-plan-execution)

This plan is executed with the **hybrid-plan-execution** discipline. If the
executing session does not have that skill, this section is a self-contained
copy of its rules — follow it as written.

**One classifier — risk — drives three decisions per task: where it executes,
whether it gets reviewed, and which model runs it. Risk is blast radius times
verifiability, not lines changed.** Effort and diff size appear nowhere in the
classifier.

Two different gates, never interchangeable:
- **Verification** = machine evidence the change works (tests, screenshots,
  build). Never skipped, any tier.
- **Review** = fresh judgment that it's the *right* change. Scales with tier.
  The author re-reading their own diff is not review.

Tiers:
- **M — Mechanical** (ALL must hold): plan supplies exact content (execution is
  transcription, zero new decisions); an automated check in the same batch
  exercises THIS change; git-reversible. Executes inline or batched (≤4,
  adjacent, same tier); no review (the check is the review); cheap model
  allowed.
- **S — Standard**: judgment at execution time; blast radius contained to the
  repo; automated verification written into the task. Fresh subagent; one
  review pass (spec compliance); inherit the session model. Groupable with
  other S only.
- **C — Critical** (ANY suffices): destructive/irreversible ops · contract
  surfaces · code deletion (a foreman-change-control gate) · behavioral change
  whose only verification is manual. Dedicated subagent; **two-stage review**
  (spec compliance, then code quality); session model or stronger — never
  downgrade. Never grouped. Destructive/interactive ops stay in the main
  session, run last, gated on all prior verification.

**Forbidden combination:** cheap model + no review + no automated check
exercising the change. Per-task commits survive grouping. The **full test
suite** runs before claiming done, regardless of which targeted checks ran
along the way. Do not reclassify your own task downward at execution time.

### Tier assignments for this plan

| Task | Tier | Where | Review | Notes |
|---|---|---|---|---|
| 1 panel_model read seam | S | subagent (may group with 2) | spec compliance | signature adaptation = judgment, tests included |
| 2 surface_target + Acts | S | subagent (may group with 1) | spec compliance | tightly coupled to Task 1's types |
| 3 crew-click retrofit | S | fresh subagent | spec compliance | modifies existing behavior; regression net = existing chat tests |
| 4 panel window + flags | S | fresh subagent | spec compliance | touches `deserted()` (app-quit) — tests cover it; if tests can't be made to cover it, escalate to C |
| 5 row UI + drains | S | fresh subagent | spec compliance | data flow unit-tested; pixels verified by screenshot in the same task |
| 6 collapse/keymap/persistence | S | fresh subagent | spec compliance | config compat test required |
| 7 delete chip taskbars | **C** | dedicated subagent | **two-stage** | code deletion gate + minimize-reachability blast radius; manual check is part of verification |
| 8 acid pass + docs | main session | run **last**, inline | n/a | interactive GUI verification, gated on Tasks 1-7 all green and committed |

Rationalizations to refuse (verbatim from the skill): "it's one line" (size
isn't in the classifier) · "a review adds nothing the test doesn't" (on C they
catch different failure classes) · "risk is covered by the other task's gate"
(borrowed verification — M2 fails) · "skip the full sweep to save time"
(targeted during, full sweep at the end, always).

---

### Task 1: `TargetPath` + `PanelModel` types and the read seam `panel_model()` — **Tier S**

**Files:**
- Create: `src/panel.rs` (plain-data model types only — no egui, no wm imports beyond `WinId`)
- Modify: `src/main.rs` (add `mod panel;` beside the other mods)
- Modify: `src/wm.rs` (add `panel_model()` on `WindowManager`; tests in the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `WinId` (public in wm.rs), `Win { tabs, active, minimized }`, `Content::{Terminal,Project,Chat}`, `Content::icon_kind()` (wm.rs:465), `WindowManager { windows, focused }`.
- Produces (later tasks rely on these exact names):
  - `panel::TargetPath { project: WinId, window: Option<WinId>, tab: Option<usize> }`
  - `panel::PanelModel { projects: Vec<ProjectEntry> }`
  - `panel::ProjectEntry { path, title, minimized, focused, tabs: Vec<TabEntry> }`
  - `panel::TabEntry { path, title, kind: RowKind, minimized, active_tab, focused, exited }`
  - `panel::RowKind::{Terminal(crate::icons::IconKind), Chat}`
  - `WindowManager::panel_model(&self) -> panel::PanelModel`

- [x] **Step 1: Write `src/panel.rs` with the model types**

```rust
//! Task-manager panel: plain-data model types (the read seam's vocabulary).
//! Pure data by design — built by `WindowManager::panel_model()`, rendered by
//! the panel view, addressed by the path-carrying `Act` variants. No egui here.

use crate::wm::WinId;

/// Address of a row: a desktop-level project window, optionally a child
/// window inside its nested manager, optionally a tab index within that.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TargetPath {
    pub project: WinId,
    pub window: Option<WinId>,
    pub tab: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowKind {
    Terminal(crate::icons::IconKind),
    Chat,
}

#[derive(Clone, Debug)]
pub struct TabEntry {
    pub path: TargetPath,
    pub title: String,
    pub kind: RowKind,
    /// The child *window* holding this tab is minimized inside its project.
    pub minimized: bool,
    /// This tab is its window's active tab (background tabs render dimmer).
    pub active_tab: bool,
    /// This tab is the focused leaf of the focused project.
    pub focused: bool,
    /// Terminal child process has exited (chat rows: always false).
    pub exited: bool,
}

#[derive(Clone, Debug)]
pub struct ProjectEntry {
    pub path: TargetPath,
    pub title: String,
    pub minimized: bool,
    pub focused: bool,
    pub tabs: Vec<TabEntry>,
}

#[derive(Clone, Debug, Default)]
pub struct PanelModel {
    pub projects: Vec<ProjectEntry>,
}
```

Add `mod panel;` to `src/main.rs` next to the other module declarations. Make `WinId` public in wm.rs if it isn't already (check its declaration; it is a type alias near the top).

- [ ] **Step 2: Write the failing tests in wm.rs's test module**

Follow the existing helper style (`push(wm, title)` at wm.rs:4378 builds windows without PTYs; `mgr_with_project` builds a desktop with a project). A project window with terminal tabs is constructed the way existing tab tests do it — copy the construction from a nearby tab test rather than inventing one.

```rust
#[test]
fn panel_model_groups_tabs_under_projects_with_state_flags() {
    // Desktop with one project window holding a nested manager of two
    // windows: window A (two terminal-less tabs standing in for terminals)
    // and window B (minimized).
    let mut desk = WindowManager::new();
    let proj = push(&mut desk, "projA");
    // make it a project: swap content for a nested manager
    let mut inner = WindowManager::new();
    let a = push(&mut inner, "termA");
    inner.windows[0].tabs.push(Tab { title: "termA2".into(),
        content: Content::Chat(crate::chat::ChatView::new(
            std::rc::Rc::new(std::cell::RefCell::new(crate::chat::ChatRoom::new())))) });
    let b = push(&mut inner, "termB");
    inner.windows.iter_mut().find(|w| w.id == b).unwrap().minimized = true;
    inner.focus(a);
    desk.windows[0].tabs[0].content = Content::Project(Box::new(inner));
    desk.focus(proj);

    let m = desk.panel_model();
    assert_eq!(m.projects.len(), 1);
    let p = &m.projects[0];
    assert_eq!(p.title, "projA");
    assert!(p.focused && !p.minimized);
    assert_eq!(p.path, crate::panel::TargetPath { project: proj, window: None, tab: None });
    // window A contributes 2 tab rows (active first tab + background chat tab),
    // window B contributes 1 row flagged minimized.
    assert_eq!(p.tabs.len(), 3);
    assert!(p.tabs[0].active_tab && !p.tabs[0].minimized);
    assert!(matches!(p.tabs[1].kind, crate::panel::RowKind::Chat));
    assert!(!p.tabs[1].active_tab);
    let bt = p.tabs.iter().find(|t| t.path.window == Some(b)).unwrap();
    assert!(bt.minimized);
}

#[test]
fn panel_model_skips_the_panel_window_itself() {
    // Once Task 4 adds the panel window, the model must not list it as a
    // project. Until then this documents the contract on non-project desktop
    // windows: only Content::Project windows produce ProjectEntry rows.
    let mut desk = WindowManager::new();
    push(&mut desk, "not-a-project"); // plain content, not Content::Project
    assert!(desk.panel_model().projects.is_empty());
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --lib wm::tests::panel_model 2>&1 | Select-Object -Last 15`
Expected: compile error — `panel_model` not found.

- [ ] **Step 4: Implement `panel_model()` on `WindowManager` (wm.rs, near `deserted()`)**

```rust
/// Pure snapshot of the whole tree for the task-manager panel (read seam).
/// Desktop-level: one ProjectEntry per Content::Project window, tab rows from
/// its nested manager in window order then tab order. Cheap (dozens of rows);
/// rebuilt each frame by the desktop show().
pub fn panel_model(&self) -> crate::panel::PanelModel {
    use crate::panel::*;
    let mut projects = Vec::new();
    for w in &self.windows {
        // Every *project tab* of every desktop window is a project row.
        for (pi, pt) in w.tabs.iter().enumerate() {
            let Content::Project(inner) = &pt.content else { continue };
            let ppath = TargetPath { project: w.id, window: None, tab: None };
            let pfocused = self.focused == Some(w.id) && w.active == pi;
            let mut tabs = Vec::new();
            for cw in &inner.windows {
                for (ti, t) in cw.tabs.iter().enumerate() {
                    let kind = match &t.content {
                        Content::Terminal(s) => RowKind::Terminal(s.icon_kind()),
                        Content::Chat(_) => RowKind::Chat,
                        Content::Project(_) => continue, // no nested projects today
                        Content::TaskManager(_) => continue, // Task 4+; unreachable inside projects
                    };
                    tabs.push(TabEntry {
                        path: TargetPath { project: w.id, window: Some(cw.id), tab: Some(ti) },
                        title: t.title.clone(),
                        kind,
                        minimized: cw.minimized,
                        active_tab: cw.active == ti,
                        focused: pfocused
                            && inner.focused == Some(cw.id)
                            && cw.active == ti,
                        exited: match &t.content {
                            Content::Terminal(s) => s.exited().is_some(),
                            _ => false,
                        },
                    });
                }
            }
            projects.push(ProjectEntry {
                path: ppath,
                title: pt.title.clone(),
                minimized: w.minimized,
                focused: pfocused,
                tabs,
            });
        }
    }
    PanelModel { projects }
}
```

Note: the `Content::TaskManager` arm doesn't exist yet — omit it in this task; Task 4's compiler errors will tell you exactly where to add arms (that is intentional: `Content` matches are exhaustive).
Check the real signature of `Session::exited()` before using it (grep `fn exited` in terminal.rs) — if it returns `Option<u32>` use `.is_some()`, if `bool` use it directly.

- [ ] **Step 5: Run tests to verify pass**

Run: `cargo test --lib wm::tests::panel_model 2>&1 | Select-Object -Last 15`
Expected: 2 passed.

- [ ] **Step 6: Commit**

```
git add src/panel.rs src/main.rs src/wm.rs
git commit -m "feat(panel): TargetPath + PanelModel read seam (panel_model)"
```

---

### Task 2: Write seam — `surface_target()` + path-carrying `Act` variants — **Tier S**

**Files:**
- Modify: `src/wm.rs` — `Act` enum (line ~525), the `Act` apply loop (search `Act::Min(id) => self.minimize(id)` ~wm.rs:3677), new method `surface_target` near `drain_chat_clicks` (~1484); tests in the wm test module.

**Interfaces:**
- Consumes: `panel::TargetPath` (Task 1), `WindowManager::focus(WinId)`, `Win::{minimized, active}`, `focused_child()` pattern (wm.rs:1861).
- Produces:
  - `Act::FocusPath(crate::panel::TargetPath)`
  - `Act::MinPath(crate::panel::TargetPath)`
  - `Act::ClosePath(crate::panel::TargetPath)`
  - `WindowManager::surface_target(&mut self, path: crate::panel::TargetPath)` — restore project → restore child window → switch active tab → focus cascade. No-ops on stale ids.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn surface_target_restores_and_focuses_across_levels() {
    // minimized project containing a minimized child window with a background tab
    let mut desk = WindowManager::new();
    let other = push(&mut desk, "other");
    let proj = push(&mut desk, "proj");
    let mut inner = WindowManager::new();
    let cw = push(&mut inner, "t1");
    inner.windows[0].tabs.push(Tab { title: "t2".into(),
        content: Content::Chat(crate::chat::ChatView::new(
            std::rc::Rc::new(std::cell::RefCell::new(crate::chat::ChatRoom::new())))) });
    inner.windows[0].minimized = true;
    desk.windows.iter_mut().find(|w| w.id == proj).unwrap().tabs[0].content =
        Content::Project(Box::new(inner));
    desk.windows.iter_mut().find(|w| w.id == proj).unwrap().minimized = true;
    desk.focus(other);

    desk.surface_target(crate::panel::TargetPath {
        project: proj, window: Some(cw), tab: Some(1),
    });

    let pw = desk.windows.iter().find(|w| w.id == proj).unwrap();
    assert!(!pw.minimized, "project must be restored");
    assert_eq!(desk.focused_id(), Some(proj), "project must take desktop focus");
    let Content::Project(inner) = &pw.tabs[0].content else { panic!() };
    let c = inner.windows.iter().find(|w| w.id == cw).unwrap();
    assert!(!c.minimized, "child window must be restored");
    assert_eq!(c.active, 1, "background tab must become active");
    assert_eq!(inner.focused_id(), Some(cw), "child must take inner focus");
}

#[test]
fn surface_target_is_a_noop_on_stale_paths() {
    let mut desk = WindowManager::new();
    let a = push(&mut desk, "a");
    desk.focus(a);
    desk.surface_target(crate::panel::TargetPath { project: 999, window: None, tab: None });
    assert_eq!(desk.focused_id(), Some(a), "stale path must change nothing");
}
```

If `focused` has no public getter, add `pub fn focused_id(&self) -> Option<WinId> { self.focused }` (check first — a getter or equivalent may exist; tests elsewhere read focus somehow — copy their approach).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib wm::tests::surface_target 2>&1 | Select-Object -Last 15`
Expected: compile error — method not found.

- [ ] **Step 3: Implement**

```rust
/// Make the addressed target visible and focused (write seam): restore the
/// project window, restore the child window, switch its active tab, and run
/// the focus cascade. Stale ids no-op silently (same staleness family as
/// drain_chat_clicks). Both the panel and the chat crew board route here.
pub fn surface_target(&mut self, path: crate::panel::TargetPath) {
    let Some(pw) = self.windows.iter_mut().find(|w| w.id == path.project) else { return };
    pw.minimized = false;
    // Activate the project tab if the project content sits on a background tab.
    if let Some(pi) = pw.tabs.iter().position(|t| matches!(t.content, Content::Project(_))) {
        pw.active = pi;
    }
    if let (Some(wid), Content::Project(inner)) =
        (path.window, &mut pw.tabs[pw.active].content)
    {
        if let Some(cw) = inner.windows.iter_mut().find(|w| w.id == wid) {
            cw.minimized = false;
            if let Some(t) = path.tab {
                if t < cw.tabs.len() {
                    cw.active = t;
                }
            }
            inner.focus(wid);
        }
    }
    self.focus(path.project);
}
```

Add the three `Act` variants and their apply arms (in the same match as `Act::Min` / `Act::Restore`, wm.rs ~3677):

```rust
Act::FocusPath(p) => self.surface_target(p),
Act::MinPath(p) => { /* Task 5 fills row-minimize; for now route project-level */ }
```

**Do not leave that comment in code.** Implement `MinPath`/`ClosePath` fully now — they are small:

```rust
Act::MinPath(p) => match p.window {
    None => self.minimize(p.project),
    Some(wid) => {
        if let Some(pw) = self.windows.iter_mut().find(|w| w.id == p.project)
            && let Content::Project(inner) = &mut pw.tabs[pw.active].content
        {
            inner.minimize(wid);
        }
    }
},
Act::ClosePath(p) => match p.window {
    None => self.close(p.project),
    Some(wid) => {
        if let Some(pw) = self.windows.iter_mut().find(|w| w.id == p.project)
            && let Content::Project(inner) = &mut pw.tabs[pw.active].content
        {
            match p.tab {
                Some(t) if inner.windows.iter().any(|w| w.id == wid && w.tabs.len() > 1) =>
                    inner.close_tab(wid, t),
                _ => inner.close(wid),
            }
        }
    }
},
```

Check the real names: `minimize(id)` exists (wm.rs:1913); `close(id)` exists; the tab-close path is whatever `Act::CloseTab` dispatches to — reuse exactly that method so the running-session confirm flow is preserved. If `close`/`close_tab` route through a pending-confirm mechanism, `ClosePath` inherits it for free — that is required behavior (spec: "close routes through the existing close path").

- [ ] **Step 4: Add modal-freeze coverage**

Find the existing modal-freeze test (`wm.rs:~7054`, minimize dropped under a modal) and add the path variants to it (or a sibling test): with a modal open, `Act::MinPath`/`Act::ClosePath`/`Act::FocusPath` must be dropped the same way `Act::Min` is. Mirror however that test injects acts.

- [ ] **Step 5: Run tests**

Run: `cargo test --lib wm 2>&1 | Select-Object -Last 10`
Expected: all pass, including the two new + modal test.

- [ ] **Step 6: Commit**

```
git add src/wm.rs
git commit -m "feat(panel): surface_target write seam + FocusPath/MinPath/ClosePath acts"
```

---

### Task 3: Retrofit the chat crew click onto the write seam — **Tier S**

**Files:**
- Modify: `src/wm.rs` — `drain_chat_clicks` (wm.rs:1484-1519).

**Interfaces:**
- Consumes: `surface_target` (Task 2). Behavior is unchanged from the outside.

- [ ] **Step 1: Confirm existing coverage**

Run: `cargo test --lib wm 2>&1 | Select-Object -Last 10` — note the crew/chat click tests that pass (search the test module for `drain_chat_clicks` / crew click tests). These are the regression net; no new test needed unless none exists — if none exists, write one first using the Task 2 test style: a chat view with `view.click = Some("t1".into())` on a desktop project, assert the terminal's window gets focus.

- [ ] **Step 2: Rewrite the resolution to produce a `TargetPath`**

`drain_chat_clicks` currently searches only `self.windows` — but note it runs on the manager that owns the chat viewer (the *project* manager). Verify where it is called from (grep `drain_chat_clicks(`). The retrofit keeps resolution local but replaces the hand-rolled surface block (lines 1511-1517):

```rust
if let Some((win, tab)) = hit {
    // Local-level surface: this manager owns both viewer and terminal.
    self.surface_target(crate::panel::TargetPath {
        project: win, window: None, tab: Some(tab),
    });
}
```

Wait — `surface_target` treats `project` as a *desktop* window and expects `Content::Project` for tab switching. Inside a project manager the hit window holds terminal tabs, not project tabs. So `surface_target` as written in Task 2 must handle both shapes. Change `surface_target`'s project-tab-activation to only apply when a `Content::Project` tab exists, and honor `path.tab` on the top-level window when `path.window` is `None`:

```rust
// in surface_target, replace the project-tab activation block with:
if path.window.is_none() {
    if let Some(t) = path.tab {
        if t < pw.tabs.len() { pw.active = t; }
    }
} else if let Some(pi) = pw.tabs.iter().position(|t| matches!(t.content, Content::Project(_))) {
    pw.active = pi;
}
```

Update Task 2's tests if this changes observable behavior (it shouldn't — project rows never carry `tab` today).
Also fix `open_chat_window` (wm.rs:1449-1477): replace its hand-rolled unminimize/tab-switch/focus block (lines 1455-1467) with `surface_target` on the found window.

- [ ] **Step 3: Run the full wm suite**

Run: `cargo test --lib wm 2>&1 | Select-Object -Last 10`
Expected: all pass — crew click behavior identical through the new seam.

- [ ] **Step 4: Commit**

```
git add src/wm.rs
git commit -m "refactor(chat): crew click + open_chat_window route through surface_target"
```

---

### Task 4: `Content::TaskManager` + panel window creation, flags, and `deserted()` exclusion — **Tier S** (escalate to C if `deserted()` can't be test-covered)

**Files:**
- Modify: `src/panel.rs` — add `PanelView`.
- Modify: `src/wm.rs` — `Content` enum + its `show`/`keepalive`/`icon_kind` matches; `ensure_panel()`; guards in `close`, `minimize`, merge/tab paths; `deserted()`.
- Modify: `src/layout.rs` — `set_root_ratio` helper + test.
- Modify: `src/main.rs` — call `ensure_panel()` on the desktop at startup.

**Interfaces:**
- Consumes: `PanelModel` (Task 1), `LayoutTree::insert_root(id, Dir)` (layout.rs:85).
- Produces:
  - `panel::PanelView { pub model: PanelModel, pub collapsed: bool, pub expanded_width: f32, pub scroll: f32, pub click: Option<TargetPath>, pub hover_act: Option<(TargetPath, PanelBtn)>, pub toggle_collapse: bool }`
  - `panel::PanelBtn::{Min, Close}`
  - `Content::TaskManager(crate::panel::PanelView)`
  - `WindowManager::ensure_panel(&mut self)` — desktop-only, idempotent.
  - `Win::is_panel(&self) -> bool`
  - `layout::LayoutTree::set_root_ratio(&mut self, ratio: f32)`
  - Constants in panel.rs: `pub const PANEL_W: f32 = 260.0; pub const RAIL_W: f32 = 36.0;`

- [ ] **Step 1: Write failing tests**

In wm.rs tests:

```rust
#[test]
fn ensure_panel_is_idempotent_and_tiled_right() {
    let mut desk = WindowManager::new();
    desk.ensure_panel();
    desk.ensure_panel();
    let panels: Vec<_> = desk.windows.iter().filter(|w| w.is_panel()).collect();
    assert_eq!(panels.len(), 1);
    assert!(desk.tree_contains(panels[0].id), "panel starts tiled");
}

#[test]
fn deserted_ignores_the_panel() {
    let mut desk = WindowManager::new();
    desk.ensure_panel();
    assert!(desk.deserted(), "a lone panel must not hold the app alive");
    let p = push(&mut desk, "proj");
    assert!(!desk.deserted());
    desk.close(p);
    assert!(desk.deserted());
}

#[test]
fn panel_refuses_close_and_minimize() {
    let mut desk = WindowManager::new();
    desk.ensure_panel();
    let id = desk.windows.iter().find(|w| w.is_panel()).unwrap().id;
    desk.close(id);
    desk.minimize(id);
    let w = desk.windows.iter().find(|w| w.id == id).unwrap();
    assert!(!w.minimized);
    assert_eq!(desk.windows.iter().filter(|w| w.is_panel()).count(), 1);
}

#[test]
fn panel_refuses_merge_as_source_and_target() {
    // Mirror the shape of merge_target_skips_minimized_windows (wm.rs:4608):
    // a titlebar drop over the panel must yield no merge target, and the
    // panel must never be offered as a drag source for tabbing.
    // Copy that test's setup, replacing the minimized window with the panel.
}
```

`tree_contains` — if no such accessor exists, use whatever existing tiling tests assert with (`tree.contains(id)` is public on LayoutTree per layout.rs:58; expose the tree or add a thin `pub fn tiled(&self, id) -> bool`). Follow existing test precedent (search tests for `.tree.`).

In layout.rs tests:

```rust
#[test]
fn set_root_ratio_moves_the_root_divider() {
    let mut t = LayoutTree::default();
    t.insert_root(1, Dir::Left);
    t.insert_root(2, Dir::Right);
    t.set_root_ratio(0.8);
    let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 500.0));
    let rects = t.layout(area, 0.0);
    let r2 = rects.iter().find(|(id, _)| *id == 2).unwrap().1;
    assert!((r2.width() - 200.0).abs() < 1.0);
}
```

(Adjust to the real `Node`/ratio representation — read `layout.rs` `Node` enum and `layout()` first; the helper sets the ratio field of the root `Node::Split`. If the root is a leaf, no-op.)

- [ ] **Step 2: Run to verify failures** — `cargo test --lib 2>&1 | Select-Object -Last 15`, expect compile errors.

- [ ] **Step 3: Implement**

panel.rs additions:

```rust
pub const PANEL_W: f32 = 260.0;
pub const RAIL_W: f32 = 36.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PanelBtn { Min, Close }

/// Per-window view state for the task-manager panel (the shallow view).
/// `model` is stashed by the desktop each frame before the draw pass;
/// `click`/`hover_act`/`toggle_collapse` are drained after it (chat pattern).
pub struct PanelView {
    pub model: PanelModel,
    pub collapsed: bool,
    pub expanded_width: f32,
    pub scroll: f32,
    pub click: Option<TargetPath>,
    pub hover_act: Option<(TargetPath, PanelBtn)>,
    pub toggle_collapse: bool,
}

impl PanelView {
    pub fn new(collapsed: bool, expanded_width: f32) -> Self {
        Self { model: PanelModel::default(), collapsed, expanded_width,
               scroll: 0.0, click: None, hover_act: None, toggle_collapse: false }
    }
}
```

wm.rs:
- `Content::TaskManager(crate::panel::PanelView)` variant. Compiler now lists every non-exhaustive match — fill them: `keepalive` → `{}`; `icon_kind` → `None` (or a dedicated icon later); `Content::show` → Task 5 (for now paint `WIN_BG` fill only and return `false`); Task 1's `panel_model` gets its `continue` arm.
- `Win::is_panel(&self) -> bool { self.tabs.iter().any(|t| matches!(t.content, Content::TaskManager(_))) }`
- `ensure_panel`:

```rust
/// Desktop-only, idempotent: create the task-manager panel window as a
/// right-edge root split if none exists. Called by main.rs at startup.
pub fn ensure_panel(&mut self) {
    if self.windows.iter().any(|w| w.is_panel()) { return; }
    let id = self.next;
    self.next += 1;
    self.z += 1;
    self.push_win(
        id,
        "panel".into(),
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(crate::panel::PANEL_W, 400.0)),
        Content::TaskManager(crate::panel::PanelView::new(false, crate::panel::PANEL_W)),
    );
    self.tree.insert_root(id, Dir::Right);
    // Root ratio so the panel takes ~PANEL_W of the current area; re-applied
    // on show once the real area is known (Task 6 owns collapse ratios).
}
```

Check how `push_win` interacts with the tree in existing creation paths (grep callers of `insert_root`) — new windows may auto-tile elsewhere; mirror that exact sequence so focus/z bookkeeping matches.
- Guards: first line of `close(&mut self, id)` and `minimize(&mut self, id)`: `if self.windows.iter().any(|w| w.id == id && w.is_panel()) { return; }`. Also guard the header buttons in Task 5 (don't draw them) and the context-menu items (wm.rs ~3324 `("Minimize", Act::Min(id))` — skip for panel windows).
- Merge/tab exclusion: in `merge_target` (wm.rs ~2447-2530) skip `w.is_panel()` windows exactly like `w.minimized`; in the `Act::Merge` apply arm, bail if src or dst is a panel; the tab-drag source can't produce multi-tab panels if merge is blocked, and `Untab` on a 1-tab window is already a no-op family — verify by reading the `Untab` arm.
- `deserted()` (wm.rs:1932): change `self.windows.is_empty()` to `self.windows.iter().all(|w| w.is_panel())`.
- main.rs: after the desktop `WindowManager` is constructed (find it in the App constructor), call `desktop.ensure_panel();`.

- [ ] **Step 4: Run all tests** — `cargo test 2>&1 | Select-Object -Last 10`. Expected: pass. Pay attention to previously-green tests that counted desktop windows or asserted `deserted()` — if any now fail because the panel exists, those tests construct the desktop via `WindowManager::new()` (no panel) so they should be unaffected; only tests calling `ensure_panel` see it.

- [ ] **Step 5: Build + run + screenshot** — panel appears as an empty right-edge pane; app still quits when last project closes.

- [ ] **Step 6: Commit**

```
git add src/panel.rs src/wm.rs src/layout.rs src/main.rs
git commit -m "feat(panel): Content::TaskManager window - creation, flags, deserted exclusion"
```

---

### Task 5: Panel UI — rows, click-to-focus, hover minimize/close — **Tier S**

**Files:**
- Modify: `src/wm.rs` — `Content::show` TaskManager arm; snapshot stash + `drain_panel_acts()` in the desktop `show()`; theme tokens if needed in `src/theme.rs`.

**Interfaces:**
- Consumes: `PanelView` fields (Task 4), `Act::{FocusPath,MinPath,ClosePath}` (Task 2), crew-board row painting style (wm.rs:148-230) as the visual template.
- Produces: rendered rows; `drain_panel_acts()` turning `view.click`/`view.hover_act` into `Act`s.

- [ ] **Step 1: Wire data flow first (testable without pixels)**

In the desktop's `show()` (wm.rs:2551), before the window render loop: build the model once and stash it into the panel view; after the loop (where `drain_chat_clicks`/`drain_chat_posts` run — find their call site), drain panel interactions:

```rust
// pre-loop (desktop only — nested managers have no panel):
let model = self.panel_model();
for w in &mut self.windows {
    for t in &mut w.tabs {
        if let Content::TaskManager(v) = &mut t.content {
            v.model = model.clone();
        }
    }
}
```

```rust
/// Drain panel-row interactions recorded during the draw into deferred Acts
/// (chat-click pattern: content cannot mutate siblings mid-loop).
fn drain_panel_acts(&mut self, acts: &mut Vec<Act>) {
    for w in &mut self.windows {
        for t in &mut w.tabs {
            if let Content::TaskManager(v) = &mut t.content {
                if let Some(p) = v.click.take() {
                    acts.push(Act::FocusPath(p));
                }
                if let Some((p, b)) = v.hover_act.take() {
                    acts.push(match b {
                        crate::panel::PanelBtn::Min => Act::MinPath(p),
                        crate::panel::PanelBtn::Close => Act::ClosePath(p),
                    });
                }
            }
        }
    }
}
```

Call it where the other drains run so the acts join the same apply pass. Unit test: set `view.click = Some(path)` on a constructed panel, run the drain, assert `surface_target`-visible effects after applying acts (reuse how existing tests drive the act loop — search for tests calling `apply_acts` or the drains directly).

- [ ] **Step 2: Render the expanded body in the `Content::show` TaskManager arm**

Visual template is the crew board (wm.rs:148-230) and `paint_taskbar` interaction pattern (`ui.interact(rect, base.with(..), Sense::click())`). Structure:

```rust
Content::TaskManager(view) => {
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0.0, WIN_BG);
    let row_h = 22.0;
    let mut y = rect.min.y + 6.0 - view.scroll;
    for proj in &view.model.projects {
        // project row
        let row = egui::Rect::from_min_size(
            egui::pos2(rect.min.x + 4.0, y), egui::vec2(rect.width() - 8.0, row_h));
        let resp = ui.interact(row, base.with(("panelrow", proj.path.project)),
                               egui::Sense::click());
        let col = if proj.focused { FOCUS } else if proj.minimized { DIM } else { TEXT };
        p.text(egui::pos2(row.min.x + 18.0, row.center().y),
               egui::Align2::LEFT_CENTER, &proj.title,
               egui::FontId::proportional(12.5), col);
        if resp.clicked() { view.click = Some(proj.path); }
        paint_row_buttons(ui, &p, row, resp.hovered(), proj.path,
                          proj.minimized, base, view);
        y += row_h;
        // tab rows, indented
        for t in &proj.tabs {
            let row = egui::Rect::from_min_size(
                egui::pos2(rect.min.x + 18.0, y), egui::vec2(rect.width() - 22.0, row_h));
            let rid = base.with(("panelrow", proj.path.project, t.path.window, t.path.tab));
            let resp = ui.interact(row, rid, egui::Sense::click());
            // icon: reuse the tab-strip icon painting for RowKind::Terminal(kind)
            // (see icon usage around wm.rs:2901-3132); Chat gets the chat glyph.
            let col = if t.focused { FOCUS }
                      else if t.exited || t.minimized { DIM }
                      else if !t.active_tab { DIM } else { TEXT };
            p.text(egui::pos2(row.min.x + 18.0, row.center().y),
                   egui::Align2::LEFT_CENTER, &t.title,
                   egui::FontId::proportional(12.0), col);
            if resp.clicked() { view.click = Some(t.path); }
            paint_row_buttons(ui, &p, row, resp.hovered(), t.path,
                              t.minimized, base, view);
            y += row_h;
        }
        y += 4.0;
    }
    // wheel scroll within bounds (copy the chat scroll_step usage, wm.rs:329-336)
    false
}
```

`paint_row_buttons` is a free helper in wm.rs: when `hovered`, draws two ~14px glyph buttons right-aligned in the row (`–`/`▢` for min/restore by `minimized`, `×` for close), each an `ui.interact` click that sets `view.hover_act = Some((path, PanelBtn::...))`. Use theme consts; exact glyph/spacing free to match crew-board aesthetics. Exact colors: reuse `FOCUS`/`TEXT`/`DIM`/`BORDER` if those names exist in theme.rs — check and substitute the real token names (grep `pub const` in theme.rs).

- [ ] **Step 3: Run tests + build + screenshot** — `cargo test 2>&1 | Select-Object -Last 10` then run the app with 2 projects and several terminals; screenshot; verify grouping, minimized dimming, click-to-focus restores a minimized window, hover buttons minimize/close (close of a live terminal must show the existing confirm dialog).

- [ ] **Step 4: Commit**

```
git add src/wm.rs src/theme.rs
git commit -m "feat(panel): row rendering, click-to-focus, hover min/close"
```

---

### Task 6: Collapse rail, keymap command, persistence — **Tier S**

**Files:**
- Modify: `src/wm.rs` (header collapse button on the panel window; rail rendering; ratio application), `src/keymap.rs` (`Command::ToggleTaskManager`), `src/config.rs` (Settings fields), `src/main.rs` (seed + persist), `src/layout.rs` (already has `set_root_ratio` from Task 4).

**Interfaces:**
- Consumes: `PanelView::{collapsed, expanded_width, toggle_collapse}`, `set_root_ratio`, `config::Settings`.
- Produces: `Command::ToggleTaskManager`; `Settings { panel_collapsed: bool, panel_width: f32 }`.

- [ ] **Step 1: Keymap tests + command**

Add `ToggleTaskManager` to `Command` (keymap.rs:19-52), to `ALL` (under Actions), `group()` → `Group::Actions`, `label()` → `"Toggle task panel"`, and a default chord in `Keymap::default` (pick a letter unused by the defaults — read the default table, `m` if free). Existing keymap tests assert ALL/label/group exhaustiveness — run `cargo test --lib keymap` first to see what fails and let those tests drive. The merge-over-defaults design gives user files the new chord automatically (test exists; verify it covers a *new* command — if yes, nothing to add).

- [ ] **Step 2: Dispatch**

In `dispatch` (wm.rs:1792), `Command::ToggleTaskManager` (desktop level): find the panel view and flip `collapsed`; when tiled, apply the ratio:

```rust
Command::ToggleTaskManager => {
    let area_w = self.area.width(); // check the real field/method for the manager's area
    for w in &mut self.windows {
        for t in &mut w.tabs {
            if let Content::TaskManager(v) = &mut t.content {
                v.collapsed = !v.collapsed;
            }
        }
    }
    self.apply_panel_ratio(area_w);
}
```

`apply_panel_ratio(&mut self, area_w: f32)`: if the panel is tiled (`self.tree` contains its id), `self.tree.set_root_ratio(1.0 - (target_w / area_w))` where `target_w` = `RAIL_W` when collapsed else `expanded_width`; if floating, set the float rect width to `target_w` instead. Call it also once per frame in the desktop `show()` *only when* `toggle_collapse` was consumed (not every frame — dividers stay user-draggable). Header button: in the titlebar painting for panel windows, draw a `«`/`»` toggle at the right end (same interact pattern as existing header buttons); clicking sets `view.toggle_collapse = true`; the drain converts it into the same flip + ratio application as the keymap path (one code path — extract `fn toggle_panel(&mut self)` used by both).
Divider drag on the panel edge while expanded should update `expanded_width`: after the layout pass, read the panel's tiled rect width and, if not collapsed and it differs, store it into `view.expanded_width` (this also captures float resizes).

- [ ] **Step 3: Rail rendering**

In the TaskManager `show` arm: if `view.collapsed`, render one icon per project stacked (24px squares, centered), tooltip = title (`resp.on_hover_text(&proj.title)`), click → `view.click = Some(proj.path)`; skip tab rows and hover buttons entirely.

- [ ] **Step 4: Persistence**

config.rs:

```rust
pub struct Settings {
    pub font_size: f32,
    pub panel_collapsed: bool,
    pub panel_width: f32,
}
impl Default for Settings {
    fn default() -> Self {
        Self { font_size: DEFAULT_FONT_SIZE, panel_collapsed: false,
               panel_width: crate::panel::PANEL_W }
    }
}
```

Extend the existing `missing_fields_fall_back_to_defaults` test with the two new fields. main.rs: seed `ensure_panel`'s `PanelView::new(settings.panel_collapsed, settings.panel_width)` from the loaded settings (change `ensure_panel` to take the two values as params); persist with the same debounce pattern the font zoom uses (main.rs:46-69): each frame read the live values off the panel view (add a small `WindowManager::panel_prefs(&self) -> Option<(bool, f32)>` getter), compare to last-saved, debounce-write.

- [ ] **Step 5: Run tests, build, screenshot collapsed rail + expanded; restart app to verify persistence.**

- [ ] **Step 6: Commit**

```
git add src/wm.rs src/keymap.rs src/config.rs src/main.rs src/panel.rs
git commit -m "feat(panel): rail collapse, ToggleTaskManager chord, persisted state"
```

---

### Task 7: Delete the chip taskbars — **Tier C** (code deletion + minimize-reachability blast radius; two-stage review)

**Files:**
- Modify: `src/wm.rs` — remove `paint_taskbar` (wm.rs:3576-3614) and its call (wm.rs:3498). `Act::Restore` stays (still used by tests/paths — verify remaining users with grep; if only the taskbar used it, keep the variant anyway this task and note it, since `Restore` semantics are exercised in tests like wm.rs:5286).

- [ ] **Step 1: Grep for dependents** — `paint_taskbar`, `"task"` widget-id salt, and any test that minimizes then asserts chip behavior. Update/delete only tests that specifically assert chips; tests asserting minimize/restore *semantics* must keep passing.
- [ ] **Step 2: Delete fn + call site. Build. Run full `cargo test`.**
- [ ] **Step 3: Manual check:** minimize a terminal and a project → both disappear from view, both listed dimmed in the panel, click restores. Nothing renders at the bottom-left.
- [ ] **Step 4: Commit**

```
git add src/wm.rs
git commit -m "feat(panel): remove minimize chip taskbars - panel is the restore surface"
```

---

### Task 8: Verification pass + docs — **main session, run last** (interactive GUI verification, gated on Tasks 1-7)

- [ ] **Step 1: Full suite** — `cargo test 2>&1 | Select-Object -Last 10` (all green).
- [ ] **Step 2: Visual acid pass (release build)** — 2+ projects, 4+ terminals incl. a chat viewer and a tab-stack: screenshot (a) expanded panel with focused/minimized/background rows distinguishable, (b) collapsed rail with tooltips, (c) hover buttons, (d) close-with-confirm from a row, (e) panel torn out to floating and re-tiled, (f) leader-WASD navigation into/out of the panel, (g) app quits on last project close with panel open.
- [ ] **Step 3: Docs** — update `CLAUDE.md` architecture list (one line for `src/panel.rs` + the taskbar replacement note in the wm.rs line) and `docs/HANDOFF.md` if it references the taskbar. Spec status header → implemented.
- [ ] **Step 4: Commit**

```
git add CLAUDE.md docs/
git commit -m "docs(panel): task-manager panel shipped - update architecture notes"
```

---

## Self-review notes (already applied)

- Spec coverage: decisions §all → Tasks 1-7; testing section → per-task steps + Task 8; out-of-scope respected (no badges, no drag-from-panel, no context menus).
- Type consistency: `TargetPath`/`PanelModel`/`PanelView`/`PanelBtn`/`surface_target`/`ensure_panel`/`drain_panel_acts`/`set_root_ratio` used with the same names throughout.
- Known judgment points left to the implementer *deliberately, with the decision rule stated inline*: real names of theme tokens, `Session::exited()` signature, act-loop injection style in tests, `Node` ratio representation — each step says exactly where to look.
