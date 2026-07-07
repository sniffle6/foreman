# Close-Confirm for Running Subprocesses — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Before a terminal or project close (or app quit) destroys a shell that still has real child processes, pop a modal listing them as Name │ Pid and require confirmation.

**Architecture:** Five deep, in-process modules along clean seams. `proc::descendants` turns a PID into a flat descendant list (pure core `collect_descendants` is the test surface). `confirm::ConfirmClose` is a self-contained modal view. `wm` gains a pure tree-walk (`terminal_shells`) that feeds grouping, a `request_close_*` gate in front of the existing close funnels, and a pure `resolve_pending` decision split off from the egui render so the state machine is testable. `main.rs` guards the OS window-close. Every interactive close routes through the gate; the programmatic `foreman close` paths stay direct and un-gated.

**Tech Stack:** Rust, egui 0.34 (`eframe`), `sysinfo` (process table, already a dep), `alacritty_terminal`, portable-pty/ConPTY. Colors from `src/theme.rs`.

**Design reference (approved):** `docs/superpowers/specs/2026-07-06-close-confirm-running-subprocess-design.md` + mockup `docs/superpowers/specs/2026-07-06-close-confirm-mockup.html`.

## Global Constraints

Every task's steps implicitly include these.

- **Toolchain / build:** Windows GNU (`rustup default stable-gnu`), w64devkit linker. **Kill the app before every build** or the link fails with `os error 5`:
  `Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500`
- **Bin crate — no `--lib` target.** Run tests with `cargo test` (optionally `cargo test <module>::` to scope). `cargo test --lib` fails.
- **Colors come from `theme.rs` only** (`TEXT`, `DIM`, `CARET`, …). No hardcoded steel/orange — the steel+orange re-theme is a separate effort; this modal inherits whatever `theme.rs` holds.
- **Never gate the programmatic paths.** `foreman close` (`close_dispatch`) and the dispatch-undo `close_terminal` keep calling `close`/`close_tab`/`close_active_tab` directly — no modal.
- **Trigger denylist** excludes only `openconsole.exe` and `conhost.exe` (case-insensitive) from the descendant list.
- **Commit trailer** (end every commit message with):
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`
- **Edits to `.rs` files go through Serena** (get_symbols_overview → find_symbol → replace_symbol_body / insert_after_symbol). Built-in Edit only for non-code.

---

### Task 1: `proc::descendants` — the descendant-enumeration seam

**Files:**
- Modify: `src/proc.rs` (add `ProcInfo`, `collect_descendants`, `descendants`; new tests)

**Interfaces:**
- Consumes: existing private `ProcRow { pid, parent, name, cmd }`, `descends_from`, the `SCANNER` thread-local, `REFRESH_EVERY`.
- Produces: `pub struct ProcInfo { pub pid: u32, pub name: String }` and `pub fn descendants(root_pid: u32) -> Vec<ProcInfo>`.

- [ ] **Step 1: Write the failing tests** (append to `src/proc.rs`'s `mod tests`, reusing the existing `row()` helper):

```rust
#[test]
fn descendants_lists_direct_child() {
    let t = vec![
        row(100, 1, "powershell.exe", &["powershell"]),
        row(200, 100, "claude.exe", &["claude"]),
    ];
    let d = collect_descendants(&t, 100);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].pid, 200);
    assert_eq!(d[0].name, "claude.exe");
}

#[test]
fn descendants_lists_grandchild() {
    let t = vec![
        row(100, 1, "powershell.exe", &["powershell"]),
        row(200, 100, "claude.exe", &["claude"]),
        row(300, 200, "rg.exe", &["rg", "foo"]),
    ];
    let pids: Vec<u32> = collect_descendants(&t, 100).iter().map(|p| p.pid).collect();
    assert!(pids.contains(&300), "grandchild not listed");
}

#[test]
fn descendants_exclude_console_host_plumbing() {
    let t = vec![
        row(100, 1, "powershell.exe", &["powershell"]),
        row(200, 100, "OpenConsole.exe", &["OpenConsole"]),
        row(210, 100, "conhost.exe", &["conhost"]),
        row(300, 100, "node.exe", &["node"]),
    ];
    let names: Vec<&str> = collect_descendants(&t, 100).iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["node.exe"], "host plumbing leaked into the list");
}

#[test]
fn descendants_never_include_the_shell_itself() {
    let t = vec![row(100, 1, "powershell.exe", &["powershell"])];
    assert!(collect_descendants(&t, 100).is_empty(), "idle shell should have no descendants");
}

