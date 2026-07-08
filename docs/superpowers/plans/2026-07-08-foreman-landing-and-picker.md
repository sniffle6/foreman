# FOREMAN Landing + Directory Picker Redesign — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give foreman an empty-state "FOREMAN" landing screen (terminal-drawn wordmark + inline directory picker + Claude/Codex/Terminal launcher icons), fronted by a rewritten directory picker whose editable path field is the source of truth with inline ghost-text completion.

**Architecture:** Two coupled subsystems on one feature branch, built picker-first (the landing embeds the picker). The picker is a rewrite of `src/dirpicker.rs` around a pure, unit-tested seam (`split`/`base_dir`/`completions`/`ghost`) with a thin egui render on top. The landing is a new `src/landing.rs` module owning its own `DirPicker`, rendered from `App::ui` when the desktop is empty, gated behind an env flag so default behavior is untouched.

**Tech Stack:** Rust, egui 0.34.3 (immediate mode), `portable-pty`; GNU toolchain on Windows; `tempfile` for filesystem tests.

## Global Constraints

- **Windows-first, GNU toolchain.** Build with `cargo build`; kill the running app first (`Stop-Process -Name foreman -Force`) or the link fails with `Access is denied (os error 5)`.
- **egui 0.34:** `App::ui(&mut Ui, ...)` (not `update`). Go through the painter (`ui.painter().layout_no_wrap`) for text measurement; `ui.fonts(|f|…)` needs `&mut` and is unavailable mid-draw.
- **Flag off = byte-for-byte today's behavior.** The landing is gated behind env `FOREMAN_LANDING=1`; when unset, startup auto-project (`main.rs:354-356`) and quit-on-deserted (`main.rs:398`) are unchanged.
- **Picker accept = Tab only.** Right-arrow stays an ordinary `TextEdit` cursor move (settled with the user). Prefix matching (not substring/fuzzy). Enter opens a path only when it `is_dir()` (never a file or missing path).
- **Never `VoidListener`; never touch the DSR/Ready path.** This work does not go near `terminal.rs` PTY code.
- **Colors from `theme.rs` tokens only** (`TEXT`, `DIM`, `BORDER`, `BORDER_FOCUS`, `DANGER`, `DESK_BG`, `SELECTION`, `SEL_BG`, `PALETTE`) — no ad-hoc RGB.
- **Commits:** `type(scope): subject`; end every commit message body with the trailer `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`. Stage files by name, never `git add -A`.
- **Branch:** `feat/foreman-landing` (already created; specs already committed).

**Specs:** `docs/superpowers/specs/2026-07-08-directory-picker-redesign-design.md`, `docs/superpowers/specs/2026-07-08-foreman-landing-design.md`.

## File Structure

- `src/dirpicker.rs` — **rewritten.** Pure seam (`split`, `base_dir`, `completions`, `ghost`) + `DirPicker { path, selected, root, focus_next, invalid }` + `Row` model + `show`/`show_modal`/`current_dir`. `list_dirs` kept.
- `src/wm.rs` — one-line swap at the `show_modals` picker call site (`3662`): `picker.show(ui)` → `picker.show_modal(ui)`.
- `src/landing.rs` — **new.** `SessionKind`, `LandingAction`, `ICON_ORDER`, `FOREMAN_ART`, pure `layout`, `Landing { picker }` + `show`.
- `src/main.rs` — `mod landing;`, `App` gains `landing: landing::Landing` + `landing_enabled: bool`; flag read at startup; render branch, startup auto-project gate, quit-guard gate, action routing.

---

## Phase A — Directory picker redesign

*(Independently shippable: after Phase A the leader `NewProject` picker is the new path-field picker. The landing depends on it.)*

### Task A1: Pure navigation seam

**Files:**
- Modify: `src/dirpicker.rs` (add functions + tests; leave the existing `DirPicker`/`Item` and their tests untouched this task so the file keeps compiling)

**Interfaces:**
- Produces: `fn split(buf: &str) -> (String, String)`; `fn base_dir(base: &str, root: &Path) -> PathBuf`; `fn completions(base: &Path, partial: &str, lister: &dyn Fn(&Path) -> Vec<PathBuf>) -> Vec<PathBuf>`; `fn ghost(partial: &str, highlighted: Option<&Path>) -> Option<String>`.

- [ ] **Step 1: Write the failing tests** — add to the existing `mod tests` in `src/dirpicker.rs`:

```rust
#[test]
fn split_lexical_posix_and_windows() {
    assert_eq!(split("/a/b/c"), ("/a/b".into(), "c".into()));
    assert_eq!(split("/a/b/"), ("/a/b".into(), "".into()));
    assert_eq!(split("/x"), ("/".into(), "x".into()));      // leading-sep root keeps its sep
    assert_eq!(split(""), ("".into(), "".into()));
    assert_eq!(split("foreman"), ("".into(), "foreman".into())); // no sep: all partial
    assert_eq!(split(r"C:\Us"), (r"C:\".into(), "Us".into())); // drive root keeps its sep
    assert_eq!(split(r"C:\"), (r"C:\".into(), "".into()));
    assert_eq!(split(r"C:\Users\"), (r"C:\Users".into(), "".into()));
    // Pinned rules for the ambiguous cases (documented degradation):
    assert_eq!(split("C:"), ("".into(), "C:".into()));        // bare drive → treated as a partial
    assert_eq!(split(r"\\srv\share"), (r"\\srv".into(), "share".into())); // UNC: base is bare \\srv
}

#[test]
fn base_dir_resolves_relative_against_root() {
    let root = Path::new("/root");
    assert_eq!(base_dir("", root), PathBuf::from("/root"));       // empty → root
    assert_eq!(base_dir("sub", root), PathBuf::from("/root/sub")); // relative → joined
}

#[test]
fn completions_is_case_insensitive_prefix_over_the_lister() {
    let lister = |_: &Path| {
        ["foreman", "formats", "platform"].iter().map(|n| PathBuf::from("/x").join(n)).collect()
    };
    let got: Vec<String> = completions(Path::new("/x"), "FoR", &lister)
        .iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(got, vec!["foreman", "formats"]); // prefix only, case-insensitive; no "platform"
}

#[test]
fn ghost_is_the_real_names_remainder_case_preserving() {
    let hl = PathBuf::from("/x/foreman");
    assert_eq!(ghost("for", Some(&hl)), Some(format!("eman{}", std::path::MAIN_SEPARATOR)));
    assert_eq!(ghost("FOR", Some(&hl)), Some(format!("eman{}", std::path::MAIN_SEPARATOR))); // real casing
    assert_eq!(ghost("", Some(&hl)), None);   // no partial → no ghost
    assert_eq!(ghost("for", None), None);     // nothing highlighted
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run (PowerShell): `cargo test --lib dirpicker 2>&1 | Select-Object -Last 20`
Expected: FAIL — `cannot find function split/base_dir/completions/ghost`.

- [ ] **Step 3: Implement the pure functions** — add near the top of `src/dirpicker.rs` (after the imports, above `DirPicker`):

```rust
fn is_sep(c: char) -> bool { c == '/' || c == '\\' }

/// Lexically split a path buffer into (base, partial). `partial` is the segment
/// after the last separator (empty if the buffer ends in one). `base` drops the
/// trailing separator, except a lone leading-sep root ("/") or a bare drive
/// ("C:") keep a separator so the base still names a directory. Pure — no fs.
fn split(buf: &str) -> (String, String) {
    match buf.rfind(is_sep) {
        None => (String::new(), buf.to_string()),
        Some(i) => {
            let head = &buf[..i];
            let sep = &buf[i..=i];
            let base = if head.is_empty() {
                buf[..=i].to_string()          // "/x" → "/"
            } else if head.ends_with(':') {
                format!("{head}{sep}")         // r"C:\Us" → r"C:\"
            } else {
                head.to_string()
            };
            (base, buf[i + 1..].to_string())
        }
    }
}

/// Resolve a lexical `base` to a directory to list: empty → `root`; relative →
/// joined onto `root`; absolute → itself.
fn base_dir(base: &str, root: &Path) -> PathBuf {
    if base.is_empty() {
        root.to_path_buf()
    } else {
        let p = Path::new(base);
        if p.is_absolute() { p.to_path_buf() } else { root.join(p) }
    }
}

/// Child dirs of `base` whose names case-insensitively prefix `partial`. A pure
/// prefix filter over the injected `lister`'s output (the lister owns sort +
/// dotfile policy — see `list_dirs`).
fn completions(base: &Path, partial: &str, lister: &dyn Fn(&Path) -> Vec<PathBuf>) -> Vec<PathBuf> {
    let needle = partial.to_lowercase();
    lister(base)
        .into_iter()
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.to_lowercase().starts_with(&needle))
                .unwrap_or(false)
        })
        .collect()
}

