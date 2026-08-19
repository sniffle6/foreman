//! The Appearance settings pane: edits the live [`Theme`] with a live preview.
//!
//! The model here (`working`/`saved` + dirty/revert/presets) is pure and
//! unit-tested; the egui view ([`AppearanceView::show`]) is responsive: a wide
//! pane lays the control form beside the live-preview terminal (side by side); a
//! tall/narrow pane stacks them (controls on top, terminal along the bottom, where
//! a terminal's landscape shape fits). The control form scrolls vertically when the
//! window is too short, and the terminal is always sized to fit its region, so
//! nothing overlaps or spills. Verified by screenshot. `working` is the theme being
//! edited; `saved` is the last persisted state, so `is_dirty` and `revert` need no
//! extra bookkeeping.

use eframe::egui;

use crate::theme::Theme;

/// The built-in, read-only theme's name. User themes are everything else.
pub const BUILTIN: &str = "Foreman Warm";

/// The control form's natural height — used to decide side-by-side vs stacked
/// (is there more room below the controls than to their right?).
const CONTROLS_H: f32 = 340.0;

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

/// What the control form reported this frame (collected, then folded into an
/// [`Outcome`] after the whole pane is drawn).
#[derive(Default)]
struct FormOut {
    changed: bool,
    preset_switch: Option<String>,
    rename: Option<String>,
    duplicate: Option<String>,
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

    /// Render the pane into `rect`, responsively: a wide pane puts the control form
    /// beside the live-preview terminal (a vertical divider between); a tall/narrow
    /// pane stacks them (controls on top, terminal along the bottom). The form
    /// scrolls vertically when short, with Duplicate/Revert pinned below it; the
    /// terminal always fits its region. Editing the built-in transparently forks an
    /// editable copy.
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
        let pad = 16.0;
        let inner = rect.shrink(pad);

        // Side-by-side vs stacked: stack when a bottom terminal would get more room
        // than a right-hand one (i.e. the pane is tall/narrow).
        let sidebar_w = (inner.width() * 0.34).clamp(240.0, 340.0);
        let right_room = inner.width() - sidebar_w;
        let bottom_room = inner.height() - CONTROLS_H;
        let stacked = bottom_room > right_room;

        let out = if stacked {
            // Controls on top (form capped + centred so wide rows don't stretch),
            // a horizontal divider, then the terminal filling the bottom.
            // Give the controls their full height when there's room (so the form
            // doesn't scroll while the terminal region sits half-empty), but never
            // more than ~60% of the pane, so the terminal keeps a fair share.
            let ctrl_h = 430.0_f32.min(inner.height() * 0.6).max(180.0);
            let fw = inner.width().min(480.0);
            let fx = inner.left() + (inner.width() - fw) * 0.5;
            let form_rect =
                egui::Rect::from_min_size(egui::pos2(fx, inner.top()), egui::vec2(fw, ctrl_h));
            let out = self.draw_form(ui, form_rect, &t);
            let divider_y = inner.top() + ctrl_h + pad * 0.5;
            ui.painter().hline(
                egui::Rangef::new(inner.left(), inner.right()),
                divider_y,
                egui::Stroke::new(1.0, t.border),
            );
            let term_region = egui::Rect::from_min_max(
                egui::pos2(inner.left(), divider_y + pad * 0.5),
                inner.max,
            );
            Self::draw_hero(ui, term_region, &t);
            out
        } else {
            // Control form on the left, terminal on the right, divider between.
            let divider_x = inner.left() + sidebar_w + pad;
            ui.painter().vline(
                divider_x,
                egui::Rangef::new(inner.top(), inner.bottom()),
                egui::Stroke::new(1.0, t.border),
            );
            let sidebar =
                egui::Rect::from_min_size(inner.min, egui::vec2(sidebar_w, inner.height()));
            let out = self.draw_form(ui, sidebar, &t);
            let hero =
                egui::Rect::from_min_max(egui::pos2(divider_x + pad, inner.top()), inner.max);
            Self::draw_hero(ui, hero, &t);
            out
        };