#[test]
fn descendants_do_not_leak_across_shells() {
    let t = vec![
        row(100, 1, "powershell.exe", &["powershell"]),
        row(200, 100, "claude.exe", &["claude"]),
        row(500, 1, "powershell.exe", &["powershell"]),
    ];
    assert!(collect_descendants(&t, 500).is_empty(), "another shell's child leaked in");
}
```

- [ ] **Step 2: Run the tests, verify they fail to compile**

Run: `cargo test proc::tests::descendants`
Expected: FAIL — `cannot find function collect_descendants` / `ProcInfo`.

- [ ] **Step 3: Implement `ProcInfo`, `collect_descendants`, `descendants`** (insert after `detect_agent` / before the `Scanner` struct):

```rust
/// One live descendant process, for the close-confirm list. Plain data so the
/// selection logic is unit-tested with synthetic tables.
pub struct ProcInfo {
    pub pid: u32,
    pub name: String,
}

/// Console-host plumbing ConPTY spawns around a shell — never real user work.
const HOST_PLUMBING: &[&str] = &["openconsole.exe", "conhost.exe"];

/// Pure: descendants of `root` worth warning about before a kill — every process
/// under `root` except `root` itself and console-host plumbing. The test surface.
fn collect_descendants(table: &[ProcRow], root: u32) -> Vec<ProcInfo> {
    table
        .iter()
        .filter(|r| r.pid != root)
        .filter(|r| descends_from(table, r.pid, root))
        .filter(|r| !HOST_PLUMBING.contains(&r.name.to_ascii_lowercase().as_str()))
        .map(|r| ProcInfo { pid: r.pid, name: r.name.clone() })
        .collect()
}

/// Live descendants of `root_pid`. Throttled through the same scanner as
/// `agent_for`; returns an empty list for an idle shell.
pub fn descendants(root_pid: u32) -> Vec<ProcInfo> {
    SCANNER.with(|s| {
        let mut s = s.borrow_mut();
        let stale = s.last_refresh.is_none_or(|t| t.elapsed() >= REFRESH_EVERY);
        if stale {
            s.refresh();
        }
        collect_descendants(&s.table, root_pid)
    })
}
```

- [ ] **Step 4: Run the tests, verify they pass**

Run: `cargo test proc::tests::descendants`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add src/proc.rs
git commit -m "feat(proc): descendants() — live child processes under a shell PID"
```

---

### Task 2: `confirm::ConfirmClose` — the modal view module

**Files:**
- Create: `src/confirm.rs`
- Modify: `src/main.rs` (add `mod confirm;` beside the other `mod` declarations)

**Interfaces:**
- Consumes: `crate::proc::ProcInfo` (Task 1); `theme` tokens.
- Produces:
  - `pub enum ConfirmOutcome { Pending, Cancelled, Confirmed }`
  - `pub struct ProcGroup { pub label: String, pub scope: Option<String>, pub procs: Vec<ProcInfo> }`
  - `pub struct ConfirmClose` with `new(title, lead, confirm_label, groups)`, `total() -> usize`, `show(&mut self, ui, area) -> ConfirmOutcome`.

- [ ] **Step 1: Create `src/confirm.rs` with the failing test first**

```rust
//! The close-confirm modal: a self-contained view over an already-grouped list
//! of doomed processes. Knows nothing about window ids or how a close is
//! performed — the owner (wm.rs / main.rs) maps the outcome to an action.

use crate::proc::ProcInfo;
use crate::theme::*;
use eframe::egui;

pub enum ConfirmOutcome {
    Pending,
    Cancelled,
    Confirmed,
}

/// One labelled cluster of doomed processes. `scope` is the optional dim suffix
/// on the header ("3 terminals" in the quit variant); None otherwise.
pub struct ProcGroup {
    pub label: String,
    pub scope: Option<String>,
    pub procs: Vec<ProcInfo>,
}

pub struct ConfirmClose {
    title: String,
    lead: String,
    confirm_label: String,
    groups: Vec<ProcGroup>,
}

impl ConfirmClose {
    pub fn new(
        title: impl Into<String>,
        lead: impl Into<String>,
        confirm_label: impl Into<String>,
        groups: Vec<ProcGroup>,
    ) -> Self {
        Self {
            title: title.into(),
            lead: lead.into(),
            confirm_label: confirm_label.into(),
            groups,
        }
    }

    /// Total processes across all groups.
    pub fn total(&self) -> usize {
        self.groups.iter().map(|g| g.procs.len()).sum()
    }

    /// True once there is more than one group — render terminal-name headers and
    /// indent the processes under them. A single group renders flat.
    fn grouped(&self) -> bool {
        self.groups.len() > 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grp(label: &str, pids: &[u32]) -> ProcGroup {
        ProcGroup {
            label: label.into(),
            scope: None,
            procs: pids.iter().map(|&pid| ProcInfo { pid, name: format!("p{pid}.exe") }).collect(),
        }
    }

    #[test]
    fn total_sums_all_groups() {
        let c = ConfirmClose::new("t", "l", "close anyway",
            vec![grp("a", &[1, 2]), grp("b", &[3])]);
        assert_eq!(c.total(), 3);
    }

    #[test]
    fn single_group_renders_flat() {
        let c = ConfirmClose::new("t", "l", "close anyway", vec![grp("a", &[1])]);
        assert!(!c.grouped());
    }

    #[test]
    fn multiple_groups_render_grouped() {
        let c = ConfirmClose::new("t", "l", "close anyway",
            vec![grp("a", &[1]), grp("b", &[2])]);
        assert!(c.grouped());
    }
}
```

