//! The Appearance settings pane: edits the live [`Theme`] with a split preview.
//!
//! The model here (`working`/`saved` + dirty/revert/presets) is pure and
//! unit-tested; the egui view ([`AppearanceView::show`]) draws the control
//! column + live preview and is verified by screenshot. `working` is the theme
//! being edited; `saved` is the last persisted state, so `is_dirty` and `revert`
//! need no extra bookkeeping.

use eframe::egui;

use crate::theme::Theme;

/// The built-in, read-only theme's name. User themes are everything else.
pub const BUILTIN: &str = "Foreman Warm";

/// What a `show` frame reports back to the settings shell.
pub enum Outcome {
    /// A control changed the working theme this frame (already copied into the
    /// out-theme; the shell reseeds it for live-apply + debounced persistence).
    Changed,
    /// Fork the active theme into a new editable user theme with this name.
    Duplicate(String),
    /// Back out to the rail (Esc/Tab).
    Close,
    /// Nothing happened this frame.
    Pending,
}

/// The Appearance pane's state.
pub struct AppearanceView {
    working: Theme,
    saved: Theme,
    active_name: String,
    /// Selectable presets: built-in first, then user themes (populated in the
    /// persistence phase).
    presets: Vec<String>,
}

impl AppearanceView {
    pub fn new() -> Self {
        Self {
            working: Theme::foreman_warm(),
            saved: Theme::foreman_warm(),
            active_name: BUILTIN.to_string(),
            presets: vec![BUILTIN.to_string()],
        }
    }

    /// Switch the pane to a theme: it becomes both the working copy and the
    /// clean baseline (so the pane opens non-dirty on the newly-active theme).
    pub fn set_active(&mut self, name: &str, theme: Theme) {
        self.active_name = name.to_string();
        self.saved = theme.clone();
        self.working = theme;
    }

    /// The currently-active theme name (matches `Settings.theme`).
    pub fn active_name(&self) -> &str {
        &self.active_name
    }

    /// Replace the preset list (built-in + user theme names).
    pub fn set_presets(&mut self, presets: Vec<String>) {
        self.presets = presets;
    }

    /// True while the built-in theme is active — its controls are read-only, so
    /// editing requires Duplicate first.
    pub fn active_is_builtin(&self) -> bool {
        self.active_name == BUILTIN
    }

    /// The theme being edited (also what the preview + live seam render).
    pub fn working(&self) -> &Theme {
        &self.working
    }

    /// Mutable access for the controls (and tests).
    pub fn working_mut(&mut self) -> &mut Theme {
        &mut self.working
    }

    /// True while edits diverge from the last persisted theme.
    pub fn is_dirty(&self) -> bool {
        self.working != self.saved
    }

    /// Discard edits back to the last persisted theme.
    pub fn revert(&mut self) {
        self.working = self.saved.clone();
    }

    /// Render the pane into `rect`. Task 14 draws the split layout + live
    /// preview and Task 15 the color pickers; this placeholder keeps the pane
    /// wired end-to-end (it appears in the rail and draws into its body).
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        reads_input: bool,
        out_theme: &mut Theme,
    ) -> Outcome {
        let _ = (reads_input, out_theme);
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Appearance",
            egui::FontId::proportional(14.0),
            crate::theme::live(ui.ctx()).dim,
        );
        Outcome::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirty_tracks_edits_and_revert_restores() {
        let mut v = AppearanceView::new();
        v.set_active(BUILTIN, Theme::foreman_warm());
        assert!(!v.is_dirty(), "freshly-activated theme is clean");
        v.working_mut().bg = egui::Color32::from_rgb(9, 9, 9);
        assert!(v.is_dirty(), "an edit makes it dirty");
        v.revert();
        assert!(!v.is_dirty(), "revert restores the saved theme");
        assert_eq!(v.working().bg, Theme::foreman_warm().bg);
    }

    #[test]
    fn builtin_is_active_by_default() {
        let v = AppearanceView::new();
        assert!(v.active_is_builtin());
        assert_eq!(v.active_name(), BUILTIN);
    }
}
