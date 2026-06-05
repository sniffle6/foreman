# Project Directory Picker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When you create a project, you pick its working directory through a Warp-style keyboard-driven navigator, and every terminal in that project spawns in that directory.

**Architecture:** A project is a nested `WindowManager`. We give each `WindowManager` an optional `cwd`, thread it into `Session::spawn` so shells start there, and add a self-contained `DirPicker` modal (pure navigation logic + a thin egui layer). Clicking "+ project" opens the picker instead of immediately spawning; accepting a directory creates the project with that `cwd`.

**Tech Stack:** Rust, eframe/egui (immediate-mode GUI), portable-pty (`CommandBuilder::cwd`), alacritty_terminal. Tests use `tempfile`.

---

## File Structure

- `src/dirpicker.rs` — **new**. The `DirPicker`: pure navigation/filter/accept logic with unit tests, plus a `show()` method that renders the modal and reports an `Outcome`. No knowledge of projects or window management — it just emits a `PathBuf`.
- `src/terminal.rs` — **modify**. `Session::spawn` gains a `cwd: Option<&Path>` argument and sets it on the `CommandBuilder`.
- `src/wm.rs` — **modify**. `WindowManager` gains `cwd: Option<PathBuf>` and `picker: Option<DirPicker>`. `add_terminal` spawns in `self.cwd`; `add_project` takes a `cwd`; the "+ project" `Act` opens the picker; the picker is rendered at the end of `show`.
- `src/main.rs` — **modify**. Declare `mod dirpicker;`; pass a default `cwd` to the startup `add_project`.
- `Cargo.toml` — **modify**. Add `tempfile` dev-dependency.
- `docs/project-directories.md` — **new**. Short feature doc (per repo convention).
- `docs/foreman.md` — **modify**. Update the AddProject flow description.

**Picker interaction model (locked during design):**
- Up / Down — move the highlight.
- Right / Tab — drill *into* the highlighted directory (breadcrumb updates, list shows its children).
- Left — go to the parent directory.
- Typing — fuzzy/substring filter of the current directory's children.
- **Enter — accept the current location** (the directory shown in the breadcrumb). To choose `docs` as the cwd you drill into it, then Enter.
- Esc — cancel.
- Only directories are listed; files are hidden. Dotfile directories (`.git`, `.serena`) are hidden.

---

## Task 1: Thread `cwd` into terminal spawning

This makes every terminal in a project inherit the project's directory. Done first with a hardcoded default so it's verifiable before the picker exists.

**Files:**
- Modify: `src/terminal.rs` (`Session::spawn`, ~line 159; add `use std::path::Path;`)
- Modify: `src/wm.rs` (`WindowManager` struct ~line 213, `new` ~line 226, `add_terminal` ~line 263, `add_project` ~line 277, `Act::AddProject` apply ~line 749)
- Modify: `src/main.rs` (startup `add_project`, line 26)

- [ ] **Step 1: Give `Session::spawn` a `cwd` parameter**

In `src/terminal.rs`, add to the imports near the top (with the other `use std::...` lines):

```rust
use std::path::Path;
```

Change the signature and the command construction. Replace:

```rust
    pub fn spawn(shell: Shell, ctx: egui::Context) -> std::io::Result<Session> {
```

with:

```rust
    pub fn spawn(shell: Shell, cwd: Option<&Path>, ctx: egui::Context) -> std::io::Result<Session> {
```

Then replace this block:

```rust
        let child = pair
            .slave
            .spawn_command(CommandBuilder::new(shell.program()))
            .map_err(|e| std::io::Error::other(e.to_string()))?;
```

with:

```rust
        let mut cmd = CommandBuilder::new(shell.program());
        if let Some(dir) = cwd {
            cmd.cwd(dir);
        }
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
```

- [ ] **Step 2: Add `cwd` field to `WindowManager`**

In `src/wm.rs`, add the import near the top with the other `use` lines:

```rust
use std::path::PathBuf;
```

In the `WindowManager` struct (the `split: egui::Vec2,` field is last), add a field:

```rust
    /// Working directory new terminals in this manager spawn into. `None` on the
    /// desktop (process cwd); `Some` on a project, set when the project is created.
    cwd: Option<PathBuf>,
```

In `WindowManager::new()`, add to the struct literal:

```rust
            cwd: None,
```