/// Remainder of the highlighted dir's REAL name after `partial` chars, plus a
/// separator — the gray inline ghost. None when partial is empty or nothing is
/// highlighted. Case-preserving (prefix match is case-insensitive).
fn ghost(partial: &str, highlighted: Option<&Path>) -> Option<String> {
    if partial.is_empty() { return None; }
    let name = highlighted?.file_name()?.to_str()?;
    let rest: String = name.chars().skip(partial.chars().count()).collect();
    Some(format!("{rest}{}", std::path::MAIN_SEPARATOR))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib dirpicker 2>&1 | Select-Object -Last 20`
Expected: PASS (the four new tests plus the existing 10 old-`DirPicker` tests — all green; the file still compiles because the old struct is untouched).

- [ ] **Step 5: Commit**

```powershell
git add src/dirpicker.rs
git commit -m "feat(dirpicker): pure split/base_dir/completions/ghost seam"
```

---

### Task A2: New picker state machine + render + wm swap

Replaces the old `DirPicker`/`Item`/old `show`/old tests in one commit (they are compilation-coupled: `wm.rs` calls `picker.show`). Delivers a working path-field picker from the leader (minus ghost, which is Task A3).

**Files:**
- Modify: `src/dirpicker.rs` (replace `Item`, `DirPicker`, its impls, and the 10 old tests; keep `list_dirs` and the Task-A1 functions and the `tree()` fixture)
- Modify: `src/wm.rs:3662` (`picker.show(ui)` → `picker.show_modal(ui)`)

**Interfaces:**
- Consumes: `split`, `base_dir`, `completions` (Task A1).
- Produces: `pub enum Outcome { Pending, Cancelled, Accepted(PathBuf) }` (unchanged); `DirPicker::new(start: PathBuf) -> Self`; `DirPicker::show(&mut self, &mut egui::Ui) -> Outcome`; `DirPicker::show_modal(&mut self, &mut egui::Ui) -> Outcome`; `DirPicker::current_dir(&self) -> Option<PathBuf>`.

- [ ] **Step 1: Write the failing state-machine tests** — replace the old `mod tests` bodies (keep the `tree()` fixture and the Task-A1 tests) with:

```rust
// Build a picker whose buffer points at `dir` (trailing sep), tests via real fs.
fn at(dir: &Path) -> DirPicker { DirPicker::new(dir.to_path_buf()) }

#[test]
fn new_seeds_highlight_to_first_completion_not_parent() {
    let d = tree();
    let p = at(d.path());
    // rows = [Parent, alpha, beta, gamma]; selected seeds to the first Dir (alpha).
    assert_eq!(p.selected(), 1);
    assert_eq!(p.highlighted_name(), Some("alpha".to_string()));
}

#[test]
fn typing_a_partial_prefix_filters_and_reseeds() {
    let d = tree();
    let mut p = at(d.path());
    p.set_path(format!("{}{}be", d.path().display(), std::path::MAIN_SEPARATOR));
    // rows = [Parent, beta]; highlight reseeds to beta.
    assert_eq!(p.highlighted_name(), Some("beta".to_string()));
}

#[test]
fn tab_completes_into_the_highlighted_dir() {
    let d = tree();
    let mut p = at(d.path());
    p.set_path(format!("{}{}be", d.path().display(), std::path::MAIN_SEPARATOR));
    p.complete(); // Tab
    assert_eq!(p.current_dir(), Some(d.path().join("beta")));
    assert_eq!(p.highlighted_name(), Some("inner".to_string())); // now inside beta
}

#[test]
fn parent_row_climbs() {
    let d = tree();
    let mut p = at(&d.path().join("beta"));
    p.select(0);   // Parent row
    p.complete();
    assert_eq!(p.current_dir(), Some(d.path().to_path_buf()));
}

#[test]
fn accept_only_for_an_existing_directory() {
    let d = tree();
    let mut p = at(d.path());
    assert!(matches!(p.accept(), Some(_))); // a real dir
    p.set_path(format!("{}{}zzz", d.path().display(), std::path::MAIN_SEPARATOR));
    assert_eq!(p.accept(), None);           // missing path
    p.set_path(d.path().join("file.txt").display().to_string());
    assert_eq!(p.accept(), None);           // a file, not a dir
}

#[test]
fn empty_completions_are_panic_free() {
    let d = tree();
    let mut p = at(d.path());
    p.set_path(format!("{}{}zzz", d.path().display(), std::path::MAIN_SEPARATOR)); // matches nothing
    p.move_down(); p.move_up(); p.complete(); // must not panic
    let _ = p.accept();
    assert!(p.selected() < p.rows_len().max(1));
}

#[test]
fn current_dir_none_for_partial_some_for_dir() {
    let d = tree();
    let mut p = at(d.path());
    assert_eq!(p.current_dir(), Some(d.path().to_path_buf()));
    p.set_path(format!("{}{}al", d.path().display(), std::path::MAIN_SEPARATOR));
    assert_eq!(p.current_dir(), None); // "…/al" is a partial, not a dir
}
```

(`selected`, `highlighted_name`, `select`, `rows_len` are small test-facing accessors defined in Step 2.)

- [ ] **Step 2: Replace the struct + impl** — delete the old `pub enum Item`, `pub struct DirPicker`, and both `impl DirPicker` blocks; write:

```rust
/// A row in the completion dropdown.
enum Row { Parent, Dir(PathBuf) }

/// Result of rendering the picker for one frame.
pub enum Outcome { Pending, Cancelled, Accepted(PathBuf) }

/// Address-bar directory picker: the editable `path` buffer is the source of
/// truth; completion shows as a ghost + a dropdown driven by `selected`.
pub struct DirPicker {
    path: String,
    selected: usize,
    root: PathBuf,
    focus_next: bool,
    invalid: bool,
}

impl DirPicker {
    pub fn new(start: PathBuf) -> Self {
        let mut path = start.display().to_string();
        if !path.ends_with(is_sep) { path.push(std::path::MAIN_SEPARATOR); }
        let mut p = Self { path, selected: 0, root: start, focus_next: true, invalid: false };
        p.reseed();
        p
    }

    // --- pure-ish derivations (real fs via list_dirs) ---

    fn base_and_partial(&self) -> (PathBuf, String) {
        let (base, partial) = split(&self.path);
        (base_dir(&base, &self.root), partial)
    }

    fn rows(&self) -> Vec<Row> {
        let (base, partial) = self.base_and_partial();
        let mut out = Vec::new();
        if base.parent().is_some() { out.push(Row::Parent); }
        for d in completions(&base, &partial, &list_dirs) { out.push(Row::Dir(d)); }
        out
    }

    fn highlighted(&self) -> Option<PathBuf> {
        match self.rows().into_iter().nth(self.selected) {
            Some(Row::Dir(p)) => Some(p),
            _ => None,
        }
    }

    /// Resolve the whole buffer to a path (relative → against root).
    fn resolve(&self) -> PathBuf {
        let p = Path::new(&self.path);
        if p.is_absolute() { p.to_path_buf() } else { self.root.join(p) }
    }

    // --- state transitions ---

    fn reseed(&mut self) {
        let rows = self.rows();
        self.selected = rows.iter().position(|r| matches!(r, Row::Dir(_))).unwrap_or(0);
        self.invalid = false;
    }

    pub fn set_path(&mut self, new: String) { self.path = new; self.reseed(); }

    pub fn move_down(&mut self) {
        let n = self.rows().len();
        if n > 0 && self.selected + 1 < n { self.selected += 1; }
    }
    pub fn move_up(&mut self) { self.selected = self.selected.saturating_sub(1); }

    /// Tab / click: drill into the highlighted dir, or climb on the Parent row.
    pub fn complete(&mut self) {
        match self.rows().into_iter().nth(self.selected) {
            Some(Row::Parent) => {
                let (base, _) = self.base_and_partial();
                if let Some(parent) = base.parent() { self.set_path(with_sep(parent)); }
            }
            Some(Row::Dir(p)) => self.set_path(with_sep(&p)),
            None => {}
        }
    }

    /// Enter: the buffer resolved to an existing directory, else None.
    pub fn current_dir(&self) -> Option<PathBuf> {
        let p = self.resolve();
        p.is_dir().then_some(p)
    }
    fn accept(&self) -> Option<PathBuf> { self.current_dir() }

    // --- test-facing accessors ---
    #[cfg(test)] fn selected(&self) -> usize { self.selected }
    #[cfg(test)] fn rows_len(&self) -> usize { self.rows().len() }
    #[cfg(test)] fn select(&mut self, i: usize) { self.selected = i; }
    #[cfg(test)] fn highlighted_name(&self) -> Option<String> {
        self.highlighted().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
    }
}

