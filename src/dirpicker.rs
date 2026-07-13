use eframe::egui;
use std::path::{Path, PathBuf};

use crate::theme::{BORDER, BORDER_FOCUS, DANGER, DESK_BG, DIM, TEXT};

fn is_sep(c: char) -> bool {
    c == '/' || c == '\\'
}

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
                buf[..=i].to_string() // "/x" → "/"
            } else if head.ends_with(':') {
                format!("{head}{sep}") // r"C:\Us" → r"C:\"
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
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            root.join(p)
        }
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
    if partial.is_empty() {
        return None;
    }
    let name = highlighted?.file_name()?.to_str()?;
    let rest: String = name.chars().skip(partial.chars().count()).collect();
    Some(format!("{rest}{}", std::path::MAIN_SEPARATOR))
}

/// A row in the completion dropdown.
enum Row {
    Parent,
    Dir(PathBuf),
}

/// Result of rendering the picker for one frame.
pub enum Outcome {
    Pending,
    Cancelled,
    Accepted(PathBuf),
    /// ↓ past the last dropdown row: the popup closed and keyboard focus
    /// should continue to whatever sits below the field (landing: buttons).
    PassedEnd,
}

/// Address-bar directory picker: the editable `path` buffer is the source of
/// truth; completion shows as a ghost + a dropdown driven by `selected`.
pub struct DirPicker {
    path: String,
    selected: usize,
    root: PathBuf,
    focus_next: bool,
    invalid: bool,
    /// Whether the dropdown is showing and the field wants focus. Esc collapses
    /// it (hiding the dropdown, revealing anything behind); a click reopens it.
    open: bool,
    /// Landing mode: the field is focused but the dropdown stays closed until the
    /// user's first interaction (typing, a click, or a key press). Merely gaining
    /// focus does not open it — we requested that focus ourselves. Set by
    /// `reopen`, cleared the moment we open.
    armed: bool,
    /// Set on keyboard navigation so the dropdown scrolls the highlight into
    /// view next frame; cleared after rendering.
    scroll_to_sel: bool,
    /// Set when tree navigation (arrows or a row click) rewrites the buffer, so
    /// the field's caret is pinned back to the end before it draws — the path
    /// stays fully visible and the ghost completion always appends at the tail.
    caret_to_end: bool,
}

impl DirPicker {
    pub fn new(start: PathBuf) -> Self {
        let mut path = start.display().to_string();
        if !path.ends_with(is_sep) {
            path.push(std::path::MAIN_SEPARATOR);
        }
        let mut p = Self {
            path,
            selected: 0,
            root: start,
            focus_next: true,
            invalid: false,
            open: true,
            armed: false,
            scroll_to_sel: false,
            caret_to_end: false,
        };
        p.reseed();
        p
    }

    /// Re-arm for the landing: focus the field but keep the dropdown closed until
    /// the user's first interaction. Used when the landing (re)appears — the same
    /// `Landing`/`DirPicker` lives for the app's lifetime, so `focus_next` would
    /// otherwise be spent after the first show.
    pub fn reopen(&mut self) {
        self.open = false;
        self.armed = true;
        self.focus_next = true;
        self.invalid = false;
    }

    /// Open the completion popup programmatically (landing: ↓ in the field).
    pub fn open_dropdown(&mut self) {
        self.open = true;
        self.armed = false;
        self.focus_next = true;
    }

    /// Ask the field to (re)take keyboard focus on its next render (landing:
    /// the zone cursor arrived on the field, so typing must work immediately).
    pub fn focus_field(&mut self) {
        self.focus_next = true;
    }

    /// Accept the field's current path from outside the popup (landing: Enter
    /// in the field zone). Paints the invalid cue on failure, exactly like
    /// Enter with the popup open.
    pub fn accept_or_flag(&mut self) -> Option<PathBuf> {
        let dir = self.accept();
        if dir.is_none() {
            self.invalid = true;
        }
        dir
    }

    // --- pure-ish derivations (real fs via list_dirs) ---

    fn base_and_partial(&self) -> (PathBuf, String) {
        let (base, partial) = split(&self.path);
        (base_dir(&base, &self.root), partial)
    }

    fn rows(&self) -> Vec<Row> {
        let (base, partial) = self.base_and_partial();
        let mut out = Vec::new();
        if base.parent().is_some() {
            out.push(Row::Parent);
        }
        for d in completions(&base, &partial, &list_dirs) {
            out.push(Row::Dir(d));
        }
        out
    }