- [ ] **Step 3: Spawn terminals in `self.cwd`**

In `src/wm.rs`, in `add_terminal`, replace:

```rust
        if let Ok(s) = Session::spawn(shell, ctx.clone()) {
```

with:

```rust
        if let Ok(s) = Session::spawn(shell, self.cwd.as_deref(), ctx.clone()) {
```

- [ ] **Step 4: `add_project` takes and stores a `cwd`**

In `src/wm.rs`, replace the whole `add_project` body:

```rust
    pub fn add_project(&mut self, shell: Shell, ctx: &egui::Context) {
        let (id, rect) = self.next_slot(egui::vec2(720.0, 480.0));
        let mut child = WindowManager::new();
        child.add_terminal(shell, ctx);
        self.push_win(
            id,
            format!("project {}", id),
            rect,
            Content::Project(Box::new(child)),
        );
    }
```

with:

```rust
    pub fn add_project(&mut self, shell: Shell, cwd: PathBuf, ctx: &egui::Context) {
        let (id, rect) = self.next_slot(egui::vec2(720.0, 480.0));
        let title = cwd
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.to_string())
            .unwrap_or_else(|| format!("project {}", id));
        let mut child = WindowManager::new();
        child.cwd = Some(cwd);
        child.add_terminal(shell, ctx);
        self.push_win(id, title, rect, Content::Project(Box::new(child)));
    }
```

- [ ] **Step 5: Update the two `add_project` call sites**

In `src/wm.rs`, in the apply loop, replace:

```rust
                Act::AddProject => self.add_project(Shell::PowerShell, &ctx),
```

with (temporary — Task 4 replaces this with the picker):

```rust
                Act::AddProject => {
                    let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                    self.add_project(Shell::PowerShell, dir, &ctx);
                }
```

In `src/main.rs`, replace:

```rust
            self.desktop.add_project(Shell::PowerShell, &ctx);
```

with:

```rust
            let dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            self.desktop.add_project(Shell::PowerShell, dir, &ctx);
```

- [ ] **Step 6: Build and manually verify**

Run: `cargo build`
Expected: compiles with no errors.

Run: `cargo run`
Manual check: the startup project's terminal — type `pwd` (PowerShell shows the path). It should be the repo directory (`H:\claude code\foreman`), not `C:\Windows\System32` or similar. In that project, click a shell chip in the titlebar to add a second terminal; it should open in the **same** directory.

> Why no unit test here: `Session::spawn` launches a real OS process and `CommandBuilder` exposes no getters, so there is nothing to assert in isolation. Verified by observation.

- [ ] **Step 7: Commit**

```bash
git add src/terminal.rs src/wm.rs src/main.rs
git commit -m "Spawn project terminals in a per-project working directory

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: `DirPicker` navigation logic (TDD)

Pure logic only — no egui. This is the testable core.

**Files:**
- Create: `src/dirpicker.rs`
- Modify: `src/main.rs` (declare module)
- Modify: `Cargo.toml` (dev-dependency)

- [ ] **Step 1: Add the `tempfile` dev-dependency**

In `Cargo.toml`, after the `[dependencies]` block, add:

```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Declare the module**

In `src/main.rs`, add to the top with the other `mod` lines:

```rust
mod dirpicker;
```

- [ ] **Step 3: Write the failing tests**

