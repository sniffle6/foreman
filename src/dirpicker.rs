use eframe::egui;
use std::path::{Path, PathBuf};

use crate::theme::{BORDER, DANGER, DESK_BG, DIM, TEXT};

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
}

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
        if !path.ends_with(is_sep) {
            path.push(std::path::MAIN_SEPARATOR);
        }
        let mut p = Self {
            path,
            selected: 0,
            root: start,
            focus_next: true,
            invalid: false,
        };
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

    /// Tab / click: drill into the highlighted dir, or climb on the Parent row.
    pub fn complete(&mut self) {
        match self.rows().into_iter().nth(self.selected) {
            Some(Row::Parent) => {
                let (base, _) = self.base_and_partial();
                if let Some(parent) = base.parent() {
                    self.set_path(with_sep(parent));
                }
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

    fn accept(&self) -> Option<PathBuf> {
        self.current_dir()
    }

    // --- test-facing accessors ---
    #[cfg(test)]
    fn selected(&self) -> usize {
        self.selected
    }
    #[cfg(test)]
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
}

/// A path's display string with a guaranteed trailing separator.
fn with_sep(p: &Path) -> String {
    let mut s = p.display().to_string();
    if !s.ends_with(is_sep) {
        s.push(std::path::MAIN_SEPARATOR);
    }
    s
}

impl DirPicker {
    /// Inline render: field + dropdown into the current `ui`. Placement-agnostic.
    pub fn show(&mut self, ui: &mut egui::Ui) -> Outcome {
        let id = egui::Id::new("dirpicker-field");

        // Intercept navigation keys BEFORE the TextEdit sees them.
        let (tab, up, down, enter, esc) = ui.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::Tab),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        if up {
            self.move_up();
        }
        if down {
            self.move_down();
        }
        if tab {
            self.complete();
        }
        if esc {
            return Outcome::Cancelled;
        }
        if enter {
            match self.accept() {
                Some(p) => return Outcome::Accepted(p),
                None => self.invalid = true, // Task A3 paints the cue; keep focus below
            }
        }

        // Field.
        let font = egui::FontId::monospace(13.0);
        let field_h = 26.0;
        let field_rect = {
            let r = ui.max_rect();
            egui::Rect::from_min_size(r.min, egui::vec2(r.width().min(520.0), field_h))
        };
        ui.painter()
            .rect_filled(field_rect, egui::CornerRadius::same(3), DESK_BG);
        ui.painter().rect_stroke(
            field_rect,
            egui::CornerRadius::same(3),
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
        if self.focus_next {
            te.request_focus();
            self.focus_next = false;
        }
        if te.changed() {
            self.reseed(); // typing re-derives
        }
        if enter && self.invalid {
            te.request_focus(); // never a dead field
        }

        // Inline ghost: the highlighted match's remainder, painted in DIM right
        // after the typed text (mono font → measured width lines it up exactly).
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

        // Dropdown, in a popup Area anchored under the field.
        let mut clicked: Option<usize> = None;
        egui::Area::new(id.with("drop"))
            .fixed_pos(field_rect.left_bottom() + egui::vec2(0.0, 2.0))
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                ui.set_max_width(field_rect.width());
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(280.0)
                        .show(ui, |ui| {
                            for (idx, row) in self.rows().into_iter().enumerate() {
                                let label = match &row {
                                    Row::Parent => "../".to_string(),
                                    Row::Dir(p) => p
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                        .into_owned(),
                                };
                                if ui.selectable_label(idx == self.selected, label).clicked() {
                                    clicked = Some(idx);
                                }
                            }
                        });
                });
            });
        if let Some(idx) = clicked {
            self.selected = idx;
            self.complete();
        }

        Outcome::Pending
    }

    /// Leader-invoked modal: `show` inside a top-center floating Area with a
    /// subtle scrim (the modality signal), replacing the old centered Window.
    pub fn show_modal(&mut self, ui: &mut egui::Ui) -> Outcome {
        let screen = ui.ctx().content_rect();
        ui.painter()
            .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(90)); // lighter than 150
        let mut outcome = Outcome::Pending;
        egui::Area::new(egui::Id::new("dirpicker-modal"))
            .anchor(
                egui::Align2::CENTER_TOP,
                egui::vec2(0.0, screen.height() * 0.18),
            )
            .show(ui.ctx(), |ui| {
                ui.set_max_width(520.0);
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    outcome = self.show(ui);
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
