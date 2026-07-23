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
    /// The user picked a different preset — the shell switches settings.theme.
    SelectPreset(String),
    /// Rename the active user theme to this name.
    Rename(String),
    /// Delete the active user theme (name given).
    Delete(String),
    /// Nothing happened this frame.
    Pending,
}

/// The Appearance pane's state.
#[derive(Clone, Debug)]
pub struct AppearanceView {
    working: Theme,
    saved: Theme,
    active_name: String,
    /// Editable buffer for the active user theme's name (the rename field), synced
    /// to `active_name` on `set_active`.
    name_edit: String,
    /// Selectable presets: built-in first, then user themes.
    presets: Vec<String>,
    /// True while the delete-confirmation modal is open.
    confirm_delete: bool,
}

impl AppearanceView {
    pub fn new() -> Self {
        let mut v = Self {
            working: Theme::foreman_warm(),
            saved: Theme::foreman_warm(),
            active_name: BUILTIN.to_string(),
            name_edit: BUILTIN.to_string(),
            presets: vec![BUILTIN.to_string()],
            confirm_delete: false,
        };
        v.refresh_presets();
        v
    }

    /// Switch the pane to a theme: it becomes both the working copy and the
    /// clean baseline (so the pane opens non-dirty on the newly-active theme).
    pub fn set_active(&mut self, name: &str, theme: Theme) {
        self.active_name = name.to_string();
        self.name_edit = name.to_string();
        self.saved = theme.clone();
        self.working = theme;
        self.refresh_presets();
    }

    /// Rebuild the preset list: the built-in first, then the user theme files
    /// (sorted — `read_dir` order is unstable).
    fn refresh_presets(&mut self) {
        let mut users: Vec<String> = Theme::user_theme_names()
            .into_iter()
            .filter(|n| n != BUILTIN)
            .collect();
        users.sort();
        let mut presets = vec![BUILTIN.to_string()];
        presets.extend(users);
        self.presets = presets;
    }

    /// A unique slug for a fork of the active theme, so an auto-fork (or an
    /// explicit Duplicate) never clobbers an existing user theme file.
    fn fork_name(&self) -> String {
        let existing: std::collections::HashSet<String> =
            Theme::user_theme_names().into_iter().collect();
        let base = crate::theme::slug(&format!("{} copy", self.active_name));
        if !existing.contains(&base) {
            return base;
        }
        (2..)
            .map(|n| format!("{base}-{n}"))
            .find(|c| !existing.contains(c))
            .unwrap_or(base)
    }