Create `src/dirpicker.rs` with ONLY the test module for now (the types it references come in Step 5; this is expected to fail to compile first):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tree() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for sub in ["alpha", "beta", "gamma", ".hidden"] {
            fs::create_dir(d.path().join(sub)).unwrap();
        }
        fs::write(d.path().join("file.txt"), b"x").unwrap();
        fs::create_dir(d.path().join("beta").join("inner")).unwrap();
        d
    }

    fn dir_names(p: &DirPicker) -> Vec<String> {
        p.items()
            .iter()
            .filter_map(|i| match i {
                Item::Dir(pb) => Some(pb.file_name().unwrap().to_string_lossy().into_owned()),
                Item::Parent => None,
            })
            .collect()
    }

    #[test]
    fn lists_dirs_only_sorted_no_dotfiles() {
        let d = tree();
        let p = DirPicker::new(d.path().to_path_buf());
        assert_eq!(dir_names(&p), vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn parent_row_present_when_query_empty() {
        let d = tree();
        let p = DirPicker::new(d.path().to_path_buf());
        assert_eq!(p.items().first(), Some(&Item::Parent));
    }

    #[test]
    fn query_filters_dirs_and_hides_parent() {
        let d = tree();
        let mut p = DirPicker::new(d.path().to_path_buf());
        p.push_char('b');
        let items = p.items();
        assert_eq!(items.len(), 1);
        assert!(matches!(items[0], Item::Dir(_)));
    }

    #[test]
    fn drill_in_enters_highlighted_dir() {
        let d = tree();
        let mut p = DirPicker::new(d.path().to_path_buf());
        // items: [Parent, alpha, beta, gamma]; move to beta (index 2).
        p.move_down();
        p.move_down();
        p.drill_in();
        assert_eq!(p.cwd(), d.path().join("beta"));
        assert_eq!(dir_names(&p), vec!["inner"]);
    }

    #[test]
    fn accept_returns_current_location() {
        let d = tree();
        let mut p = DirPicker::new(d.path().to_path_buf());
        p.move_down();
        p.move_down();
        p.drill_in(); // into beta
        assert_eq!(p.accept(), d.path().join("beta"));
    }

    #[test]
    fn go_parent_climbs_up() {
        let d = tree();
        let mut p = DirPicker::new(d.path().join("beta"));
        p.go_parent();
        assert_eq!(p.cwd(), d.path());
    }

    #[test]
    fn move_down_clamps_to_last_item() {
        let d = tree();
        let mut p = DirPicker::new(d.path().to_path_buf());
        for _ in 0..50 {
            p.move_down();
        }
        assert!(p.selected() < p.items().len());
    }

    #[test]
    fn backspace_restores_parent_row() {
        let d = tree();
        let mut p = DirPicker::new(d.path().to_path_buf());
        p.push_char('b');
        assert_eq!(p.items().first(), Some(&Item::Dir(d.path().join("beta"))));
        p.pop_char();
        assert_eq!(p.items().first(), Some(&Item::Parent));
    }
}
```

- [ ] **Step 4: Run the tests to verify they fail**

Run: `cargo test --bin foreman dirpicker`
Expected: FAIL — compile error, `cannot find type DirPicker` / `Item` in this scope.

- [ ] **Step 5: Implement the logic**

At the TOP of `src/dirpicker.rs` (above the `#[cfg(test)] mod tests`), add:

```rust
use std::path::{Path, PathBuf};

/// One row in the picker list.
#[derive(Debug, PartialEq)]
pub enum Item {
    /// ".." — navigate to the parent of the current location.
    Parent,
    /// A child directory of the current location.
    Dir(PathBuf),
}

/// Result of rendering the modal for one frame.
pub enum Outcome {
    Pending,
    Cancelled,
    Accepted(PathBuf),
}

/// Keyboard-driven directory navigator. Enter accepts the current location.
pub struct DirPicker {
    cwd: PathBuf,
    query: String,
    dirs: Vec<PathBuf>, // child directories of `cwd`; sorted; dotfiles excluded
    selected: usize,    // index into `items()`
}

impl DirPicker {
    pub fn new(start: PathBuf) -> Self {
        let mut p = Self {
            cwd: start,
            query: String::new(),
            dirs: vec![],
            selected: 0,
        };
        p.reload();
        p
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Reload `dirs` from disk and reset query/selection. Called on every move
    /// between directories.
    fn reload(&mut self) {
        self.query.clear();
        self.selected = 0;
        self.dirs = list_dirs(&self.cwd);
    }

    /// Visible rows after applying the query filter. A `Parent` row leads the
    /// list only when the query is empty and `cwd` has a parent.
    pub fn items(&self) -> Vec<Item> {
        let mut out = Vec::new();
        if self.query.is_empty() && self.cwd.parent().is_some() {
            out.push(Item::Parent);
        }
        let q = self.query.to_lowercase();
        for d in &self.dirs {
            let name = d.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if q.is_empty() || name.to_lowercase().contains(&q) {
                out.push(Item::Dir(d.clone()));
            }
        }
        out
    }

    pub fn move_down(&mut self) {
        let n = self.items().len();
        if n > 0 && self.selected + 1 < n {
            self.selected += 1;
        }
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn push_char(&mut self, c: char) {
        self.query.push(c);
        self.clamp();
    }

    pub fn pop_char(&mut self) {
        self.query.pop();
        self.clamp();
    }

    fn clamp(&mut self) {
        let n = self.items().len();
        if n == 0 {
            self.selected = 0;
        } else if self.selected >= n {
            self.selected = n - 1;
        }
    }

    /// Right / Tab: enter the highlighted directory (or climb if it is `Parent`).
    pub fn drill_in(&mut self) {
        match self.items().into_iter().nth(self.selected) {
            Some(Item::Parent) => self.go_parent(),
            Some(Item::Dir(p)) => {
                self.cwd = p;
                self.reload();
            }
            None => {}
        }
    }

    /// Left: go to the parent of the current location.
    pub fn go_parent(&mut self) {
        if let Some(parent) = self.cwd.parent() {
            self.cwd = parent.to_path_buf();
            self.reload();
        }
    }

    /// Enter: accept the current location as the chosen directory.
    pub fn accept(&self) -> PathBuf {
        self.cwd.clone()
    }
}

/// Child directories of `dir`, sorted case-insensitively, dotfiles excluded.
/// Unreadable directories yield an empty list rather than erroring.
fn list_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| !n.starts_with('.'))
                    .unwrap_or(false)
            })
            .collect(),
        Err(_) => vec![],
    };
    v.sort_by_key(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.to_lowercase())
            .unwrap_or_default()
    });
    v
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --bin foreman dirpicker`
Expected: PASS — 8 passed.

- [ ] **Step 7: Commit**

```bash
git add src/dirpicker.rs src/main.rs Cargo.toml Cargo.lock
git commit -m "Add DirPicker navigation logic with tests

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Render the `DirPicker` modal in egui

Adds the `show()` method. egui rendering isn't unit-testable here, so verification is manual — but it's driven entirely by the Task 2 logic, which is tested.

**Files:**
- Modify: `src/dirpicker.rs` (add `use eframe::egui;` and an `impl DirPicker { fn show }`)

- [ ] **Step 1: Add the egui import**

At the top of `src/dirpicker.rs`, add above the `use std::path` line:

```rust
use eframe::egui;
```

- [ ] **Step 2: Add the `show` method**

Insert this `impl` block after the existing `impl DirPicker { ... }` block (above `fn list_dirs`):

```rust
impl DirPicker {
    /// Render the modal for one frame and report the outcome. Keyboard:
    /// Up/Down move, Right/Tab drill in, Left up, Enter accept, Esc cancel,
    /// typing filters. Characters are captured manually (no focusable text
    /// field) so the arrow/Tab/Enter keys are free for navigation.
    pub fn show(&mut self, ui: &mut egui::Ui) -> Outcome {
        let mut outcome = Outcome::Pending;

        ui.input(|i| {
            for ev in &i.events {
                if let egui::Event::Text(t) = ev {
                    for c in t.chars() {
                        self.push_char(c);
                    }
                }
            }
            if i.key_pressed(egui::Key::Backspace) {
                self.pop_char();
            }
            if i.key_pressed(egui::Key::ArrowDown) {
                self.move_down();
            }
            if i.key_pressed(egui::Key::ArrowUp) {
                self.move_up();
            }
            if i.key_pressed(egui::Key::Tab) || i.key_pressed(egui::Key::ArrowRight) {
                self.drill_in();
            }
            if i.key_pressed(egui::Key::ArrowLeft) {
                self.go_parent();
            }
            if i.key_pressed(egui::Key::Enter) {
                outcome = Outcome::Accepted(self.accept());
            }
            if i.key_pressed(egui::Key::Escape) {
                outcome = Outcome::Cancelled;
            }
        });

        // Dim the desktop behind the modal.
        let screen = ui.ctx().screen_rect();
        ui.painter()
            .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(150));

        egui::Window::new("set project directory")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.set_min_width(440.0);
                ui.label(egui::RichText::new(self.cwd.display().to_string()).strong());
                let hint = if self.query.is_empty() {
                    "type to filter · → enter · ← up · Enter open here · Esc cancel".to_string()
                } else {
                    format!("filter: {}", self.query)
                };
                ui.label(egui::RichText::new(hint).weak());
                ui.separator();

                egui::ScrollArea::vertical()
                    .max_height(280.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (idx, item) in self.items().into_iter().enumerate() {
                            let (label, is_parent) = match &item {
                                Item::Parent => (".. (parent)".to_string(), true),
                                Item::Dir(p) => (
                                    p.file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                        .into_owned(),
                                    false,
                                ),
                            };
                            let resp = ui.selectable_label(idx == self.selected, label);
                            if resp.clicked() {
                                self.selected = idx;
                                if is_parent {
                                    self.go_parent();
                                } else {
                                    self.drill_in();
                                }
                            }
                        }
                    });

                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("open here").clicked() {
                        outcome = Outcome::Accepted(self.accept());
                    }
                    if ui.button("cancel").clicked() {
                        outcome = Outcome::Cancelled;
                    }
                });
            });

        outcome
    }
}
```

- [ ] **Step 3: Build to verify it compiles**

Run: `cargo build`
Expected: compiles. (The method is unused until Task 4, so expect a `dead_code` warning for `show` — acceptable until the next task wires it.)

- [ ] **Step 4: Commit**

```bash
git add src/dirpicker.rs
git commit -m "Render DirPicker modal in egui

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: Wire the picker to project creation