/// A path's display string with a guaranteed trailing separator.
fn with_sep(p: &Path) -> String {
    let mut s = p.display().to_string();
    if !s.ends_with(is_sep) { s.push(std::path::MAIN_SEPARATOR); }
    s
}
```

- [ ] **Step 3: Run the state-machine tests**

Run: `cargo test --lib dirpicker 2>&1 | Select-Object -Last 30`
Expected: PASS for the seven new state tests + the four Task-A1 tests. (Compilation will still fail until Step 4 provides `show`/`show_modal` — that's expected; if you want a green checkpoint, run after Step 5.)

- [ ] **Step 4: Add the basic render (`show` + `show_modal`)** — append an `impl DirPicker` with the render. Modeled on the chat-input field (`wm.rs:426-445`):

```rust
use crate::theme::{TEXT, DIM, BORDER, BORDER_FOCUS, DANGER, DESK_BG, SEL_BG};

impl DirPicker {
    /// Inline render: field + dropdown into the current `ui`. Placement-agnostic.
    pub fn show(&mut self, ui: &mut egui::Ui) -> Outcome {
        let id = egui::Id::new("dirpicker-field");

        // Intercept navigation keys BEFORE the TextEdit sees them.
        let (tab, up, down, enter, esc) = ui.input_mut(|i| (
            i.consume_key(egui::Modifiers::NONE, egui::Key::Tab),
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
            i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
            i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
        ));
        if up { self.move_up(); }
        if down { self.move_down(); }
        if tab { self.complete(); }
        if esc { return Outcome::Cancelled; }
        if enter {
            match self.accept() {
                Some(p) => return Outcome::Accepted(p),
                None => self.invalid = true,   // Task A3 paints the cue; keep focus below
            }
        }

        // Field.
        let font = egui::FontId::monospace(13.0);
        let field_h = 26.0;
        let field_rect = {
            let r = ui.max_rect();
            egui::Rect::from_min_size(r.min, egui::vec2(r.width().min(520.0), field_h))
        };
        ui.painter().rect_filled(field_rect, egui::CornerRadius::same(3), DESK_BG);
        ui.painter().rect_stroke(
            field_rect, egui::CornerRadius::same(3),
            egui::Stroke::new(1.0, if self.invalid { DANGER } else { BORDER }),
            egui::StrokeKind::Inside,
        );
        let te = ui.put(
            field_rect,
            egui::TextEdit::singleline(&mut self.path)
                .id(id)
                .font(font.clone())
                .text_color(TEXT)
                .frame(egui::Frame::NONE)
                .vertical_align(egui::Align::Center)
                .margin(egui::Margin::symmetric(6, 0))
                .desired_width(field_rect.width()),
        );
        if self.focus_next { te.request_focus(); self.focus_next = false; }
        if te.changed() { self.reseed(); }     // typing re-derives
        if enter && self.invalid { te.request_focus(); } // never a dead field

        // Dropdown, in a popup Area anchored under the field.
        let mut clicked: Option<usize> = None;
        egui::Area::new(id.with("drop"))
            .fixed_pos(field_rect.left_bottom() + egui::vec2(0.0, 2.0))
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                ui.set_max_width(field_rect.width());
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
                        for (idx, row) in self.rows().into_iter().enumerate() {
                            let label = match &row {
                                Row::Parent => "../".to_string(),
                                Row::Dir(p) => p.file_name().unwrap_or_default()
                                    .to_string_lossy().into_owned(),
                            };
                            if ui.selectable_label(idx == self.selected, label).clicked() {
                                clicked = Some(idx);
                            }
                        }
                    });
                });
            });
        if let Some(idx) = clicked { self.selected = idx; self.complete(); }

        Outcome::Pending
    }

    /// Leader-invoked modal: `show` inside a top-center floating Area with a
    /// subtle scrim (the modality signal), replacing the old centered Window.
    pub fn show_modal(&mut self, ui: &mut egui::Ui) -> Outcome {
        let screen = ui.ctx().content_rect();
        ui.painter().rect_filled(screen, 0.0, egui::Color32::from_black_alpha(90)); // lighter than 150
        let mut outcome = Outcome::Pending;
        egui::Area::new(egui::Id::new("dirpicker-modal"))
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, screen.height() * 0.18))
            .show(ui.ctx(), |ui| {
                ui.set_max_width(520.0);
                egui::Frame::popup(ui.style()).show(ui, |ui| { outcome = self.show(ui); });
            });
        outcome
    }
}
```

- [ ] **Step 5: Swap the wm call site** — in `src/wm.rs`, at the `show_modals` picker block (`3662`):

```rust
// before:
match picker.show(ui) {
// after:
match picker.show_modal(ui) {
```

- [ ] **Step 6: Build, then exercise the leader picker**

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 20
cargo test --lib dirpicker 2>&1 | Select-Object -Last 30   # all state + pure tests green
```
Expected: build OK; tests PASS. Then `cargo run`, press the leader (`Ctrl+B`) + the `NewProject` chord, and confirm a top-anchored path field with a dropdown appears; typing filters, ↑/↓ moves the highlight, Tab drills, a click drills, Enter opens an existing dir, Esc cancels. Screenshot and `Read` the PNG to verify (GUI can't be checked from the terminal).

- [ ] **Step 7: Commit**

```powershell
git add src/dirpicker.rs src/wm.rs
git commit -m "feat(dirpicker): path-field picker with dropdown (Tab-only accept)"
```

---

### Task A3: Inline ghost text + invalid cue

**Files:**
- Modify: `src/dirpicker.rs` (`show`: paint the ghost; the invalid border is already wired in A2)

**Interfaces:**
- Consumes: `ghost` (A1), `highlighted` (A2).
- Produces: no new public API.

- [ ] **Step 1: Add a ghost helper** — inside `impl DirPicker`:

```rust
/// The ghost suffix for the current field: the highlighted match's remainder.
fn ghost_text(&self) -> Option<String> {
    let (_, partial) = self.base_and_partial();
    ghost(&partial, self.highlighted().as_deref())
}
```

- [ ] **Step 2: Paint the ghost in `show`** — immediately after the `ui.put(...)` field render (and after the `focus_next` block), before the dropdown Area:

```rust
if let Some(g) = self.ghost_text() {
    let text_w = ui.painter()
        .layout_no_wrap(self.path.clone(), font.clone(), TEXT)
        .rect.width();
    let x = field_rect.min.x + 6.0 + text_w; // 6.0 == field margin
    ui.painter().text(
        egui::pos2(x, field_rect.center().y),
        egui::Align2::LEFT_CENTER,
        g, font.clone(), DIM,
    );
}
```

(The field font is `monospace(13.0)`, so the measured width lines the ghost up exactly after the typed text. `DIM` is the muted theme token.)

- [ ] **Step 3: Build and verify the ghost + cue**

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 20
```
Expected: build OK. `cargo run`, open the picker, type a partial (e.g. a prefix of a real child dir): a gray ghost completes it; Tab fills it in; ↑/↓ moves the ghost to the highlighted row; typing a bad path then Enter turns the border `DANGER` red and keeps the field focused (still typeable). Screenshot and `Read` to confirm.

- [ ] **Step 4: Commit**

```powershell
git add src/dirpicker.rs
git commit -m "feat(dirpicker): inline ghost-text completion + invalid-path cue"
```

---

## Phase B — FOREMAN landing

*(Depends on Phase A: the landing embeds `DirPicker::show` and calls `current_dir`.)*

### Task B1: Landing layout seam + constants

**Files:**
- Create: `src/landing.rs`

**Interfaces:**
- Produces: `pub enum SessionKind { Claude, Codex, Terminal }`; `pub struct LandingAction { pub path: PathBuf, pub kind: SessionKind }`; `const ICON_ORDER: [SessionKind; 3]`; `const FOREMAN_ART: &str`; `struct LandingLayout { wordmark, tagline, field, icons }`; `fn layout(area: egui::Rect, n_icons: usize) -> LandingLayout`.

- [ ] **Step 1: Write the failing layout tests** — create `src/landing.rs` with the tests first:

```rust
use eframe::egui;
use std::path::PathBuf;

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1200.0, 800.0))
    }

    #[test]
    fn every_element_is_inside_the_area_and_disjoint() {
        let a = area();
        let l = layout(a, 3);
        for r in [l.wordmark, l.tagline, l.field] {
            assert!(a.contains_rect(r), "{r:?} escapes {a:?}");
        }
        assert!(l.wordmark.bottom() <= l.tagline.top());
        assert!(l.tagline.bottom() <= l.field.top());
        assert!(l.field.bottom() <= l.icons[0].top());
    }

    #[test]
    fn stack_is_horizontally_centered() {
        let a = area();
        let l = layout(a, 3);
        let c = a.center().x;
        for r in [l.wordmark, l.tagline, l.field] {
            assert!((r.center().x - c).abs() < 1.0, "{r:?} not centered");
        }
    }

    #[test]
    fn icons_are_equal_width_and_evenly_spaced() {
        let l = layout(area(), 3);
        assert_eq!(l.icons.len(), 3);
        let w = l.icons[0].width();
        assert!(l.icons.iter().all(|r| (r.width() - w).abs() < 0.5));
        let g1 = l.icons[1].left() - l.icons[0].right();
        let g2 = l.icons[2].left() - l.icons[1].right();
        assert!((g1 - g2).abs() < 0.5);
    }

    #[test]
    fn tiny_area_degrades_without_negative_sizes() {
        let a = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(240.0, 180.0));
        let l = layout(a, 3);
        for r in [l.wordmark, l.tagline, l.field] {
            assert!(r.width() >= 0.0 && r.height() >= 0.0);
            assert!(a.contains_rect(r.intersect(a)));
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib landing 2>&1 | Select-Object -Last 20`
Expected: FAIL — `cannot find function layout` / unresolved `landing` (module not declared yet — Step 3 adds the code; the `mod landing;` line comes in Task B3, but `cargo test --lib landing` will report the missing symbols once the file is referenced. If the module isn't compiled yet, temporarily add `mod landing;` to `src/main.rs` now and keep it.)

- [ ] **Step 3: Implement the constants + layout** — at the top of `src/landing.rs` (above the tests):

```rust
/// Provisional, landing-local taxonomy (phase-2 replaces it with the dispatch model).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SessionKind { Claude, Codex, Terminal }

/// Open `path` as a new project running `kind`.
pub struct LandingAction { pub path: PathBuf, pub kind: SessionKind }

/// Fixed icon order so positional rects and the hit-test agree.
const ICON_ORDER: [SessionKind; 3] = [SessionKind::Claude, SessionKind::Codex, SessionKind::Terminal];

/// FOREMAN in a mono block font — real terminal art.
const FOREMAN_ART: &str = r"
███████  ██████  ██████  ███████ ███    ███  █████  ███    ██
██      ██    ██ ██   ██ ██      ████  ████ ██   ██ ████   ██
█████   ██    ██ ██████  █████   ██ ████ ██ ███████ ██ ██  ██
██      ██    ██ ██   ██ ██      ██  ██  ██ ██   ██ ██  ██ ██
██       ██████  ██   ██ ███████ ██      ██ ██   ██ ██   ████";

struct LandingLayout {
    wordmark: egui::Rect,
    tagline: egui::Rect,
    field: egui::Rect,
    icons: Vec<egui::Rect>,
}

/// Place the stack (wordmark → tagline → field → icon row) centered in `area`.
/// Pure arithmetic — no fonts, no fs.
fn layout(area: egui::Rect, n_icons: usize) -> LandingLayout {
    let cx = area.center().x;
    let field_w = area.width().min(520.0).max(0.0);
    let (word_h, tag_h, field_h, icon, gap) = (120.0_f32, 24.0, 26.0, 72.0_f32, 18.0);
    let total = word_h + 16.0 + tag_h + 28.0 + field_h + 36.0 + icon;
    let mut y = (area.center().y - total / 2.0).max(area.top());

    let centered = |w: f32, y: f32, h: f32| {
        egui::Rect::from_min_size(egui::pos2(cx - w / 2.0, y), egui::vec2(w, h))
    };
    let word_w = area.width().min(760.0);
    let wordmark = centered(word_w, y, word_h); y += word_h + 16.0;
    let tagline = centered(word_w, y, tag_h);   y += tag_h + 28.0;
    let field = centered(field_w, y, field_h);  y += field_h + 36.0;

    let n = n_icons.max(1);
    let row_w = (icon * n as f32 + gap * (n as f32 - 1.0)).min(area.width());
    let mut x = cx - row_w / 2.0;
    let icons = (0..n_icons).map(|_| {
        let r = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(icon, icon));
        x += icon + gap;
        r
    }).collect();

    LandingLayout { wordmark, tagline, field, icons }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib landing 2>&1 | Select-Object -Last 20`
Expected: PASS (four layout tests).

- [ ] **Step 5: Commit**

```powershell
git add src/landing.rs src/main.rs
git commit -m "feat(landing): pure layout seam + wordmark/session constants"
```

---

### Task B2: Landing render

**Files:**
- Modify: `src/landing.rs` (add `Landing` + `show`)

**Interfaces:**
- Consumes: `layout`, `FOREMAN_ART`, `ICON_ORDER` (B1); `DirPicker::{new, show, current_dir}` (Phase A); `icons::texture` + `IconKind`; theme tokens.
- Produces: `Landing::new(start: PathBuf) -> Self`; `Landing::show(&mut self, &mut egui::Ui, egui::Rect) -> Option<LandingAction>`.

- [ ] **Step 1: Implement `Landing`** — add to `src/landing.rs`:

```rust
use crate::dirpicker::{DirPicker, Outcome};
use crate::icons::{self, IconKind};
use crate::theme::{TEXT, DIM, BORDER_FOCUS};

fn icon_of(k: SessionKind) -> IconKind {
    match k {
        SessionKind::Claude => IconKind::Claude,
        SessionKind::Codex => IconKind::Codex,
        SessionKind::Terminal => IconKind::PowerShell, // shared shell-prompt glyph
    }
}
fn label_of(k: SessionKind) -> &'static str {
    match k { SessionKind::Claude => "Claude", SessionKind::Codex => "Codex", SessionKind::Terminal => "Terminal" }
}