- [ ] **Step 2: Register the module.** In `src/main.rs`, add `mod confirm;` next to the other module declarations (e.g. after `mod chat;`).

- [ ] **Step 3: Run the tests, verify they pass**

Run: `cargo test confirm::tests`
Expected: PASS (3 tests). (Compiles because `show` isn't referenced yet.)

- [ ] **Step 4: Implement `show`** (add to `impl ConfirmClose`, before the `#[cfg(test)]`):

```rust
    /// Render one frame over `area` (dim + centered panel) and report the
    /// outcome. Esc → Cancelled, Enter → Confirmed; buttons mirror the keys.
    /// Flat when a single group, grouped + indented otherwise.
    pub fn show(&mut self, ui: &mut egui::Ui, area: egui::Rect) -> ConfirmOutcome {
        let mut outcome = ConfirmOutcome::Pending;

        ui.input(|i| {
            if i.key_pressed(egui::Key::Enter) {
                outcome = ConfirmOutcome::Confirmed;
            }
            if i.key_pressed(egui::Key::Escape) {
                outcome = ConfirmOutcome::Cancelled;
            }
        });

        // Dim only the owning manager's area (desktop for a project/quit close,
        // the project's rect for a terminal close).
        ui.painter()
            .rect_filled(area, 0.0, egui::Color32::from_black_alpha(150));

        let grouped = self.grouped();
        egui::Window::new(&self.title)
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.set_min_width(360.0);
                ui.label(egui::RichText::new(&self.title).strong().color(TEXT));
                ui.label(egui::RichText::new(&self.lead).color(DIM));
                ui.separator();

                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for g in &self.groups {
                            if grouped {
                                let mut header = egui::RichText::new(&g.label).color(DIM).monospace();
                                if let Some(scope) = &g.scope {
                                    header = egui::RichText::new(format!("{}   {scope}", g.label))
                                        .color(DIM)
                                        .monospace();
                                }
                                ui.label(header);
                            }
                            for p in &g.procs {
                                ui.horizontal(|ui| {
                                    if grouped {
                                        ui.add_space(14.0);
                                    }
                                    ui.label(egui::RichText::new(&p.name).color(TEXT).monospace());
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.label(
                                                egui::RichText::new(p.pid.to_string())
                                                    .color(DIM)
                                                    .monospace(),
                                            );
                                        },
                                    );
                                });
                            }
                        }
                    });

                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("cancel").clicked() {
                        outcome = ConfirmOutcome::Cancelled;
                    }
                    if ui
                        .button(egui::RichText::new(&self.confirm_label).color(CARET).strong())
                        .clicked()
                    {
                        outcome = ConfirmOutcome::Confirmed;
                    }
                });
            });

        outcome
    }
```

- [ ] **Step 5: Build + run tests, verify everything compiles and passes**

Run: `Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500; cargo test confirm::`
Expected: PASS (3 tests), no warnings from `confirm.rs`.

- [ ] **Step 6: Commit**

```bash
git add src/confirm.rs src/main.rs
git commit -m "feat(confirm): ConfirmClose modal view (grouped Name|Pid list)"
```

---

### Task 3: `Session::root_pid` + the window-tree grouping seam

**Files:**
- Modify: `src/terminal.rs` (add `root_pid` accessor)
- Modify: `src/wm.rs` (add `terminal_shells`, `terminal_groups`, `all_procs`, `project_groups`, `groups_in_tab`; tests)

**Interfaces:**
- Consumes: `Session { root_pid: Option<u32> }`; `proc::descendants` (Task 1); `confirm::ProcGroup` (Task 2); `Tab { title, content }`, `Content::{Terminal,Project,Chat}`.
- Produces (all on `WindowManager`, private): `fn terminal_shells(&self) -> Vec<(String, u32)>`, `fn terminal_groups(&self) -> Vec<ProcGroup>`, `fn all_procs(&self) -> Vec<ProcInfo>`, `fn project_groups(&self) -> Vec<ProcGroup>`, and free `fn groups_in_tab(tab: &Tab) -> Vec<ProcGroup>`.

- [ ] **Step 1: Add the `Session::root_pid` accessor.** In `src/terminal.rs`, next to `term_id`:

```rust
/// The shell's own process id, if the spawn reported one. Root of the
/// descendant scan the close-confirm walks.
pub fn root_pid(&self) -> Option<u32> {
    self.root_pid
}
```

- [ ] **Step 2: Write the failing grouping tests** in `src/wm.rs` `mod tests` (uses the existing `child WindowManager` + `push_win` test helpers; spawns idle shells like `dropping_a_session_kills_its_child`):

```rust
#[test]
fn terminal_shells_lists_one_pair_per_terminal_tab() {
    let ctx = egui::Context::default();
    let mut m = WindowManager::new();
    let env: Vec<(String, String)> = vec![];
    let s1 = Session::spawn(Shell::Cmd, None, &env, ctx.clone()).unwrap();
    let s2 = Session::spawn(Shell::Cmd, None, &env, ctx.clone()).unwrap();
    let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0));
    m.push_win(1, "one".into(), r, Content::Terminal(s1));
    m.push_win(2, "two".into(), r, Content::Terminal(s2));

    let shells = m.terminal_shells();
    assert_eq!(shells.len(), 2);
    let titles: Vec<&str> = shells.iter().map(|(t, _)| t.as_str()).collect();
    assert!(titles.contains(&"one") && titles.contains(&"two"));
    assert!(shells.iter().all(|(_, pid)| *pid != 0));
}

#[test]
fn idle_terminals_produce_no_groups() {
    let ctx = egui::Context::default();
    let mut m = WindowManager::new();
    let env: Vec<(String, String)> = vec![];
    let s = Session::spawn(Shell::Cmd, None, &env, ctx.clone()).unwrap();
    let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0));
    m.push_win(1, "idle".into(), r, Content::Terminal(s));
    // An idle cmd.exe has no non-plumbing descendants → no group to warn about.
    assert!(m.terminal_groups().is_empty());
    assert!(m.all_procs().is_empty());
}
```

(Match `Session::spawn`'s real signature — check `spawn(shell, cwd, env, ctx)` argument order/types in `terminal.rs` and adjust the calls if needed.)

- [ ] **Step 3: Run, verify failure**

Run: `cargo test wm::tests::terminal_shells wm::tests::idle_terminals`
Expected: FAIL — `no method named terminal_shells`.

- [ ] **Step 4: Implement the grouping helpers.** Add to `impl WindowManager` (private), and `groups_in_tab` as a free function in `wm.rs`:

```rust
/// (title, root_pid) for every terminal tab in THIS manager whose shell
/// reported a pid. Pure tree read — the testable surface for grouping.
fn terminal_shells(&self) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    for w in &self.windows {
        for t in &w.tabs {
            if let Content::Terminal(s) = &t.content {
                if let Some(pid) = s.root_pid() {
                    out.push((t.title.clone(), pid));
                }
            }
        }
    }
    out
}

/// One group per terminal tab in THIS manager that has running processes
/// (label = the tab title, empties skipped). Used for project-close.
fn terminal_groups(&self) -> Vec<crate::confirm::ProcGroup> {
    self.terminal_shells()
        .into_iter()
        .filter_map(|(label, pid)| {
            let procs = crate::proc::descendants(pid);
            (!procs.is_empty())
                .then(|| crate::confirm::ProcGroup { label, scope: None, procs })
        })
        .collect()
}

/// Flat aggregate of every running subprocess anywhere in this manager
/// (recurses into nested projects). The cheap "is anything running?" check.
fn all_procs(&self) -> Vec<crate::proc::ProcInfo> {
    let mut out = Vec::new();
    for w in &self.windows {
        for t in &w.tabs {
            match &t.content {
                Content::Terminal(s) => {
                    if let Some(pid) = s.root_pid() {
                        out.extend(crate::proc::descendants(pid));
                    }
                }
                Content::Project(wm) => out.extend(wm.all_procs()),
                Content::Chat(_) => {}
            }
        }
    }
    out
}

/// One group per project tab in THIS (desktop) manager that has running
/// processes: label = project title, scope = "N terminals", procs = aggregate.
/// Used by the quit guard.
fn project_groups(&self) -> Vec<crate::confirm::ProcGroup> {
    let mut out = Vec::new();
    for w in &self.windows {
        for t in &w.tabs {
            if let Content::Project(wm) = &t.content {
                let procs = wm.all_procs();
                if procs.is_empty() {
                    continue;
                }
                let n = wm
                    .terminal_shells()
                    .iter()
                    .filter(|(_, pid)| !crate::proc::descendants(*pid).is_empty())
                    .count();
                out.push(crate::confirm::ProcGroup {
                    label: t.title.clone(),
                    scope: Some(format!("{n} terminal{}", if n == 1 { "" } else { "s" })),
                    procs,
                });
            }
        }
    }
    out
}
```

```rust
/// Processes that closing this one tab would kill: a terminal → at most its own
/// group; a project → one group per terminal inside it; chat → none.
fn groups_in_tab(tab: &Tab) -> Vec<crate::confirm::ProcGroup> {
    match &tab.content {
        Content::Terminal(s) => {
            let procs = s.root_pid().map(crate::proc::descendants).unwrap_or_default();
            if procs.is_empty() {
                Vec::new()
            } else {
                vec![crate::confirm::ProcGroup {
                    label: tab.title.clone(),
                    scope: None,
                    procs,
                }]
            }
        }
        Content::Project(wm) => wm.terminal_groups(),
        Content::Chat(_) => Vec::new(),
    }
}
```

- [ ] **Step 5: Build + run, verify pass**

Run: `Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500; cargo test wm::tests::terminal_shells wm::tests::idle_terminals`
Expected: PASS. (If the compiler warns `project_groups`/`groups_in_tab` are unused, that's expected — Task 4 wires them; leave them.)

- [ ] **Step 6: Commit**

```bash
git add src/terminal.rs src/wm.rs
git commit -m "feat(wm): tree-walk grouping of running subprocesses (root_pid, groups)"
```

---

### Task 4: The close gate — state, `resolve_pending`, `show_modals`, routing

**Files:**
- Modify: `src/wm.rs` (`pending_close` state, `CloseTarget`, `PendingClose`, `build_confirm`, `request_close_active_tab`, `request_close_tab`, `resolve_pending`, `show_modals` branch, `deserted`, and the four close-trigger call sites; tests)

**Interfaces:**
- Consumes: `groups_in_tab`, `close_active_tab`, `close_tab`, `swallow_input`, `show_modals(&mut self, ui, area, ctx)`; `confirm::{ConfirmClose, ConfirmOutcome}` (Task 2).
- Produces: `fn request_close_active_tab(&mut self, id: WinId)`, `fn request_close_tab(&mut self, id: WinId, idx: usize)`, `fn resolve_pending(&mut self, outcome: ConfirmOutcome)`; `pending_close` participates in `deserted()`.

- [ ] **Step 1: Add the state.** On `struct WindowManager`, add `pending_close: Option<PendingClose>,` and initialize it `None` in every constructor (`new`, `as_desktop`, any `Default`). Add near the struct:

```rust
enum CloseTarget {
    ActiveTab(WinId),
    Tab(WinId, usize),
    Quit, // set by the app-quit guard (Task 5)
}

struct PendingClose {
    target: CloseTarget,
    view: crate::confirm::ConfirmClose,
}
```

- [ ] **Step 2: Write the failing state-machine tests** in `wm::tests` (pure — no egui, no OS; they drive `resolve_pending` directly):

```rust
#[test]
fn resolve_confirmed_closes_the_target_window() {
    let mut m = WindowManager::new();
    let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0));
    let child = WindowManager::new();
    m.push_win(7, "proj".into(), r, Content::Project(Box::new(child)));
    m.pending_close = Some(PendingClose {
        target: CloseTarget::ActiveTab(7),
        view: crate::confirm::ConfirmClose::new("t", "l", "close anyway", vec![]),
    });
    m.resolve_pending(crate::confirm::ConfirmOutcome::Confirmed);
    assert!(m.windows.iter().all(|w| w.id != 7), "window not closed on confirm");
    assert!(m.pending_close.is_none());
}

#[test]
fn resolve_cancelled_keeps_the_window() {
    let mut m = WindowManager::new();
    let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0));
    let child = WindowManager::new();
    m.push_win(7, "proj".into(), r, Content::Project(Box::new(child)));
    m.pending_close = Some(PendingClose {
        target: CloseTarget::ActiveTab(7),
        view: crate::confirm::ConfirmClose::new("t", "l", "close anyway", vec![]),
    });
    m.resolve_pending(crate::confirm::ConfirmOutcome::Cancelled);
    assert!(m.windows.iter().any(|w| w.id == 7), "window closed on cancel");
    assert!(m.pending_close.is_none());
}

#[test]
fn deserted_is_false_while_a_close_is_pending() {
    let mut m = WindowManager::new().as_desktop();
    m.pending_close = Some(PendingClose {
        target: CloseTarget::Quit,
        view: crate::confirm::ConfirmClose::new("quit foreman?", "l", "quit anyway", vec![]),
    });
    assert!(!m.deserted(), "a pending confirm must hold the app alive");
}

#[test]
fn build_confirm_wording_terminal_vs_project() {
    let g = |label: &str, n: usize| crate::confirm::ProcGroup {
        label: label.into(),
        scope: None,
        procs: (0..n).map(|i| crate::proc::ProcInfo { pid: i as u32, name: "x.exe".into() }).collect(),
    };
    let term = build_confirm(false, vec![g("claude", 1)]);
    assert_eq!(term.title(), "close this terminal?");
    let proj = build_confirm(true, vec![g("a", 2), g("b", 1)]);
    assert_eq!(proj.title(), "close this project?");
    assert!(proj.lead().contains("across 2 terminals"), "got: {}", proj.lead());
}
```

(This needs read-only accessors on `ConfirmClose` for assertions. Add to `confirm.rs`: `pub fn title(&self) -> &str { &self.title }` and `pub fn lead(&self) -> &str { &self.lead }`.)

- [ ] **Step 3: Run, verify failure**

Run: `cargo test wm::tests::resolve wm::tests::deserted_is_false wm::tests::build_confirm`
Expected: FAIL — `resolve_pending` / `build_confirm` / `title` not found.

- [ ] **Step 4: Implement the gate, `build_confirm`, and `resolve_pending`.** Add `build_confirm` as a free fn and the methods to `impl WindowManager`:

```rust
/// Compose the confirm copy for a pane/project close from the gathered groups.
fn build_confirm(is_project: bool, groups: Vec<crate::confirm::ProcGroup>) -> crate::confirm::ConfirmClose {
    let total: usize = groups.iter().map(|g| g.procs.len()).sum();
    if is_project {
        let k = groups.len();
        crate::confirm::ConfirmClose::new(
            "close this project?",
            format!(
                "{total} process{} still running across {k} terminal{}:",
                if total == 1 { " is" } else { "es are" },
                if k == 1 { "" } else { "s" },
            ),
            "close anyway",
            groups,
        )
    } else {
        crate::confirm::ConfirmClose::new(
            "close this terminal?",
            format!(
                "{total} process{} still running here:",
                if total == 1 { " is" } else { "es are" },
            ),
            "close anyway",
            groups,
        )
    }
}
```

```rust
/// Close the active tab of `id`, or open the confirm modal if it has running
/// subprocesses. No-op guard-wise if a confirm is already open.
fn request_close_active_tab(&mut self, id: WinId) {
    if self.pending_close.is_some() {
        return;
    }
    let Some(w) = self.windows.iter().find(|w| w.id == id) else {
        return;
    };
    let tab = &w.tabs[w.active];
    let is_project = matches!(tab.content, Content::Project(_));
    let groups = Self::groups_in_tab(tab);
    if groups.is_empty() {
        self.close_active_tab(id);
        return;
    }
    self.pending_close = Some(PendingClose {
        target: CloseTarget::ActiveTab(id),
        view: build_confirm(is_project, groups),
    });
}

/// Same, for a specific tab index (tab-bar X).
fn request_close_tab(&mut self, id: WinId, idx: usize) {
    if self.pending_close.is_some() {
        return;
    }
    let Some(w) = self.windows.iter().find(|w| w.id == id) else {
        return;
    };
    let Some(tab) = w.tabs.get(idx) else {
        return;
    };
    let is_project = matches!(tab.content, Content::Project(_));
    let groups = Self::groups_in_tab(tab);
    if groups.is_empty() {
        self.close_tab(id, idx);
        return;
    }
    self.pending_close = Some(PendingClose {
        target: CloseTarget::Tab(id, idx),
        view: build_confirm(is_project, groups),
    });
}

/// Apply a modal outcome to the pending close. Pure decision, split from the
/// egui render so it is unit-tested without a UI context.
fn resolve_pending(&mut self, outcome: crate::confirm::ConfirmOutcome) {
    let Some(pending) = self.pending_close.take() else {
        return;
    };
    match outcome {
        crate::confirm::ConfirmOutcome::Pending => self.pending_close = Some(pending),
        crate::confirm::ConfirmOutcome::Cancelled => {}
        crate::confirm::ConfirmOutcome::Confirmed => match pending.target {
            CloseTarget::ActiveTab(id) => self.close_active_tab(id),
            CloseTarget::Tab(id, idx) => self.close_tab(id, idx),
            CloseTarget::Quit => self.quit_confirmed = true,
        },
    }
}
```

Note: `CloseTarget::Quit` references `self.quit_confirmed`, added in Task 5. To keep Task 4 compiling on its own, add the field now: on `struct WindowManager` add `quit_confirmed: bool,` (init `false` everywhere) — it stays inert until Task 5 wires it.

- [ ] **Step 5: Update `deserted` and render in `show_modals`.**

`deserted`:

```rust
pub fn deserted(&self) -> bool {
    self.windows.is_empty()
        && self.picker.is_none()
        && self.settings.is_none()
        && self.pending_close.is_none()
}
```

In `show_modals`, add a branch after the settings block (keeps the render thin; the decision lives in `resolve_pending`):

```rust
if let Some(mut pending) = self.pending_close.take() {
    let outcome = pending.view.show(ui, area);
    self.pending_close = Some(pending);
    self.resolve_pending(outcome);
    self.swallow_input(ui);
}
```

- [ ] **Step 6: Route the four interactive close triggers through the gate.**

- `Act::Close(id)` handler: `self.close_active_tab(id)` → `self.request_close_active_tab(id)`.
- `Act::CloseTab(id, idx)` handler: `self.close_tab(id, idx)` → `self.request_close_tab(id, idx)`.
- Leader `Command::CloseTerm`: `child.close_active_tab(id)` → `child.request_close_active_tab(id)`.
- Leader `Command::CloseProject`: `self.close_active_tab(id)` → `self.request_close_active_tab(id)`.

Leave `close`, `close_tab`, `close_active_tab`, `close_dispatch`, and `close_terminal` unchanged.

- [ ] **Step 7: Build + run the full suite**

Run: `Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500; cargo test`
Expected: PASS — the new `wm::tests::resolve*`, `deserted_is_false*`, `build_confirm*` plus the existing 415, none regressed.

- [ ] **Step 8: Commit**

```bash
git add src/wm.rs src/confirm.rs
git commit -m "feat(wm): confirm gate before interactive terminal/project close"
```

---

### Task 5: App-quit guard (window X / Alt+F4) — separable

**Files:**
- Modify: `src/wm.rs` (`begin_quit_confirm`, `take_quit_confirmed`; the `CloseTarget::Quit` arm already exists)
- Modify: `src/main.rs` (`force_quit` field; the `close_requested` interception)

**Interfaces:**
- Consumes: `desktop.all_procs()`, `desktop.project_groups()`, `pending_close`, `quit_confirmed` (Task 4); `ctx.input(...).viewport().close_requested()`, `ViewportCommand::{Close, CancelClose}`.
- Produces: `pub fn begin_quit_confirm(&mut self) -> bool`, `pub fn take_quit_confirmed(&mut self) -> bool`.

- [ ] **Step 1: Write the failing WM tests** (`wm::tests`, deterministic — an empty desktop has nothing running):

```rust
#[test]
fn begin_quit_confirm_is_false_when_nothing_runs() {
    let mut m = WindowManager::new().as_desktop();
    assert!(!m.begin_quit_confirm(), "empty desktop should let the app quit");
    assert!(m.pending_close.is_none());
}

#[test]
fn take_quit_confirmed_reports_once_then_resets() {
    let mut m = WindowManager::new().as_desktop();
    m.quit_confirmed = true;
    assert!(m.take_quit_confirmed());
    assert!(!m.take_quit_confirmed(), "flag must reset after being taken");
}
```

- [ ] **Step 2: Run, verify failure**

Run: `cargo test wm::tests::begin_quit_confirm wm::tests::take_quit_confirmed`
Expected: FAIL — methods not found.

- [ ] **Step 3: Implement the two methods** on `impl WindowManager` (public):

```rust
/// Open the quit confirm if any subprocess is running anywhere; return true
/// when it did (caller should cancel the OS close). False → nothing running,
/// let the app quit.
pub fn begin_quit_confirm(&mut self) -> bool {
    if self.pending_close.is_some() {
        return true; // already confirming (a quit or a close)
    }
    let groups = self.project_groups();
    if groups.is_empty() {
        return false;
    }
    let total: usize = groups.iter().map(|g| g.procs.len()).sum();
    let k = groups.len();
    let view = crate::confirm::ConfirmClose::new(
        "quit foreman?",
        format!(
            "{total} process{} still running across {k} project{}:",
            if total == 1 { " is" } else { "es are" },
            if k == 1 { "" } else { "s" },
        ),
        "quit anyway",
        groups,
    );
    self.pending_close = Some(PendingClose { target: CloseTarget::Quit, view });
    true
}

/// True once, when the quit confirm was accepted. Resets on read.
pub fn take_quit_confirmed(&mut self) -> bool {
    std::mem::take(&mut self.quit_confirmed)
}
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test wm::tests::begin_quit_confirm wm::tests::take_quit_confirmed`
Expected: PASS.

- [ ] **Step 5: Add the `force_quit` field** to `struct App` (`src/main.rs`) and init `false` in `App::new`:

```rust
    /// Set once the quit confirm was accepted, so the next viewport Close isn't
    /// intercepted again.
    force_quit: bool,
```

- [ ] **Step 6: Wire the interception in `App::ui`,** right after `self.desktop.show(...)` and before/around the existing `deserted()` → `Close` block:

```rust
        // Quit guard: the window's title-bar X and Alt+F4 send
        // ViewportCommand::Close straight to the viewport, bypassing every WM
        // close funnel. Intercept while any subprocess is running and confirm
        // first; the modal renders next frame via the desktop's show_modals.
        if self.started
            && !self.force_quit
            && ctx.input(|i| i.viewport().close_requested())
            && self.desktop.begin_quit_confirm()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        if self.desktop.take_quit_confirmed() {
            self.force_quit = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
```

The existing `deserted()` → `Close` path needs no change: once the last project has actually closed, its subprocesses are gone, so `begin_quit_confirm` returns false and the quit proceeds.

- [ ] **Step 7: Build + full suite**

Run: `Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500; cargo test`
Expected: PASS (all, including the two new quit tests).

- [ ] **Step 8: Commit**

```bash
git add src/wm.rs src/main.rs
git commit -m "feat: confirm before quitting foreman with running subprocesses"
```

---

## Live verification (after Task 5, manual — not CI)

Build release and drive with the headless loop + a screenshot (kill the app first):

1. **Idle closes silently.** Open a project, spawn a terminal, close the pane immediately → **no** modal (idle shell, denylist filters the console host).
2. **Busy pane prompts.** In a pane run a blocker (`pause`) or an agent, then close the pane → modal titled `close this terminal?` lists that process as Name │ Pid. `cancel` keeps it; `close anyway` kills it (confirm with `foreman status` — the pane and process are gone).
3. **Project aggregates + groups.** A project with two busy terminals → project close shows both terminals as indented groups under `close this project?`.
4. **Quit guard.** With a busy pane, click the window's title-bar X (or Alt+F4) → `quit foreman?` lists processes grouped by project; `cancel` keeps the app open, `quit anyway` exits.
5. **Screenshot** the modal (build-screenshot skill) and compare against `2026-07-06-close-confirm-mockup.html` for spacing/legibility. Note: it renders in the current warm `theme.rs` palette, not the mockup's steel/orange (theming is a separate effort).

## Self-review notes (spec coverage)

- Trigger policy (live child, host denylist) → Task 1.
- Modal view + grouped/flat + copy → Task 2, `build_confirm` in Task 4.
- Recursive collection (terminal/project/quit) → Task 3.
- Gate in front of all interactive closes + single-modal discipline + `deserted` → Task 4.
- App-quit guard (separable) → Task 5.
- Programmatic paths stay un-gated → unchanged `close*`/`close_dispatch`/`close_terminal` (Global Constraints).
- Testability: pure `collect_descendants`, `terminal_shells`, `resolve_pending`, `build_confirm` are the unit-test surfaces; OS-scan integration and egui render are live-verified.
