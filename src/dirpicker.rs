use eframe::egui;
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

    #[test]
    fn empty_filter_clamps_and_drill_is_safe() {
        let d = tree();
        let mut p = DirPicker::new(d.path().to_path_buf());
        p.move_down();
        p.move_down(); // highlight some non-zero row
        p.push_char('z'); // matches nothing
        assert_eq!(p.items().len(), 0);
        assert_eq!(p.selected(), 0); // clamp pinned to zero
        p.drill_in(); // must NOT panic on empty list
        p.move_down(); // must NOT panic / overflow on empty list
        assert_eq!(p.cwd(), d.path()); // drill_in on empty list was a no-op
    }
}