/// The empty-desktop landing. Owns its own path-field picker (separate from the
/// desktop's leader picker).
pub struct Landing { picker: DirPicker }

impl Landing {
    pub fn new(start: PathBuf) -> Self { Self { picker: DirPicker::new(start) } }

    pub fn show(&mut self, ui: &mut egui::Ui, area: egui::Rect) -> Option<LandingAction> {
        let l = layout(area, ICON_ORDER.len());
        let mut action: Option<LandingAction> = None;

        // Wordmark (mono block art) + tagline, centered.
        let word_font = egui::FontId::monospace(14.0);
        ui.painter().text(l.wordmark.center_top(), egui::Align2::CENTER_TOP,
            FOREMAN_ART.trim_matches('\n'), word_font, BORDER_FOCUS);
        ui.painter().text(l.tagline.center(), egui::Align2::CENTER_CENTER,
            "tmux for AI agents", egui::FontId::proportional(14.0), DIM);

        // Inline picker in the field rect.
        let picker_out = {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(l.field));
            self.picker.show(&mut child)
        };
        if let Outcome::Accepted(path) = picker_out {
            action = Some(LandingAction { path, kind: SessionKind::Terminal });
        }

        // Icon row — each opens the picker's current path with that kind.
        for (r, &kind) in l.icons.iter().zip(ICON_ORDER.iter()) {
            let tex = icons::texture(ui.ctx(), icon_of(kind), 48);
            let resp = ui.put(*r, egui::ImageButton::new(&tex));
            ui.painter().text(r.center_bottom() + egui::vec2(0.0, 12.0),
                egui::Align2::CENTER_TOP, label_of(kind), egui::FontId::proportional(12.0), TEXT);
            if resp.clicked() {
                if let Some(path) = self.picker.current_dir() {
                    action = Some(LandingAction { path, kind });
                }
            }
        }