    /// The currently-active theme name (matches `Settings.theme`).
    pub fn active_name(&self) -> &str {
        &self.active_name
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
        // they need no reads_input gate; keyboard is handled by the settings shell.
        let _ = reads_input;
        let t = self.working.clone();
        let pad = 12.0;
        let inner = rect.shrink(pad);
        // Responsive split: controls and preview each take ~half and BOTH grow
        // with the window (the controls' swatches/name/palette fill their column,
        // so the left side isn't dead space); the controls scroll if too tall.
        let controls_w = (inner.width() * 0.5).clamp(240.0, (inner.width() - 200.0).max(240.0));
        let controls_rect =
            egui::Rect::from_min_size(inner.min, egui::vec2(controls_w, inner.height()));
        let preview_rect = egui::Rect::from_min_max(
            egui::pos2(inner.left() + controls_w + pad, inner.top()),
            inner.max,
        );

        Self::paint_preview(ui, preview_rect, &t);

        let mut changed = false;
        let mut duplicate: Option<String> = None;
        let mut preset_switch: Option<String> = None;
        let mut rename: Option<String> = None;
        let mut delete: Option<String> = None;
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(controls_rect));
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(&mut child, |ui| {
                // Preset switcher — choosing another asks the shell to change
                // settings.theme; the App reloads and the pane resyncs.
                let mut selected = self.active_name.clone();
                egui::ComboBox::from_id_salt("appearance_preset")
                    .width(ui.available_width().min(220.0))
                    .selected_text(&self.active_name)
                    .show_ui(ui, |ui| {
                        for p in &self.presets {
                            ui.selectable_value(&mut selected, p.clone(), p.as_str());
                        }
                    });
                if selected != self.active_name {
                    preset_switch = Some(selected);
                }
                if self.active_is_builtin() {
                    ui.label(
                        egui::RichText::new("Built-in — edits save as a copy")
                            .size(12.0)
                            .color(t.dim),
                    );
                } else {
                    // Rename the active user theme: commit on Enter / focus-loss.
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Name").size(12.0).color(t.dim));
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.name_edit)
                                .desired_width((ui.available_width() - 28.0).max(60.0)),
                        );
                        if resp.lost_focus()
                            && !self.name_edit.trim().is_empty()
                            && crate::theme::slug(&self.name_edit) != self.active_name
                        {
                            rename = Some(self.name_edit.clone());
                        }
                        if ui.button("–").on_hover_text("Delete this theme").clicked() {
                            self.confirm_delete = true;
                        }
                    });
                }
                ui.add_space(8.0);

                // Named UI/terminal colors — each a full-width swatch bar (fills
                // the column, big click target). Editing the built-in forks a copy.
                changed |= opaque_row(ui, "Background", &mut self.working.bg);
                changed |= opaque_row(ui, "Foreground", &mut self.working.fg);
                changed |= translucent_row(ui, "Selection", &mut self.working.selection);
                changed |= opaque_row(ui, "Focus border", &mut self.working.border_focus);
                changed |= translucent_row(ui, "Cursor", &mut self.working.caret);

                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("Terminal palette")
                        .size(12.0)
                        .color(t.dim),
                );
                ui.label(
                    egui::RichText::new(
                        "The 16 colors terminal programs use for colored output. \
                         Hover a swatch to name it.",
                    )
                    .size(11.0)
                    .color(t.dim),
                );
                ui.add_space(2.0);
                const NAMES: [&str; 16] = [
                    "Black",
                    "Red",
                    "Green",
                    "Yellow",
                    "Blue",
                    "Magenta",
                    "Cyan",
                    "White",
                    "Bright Black",
                    "Bright Red",
                    "Bright Green",
                    "Bright Yellow",
                    "Bright Blue",
                    "Bright Magenta",
                    "Bright Cyan",
                    "Bright White",
                ];
                let gap = 5.0;
                let label_w = 44.0;
                let sw = ((ui.available_width() - label_w - gap * 8.0) / 8.0).clamp(16.0, 64.0);
                let sh = (sw * 0.6).clamp(14.0, 30.0);
                let mut palette = self.working.palette;
                let prev_pal = ui.spacing().interact_size;
                ui.spacing_mut().interact_size = egui::vec2(sw, sh);
                egui::Grid::new("appearance_palette")
                    .spacing([gap, gap])
                    .show(ui, |ui| {
                        for (base, label) in [(0usize, "Base"), (8usize, "Bright")] {
                            ui.label(egui::RichText::new(label).size(11.0).color(t.dim));
                            for c in 0..8usize {
                                let i = base + c;
                                ui.push_id(i, |ui| {
                                    let mut rgb = [palette[i].r(), palette[i].g(), palette[i].b()];
                                    if ui
                                        .color_edit_button_srgb(&mut rgb)
                                        .on_hover_text(format!("{} · color {i}", NAMES[i]))
                                        .changed()
                                    {
                                        palette[i] =
                                            egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                                    }
                                });
                            }
                            ui.end_row();
                        }
                    });
                ui.spacing_mut().interact_size = prev_pal;
                if palette != self.working.palette {
                    self.working.palette = palette;
                    changed = true;
                }

                ui.add_space(8.0);
                // Font size shares the Ctrl+Scroll zoom seam (a Settings field).
                let mut fs = crate::terminal::font_size(ui.ctx());
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Font size").color(t.dim));
                    if ui.small_button("–").clicked() {
                        fs = (fs - 1.0).max(8.0);
                    }
                    ui.label(format!("{fs:.0}"));
                    if ui.small_button("+").clicked() {
                        fs = (fs + 1.0).min(40.0);
                    }
                });
                crate::terminal::set_font_size(ui.ctx(), fs);

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Duplicate…").clicked() {
                        duplicate = Some(self.fork_name());
                    }
                    if self.is_dirty() && ui.button("Revert").clicked() {
                        self.revert();
                        changed = true;
                    }
                });
            });

        // Delete-confirmation modal (opened by the − button on a user theme).
        if self.confirm_delete {
            let m =
                egui::Modal::new(egui::Id::new("appearance_delete_confirm")).show(ui.ctx(), |ui| {
                    ui.set_width(280.0);
                    ui.strong("Delete theme?");
                    ui.add_space(4.0);
                    ui.label(format!(
                        "Permanently delete \u{201c}{}\u{201d}?",
                        self.active_name
                    ));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("Delete").clicked() {
                            delete = Some(self.active_name.clone());
                            self.confirm_delete = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.confirm_delete = false;
                        }
                    });
                });
            if m.should_close() {
                self.confirm_delete = false;
            }
        }

        if let Some(name) = delete {
            return Outcome::Delete(name);
        }
        // Editing the built-in transparently forks an editable user copy — the
        // built-in stays a pristine preset you can switch back to.
        if changed && self.active_is_builtin() {
            *out_theme = self.working.clone();
            return Outcome::Duplicate(self.fork_name());
        }
        if let Some(name) = duplicate {
            return Outcome::Duplicate(name);
        }
        if let Some(name) = preset_switch {
            return Outcome::SelectPreset(name);
        }
        if let Some(name) = rename {
            return Outcome::Rename(name);
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
        let line = |segs: &[(&str, egui::Color32)], y: f32| {
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
/// Fixed-width label so every swatch bar starts at the same x (aligned rows).
fn color_row_label(ui: &mut egui::Ui, label: &str) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(90.0, 22.0), egui::Sense::hover());
    ui.painter().text(
        r.left_center(),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(13.0),
        ui.visuals().text_color(),
    );
}

fn opaque_row(ui: &mut egui::Ui, label: &str, c: &mut egui::Color32) -> bool {
    ui.horizontal(|ui| {
        color_row_label(ui, label);
        // Full-width swatch bar: fills the rest of the row (reactive + big target).
        ui.spacing_mut().interact_size = egui::vec2(ui.available_width().max(24.0), 22.0);
        let mut rgb = [c.r(), c.g(), c.b()];
        let ch = ui.color_edit_button_srgb(&mut rgb).changed();
        if ch {
            *c = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
        }
        ch
    })
    .inner
}

/// One picker row for a TRANSLUCENT token, edited in STRAIGHT (un-premultiplied)
/// alpha via `color_edit_button_srgba_unmultiplied` — the egui path that avoids
/// the low-alpha premultiplied round-trip drift.
fn translucent_row(ui: &mut egui::Ui, label: &str, c: &mut egui::Color32) -> bool {
    ui.horizontal(|ui| {
        color_row_label(ui, label);
        ui.spacing_mut().interact_size = egui::vec2(ui.available_width().max(24.0), 22.0);
        let mut a = c.to_srgba_unmultiplied();
        let ch = ui.color_edit_button_srgba_unmultiplied(&mut a).changed();
        if ch {
            *c = egui::Color32::from_rgba_unmultiplied(a[0], a[1], a[2], a[3]);
        }
        ch
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
