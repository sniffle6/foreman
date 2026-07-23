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
        // Mouse-only pane: the color pickers are position-routed egui widgets, so
        // (unlike the keyboard-capturing Keybindings editor) they need no
        // reads_input gate; keyboard is handled by the settings shell.
        let _ = reads_input;
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

        // Left: interactive control column.
        let mut changed = false;
        let mut duplicate: Option<String> = None;
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(left));
        let lui = &mut child;
        lui.label(
            egui::RichText::new(&self.active_name)
                .size(15.0)
                .color(t.text),
        );
        lui.label(
            egui::RichText::new(if self.active_is_builtin() {
                "Built-in theme"
            } else {
                "User theme"
            })
            .size(12.0)
            .color(t.dim),
        );
        lui.add_space(8.0);

        changed |= opaque_row(lui, "Background", &mut self.working.bg);
        changed |= opaque_row(lui, "Foreground", &mut self.working.fg);
        changed |= translucent_row(lui, "Selection", &mut self.working.selection);
        changed |= opaque_row(lui, "Focus border", &mut self.working.border_focus);
        changed |= translucent_row(lui, "Cursor", &mut self.working.caret);

        lui.add_space(6.0);
        lui.label(egui::RichText::new("ANSI palette").size(12.0).color(t.dim));
        let mut palette = self.working.palette;
        egui::Grid::new("appearance_palette")
            .spacing([4.0, 4.0])
            .show(lui, |ui| {
                for i in 0..16usize {
                    ui.push_id(i, |ui| {
                        let mut rgb = [palette[i].r(), palette[i].g(), palette[i].b()];
                        if ui.color_edit_button_srgb(&mut rgb).changed() {
                            palette[i] = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                        }
                    });
                    if i % 8 == 7 {
                        ui.end_row();
                    }
                }
            });
        if palette != self.working.palette {
            self.working.palette = palette;
            changed = true;
        }

        lui.add_space(6.0);
        // Font size shares the Ctrl+Scroll zoom seam (a Settings field, not a theme
        // token) — the App reads it back and persists it separately.
        let mut fs = crate::terminal::font_size(lui.ctx());
        lui.horizontal(|ui| {
            ui.label(egui::RichText::new("Font size").color(t.dim));
            if ui.small_button("–").clicked() {
                fs = (fs - 1.0).max(8.0);
            }
            ui.label(format!("{fs:.0}"));
            if ui.small_button("+").clicked() {
                fs = (fs + 1.0).min(40.0);
            }
        });
        crate::terminal::set_font_size(lui.ctx(), fs);

        lui.add_space(8.0);
        lui.horizontal(|ui| {
            if ui.button("Duplicate…").clicked() {
                duplicate = Some(format!("{} copy", self.active_name));
            }
            if self.is_dirty() && ui.button("Revert to saved").clicked() {
                self.revert();
                changed = true;
            }
        });

        if let Some(name) = duplicate {
            return Outcome::Duplicate(name);
        }
        if changed {
            *out_theme = self.working.clone();
            return Outcome::Changed;
        }
        Outcome::Pending
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

/// One picker row for an OPAQUE token: a swatch button + label. Returns true if
/// the color changed this frame.
fn opaque_row(ui: &mut egui::Ui, label: &str, c: &mut egui::Color32) -> bool {
    ui.horizontal(|ui| {
        let mut rgb = [c.r(), c.g(), c.b()];
        let r = ui.color_edit_button_srgb(&mut rgb);
        if r.changed() {
            *c = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
        }
        ui.label(label);
        r.changed()
    })
    .inner
}

/// One picker row for a TRANSLUCENT token, edited in STRAIGHT (un-premultiplied)
/// alpha via `color_edit_button_srgba_unmultiplied` — the egui path that avoids
/// the low-alpha premultiplied round-trip drift.
fn translucent_row(ui: &mut egui::Ui, label: &str, c: &mut egui::Color32) -> bool {
    ui.horizontal(|ui| {
        let mut a = c.to_srgba_unmultiplied();
        let r = ui.color_edit_button_srgba_unmultiplied(&mut a);
        if r.changed() {
            *c = egui::Color32::from_rgba_unmultiplied(a[0], a[1], a[2], a[3]);
        }
        ui.label(label);
        r.changed()
    })
    .inner
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