        ui.ctx().request_repaint(); // no PTYs drive repaint on an empty desktop
        action
    }
}
```

- [ ] **Step 2: Build (module still not wired into the render path)**

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 20
```
Expected: build OK (the module compiles; `Landing::show` is not yet called — a `dead_code` warning on `Landing`/`show` is fine until Task B3). Adjust any egui-0.34 API drift the compiler reports (e.g. `new_child`/`UiBuilder`, `ImageButton::new` argument) following the `egui-immediate-mode-reference` skill.

- [ ] **Step 3: Commit**

```powershell
git add src/landing.rs
git commit -m "feat(landing): wordmark + inline picker + session icon row render"
```

---

### Task B3: Wire the landing into `main.rs` (flag-gated)

**Files:**
- Modify: `src/main.rs` — `mod landing;` (if not added in B1); `App` fields; startup flag + auto-project gate; render branch; quit-guard gate; action routing.

**Interfaces:**
- Consumes: `landing::{Landing, LandingAction, SessionKind}`; `WindowManager::{deserted, add_project, tile_new}`; `Shell::PowerShell`.

- [ ] **Step 1: Add module + `App` fields** — in `src/main.rs`, ensure `mod landing;` is present with the other `mod` lines. Add to `struct App`:

```rust
    landing: landing::Landing,
    landing_enabled: bool,
```

