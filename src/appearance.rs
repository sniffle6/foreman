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
#[derive(Clone, Debug)]
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

    /// Render the split-preview pane into `rect`: a control column on the left
    /// and a sticky live preview (a fake terminal sample + the ANSI palette grid)
    /// on the right, both drawn with the *working* theme so edits show live. The
    /// color controls land in the next step; here we lay out the split, paint the
    /// preview, and offer "Revert to saved" while dirty.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        reads_input: bool,
        out_theme: &mut Theme,
    ) -> Outcome {
        let t = self.working.clone();
        let pad = 12.0;
        let split_x = rect.left() + (rect.width() * 0.5).max(rect.width() - 340.0);
        let left = egui::Rect::from_min_max(
            rect.min + egui::vec2(pad, pad),
            egui::pos2(split_x - pad, rect.bottom() - pad),
        );
        let right = egui::Rect::from_min_max(
            egui::pos2(split_x + pad, rect.top() + pad),
            rect.max - egui::vec2(pad, pad),
        );

        // Right: the sticky live preview (drawn with the working theme).
        Self::paint_preview(ui, right, &t);

        // Left: heading + (Revert while dirty). Controls arrive next step.
        let p = ui.painter();
        p.text(
            left.min,
            egui::Align2::LEFT_TOP,
            &self.active_name,
            egui::FontId::proportional(15.0),
            t.text,
        );
        p.text(
            left.min + egui::vec2(0.0, 24.0),
            egui::Align2::LEFT_TOP,
            if self.active_is_builtin() {
                "Built-in theme — Duplicate to customize"
            } else {
                "User theme"
            },
            egui::FontId::proportional(12.0),
            t.dim,
        );

        let mut outcome = Outcome::Pending;
        if self.is_dirty() {
            let btn = egui::Rect::from_min_size(
                egui::pos2(left.left(), left.bottom() - 26.0),
                egui::vec2(140.0, 24.0),
            );
            let resp = ui.interact(btn, ui.id().with("appearance_revert"), egui::Sense::click());
            let fill = if resp.hovered() {
                t.sel_bg
            } else {
                egui::Color32::TRANSPARENT
            };
            let p = ui.painter();
            p.rect_filled(btn, egui::CornerRadius::same(3), fill);
            p.rect_stroke(
                btn,
                egui::CornerRadius::same(3),
                egui::Stroke::new(1.0, t.border),
                egui::StrokeKind::Inside,
            );
            p.text(
                btn.center(),
                egui::Align2::CENTER_CENTER,
                "Revert to saved",
                egui::FontId::proportional(12.0),
                t.text,
            );
            if reads_input && resp.clicked() {
                self.revert();
                *out_theme = self.working.clone();
                outcome = Outcome::Changed;
            }
        }
        outcome
    }

    /// A self-contained mini-terminal sample rendered with `t` — NO PTY. Shows the
    /// surface, foreground text, several palette colors, a selection wash, the
    /// caret, and the full 16-swatch ANSI grid, so an edit is visible at a glance.
    fn paint_preview(ui: &egui::Ui, rect: egui::Rect, t: &Theme) {
        let p = ui.painter_at(rect);
        p.rect_filled(rect, egui::CornerRadius::same(4), t.bg);
        p.rect_stroke(
            rect,
            egui::CornerRadius::same(4),
            egui::Stroke::new(1.0, t.border_focus),
            egui::StrokeKind::Inside,
        );
        let mono = egui::FontId::monospace(13.0);
        let x0 = rect.left() + 10.0;
        let mut y = rect.top() + 10.0;
        let lh = 18.0;
        // Draw a left-to-right run of (text, color) segments on one line.
        let mut line = |segs: &[(&str, egui::Color32)], y: f32| {
            let mut x = x0;
            for (s, c) in segs {
                let r = p.text(
                    egui::pos2(x, y),
                    egui::Align2::LEFT_TOP,
                    *s,
                    mono.clone(),
                    *c,
                );
                x = r.right();
            }
        };
        line(
            &[
                ("andy", t.palette[2]),
                (":", t.fg),
                ("~/foreman", t.palette[4]),
                ("$ ", t.fg),
                ("git status", t.fg),
            ],
            y,
        );
        y += lh;
        line(&[("On branch ", t.fg), ("main", t.palette[2])], y);
        y += lh;
        line(&[("  modified: ", t.palette[3]), ("src/theme.rs", t.fg)], y);
        y += lh;
        // Selected line: wash then text.
        let sel_text = "  new file: src/appearance.rs";
        let sel_w = p
            .layout_no_wrap(sel_text.to_string(), mono.clone(), t.fg)
            .rect
            .width();
        p.rect_filled(
            egui::Rect::from_min_size(egui::pos2(x0, y), egui::vec2(sel_w, lh)),
            egui::CornerRadius::ZERO,
            t.selection,
        );
        line(&[(sel_text, t.fg)], y);
        y += lh;
        // Prompt + caret block.
        let after = p
            .text(
                egui::pos2(x0, y),
                egui::Align2::LEFT_TOP,
                "$ ",
                mono.clone(),
                t.fg,
            )
            .right();
        p.rect_filled(
            egui::Rect::from_min_size(egui::pos2(after, y), egui::vec2(8.0, lh - 3.0)),
            egui::CornerRadius::ZERO,
            t.caret,
        );

        // ANSI 16-swatch grid, two rows of eight, along the bottom.
        let cols = 8.0;
        let gap = 3.0;
        let sw = ((rect.width() - 20.0) - gap * (cols - 1.0)) / cols;
        let sh = 14.0;
        let gx = rect.left() + 10.0;
        let gy = rect.bottom() - (sh * 2.0 + gap) - 10.0;
        for i in 0..16 {
            let r = i / 8;
            let c = i % 8;
            let cell = egui::Rect::from_min_size(
                egui::pos2(gx + c as f32 * (sw + gap), gy + r as f32 * (sh + gap)),
                egui::vec2(sw, sh),
            );
            p.rect_filled(cell, egui::CornerRadius::same(2), t.palette[i as usize]);
        }
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