    /// The highlighted dir, if `selected` points at a `Dir` row. Consumed by the
    /// inline ghost text (`ghost_text`) and by the test accessors below.
    fn highlighted(&self) -> Option<PathBuf> {
        match self.rows().into_iter().nth(self.selected) {
            Some(Row::Dir(p)) => Some(p),
            _ => None,
        }
    }

    /// The ghost suffix for the current field: the highlighted match's remainder.
    fn ghost_text(&self) -> Option<String> {
        let (_, partial) = self.base_and_partial();
        ghost(&partial, self.highlighted().as_deref())
    }

    /// Resolve the whole buffer to a path (relative → against root).
    fn resolve(&self) -> PathBuf {
        let p = Path::new(&self.path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.root.join(p)
        }
    }

    // --- state transitions ---

    fn reseed(&mut self) {
        let rows = self.rows();
        self.selected = rows
            .iter()
            .position(|r| matches!(r, Row::Dir(_)))
            .unwrap_or(0);
        self.invalid = false;
    }

    pub fn set_path(&mut self, new: String) {
        self.path = new;
        self.reseed();
    }

    pub fn move_down(&mut self) {
        let n = self.rows().len();
        if n > 0 && self.selected + 1 < n {
            self.selected += 1;
        }
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Mouse click on a row: drill into the highlighted dir, or climb on `../`.
    pub fn complete(&mut self) {
        match self.rows().into_iter().nth(self.selected) {
            Some(Row::Parent) => self.go_parent(),
            Some(Row::Dir(p)) => self.set_path(with_sep(&p)),
            None => {}
        }
    }

    /// ← : climb to the parent of the current folder, regardless of the
    /// highlight (file-browser model — Left always goes up).
    pub fn go_parent(&mut self) {
        let (base, _) = self.base_and_partial();
        if let Some(parent) = base.parent() {
            self.set_path(with_sep(parent));
        }
    }

    /// → : descend into the highlighted directory. No-op on the `../` row (use
    /// ← to go up) — Right always goes into a child.
    pub fn go_child(&mut self) {
        if let Some(dir) = self.highlighted() {
            self.set_path(with_sep(&dir));
        }
    }

    /// Enter: the buffer resolved to an existing directory, else None.
    pub fn current_dir(&self) -> Option<PathBuf> {
        let p = self.resolve();
        p.is_dir().then_some(p)
    }

    fn accept(&self) -> Option<PathBuf> {
        self.current_dir()
    }

    // --- test-facing accessors ---
    #[cfg(test)]
    fn selected(&self) -> usize {
        self.selected
    }
    fn rows_len(&self) -> usize {
        self.rows().len()
    }
    #[cfg(test)]
    fn select(&mut self, i: usize) {
        self.selected = i;
    }
    #[cfg(test)]
    fn highlighted_name(&self) -> Option<String> {
        self.highlighted()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
    }
    /// Whether the completion popup is showing (and therefore owns Tab/arrows).
    pub fn is_open(&self) -> bool {
        self.open
    }
    #[cfg(test)]
    fn is_armed(&self) -> bool {
        self.armed
    }
}

/// A path's display string with a guaranteed trailing separator.
fn with_sep(p: &Path) -> String {
    let mut s = p.display().to_string();
    if !s.ends_with(is_sep) {
        s.push(std::path::MAIN_SEPARATOR);
    }
    s
}

/// Dropdown vertical rule: grow with the number of rows, never past this cap.
/// Independent of field Y / free space under the field (that made landing vs
/// the `+` modal disagree). Horizontal stays the field width.
const DROPDOWN_MAX_H: f32 = 280.0;

impl DirPicker {
    /// Inline render: field + dropdown into the current `ui`. Placement-agnostic.
    /// Landing uses this so ↓ past the last row can exit downward onto icons.
    pub fn show(&mut self, ui: &mut egui::Ui) -> Outcome {
        self.show_inner(ui, true)
    }