In `App::default()` (or wherever `App` is constructed — the block that sets `started: false`), initialize:

```rust
    landing: landing::Landing::new(
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))),
    landing_enabled: std::env::var_os("FOREMAN_LANDING").is_some(),
```

- [ ] **Step 2: Gate ONLY the startup auto-project** — in `App::ui`, the `if !self.started` block (`main.rs:343`). Keep the zoom opt-out (`349-352`) and `self.started = true` (`357`) unconditional; wrap only the two project lines:

```rust
    // Desktop hosts project windows; each project is its own sandbox.
    if !self.landing_enabled {
        let dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let nid = self.desktop.add_project(Shell::PowerShell, dir, &ctx);
        self.desktop.tile_new(nid, None);
    }
    self.started = true;
```

- [ ] **Step 3: Add the render branch** — replace the unconditional `self.desktop.show(ui, area, ...)` call (`main.rs:377`) with:

```rust
    if self.landing_enabled && self.desktop.deserted() {
        if let Some(act) = self.landing.show(ui, area) {
            let anchor = None;
            let nid = self.desktop.add_project(Shell::PowerShell, act.path, &ctx);
            self.desktop.tile_new(nid, anchor);
            // NOTE (phase-2 gap): act.kind is cosmetic in the mock — every kind
            // spawns a plain PowerShell shell; Claude/Codex spawn comes later.
            let _ = act.kind;
        }
    } else {
        self.desktop.show(ui, area, true, egui::Id::new("desktop"), false);
    }
```