The "+ project" button now opens the picker; accepting a directory creates the project there.

**Files:**
- Modify: `src/wm.rs` (imports, `Act` enum ~line 198, struct/`new`, button apply ~line 749, the "+ project" button ~line 585, end of `show` ~line 787)

- [ ] **Step 1: Import `DirPicker`**

In `src/wm.rs`, add near the other `use crate::...` / `use` lines at the top:

```rust
use crate::dirpicker::{DirPicker, Outcome};
```

- [ ] **Step 2: Add the `picker` field**

In the `WindowManager` struct, add after the `cwd` field from Task 1:

```rust
    /// When `Some`, the directory picker modal is open (desktop only). Opening it
    /// defers project creation until the user accepts a directory.
    picker: Option<DirPicker>,
```

In `WindowManager::new()`, add to the struct literal:

```rust
            picker: None,
```

- [ ] **Step 3: Rename the Act to open the picker**

In the `Act` enum, replace:

```rust
    /// Spawn a new sibling project on the desktop. Fired by the "+" on a project
    /// titlebar; applied after the render borrow drops like the rest.
    AddProject,
```

with:

```rust
    /// Open the directory picker to create a new sibling project on the desktop.
    /// Fired by the "+" on a project titlebar; the actual project is created when
    /// the user accepts a directory in the picker.
    OpenProjectPicker,
```