        // Delete-confirmation modal (opened by the − button on a user theme).
        let mut delete: Option<String> = None;
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
        if out.changed && self.active_is_builtin() {
            *out_theme = self.working.clone();
            return Outcome::Duplicate(self.fork_name());
        }
        if let Some(name) = out.duplicate {
            return Outcome::Duplicate(name);
        }
        if let Some(name) = out.preset_switch {
            return Outcome::SelectPreset(name);
        }
        if let Some(name) = out.rename {
            return Outcome::Rename(name);
        }
        if out.changed {
            *out_theme = self.working.clone();
            return Outcome::Changed;
        }
        Outcome::Pending
    }

    /// Draw the control form into `rect`: a vertically-scrolling body (preset ·
    /// name/delete · the five named colours · font size · palette grid) with the
    /// Duplicate/Revert actions pinned in a strip below it. Returns what changed.
    fn draw_form(&mut self, ui: &mut egui::Ui, rect: egui::Rect, t: &Theme) -> FormOut {
        let mut out = FormOut::default();
        let actions_h = 34.0;
        let form_rect = egui::Rect::from_min_max(
            rect.min,
            egui::pos2(
                rect.right(),
                (rect.bottom() - actions_h - 6.0).max(rect.top()),
            ),
        );
        let actions_rect = egui::Rect::from_min_max(
            egui::pos2(rect.left(), (rect.bottom() - actions_h).max(rect.top())),
            rect.max,
        );

        let mut form = ui.new_child(egui::UiBuilder::new().max_rect(form_rect));
        egui::ScrollArea::vertical()
            .id_salt("appearance_form")
            .auto_shrink([false, false])
            .show(&mut form, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;

                ui.label(
                    egui::RichText::new("THEME")
                        .size(11.0)
                        .color(t.dim)
                        .strong(),
                );

                // Preset switcher.
                ui.label(egui::RichText::new("Preset").size(11.0).color(t.dim));
                let mut selected = self.active_name.clone();
                egui::ComboBox::from_id_salt("appearance_preset")
                    .width(ui.available_width())
                    .selected_text(&self.active_name)
                    .show_ui(ui, |ui| {
                        for p in &self.presets {
                            ui.selectable_value(&mut selected, p.clone(), p.as_str());
                        }
                    });
                if selected != self.active_name {
                    out.preset_switch = Some(selected);
                }

                // Name (user theme) or the built-in note.
                if self.active_is_builtin() {
                    ui.label(
                        egui::RichText::new("Built-in — edits save as a copy")
                            .size(12.0)
                            .color(t.dim),
                    );
                } else {
                    ui.label(egui::RichText::new("Name").size(11.0).color(t.dim));
                    ui.horizontal(|ui| {
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.name_edit)
                                .desired_width((ui.available_width() - 30.0).max(60.0)),
                        );
                        if resp.lost_focus()
                            && !self.name_edit.trim().is_empty()
                            && crate::theme::slug(&self.name_edit) != self.active_name
                        {
                            out.rename = Some(self.name_edit.clone());
                        }
                        if ui.button("–").on_hover_text("Delete this theme").clicked() {
                            self.confirm_delete = true;
                        }
                    });
                }

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);

                // The five named UI/terminal colours (label left, swatch right).
                out.changed |= opaque_row(ui, "Background", &mut self.working.bg, t.text);
                out.changed |= opaque_row(ui, "Foreground", &mut self.working.fg, t.text);
                out.changed |=
                    translucent_row(ui, "Selection", &mut self.working.selection, t.text);
                out.changed |=
                    opaque_row(ui, "Focus border", &mut self.working.border_focus, t.text);
                out.changed |= translucent_row(ui, "Cursor", &mut self.working.caret, t.text);

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);

                // Font size (shares the Ctrl+Scroll zoom seam), then the palette grid.
                let mut fs = crate::terminal::font_size(ui.ctx());
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Font size").size(13.0).color(t.text));
                    if ui.small_button("–").clicked() {
                        fs = (fs - 1.0).max(8.0);
                    }
                    ui.label(egui::RichText::new(format!("{fs:.0}")).color(t.text));
                    if ui.small_button("+").clicked() {
                        fs = (fs + 1.0).min(40.0);
                    }
                });
                crate::terminal::set_font_size(ui.ctx(), fs);

                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("Terminal palette")
                        .size(11.0)
                        .color(t.dim),
                );
                ui.add_space(2.0);
                out.changed |= palette_grid(ui, &mut self.working.palette, t);
                ui.add_space(4.0);
            });

        // Pinned actions strip (always visible, below the scrolling form).
        let mut act = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(actions_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        if act.button("Duplicate…").clicked() {
            out.duplicate = Some(self.fork_name());
        }
        if self.is_dirty() && act.button("Revert").clicked() {
            self.revert();
            out.changed = true;
        }
        out
    }

    /// Draw the live-preview terminal centred in `region`, with the caption below.
    /// The terminal is sized to fit `region`, so it never spills over the divider
    /// or the window edge.
    fn draw_hero(ui: &mut egui::Ui, region: egui::Rect, t: &Theme) {
        if region.width() < 40.0 || region.height() < 40.0 {
            return;
        }
        let term_w = (region.width() - 24.0).clamp(40.0, 720.0);
        let term_h = 178.0_f32.min((region.height() - 44.0).max(80.0));
        let cap_gap = 12.0;
        let cap_h = 16.0;
        let block_h = term_h + cap_gap + cap_h;
        let block_top = region.top() + ((region.height() - block_h) * 0.5).max(0.0);
        let cx = region.center().x;
        let term_rect = egui::Rect::from_min_size(
            egui::pos2(cx - term_w / 2.0, block_top),
            egui::vec2(term_w, term_h),
        );
        Self::paint_preview(ui, term_rect, t);
        // Clip the caption to the region so a narrow preview never spills the text.
        ui.painter_at(region).text(
            egui::pos2(cx, term_rect.bottom() + cap_gap),
            egui::Align2::CENTER_TOP,
            "Live preview · changes apply to every terminal instantly",
            egui::FontId::proportional(12.0),
            t.dim,
        );
    }

    /// A self-contained mini-terminal sample rendered with `t` — NO PTY. Shows the
    /// surface, foreground text, several palette colours, a selection wash, and the
    /// caret, so an edit is visible at a glance. The palette itself is edited in
    /// the form grid; here the sample text exercises a few palette slots. Text is
    /// clipped to `rect`, so a narrow preview never spills over its bounds.
    fn paint_preview(ui: &egui::Ui, rect: egui::Rect, t: &Theme) {
        let p = ui.painter_at(rect);
        p.rect_filled(rect, egui::CornerRadius::same(6), t.bg);
        p.rect_stroke(
            rect,
            egui::CornerRadius::same(6),
            egui::Stroke::new(1.0, t.border),
            egui::StrokeKind::Inside,
        );
        let mono = egui::FontId::monospace(13.0);
        let x0 = rect.left() + 12.0;
        let lh = 19.0;
        let mut y = rect.top() + 12.0;
        // Draw a left-to-right run of (text, colour) segments on one line.
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
            egui::Rect::from_min_size(egui::pos2(after, y + 1.0), egui::vec2(8.0, lh - 5.0)),
            egui::CornerRadius::ZERO,
            t.caret,
        );
    }
}