- [ ] **Step 4: Gate the quit-on-deserted guard** — at `main.rs:398`:

```rust
    if self.started && !self.landing_enabled && self.desktop.deserted() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
```

- [ ] **Step 5: Build and verify both paths**

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 20
cargo test 2>&1 | Select-Object -Last 15         # dirpicker + landing suites green
$env:FOREMAN_LANDING = "1"; cargo run            # landing path
```
Expected: the FOREMAN wordmark renders with the tagline under it, the inline path field is focused and shows a gray ghost as you type, the three labelled icons (Claude/Codex/Terminal) render, Enter (or an icon) opens the field's directory as a project. Screenshot and `Read` the PNG. Then verify the default path is unchanged:

```powershell
Remove-Item Env:\FOREMAN_LANDING; cargo run       # still auto-opens cwd; closing last project quits
```

- [ ] **Step 6: Commit**

```powershell
git add src/main.rs
git commit -m "feat(landing): flag-gated empty-state landing wired into App"
```

---

## Self-Review

**Spec coverage:**
- Picker: editable path source-of-truth (A2), split/base_dir/completions/ghost pure seam (A1), ghost text + Tab-only accept (A2/A3), prefix match (A1), inline `show` + floating `show_modal` (A2), `current_dir` accessor (A2), is_dir + no-dead-field on Enter (A2/A3), Windows split tests + empty-completions safety (A1/A2), scrim (A2). ✓
- Landing: empty-state render (B3), figlet wordmark (B1/B2), inline picker hero (B2), Claude/Codex/Terminal icons via `icons::texture` (B2), `LandingAction{path,kind}` + `ICON_ORDER` (B1), flag gates only auto-project + quit (B3), `request_repaint` for animation (B2), provisional `SessionKind` (B1). ✓
- Phase-2 gaps left explicit: agent spawn per kind (B3 note). ✓

**Type consistency:** `Outcome` (unchanged, A2) consumed by `wm.rs` (A2 step 5) and `landing.rs` (B2). `DirPicker::{new, show, current_dir}` produced in A2, consumed in B2. `layout`/`ICON_ORDER`/`FOREMAN_ART`/`SessionKind`/`LandingAction` produced in B1, consumed in B2/B3. `icon_of`→`IconKind::{Claude,Codex,PowerShell}` matches `icons.rs`. Consistent.

**Known implementation-verify points (GUI, not unit-testable):** exact egui-0.34 spellings for `ui.new_child`/`UiBuilder::new().max_rect`, `ImageButton::new(&TextureHandle)`, `content_rect`, `consume_key`, `rect_stroke` args, and `Frame::popup` — confirm against the compiler and the `egui-immediate-mode-reference` skill during A2/B2; the surrounding logic and the pure seams are fixed.

## Execution Handoff

Two execution options:

1. **Subagent-Driven (recommended)** — a fresh subagent per task, two-stage review between tasks, fast iteration.
2. **Inline Execution** — execute tasks in this session with checkpoints for review.