- [ ] **Step 4: Update the button**

In `src/wm.rs`, in the "+ project" button block, replace:

```rust
                if resp.clicked() {
                    acts.push(Act::AddProject);
                }
```

with:

```rust
                if resp.clicked() {
                    acts.push(Act::OpenProjectPicker);
                }
```

- [ ] **Step 5: Open the picker in the apply loop**

In `src/wm.rs`, replace the temporary block from Task 1:

```rust
                Act::AddProject => {
                    let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                    self.add_project(Shell::PowerShell, dir, &ctx);
                }
```

with:

```rust
                Act::OpenProjectPicker => {
                    self.picker = Some(DirPicker::new(self.picker_start()));
                }
```

- [ ] **Step 6: Add the `picker_start` helper**

In `src/wm.rs`, inside `impl WindowManager`, add this method (next to `add_project`):

```rust
    /// Where the picker opens: the focused project's cwd if there is one, else the
    /// process working directory, else `.`.
    fn picker_start(&self) -> PathBuf {
        self.focused
            .and_then(|id| self.windows.iter().find(|w| w.id == id))
            .and_then(|w| match &w.content {
                Content::Project(wm) => wm.cwd.clone(),
                _ => None,
            })
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."))
    }
```

- [ ] **Step 7: Render the picker at the end of `show`**

In `src/wm.rs`, in `show`, find the end of the apply loop. After the `for a in acts { ... }` loop closes and before `interacted` is returned, insert:

```rust
        if let Some(mut picker) = self.picker.take() {
            match picker.show(ui) {
                Outcome::Pending => self.picker = Some(picker),
                Outcome::Cancelled => {}
                Outcome::Accepted(path) => self.add_project(Shell::PowerShell, path, &ctx),
            }
        }
```

(`ctx` is already cloned earlier in `show` as `let ctx = ui.ctx().clone();`.)

- [ ] **Step 8: Build and manually verify**

Run: `cargo build`
Expected: compiles, no `dead_code` warning for `DirPicker::show` anymore.