    /// `exit_down`: landing wants ↓ past the last row to close the popup
    /// (`PassedEnd`); the `+` project modal clamps on the last row instead.
    fn show_inner(&mut self, ui: &mut egui::Ui, exit_down: bool) -> Outcome {
        let id = egui::Id::new("dirpicker-field");

        let mut outcome = Outcome::Pending;

        // Navigation keys are intercepted BEFORE the TextEdit sees them — but
        // only while open, so a collapsed field (post-Esc) leaves keys alone and
        // no sibling widget is starved of them.
        if self.open {
            // File-browser model: ←/→ drive the tree (not the text caret), so
            // they are always consumed. ← up a dir, → into the highlighted dir,
            // ↑/↓ move the highlight. Edit the path with Backspace/Home/End/click.
            let (up, down, left, right, enter, esc) = ui.input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, egui::Key::Tab); // eat Tab: no focus escape
                (
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
                )
            });
            // ↑ on the top row closes the dropdown, same as Esc.
            let esc = esc || (up && self.selected == 0);
            let up = up && self.selected > 0;
            if up || down || left || right {
                self.caret_to_end = true;
            }
            if up {
                self.move_up();
                self.scroll_to_sel = true;
            }
            // ↓ past the last row: landing exits downward; modal clamps (move_down).
            let past_end = exit_down && down && self.selected + 1 >= self.rows_len();
            if down && !past_end {
                self.move_down();
                self.scroll_to_sel = true;
            }
            if left {
                self.go_parent(); // ← up a directory, always
                self.scroll_to_sel = true;
            }
            if right {
                self.go_child(); // → into the highlighted directory
                self.scroll_to_sel = true;
            }
            if enter {
                match self.accept() {
                    Some(p) => return Outcome::Accepted(p),
                    None => self.invalid = true, // A3 paints the cue; Enter is consumed so focus is kept
                }
            }
            if esc {
                // Collapse, not cancel: the leader modal turns this Cancelled
                // into dropping the picker; the landing ignores it, leaving the
                // dropdown hidden and the field defocused so its icons show.
                self.open = false;
                ui.memory_mut(|m| m.surrender_focus(id));
                outcome = Outcome::Cancelled;
            }
            if past_end {
                self.open = false;
                ui.memory_mut(|m| m.surrender_focus(id));
                outcome = Outcome::PassedEnd;
            }
        }

        // Pin the caret to the end of the (possibly rewritten) buffer before the
        // field draws, so navigating the tree never strands it mid-path.
        if std::mem::take(&mut self.caret_to_end) {
            let end = egui::text::CCursor::new(self.path.chars().count());
            let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(end)));
            egui::TextEdit::store_state(ui.ctx(), id, state);
        }

        // Field (always drawn — a collapsed field is still visible and clickable).
        let font = egui::FontId::monospace(13.0);
        let field_h = 26.0;
        let field_rect = {
            let r = ui.max_rect();
            egui::Rect::from_min_size(r.min, egui::vec2(r.width().min(520.0), field_h))
        };
        let border = if self.invalid {
            DANGER
        } else if self.open || self.armed {
            BORDER_FOCUS
        } else {
            BORDER
        };
        ui.painter()
            .rect_filled(field_rect, egui::CornerRadius::same(3), DESK_BG);
        ui.painter().rect_stroke(
            field_rect,
            egui::CornerRadius::same(3),
            egui::Stroke::new(1.0, border),
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
        // Keep the field focused the whole time the picker is open or armed, so
        // typing and the ←/→/↑/↓ handlers never desync from a click that stole
        // focus (e.g. a mouse click on a dropdown row).
        if self.focus_next || ((self.open || self.armed) && !te.has_focus()) {
            te.request_focus();
            self.focus_next = false;
        }
        if te.changed() {
            self.open = true; // typing re-derives, and opens if armed
            self.armed = false;
            self.reseed();
        } else if self.armed {
            // Armed (landing): open on the first real interaction — a click or a
            // key press while focused — but NOT on the focus we requested above.
            let key_pressed = te.has_focus()
                && ui.input(|i| {
                    i.events.iter().any(|e| {
                        matches!(
                            e,
                            egui::Event::Key { pressed: true, .. }
                                | egui::Event::Text(_)
                                | egui::Event::Paste(_)
                        )
                    })
                });
            if te.clicked() || key_pressed {
                self.open = true;
                self.armed = false;
            }
        } else if te.gained_focus() && ui.input(|i| i.pointer.any_down()) {
            // Clicking a collapsed field reopens it. Pointer-gained focus only:
            // egui's Tab focus-traversal also lands focus here (it runs at the
            // raw-input layer, before consume_key can eat the Tab), and that
            // must not pop the dropdown open — the landing's recents zone owns
            // that Tab press.
            self.open = true;
        }

        if self.open {
            // Inline ghost: the highlighted match's remainder, painted in DIM
            // right after the typed text (mono font → measured width aligns it).
            if let Some(g) = self.ghost_text() {
                let text_w = ui
                    .painter()
                    .layout_no_wrap(self.path.clone(), font.clone(), TEXT)
                    .rect
                    .width();
                let x = field_rect.min.x + 6.0 + text_w; // 6.0 == field margin
                ui.painter().text(
                    egui::pos2(x, field_rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    g,
                    font.clone(),
                    DIM,
                );
            }

            // Dropdown under the field.
            // Vertical rule (explicit, not "space under field"):
            //   height = min((dirs + 1 for ../) * row_h, DROPDOWN_MAX_H), scroll.
            // Horizontal: field width.
            // constrain(false): mid-screen landing must not relocate the Area.
            let rows = self.rows();
            let row_h = ui.spacing().interact_size.y;
            // rows already includes Parent when the base has one; still count
            // height as directories + 1 so the '../' row always has a slot (and so a
            // slightly short row_h never clips the last line).
            let n_dirs = rows.iter().filter(|r| matches!(r, Row::Dir(_))).count();
            let n_rows = n_dirs + 1; // +1 for ../
            let drop_h = if rows.is_empty() {
                0.0
            } else {
                ((n_rows as f32) * row_h).min(DROPDOWN_MAX_H)
            };
            let mut clicked: Option<usize> = None;
            if drop_h > 0.0 {
                egui::Area::new(id.with("drop"))
                    .fixed_pos(field_rect.left_bottom() + egui::vec2(0.0, 2.0))
                    .order(egui::Order::Foreground)
                    .constrain(false)
                    .movable(false)
                    .default_size(egui::vec2(field_rect.width(), drop_h))
                    .show(ui.ctx(), |ui| {
                        ui.set_max_width(field_rect.width());
                        ui.set_min_width(field_rect.width());
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            // Exact height from row count (capped). Do not rely on
                            // ScrollArea auto-shrink alone — Area default/memory
                            // sizing was leaving the list stuck at max height.
                            egui::ScrollArea::vertical()
                                .max_height(drop_h)
                                .min_scrolled_height(drop_h)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    ui.set_min_width(field_rect.width() - 12.0);
                                    for (idx, row) in rows.into_iter().enumerate() {
                                        let label = match &row {
                                            Row::Parent => "../".to_string(),
                                            Row::Dir(p) => p
                                                .file_name()
                                                .unwrap_or_default()
                                                .to_string_lossy()
                                                .into_owned(),
                                        };
                                        let resp = ui.selectable_label(idx == self.selected, label);
                                        if resp.clicked() {
                                            clicked = Some(idx);
                                        }
                                        if idx == self.selected && self.scroll_to_sel {
                                            resp.scroll_to_me(Some(egui::Align::Center));
                                        }
                                    }
                                });
                        });
                    });
            }
            if let Some(idx) = clicked {
                self.selected = idx;
                self.complete();
                self.caret_to_end = true; // buffer rewritten — re-pin next frame
            }
        }

        self.scroll_to_sel = false;
        outcome
    }

    /// Leader / "+" project modal: top-center floating Area with a scrim.
    /// ↓ clamps on the last row (does not collapse the dropdown).
    pub fn show_modal(&mut self, ui: &mut egui::Ui) -> Outcome {
        let screen = ui.ctx().content_rect();
        ui.painter()
            .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(90)); // lighter than 150
        let mut outcome = Outcome::Pending;
        egui::Area::new(egui::Id::new("dirpicker-modal"))
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 16.0))
            .show(ui.ctx(), |ui| {
                ui.set_max_width(520.0);
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    outcome = self.show_inner(ui, false);
                });
            });
        outcome
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

    // Build a picker whose buffer points at `dir` (trailing sep), tests via real fs.
    fn at(dir: &Path) -> DirPicker {
        DirPicker::new(dir.to_path_buf())
    }

    #[test]
    fn reopen_arms_focused_with_dropdown_closed() {
        let d = tree();
        let mut p = at(d.path());
        // A freshly-built picker (the leader modal path) opens its dropdown.
        assert!(p.is_open() && !p.is_armed());
        // The landing re-arms it: focused, but the dropdown stays closed until
        // the user's first interaction.
        p.reopen();
        assert!(!p.is_open() && p.is_armed());
    }

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
        p.set_path(format!(
            "{}{}be",
            d.path().display(),
            std::path::MAIN_SEPARATOR
        ));
        // rows = [Parent, beta]; highlight reseeds to beta.
        assert_eq!(p.highlighted_name(), Some("beta".to_string()));
    }

    #[test]
    fn tab_completes_into_the_highlighted_dir() {
        let d = tree();
        let mut p = at(d.path());
        p.set_path(format!(
            "{}{}be",
            d.path().display(),
            std::path::MAIN_SEPARATOR
        ));
        p.complete(); // Tab
        assert_eq!(p.current_dir(), Some(d.path().join("beta")));
        assert_eq!(p.highlighted_name(), Some("inner".to_string())); // now inside beta
    }

    #[test]
    fn parent_row_climbs() {
        let d = tree();
        let mut p = at(&d.path().join("beta"));
        p.select(0); // Parent row
        p.complete();
        assert_eq!(p.current_dir(), Some(d.path().to_path_buf()));
    }

    #[test]
    fn left_climbs_and_right_descends_regardless_of_highlight() {
        let d = tree();
        // ← climbs to the parent even though a child (not `../`) is highlighted.
        let mut p = at(&d.path().join("beta"));
        assert_eq!(p.highlighted_name(), Some("inner".to_string()));
        p.go_parent();
        assert_eq!(p.current_dir(), Some(d.path().to_path_buf()));
        // → descends into the highlighted child.
        p.set_path(format!(
            "{}{}be",
            d.path().display(),
            std::path::MAIN_SEPARATOR
        ));
        p.go_child();
        assert_eq!(p.current_dir(), Some(d.path().join("beta")));
    }

    #[test]
    fn accept_only_for_an_existing_directory() {
        let d = tree();
        let mut p = at(d.path());
        assert!(matches!(p.accept(), Some(_))); // a real dir
        p.set_path(format!(
            "{}{}zzz",
            d.path().display(),
            std::path::MAIN_SEPARATOR
        ));
        assert_eq!(p.accept(), None); // missing path
        p.set_path(d.path().join("file.txt").display().to_string());
        assert_eq!(p.accept(), None); // a file, not a dir
    }

    #[test]
    fn empty_completions_are_panic_free() {
        let d = tree();
        let mut p = at(d.path());
        p.set_path(format!(
            "{}{}zzz",
            d.path().display(),
            std::path::MAIN_SEPARATOR
        )); // matches nothing
        p.move_down();
        p.move_up();
        p.complete(); // must not panic
        let _ = p.accept();
        assert!(p.selected() < p.rows_len().max(1));
    }

    #[test]
    fn current_dir_none_for_partial_some_for_dir() {
        let d = tree();
        let mut p = at(d.path());
        assert_eq!(p.current_dir(), Some(d.path().to_path_buf()));
        p.set_path(format!(
            "{}{}al",
            d.path().display(),
            std::path::MAIN_SEPARATOR
        ));
        assert_eq!(p.current_dir(), None); // "…/al" is a partial, not a dir
    }

    #[test]
    fn split_lexical_posix_and_windows() {
        assert_eq!(split("/a/b/c"), ("/a/b".into(), "c".into()));
        assert_eq!(split("/a/b/"), ("/a/b".into(), "".into()));
        assert_eq!(split("/x"), ("/".into(), "x".into())); // leading-sep root keeps its sep
        assert_eq!(split(""), ("".into(), "".into()));
        assert_eq!(split("foreman"), ("".into(), "foreman".into())); // no sep: all partial
        assert_eq!(split(r"C:\Us"), (r"C:\".into(), "Us".into())); // drive root keeps its sep
        assert_eq!(split(r"C:\"), (r"C:\".into(), "".into()));
        assert_eq!(split(r"C:\Users\"), (r"C:\Users".into(), "".into()));
        // Pinned rules for the ambiguous cases (documented degradation):
        assert_eq!(split("C:"), ("".into(), "C:".into())); // bare drive → treated as a partial
        assert_eq!(split(r"\\srv\share"), (r"\\srv".into(), "share".into())); // UNC: base is bare \\srv
    }

    #[test]
    fn base_dir_resolves_relative_against_root() {
        let root = Path::new("/root");
        assert_eq!(base_dir("", root), PathBuf::from("/root")); // empty → root
        assert_eq!(base_dir("sub", root), PathBuf::from("/root/sub")); // relative → joined
    }

    #[test]
    fn completions_is_case_insensitive_prefix_over_the_lister() {
        let lister = |_: &Path| {
            ["foreman", "formats", "platform"]
                .iter()
                .map(|n| PathBuf::from("/x").join(n))
                .collect()
        };
        let got: Vec<String> = completions(Path::new("/x"), "FoR", &lister)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(got, vec!["foreman", "formats"]); // prefix only, case-insensitive; no "platform"
    }

    #[test]
    fn ghost_is_the_real_names_remainder_case_preserving() {
        let hl = PathBuf::from("/x/foreman");
        assert_eq!(
            ghost("for", Some(&hl)),
            Some(format!("eman{}", std::path::MAIN_SEPARATOR))
        );
        assert_eq!(
            ghost("FOR", Some(&hl)),
            Some(format!("eman{}", std::path::MAIN_SEPARATOR))
        ); // real casing
        assert_eq!(ghost("", Some(&hl)), None); // no partial → no ghost
        assert_eq!(ghost("for", None), None); // nothing highlighted
    }
}