/// One row for an OPAQUE token: a left label with the swatch pushed to the right
/// edge (a compact, aligned control that fits the form). Returns true on change.
fn opaque_row(ui: &mut egui::Ui, label: &str, c: &mut egui::Color32, text: egui::Color32) -> bool {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).size(13.0).color(text));
        ui.add_space((ui.available_width() - 50.0).max(0.0));
        ui.spacing_mut().interact_size = egui::vec2(50.0, 22.0);
        let mut rgb = [c.r(), c.g(), c.b()];
        let changed = ui.color_edit_button_srgb(&mut rgb).changed();
        if changed {
            *c = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
        }
        changed
    })
    .inner
}

/// One row for a TRANSLUCENT token, edited in STRAIGHT (un-premultiplied) alpha
/// via `color_edit_button_srgba_unmultiplied` — the egui path that avoids the
/// low-alpha premultiplied round-trip drift.
fn translucent_row(
    ui: &mut egui::Ui,
    label: &str,
    c: &mut egui::Color32,
    text: egui::Color32,
) -> bool {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).size(13.0).color(text));
        ui.add_space((ui.available_width() - 50.0).max(0.0));
        ui.spacing_mut().interact_size = egui::vec2(50.0, 22.0);
        let mut a = c.to_srgba_unmultiplied();
        let changed = ui.color_edit_button_srgba_unmultiplied(&mut a).changed();
        if changed {
            *c = egui::Color32::from_rgba_unmultiplied(a[0], a[1], a[2], a[3]);
        }
        changed
    })
    .inner
}

/// The editable 16-swatch ANSI palette as a two-row grid (Base / Bright), sized
/// to the form width. Returns true if any swatch changed.
fn palette_grid(ui: &mut egui::Ui, palette: &mut [egui::Color32; 16], t: &Theme) -> bool {
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
    let sw = ((ui.available_width() - label_w - gap * 8.0) / 8.0).clamp(14.0, 40.0);
    let sh = (sw * 0.62).clamp(12.0, 26.0);
    let mut pal = *palette;
    let prev = ui.spacing().interact_size;
    ui.spacing_mut().interact_size = egui::vec2(sw, sh);
    egui::Grid::new("appearance_palette")
        .spacing([gap, gap])
        .show(ui, |ui| {
            for (base, label) in [(0usize, "Base"), (8usize, "Bright")] {
                ui.label(egui::RichText::new(label).size(11.0).color(t.dim));
                for cc in 0..8usize {
                    let i = base + cc;
                    ui.push_id(i, |ui| {
                        let mut rgb = [pal[i].r(), pal[i].g(), pal[i].b()];
                        if ui
                            .color_edit_button_srgb(&mut rgb)
                            .on_hover_text(format!("{} · color {i}", NAMES[i]))
                            .changed()
                        {
                            pal[i] = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                        }
                    });
                }
                ui.end_row();
            }
        });
    ui.spacing_mut().interact_size = prev;
    if pal != *palette {
        *palette = pal;
        true
    } else {
        false
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