Run: `cargo run`
Manual checklist:
1. Click the "+" on a project titlebar → the picker modal appears, breadcrumb showing the current project's directory.
2. Type letters → the list filters. Backspace → filter clears.
3. Arrow Down/Up moves the highlight; Right/Tab drills into a folder (breadcrumb updates); Left goes up.
4. Press Enter → a new project window appears, titled with the folder name, and its terminal's `pwd` is the directory you accepted.
5. Re-open the picker and press Esc → it closes with no new project.
6. Add a terminal inside the new project (shell chip) → it opens in the same directory.

- [ ] **Step 9: Commit**

```bash
git add src/wm.rs
git commit -m "Open directory picker on + project; create project in chosen dir

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: Documentation

**Files:**
- Create: `docs/project-directories.md`
- Modify: `docs/foreman.md` (the AddProject flow description)

- [ ] **Step 1: Write the feature doc**

Create `docs/project-directories.md`:

```markdown
# Project Directories

## What it does

Each project has a working directory. Every terminal you open in that project —
the first one and any you add later — starts in that directory. When you make a
new project you pick its directory through a keyboard-driven navigator.

## Why it exists

Before this, every shell started wherever Foreman itself was launched, so a
second terminal in a project did not land next to the first one. Projects are
meant to be per-repo sandboxes, so they need their own directory.

## How to use it

- Click the "+" on a project titlebar. A picker opens at the focused project's
  directory (or the process directory if none).
- Navigate:
  - Up / Down — move the highlight
  - Right / Tab — go into the highlighted folder
  - Left — go up to the parent
  - Type — filter the current folder's subfolders
  - Enter — open a project here (the folder shown at the top)
  - Esc — cancel
- To choose a folder as the project directory, go *into* it, then press Enter.

## Gotchas

- Only directories are shown; files are hidden. Dotfile folders (`.git`,
  `.serena`) are hidden too.
- "Enter accepts the current location" — not the highlighted row. The highlight
  is only for drilling in. This is deliberate: you navigate *to* the directory
  you want, then accept it.
- The directory is set once, at creation. Terminals spawn there but the project
  has no live "follow the shell's cwd" tracking yet (that would need OSC 7).

## Key files

- `src/dirpicker.rs` — the picker: navigation logic (`DirPicker`, unit-tested)
  plus the egui modal (`show`).
- `src/terminal.rs` — `Session::spawn` takes the cwd and sets it on the PTY
  command.
- `src/wm.rs` — `WindowManager.cwd` (per-project dir), `add_project` (creates a
  project at a dir), `picker` field + `OpenProjectPicker` act (opens the modal,
  creates the project on accept).
```

- [ ] **Step 2: Update the architecture overview**

In `docs/foreman.md`, find the line describing the AddProject flow (around line 127, "queues `Act::AddProject`"). Replace that sentence so it reads:

```markdown
  which queues `Act::OpenProjectPicker`. That opens the directory picker
  (`src/dirpicker.rs`); accepting a directory spawns a sibling project whose
  terminals start in that directory. See `docs/project-directories.md`.
```

- [ ] **Step 3: Commit**

```bash
git add docs/project-directories.md docs/foreman.md
git commit -m "Document project directories feature

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review notes

- **Spec coverage:** per-project cwd (Task 1), Warp-style navigator with the locked interaction model (Tasks 2–3), "+ project" opens it and creates at the chosen dir (Task 4), files/dotfiles hidden (Task 2 `list_dirs` + tests), default start = focused project cwd (Task 4 `picker_start`), docs (Task 5). OSC 7 live-cwd tracking is explicitly out of scope (noted in the doc as a later layer).
- **Type consistency:** `DirPicker`, `Item::{Parent,Dir}`, `Outcome::{Pending,Cancelled,Accepted}`, `DirPicker::{new,items,move_up,move_down,push_char,pop_char,drill_in,go_parent,accept,cwd,selected,query,show}` are used identically across tasks. `add_project(shell, cwd, ctx)` and `Session::spawn(shell, cwd, ctx)` signatures match their call sites. `WindowManager.cwd` / `.picker` fields are added in Task 1/4 and read in `picker_start` and `add_terminal`.
- **Honest test boundary:** only the pure logic is unit-tested (Task 2). Process spawning and egui rendering are verified by the manual checklists in Tasks 1 and 4, with the reason stated.
```