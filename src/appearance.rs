//! The Appearance settings pane: edits the live [`Theme`] with a live preview.
//!
//! Painted in the settings-menu house style — hand-painted rows with a label and
//! a dim description, bordered chips, controls anchored right — rather than
//! egui's stock widgets. The one stock control left is the colour-picker popup,
//! which `Theme::visuals` themes; it sits hidden under our swatch chip. The token
//! rows are generated from [`crate::theme::TOKENS`], so every colour field is
//! editable, labelled, and shows its JSON key.
//!
//! The model (`working`/`saved` + dirty/revert/presets) is pure and unit-tested:
//! `working` is the theme being edited and `saved` the last synced state, so
//! `is_dirty` and `revert` need no extra bookkeeping. The view is responsive: a
//! wide pane lays the token list beside the preview + theme expert; a tall or
//! narrow pane stacks them. Verified by screenshot.
//!
//! Allocation rule: the pane's own `ui` belongs to the settings shell, which
//! allocates its footer after us. Everything here therefore paints via
//! `ui.painter()` + `ui.interact` or draws inside `ui.new_child` children (which
//! never move the shell's cursor); `put`/`scope_builder` only ever run inside
//! those children.

use eframe::egui;

use crate::theme::{CHAT_COLOR_DESC, PALETTE_NAMES, TOKENS, Theme, TokenGroup, TokenSpec};

/// The built-in, read-only theme's name. User themes are everything else.
pub const BUILTIN: &str = "Foreman Warm";

/// House metrics (match `settings_menu::draw_pane`).
const PAD: f32 = 18.0;
const ROW_H: f32 = 46.0;
const HEADER_H: f32 = 30.0;
const CHIP_H: f32 = 22.0;
const SWATCH_W: f32 = 44.0;
const SWATCH_H: f32 = 20.0;
/// Preset chips + status line at the top of the pane.
const STRIP_H: f32 = 66.0;
/// Token list's natural height — decides side-by-side vs stacked (is there
/// more room below the list than to its right?).
const TOKENS_MIN_H: f32 = 360.0;
const FONT_SMALL: f32 = 11.0;
const FONT_LABEL: f32 = 13.0;
const FONT_CHIP: f32 = 12.5;

const STARTERS: [&str; 3] = [
    "Warmer, less orange",
    "High-contrast dark",
    "Soft pastel light",
];

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
    /// Re-read the active theme from its file (the shell loads it strictly and
    /// republishes; an invalid file toasts and changes nothing).
    Reload,
    /// Open the themes folder in the file manager.
    OpenThemesFolder,
    /// Nothing happened this frame.
    Pending,
}

/// What the top strip + token list reported this frame (collected, then folded
/// into an [`Outcome`] after the whole pane is drawn).
#[derive(Default)]
struct FormOut {
    changed: bool,
    preset_switch: Option<String>,
    rename: Option<String>,
    duplicate: bool,
    reload: bool,
    open_folder: bool,
}

/// The Appearance pane's state.
#[derive(Debug)]
pub struct AppearanceView {
    working: Theme,
    saved: Theme,
    active_name: String,
    /// The inline rename buffer while the name is being edited.
    name_edit: Option<String>,
    /// The theme-expert model field while it is being edited.
    model_edit: Option<String>,
    /// Selectable presets: built-in first, then user themes.
    presets: Vec<String>,
    /// True while the delete-confirmation modal is open.
    confirm_delete: bool,
    /// True while the theme dropdown list is open.
    preset_open: bool,
    expert: crate::theme_expert::ThemeExpert,
}

impl AppearanceView {
    pub fn new() -> Self {
        let mut v = Self {
            working: Theme::foreman_warm(),
            saved: Theme::foreman_warm(),
            active_name: BUILTIN.to_string(),
            name_edit: None,
            model_edit: None,
            presets: vec![BUILTIN.to_string()],
            confirm_delete: false,
            preset_open: false,
            expert: crate::theme_expert::ThemeExpert::new(),
        };
        v.refresh_presets();
        v
    }

    /// Switch the pane to a theme: it becomes both the working copy and the
    /// clean baseline (so the pane opens non-dirty on the newly-active theme).
    pub fn set_active(&mut self, name: &str, theme: Theme) {
        self.active_name = name.to_string();
        self.name_edit = None;
        self.saved = theme.clone();
        self.working = theme;
        self.expert.selected = None;
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
        self.fork_name_from(None)
    }

    /// Like [`fork_name`](Self::fork_name) but preferring `suggested` (the
    /// expert's name for a proposal) when it slugs to something usable.
    fn fork_name_from(&self, suggested: Option<&str>) -> String {
        let existing: std::collections::HashSet<String> =
            Theme::user_theme_names().into_iter().collect();
        let base = suggested
            .map(crate::theme::slug)
            .filter(|s| !s.trim_matches('-').is_empty())
            .unwrap_or_else(|| crate::theme::slug(&format!("{} copy", self.active_name)));
        unique_slug(&base, &existing)
    }

    /// The currently-active theme name (matches `Settings.theme`). The shell
    /// decides resync through `needs_resync`; this is for tests.
    #[cfg(test)]
    pub fn active_name(&self) -> &str {
        &self.active_name
    }

    /// Should the pane re-adopt the live theme as its working copy? Yes on a
    /// name change (preset switch / Duplicate / rename) and whenever the live
    /// theme differs from `working` — which only happens when something other
    /// than this pane changed it (the App's disk poll). After the pane's own
    /// edit the two are equal (the edit was published as the live theme), so
    /// an in-progress edit is never clobbered.
    pub fn needs_resync(&self, name: &str, live: &Theme) -> bool {
        name != self.active_name || *live != self.working
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

    /// Mutable access for tests (the view edits `working` directly).
    #[cfg(test)]
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

    /// Render the pane into `rect`: the preset/status strip across the top, then
    /// the token list beside (or above, when tall/narrow) the preview + theme
    /// expert. Editing the built-in transparently forks an editable copy.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        reads_input: bool,
        out_theme: &mut Theme,
    ) -> Outcome {
        // Mouse-only pane: every control is position-routed, so there is no
        // reads_input gate; keyboard is handled by the settings shell.
        let _ = reads_input;
        self.expert.poll();
        let t = self.working.clone();
        let previewing = self
            .expert
            .selected
            .filter(|i| *i < self.expert.proposals.len());
        let preview = self.expert.preview().cloned().unwrap_or_else(|| t.clone());

        // --- top strip: preset chips, actions, status line ---
        let strip = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), STRIP_H));
        let mut out = self.draw_strip(ui, strip, &t);
        ui.painter().hline(
            egui::Rangef::new(rect.left(), rect.right()),
            strip.bottom(),
            egui::Stroke::new(1.0, t.border),
        );

        // --- body: tokens | preview + expert (stacked when tall/narrow) ---
        let inner =
            egui::Rect::from_min_max(egui::pos2(rect.left(), strip.bottom() + 1.0), rect.max);
        let sidebar_w = (inner.width() * 0.48).clamp(300.0, 470.0);
        let right_room = inner.width() - sidebar_w;
        let bottom_room = inner.height() - TOKENS_MIN_H;
        let stacked = bottom_room > right_room;
        let (tokens_rect, side) = if stacked {
            let list_h = (inner.height() * 0.45).clamp(180.0, TOKENS_MIN_H);
            let list = egui::Rect::from_min_size(inner.min, egui::vec2(inner.width(), list_h));
            ui.painter().hline(
                egui::Rangef::new(inner.left(), inner.right()),
                list.bottom(),
                egui::Stroke::new(1.0, t.border),
            );
            let side =
                egui::Rect::from_min_max(egui::pos2(inner.left(), list.bottom() + 1.0), inner.max);
            (list, side)
        } else {
            let list = egui::Rect::from_min_size(inner.min, egui::vec2(sidebar_w, inner.height()));
            ui.painter().vline(
                list.right(),
                egui::Rangef::new(inner.top(), inner.bottom()),
                egui::Stroke::new(1.0, t.border),
            );
            let side =
                egui::Rect::from_min_max(egui::pos2(list.right() + 1.0, inner.top()), inner.max);
            (list, side)
        };
        out.changed |= self.draw_tokens(ui, tokens_rect, &t);

        let side_inner = side.shrink2(egui::vec2(PAD * 0.7, 10.0));
        // Never taller than the column itself (a very short Settings window would
        // otherwise push the hero past the footer and under the chat).
        let hero_h = (side_inner.height() * 0.42)
            .clamp(170.0, 260.0)
            .min(side_inner.height().max(0.0));
        let hero =
            egui::Rect::from_min_size(side_inner.min, egui::vec2(side_inner.width(), hero_h));
        if Self::draw_hero(ui, hero, &preview, previewing, &t) {
            self.expert.selected = None;
        }
        let chat = egui::Rect::from_min_max(
            egui::pos2(side_inner.left(), hero.bottom() + 8.0),
            side_inner.max,
        );
        let apply = self.draw_expert(ui, chat, &t);

        // Delete-confirmation modal (opened by the Delete chip on a user theme).
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
        if apply {
            if let Some(prop) = self.expert.preview_proposal() {
                let proposal = prop.theme.clone();
                let suggested = prop.name.clone();
                if self.active_is_builtin() {
                    self.working = proposal;
                    return Outcome::Duplicate(self.fork_name_from(suggested.as_deref()));
                }
                match proposal.save(&self.active_name) {
                    Ok(()) => {
                        self.working = proposal.clone();
                        self.saved = proposal.clone();
                        *out_theme = proposal;
                        self.expert.selected = None;
                        return Outcome::Changed;
                    }
                    Err(e) => self.expert.error = Some(format!("Could not save theme: {e}")),
                }
            }
        }
        // Editing the built-in transparently forks an editable user copy — the
        // built-in stays a pristine preset you can switch back to.
        if out.changed && self.active_is_builtin() {
            *out_theme = self.working.clone();
            return Outcome::Duplicate(self.fork_name());
        }
        if out.duplicate {
            return Outcome::Duplicate(self.fork_name());
        }
        if let Some(name) = out.preset_switch {
            return Outcome::SelectPreset(name);
        }
        if let Some(name) = out.rename {
            return Outcome::Rename(name);
        }
        if out.reload {
            return Outcome::Reload;
        }
        if out.open_folder {
            return Outcome::OpenThemesFolder;
        }
        if out.changed {
            *out_theme = self.working.clone();
            return Outcome::Changed;
        }
        Outcome::Pending
    }

    // ----------------------------------------------------------------- strip

    /// Band 1: preset chips (left) and action chips (right). Band 2: the status
    /// line — inline-renamable name · file · auto-saves, plus Revert when dirty.
    fn draw_strip(&mut self, ui: &mut egui::Ui, strip: egui::Rect, t: &Theme) -> FormOut {
        let mut out = FormOut::default();
        let builtin = self.active_is_builtin();
        let cy1 = strip.top() + 8.0 + CHIP_H / 2.0;
        let cy2 = strip.top() + 8.0 + CHIP_H + 10.0 + 9.0;

        // Action chips, right-anchored, laid right-to-left.
        let mut x = strip.right() - PAD;
        let mut actions: Vec<(&str, &str)> = vec![("Folder", "Open the themes folder")];
        if !builtin {
            actions.push(("Reload", "Re-read this theme's file"));
        }
        actions.push(("Duplicate", "Copy this theme into a new editable one"));
        for (label, tip) in actions {
            let w = chip_width(ui, label);
            let r = egui::Rect::from_min_size(
                egui::pos2(x - w, cy1 - CHIP_H / 2.0),
                egui::vec2(w, CHIP_H),
            );
            let resp =
                chip(ui, r, ("appearance_action", label), label, t, false).on_hover_text(tip);
            if resp.clicked() {
                match label {
                    "Folder" => out.open_folder = true,
                    "Reload" => out.reload = true,
                    _ => out.duplicate = true,
                }
            }
            x = r.left() - 6.0;
        }
        let actions_left = x;

        // Theme dropdown (active name + caret) with a `−` delete button beside it.
        let room = (actions_left - 6.0 - CHIP_H - 6.0 - (strip.left() + PAD)).max(80.0);
        let dd_w = (chip_width(ui, &self.active_name) + 18.0).clamp(140.0, room.min(260.0));
        let dd = egui::Rect::from_min_size(
            egui::pos2(strip.left() + PAD, cy1 - CHIP_H / 2.0),
            egui::vec2(dd_w, CHIP_H),
        );
        if let Some(p) = self.preset_dropdown(ui, dd, strip, t) {
            out.preset_switch = Some(p);
        }
        let del = egui::Rect::from_min_size(
            egui::pos2(dd.right() + 6.0, cy1 - CHIP_H / 2.0),
            egui::vec2(CHIP_H, CHIP_H),
        );
        if builtin {
            // Disabled look: the built-in cannot be deleted.
            let p = ui.painter();
            p.rect_stroke(
                del,
                egui::CornerRadius::same(4),
                egui::Stroke::new(1.0, t.border),
                egui::StrokeKind::Inside,
            );
            p.text(
                del.center(),
                egui::Align2::CENTER_CENTER,
                "−",
                egui::FontId::proportional(FONT_CHIP + 1.0),
                t.dim,
            );
        } else if chip(ui, del, "appearance_delete", "−", t, false)
            .on_hover_text("Delete this theme")
            .clicked()
        {
            self.confirm_delete = true;
        }

        // Status line.
        let small = egui::FontId::proportional(FONT_SMALL + 0.5);
        let mut sx = strip.left() + PAD;
        if builtin {
            let r = ui.painter().text(
                egui::pos2(sx, cy2),
                egui::Align2::LEFT_CENTER,
                "Built-in · edits save as a copy",
                small.clone(),
                t.dim,
            );
            sx = r.right();
        } else {
            // Name: click to rename inline (Enter commits, Esc / click-away cancels).
            let name_w = 180.0_f32.min((actions_left - sx - 160.0).max(80.0));
            if let Some(buf) = self.name_edit.as_mut() {
                let te_rect = egui::Rect::from_min_size(
                    egui::pos2(sx - 4.0, cy2 - 11.0),
                    egui::vec2(name_w, 22.0),
                );
                let mut host = ui.new_child(egui::UiBuilder::new().max_rect(te_rect));
                host.visuals_mut().selection.bg_fill = t.selection_text_bg;
                let resp = host.put(
                    te_rect,
                    egui::TextEdit::singleline(buf)
                        .font(egui::FontId::proportional(FONT_CHIP))
                        .text_color(t.text)
                        .desired_width(te_rect.width()),
                );
                if resp.lost_focus() {
                    let committed = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let buf = self.name_edit.take().unwrap_or_default();
                    if committed
                        && !buf.trim().is_empty()
                        && crate::theme::slug(&buf) != self.active_name
                    {
                        out.rename = Some(buf);
                    }
                } else {
                    resp.request_focus();
                }
                sx = te_rect.right() + 6.0;
            } else {
                let g = truncated(
                    ui,
                    &self.active_name,
                    egui::FontId::proportional(FONT_CHIP),
                    t.text,
                    name_w,
                );
                let r = egui::Rect::from_min_size(egui::pos2(sx, cy2 - g.size().y / 2.0), g.size());
                let resp = ui
                    .interact(
                        r.expand(3.0),
                        egui::Id::new("appearance_name"),
                        egui::Sense::click(),
                    )
                    .on_hover_text("Rename");
                ui.painter().galley(
                    r.min,
                    g,
                    if resp.hovered() {
                        t.border_focus
                    } else {
                        t.text
                    },
                );
                if resp.hovered() {
                    ui.painter().hline(
                        egui::Rangef::new(r.left(), r.right()),
                        r.bottom() + 1.0,
                        egui::Stroke::new(1.0, t.border_focus),
                    );
                }
                if resp.clicked() {
                    self.name_edit = Some(self.active_name.clone());
                }
                sx = r.right() + 6.0;
            }
            let file = format!(
                "· themes\\{}.json · auto-saves",
                crate::theme::slug(&self.active_name)
            );
            let g = truncated(
                ui,
                &file,
                small.clone(),
                t.dim,
                (actions_left - sx - 80.0).max(40.0),
            );
            let r = egui::Rect::from_min_size(egui::pos2(sx, cy2 - g.size().y / 2.0), g.size());
            ui.painter().galley(r.min, g, t.dim);
            sx = r.right() + 8.0;
        }
        if self.is_dirty() {
            let w = chip_width(ui, "Revert");
            let r = egui::Rect::from_min_size(
                egui::pos2(sx, cy2 - CHIP_H / 2.0 + 1.0),
                egui::vec2(w, CHIP_H - 2.0),
            );
            if chip(ui, r, "appearance_revert", "Revert", t, false)
                .on_hover_text("Discard edits made since this theme was opened")
                .clicked()
            {
                self.revert();
                out.changed = true;
            }
        }
        out
    }

    /// The theme selector: a chip showing the active name with a caret that opens
    /// a hand-painted list (built-in first, then user themes) in a Foreground
    /// `Area`, like `hover_menu` but click-toggled and with dynamic names. Click
    /// outside or Esc closes it. Returns the picked name when it differs.
    fn preset_dropdown(
        &mut self,
        ui: &mut egui::Ui,
        anchor: egui::Rect,
        area: egui::Rect,
        t: &Theme,
    ) -> Option<String> {
        let resp = ui.interact(
            anchor,
            egui::Id::new("appearance_preset_dd"),
            egui::Sense::click(),
        );
        if resp.clicked() {
            self.preset_open = !self.preset_open;
        }
        let cr = egui::CornerRadius::same(4);
        let p = ui.painter();
        if self.preset_open {
            p.rect_filled(anchor, cr, t.title_bg_focus);
        } else if resp.hovered() {
            p.rect_filled(anchor, cr, t.sel_bg);
        }
        p.rect_stroke(
            anchor,
            cr,
            egui::Stroke::new(
                1.0,
                if self.preset_open || resp.hovered() {
                    t.border_focus
                } else {
                    t.border
                },
            ),
            egui::StrokeKind::Inside,
        );
        let caret_w = 16.0;
        let g = truncated(
            ui,
            &self.active_name,
            egui::FontId::proportional(FONT_CHIP),
            t.text,
            anchor.width() - 10.0 - caret_w,
        );
        p.galley(
            egui::pos2(anchor.left() + 8.0, anchor.center().y - g.size().y / 2.0),
            g,
            t.text,
        );
        p.text(
            egui::pos2(anchor.right() - 8.0, anchor.center().y),
            egui::Align2::RIGHT_CENTER,
            if self.preset_open { "▴" } else { "▾" },
            egui::FontId::proportional(FONT_SMALL),
            t.dim,
        );
        if !self.preset_open {
            return None;
        }

        // The list.
        let font = egui::FontId::proportional(FONT_CHIP);
        let row_h = 24.0;
        let pad = 10.0;
        let label_w = self
            .presets
            .iter()
            .map(|n| {
                ui.painter()
                    .layout_no_wrap(n.clone(), font.clone(), t.text)
                    .size()
                    .x
            })
            .fold(0.0f32, f32::max);
        let w = (label_w + pad * 2.0 + 22.0).max(anchor.width());
        let h = self.presets.len() as f32 * row_h + 8.0;
        let below = anchor.bottom() + 2.0;
        let oy = if below + h > area.max.y {
            (anchor.top() - 2.0 - h).max(area.min.y)
        } else {
            below
        };
        let panel = egui::Rect::from_min_size(egui::pos2(anchor.left(), oy), egui::vec2(w, h));
        let menu_id = egui::Id::new("appearance_preset_menu");
        let mut picked: Option<String> = None;
        let mut clicked_inside = false;
        egui::Area::new(menu_id)
            .order(egui::Order::Foreground)
            .fixed_pos(panel.min)
            .constrain(false)
            .default_size(panel.size())
            .movable(false)
            .show(ui.ctx(), |mui| {
                let mp = mui.painter();
                mp.rect_filled(panel, cr, t.win_bg);
                mp.rect_stroke(
                    panel,
                    cr,
                    egui::Stroke::new(1.0, t.border),
                    egui::StrokeKind::Inside,
                );
                let mut y = panel.top() + 4.0;
                for (i, name) in self.presets.iter().enumerate() {
                    let rr = egui::Rect::from_min_size(
                        egui::pos2(panel.left(), y),
                        egui::vec2(w, row_h),
                    );
                    let r = mui.interact(rr, menu_id.with(("row", i)), egui::Sense::click());
                    let active = *name == self.active_name;
                    if r.hovered() {
                        mui.painter()
                            .rect_filled(rr.shrink2(egui::vec2(3.0, 1.0)), cr, t.sel_bg);
                    }
                    mui.painter().text(
                        egui::pos2(rr.left() + pad, rr.center().y),
                        egui::Align2::LEFT_CENTER,
                        name,
                        font.clone(),
                        if active { t.border_focus } else { t.text },
                    );
                    if active {
                        mui.painter().text(
                            egui::pos2(rr.right() - pad, rr.center().y),
                            egui::Align2::RIGHT_CENTER,
                            "●",
                            egui::FontId::proportional(FONT_SMALL - 2.0),
                            t.dim,
                        );
                    }
                    if r.clicked() {
                        clicked_inside = true;
                        if !active {
                            picked = Some(name.clone());
                        }
                    }
                    y += row_h;
                }
            });
        // Close on pick, Esc, or a click anywhere outside the list and the chip.
        let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
        let outside = ui.input(|i| {
            i.pointer.any_pressed()
                && i.pointer
                    .interact_pos()
                    .is_some_and(|pos| !panel.contains(pos) && !anchor.contains(pos))
        });
        if picked.is_some() || clicked_inside || esc || outside {
            self.preset_open = false;
        }
        picked
    }

    // ---------------------------------------------------------------- tokens

    /// The grouped token list: one house-style row per [`TOKENS`] entry, plus the
    /// font-size stepper and the palette / member-colour swatch grids. Scrolls.
    fn draw_tokens(&mut self, ui: &mut egui::Ui, rect: egui::Rect, t: &Theme) -> bool {
        let mut changed = false;
        let mut pane_ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        pane_ui.set_clip_rect(rect);
        egui::ScrollArea::vertical()
            .id_salt("appearance_tokens")
            .auto_shrink([false, false])
            .show(&mut pane_ui, |ui| {
                ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                let width = ui.available_width();
                ui.set_min_width(width);
                for g in TokenGroup::ALL {
                    section_header(ui, width, g.label(), t);
                    if g == TokenGroup::Terminal {
                        font_size_row(ui, width, t);
                    }
                    for spec in TOKENS.iter().filter(|s| s.group == g) {
                        changed |= token_row(ui, width, spec, &mut self.working, t);
                    }
                    if g == TokenGroup::Terminal {
                        changed |= palette_rows(ui, width, &mut self.working.palette, t);
                    }
                    if g == TokenGroup::Chat {
                        changed |= member_colour_row(ui, width, &mut self.working.chat_colors, t);
                    }
                }
                row(ui, width, 10.0, |_, _| {});
            });
        changed
    }

    // --------------------------------------------------------------- preview

    /// The preview: a desktop patch holding a mock window — focused title bar
    /// with tab chips and a close button, the focus border, and a terminal body
    /// with sample text, selection wash and caret — so the Windows tokens visibly
    /// do something. Returns true when the Discard chip (proposal preview) is hit.
    fn draw_hero(
        ui: &mut egui::Ui,
        region: egui::Rect,
        t: &Theme,
        previewing: Option<usize>,
        live: &Theme,
    ) -> bool {
        if region.width() < 60.0 || region.height() < 60.0 {
            return false;
        }
        let p = ui.painter_at(region);
        let cap_h = 22.0;
        p.rect_filled(region, egui::CornerRadius::same(6), t.desk_bg);
        p.rect_stroke(
            region,
            egui::CornerRadius::same(6),
            egui::Stroke::new(1.0, live.border),
            egui::StrokeKind::Inside,
        );
        let win = egui::Rect::from_min_max(
            region.min + egui::vec2(12.0, 12.0),
            egui::pos2(region.right() - 12.0, region.bottom() - 12.0 - cap_h),
        );
        let cr = egui::CornerRadius::same(5);
        p.rect_filled(win, cr, t.bg);
        // Title band with tab chips + close.
        let title_h = 24.0;
        let title = egui::Rect::from_min_size(win.min, egui::vec2(win.width(), title_h));
        p.rect_filled(
            title,
            egui::CornerRadius {
                nw: 5,
                ne: 5,
                sw: 0,
                se: 0,
            },
            t.title_bg_focus,
        );
        let mono = egui::FontId::monospace(12.0);
        let small = egui::FontId::proportional(11.0);
        let mut tx = title.left() + 8.0;
        for (i, (name, fill, col)) in [
            ("foreman", t.tab_bg, t.text),
            ("worker", t.tab_bg_hover, t.dim),
        ]
        .into_iter()
        .enumerate()
        {
            let w = 58.0;
            let chip = egui::Rect::from_min_size(
                egui::pos2(tx, title.top() + 4.0),
                egui::vec2(w, title_h - 8.0),
            );
            p.rect_filled(chip, egui::CornerRadius::same(3), fill);
            if i == 0 {
                p.rect_stroke(
                    chip,
                    egui::CornerRadius::same(3),
                    egui::Stroke::new(1.0, t.border),
                    egui::StrokeKind::Inside,
                );
            }
            p.text(
                chip.center(),
                egui::Align2::CENTER_CENTER,
                name,
                small.clone(),
                col,
            );
            tx = chip.right() + 4.0;
        }
        let close = egui::Rect::from_min_size(
            egui::pos2(title.right() - 8.0 - 16.0, title.top() + 4.0),
            egui::vec2(16.0, title_h - 8.0),
        );
        p.rect_filled(close, egui::CornerRadius::same(3), t.win_btn_danger_hover);
        p.text(
            close.center(),
            egui::Align2::CENTER_CENTER,
            "×",
            small,
            t.text,
        );
        p.rect_stroke(
            win,
            cr,
            egui::Stroke::new(1.0, t.border_focus),
            egui::StrokeKind::Inside,
        );

        // Terminal body: sample lines (clipped to the window).
        let body = egui::Rect::from_min_max(egui::pos2(win.left(), title.bottom()), win.max);
        let bp = ui.painter_at(body);
        let x0 = body.left() + 10.0;
        let lh = 18.0;
        let mut y = body.top() + 8.0;
        let line = |segs: &[(&str, egui::Color32)], y: f32| {
            let mut x = x0;
            for (s, c) in segs {
                let r = bp.text(
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
        let sel_text = "  new file: src/appearance.rs";
        let sel_w = bp
            .layout_no_wrap(sel_text.to_string(), mono.clone(), t.fg)
            .rect
            .width();
        bp.rect_filled(
            egui::Rect::from_min_size(egui::pos2(x0, y), egui::vec2(sel_w, lh)),
            egui::CornerRadius::ZERO,
            t.selection,
        );
        line(&[(sel_text, t.fg)], y);
        y += lh;
        let after = bp
            .text(
                egui::pos2(x0, y),
                egui::Align2::LEFT_TOP,
                "$ ",
                mono.clone(),
                t.fg,
            )
            .right();
        bp.rect_filled(
            egui::Rect::from_min_size(egui::pos2(after, y + 1.0), egui::vec2(8.0, lh - 4.0)),
            egui::CornerRadius::ZERO,
            t.caret,
        );

        // Caption (+ Discard chip while a proposal is previewed).
        let cap_y = region.bottom() - 6.0 - cap_h / 2.0;
        let mut discard = false;
        match previewing {
            Some(i) => {
                let r = p.text(
                    egui::pos2(region.left() + 12.0, cap_y),
                    egui::Align2::LEFT_CENTER,
                    format!("Proposal {} preview · Apply from its card", i + 1),
                    egui::FontId::proportional(11.5),
                    live.dim,
                );
                let w = chip_width(ui, "Discard");
                let cr = egui::Rect::from_min_size(
                    egui::pos2(r.right() + 10.0, cap_y - (CHIP_H - 4.0) / 2.0),
                    egui::vec2(w, CHIP_H - 4.0),
                );
                discard =
                    chip(ui, cr, "appearance_discard_preview", "Discard", live, false).clicked();
            }
            None => {
                p.text(
                    egui::pos2(region.left() + 12.0, cap_y),
                    egui::Align2::LEFT_CENTER,
                    "Live preview · edits apply to every terminal instantly",
                    egui::FontId::proportional(11.5),
                    live.dim,
                );
            }
        }
        discard
    }

    // ---------------------------------------------------------------- expert

    /// The Theme Expert: header with provider/model, a bubble log with proposal
    /// cards, starter chips when empty, and a two-row input (Enter sends,
    /// Shift+Enter inserts a newline). Returns true when a card's Apply is hit.
    fn draw_expert(&mut self, ui: &mut egui::Ui, rect: egui::Rect, t: &Theme) -> bool {
        if rect.width() < 120.0 || rect.height() < 90.0 {
            return false;
        }
        let mut panel = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        panel.set_clip_rect(rect);
        let p = panel.painter().clone();

        // --- header: title left, model + provider right ---
        let head = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), HEADER_H));
        p.text(
            egui::pos2(head.left(), head.center().y),
            egui::Align2::LEFT_CENTER,
            "THEME EXPERT",
            egui::FontId::proportional(FONT_SMALL),
            t.dim,
        );
        let prov_label = self.expert.provider.label();
        let pw = chip_width(&panel, prov_label);
        let prov_rect = egui::Rect::from_min_size(
            egui::pos2(head.right() - pw, head.center().y - CHIP_H / 2.0),
            egui::vec2(pw, CHIP_H),
        );
        if chip(
            &mut panel,
            prov_rect,
            "theme_expert_provider",
            prov_label,
            t,
            false,
        )
        .on_hover_text("Which CLI answers (click to cycle)")
        .clicked()
        {
            use crate::config::NamingProvider as P;
            self.expert.provider = match self.expert.provider {
                P::Codex => P::Claude,
                P::Claude => P::Grok,
                P::Grok => P::Codex,
            };
            self.expert.model.clear();
            self.model_edit = None;
        }
        // Model: dim click-to-edit text (house Kind::Text idiom).
        let model_right = prov_rect.left() - 10.0;
        if let Some(buf) = self.model_edit.as_mut() {
            let w = (rect.width() * 0.45).clamp(90.0, 220.0);
            let te_rect = egui::Rect::from_min_size(
                egui::pos2(model_right - w, head.center().y - 11.0),
                egui::vec2(w, 22.0),
            );
            panel.visuals_mut().selection.bg_fill = t.selection_text_bg;
            let resp = panel.put(
                te_rect,
                egui::TextEdit::singleline(buf)
                    .font(egui::FontId::proportional(FONT_CHIP))
                    .text_color(t.text)
                    .hint_text("model id")
                    .desired_width(te_rect.width()),
            );
            if resp.lost_focus() {
                let committed = panel.input(|i| i.key_pressed(egui::Key::Enter));
                let buf = self.model_edit.take().unwrap_or_default();
                if committed {
                    self.expert.model = buf.trim().to_string();
                }
            } else {
                resp.request_focus();
            }
        } else {
            let shown = if self.expert.model.trim().is_empty() {
                "(provider default)".to_string()
            } else {
                self.expert.model.clone()
            };
            let g = truncated(
                &panel,
                &shown,
                egui::FontId::proportional(FONT_CHIP),
                t.dim,
                (rect.width() * 0.45).clamp(60.0, 220.0),
            );
            let r = egui::Rect::from_min_size(
                egui::pos2(model_right - g.size().x, head.center().y - g.size().y / 2.0),
                g.size(),
            );
            let resp = panel
                .interact(
                    r.expand(3.0),
                    egui::Id::new("theme_expert_model"),
                    egui::Sense::click(),
                )
                .on_hover_text("Model ID (blank = CLI default) · click to edit");
            p.galley(r.min, g, if resp.hovered() { t.text } else { t.dim });
            if resp.clicked() {
                self.model_edit = Some(self.expert.model.clone());
            }
        }

        // --- input well at the bottom ---
        let input_h = 58.0;
        let input = egui::Rect::from_min_size(
            egui::pos2(rect.left(), rect.bottom() - input_h),
            egui::vec2(rect.width(), input_h),
        );
        // --- scope chips above the input (wrap into rows when narrow) ---
        let scope_rows = scope_chip_rows(&panel, rect.width());
        let scope_row_h = CHIP_H + 4.0;
        let scope_h = scope_rows.len() as f32 * scope_row_h;
        let scope_band = egui::Rect::from_min_size(
            egui::pos2(rect.left(), input.top() - 6.0 - scope_h),
            egui::vec2(rect.width(), scope_h),
        );
        for (ri, row_chips) in scope_rows.iter().enumerate() {
            let mut x = scope_band.left();
            let cy = scope_band.top() + ri as f32 * scope_row_h + scope_row_h / 2.0;
            for (scope, w) in row_chips {
                let r = egui::Rect::from_min_size(
                    egui::pos2(x, cy - (CHIP_H - 2.0) / 2.0),
                    egui::vec2(*w, CHIP_H - 2.0),
                );
                let active = self.expert.scope == *scope;
                if chip(
                    &mut panel,
                    r,
                    ("theme_expert_scope", scope.label()),
                    scope.label(),
                    t,
                    active,
                )
                .on_hover_text(match scope {
                    crate::theme_expert::Scope::All => {
                        "The expert may change any colour".to_string()
                    }
                    s => format!(
                        "Only these keys may change; everything else is kept:\n{}",
                        s.keys().join(", ")
                    ),
                })
                .clicked()
                {
                    self.expert.scope = *scope;
                }
                x += w + 6.0;
            }
        }
        // --- log between ---
        let log = egui::Rect::from_min_max(
            egui::pos2(rect.left(), head.bottom()),
            egui::pos2(rect.right(), scope_band.top() - 4.0),
        );
        let mut apply = false;
        let mut select: Option<Option<usize>> = None;
        let mut starter: Option<&str> = None;
        let mut log_ui = panel.new_child(egui::UiBuilder::new().max_rect(log));
        log_ui.set_clip_rect(log);
        let busy = self.expert.busy();
        let error = self.expert.error.as_deref();
        let turns = &self.expert.turns;
        let proposals = &self.expert.proposals;
        let selected = self.expert.selected;
        egui::ScrollArea::vertical()
            .id_salt("theme_expert_log")
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(&mut log_ui, |ui| {
                ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                let width = ui.available_width();
                ui.set_min_width(width);
                if turns.is_empty() {
                    row(ui, width, 30.0, |ui, r| {
                        ui.painter().text(
                            egui::pos2(r.left(), r.center().y),
                            egui::Align2::LEFT_CENTER,
                            "Describe colours, contrast or a mood.",
                            egui::FontId::proportional(FONT_CHIP),
                            t.dim,
                        );
                    });
                    row(ui, width, CHIP_H + 8.0, |ui, r| {
                        let mut x = r.left();
                        for s in STARTERS {
                            let w = chip_width(ui, s);
                            if x + w > r.right() {
                                break;
                            }
                            let cr = egui::Rect::from_min_size(
                                egui::pos2(x, r.center().y - CHIP_H / 2.0),
                                egui::vec2(w, CHIP_H),
                            );
                            if chip(ui, cr, ("theme_expert_starter", s), s, t, false).clicked() {
                                starter = Some(s);
                            }
                            x = cr.right() + 6.0;
                        }
                    });
                }
                let bubble_w = (width * 0.8).max(60.0) - 20.0;
                for (ti, turn) in turns.iter().enumerate() {
                    let g = ui.painter().layout(
                        turn.text.clone(),
                        egui::FontId::proportional(FONT_CHIP),
                        t.text,
                        bubble_w,
                    );
                    let bw = g.size().x + 20.0;
                    let bh = g.size().y + 14.0;
                    row(ui, width, bh + 6.0, |ui, r| {
                        let b = if turn.user {
                            egui::Rect::from_min_size(
                                egui::pos2(r.right() - bw, r.top() + 3.0),
                                egui::vec2(bw, bh),
                            )
                        } else {
                            egui::Rect::from_min_size(
                                egui::pos2(r.left(), r.top() + 3.0),
                                egui::vec2(bw, bh),
                            )
                        };
                        let p = ui.painter();
                        if turn.user {
                            p.rect_filled(b, egui::CornerRadius::same(6), t.sel_bg);
                        } else {
                            p.rect_filled(b, egui::CornerRadius::same(6), t.win_bg);
                            p.rect_stroke(
                                b,
                                egui::CornerRadius::same(6),
                                egui::Stroke::new(1.0, t.border),
                                egui::StrokeKind::Inside,
                            );
                        }
                        p.galley(b.min + egui::vec2(10.0, 7.0), g, t.text);
                    });
                    if let Some(i) = turn.proposal.filter(|i| *i < proposals.len()) {
                        let prop = &proposals[i];
                        let is_sel = selected == Some(i);
                        let card_h = 56.0;
                        row(ui, width, card_h + 6.0, |ui, r| {
                            let card = egui::Rect::from_min_size(
                                egui::pos2(r.left(), r.top() + 2.0),
                                egui::vec2(width.min(bw.max(240.0)), card_h),
                            );
                            let p = ui.painter();
                            p.rect_filled(card, egui::CornerRadius::same(5), t.title_bg);
                            p.rect_stroke(
                                card,
                                egui::CornerRadius::same(5),
                                egui::Stroke::new(
                                    1.0,
                                    if is_sel { t.border_focus } else { t.border },
                                ),
                                egui::StrokeKind::Inside,
                            );
                            // Line 1: the diff summary and the suggested name.
                            let n = prop.changes.len();
                            let mut head = match n {
                                0 => "No changes".to_string(),
                                1 => "1 change".to_string(),
                                n => format!("{n} changes"),
                            };
                            if let Some(name) = &prop.name {
                                head.push_str(&format!("  ·  \u{201c}{name}\u{201d}"));
                            }
                            let hg = truncated(
                                ui,
                                &head,
                                egui::FontId::proportional(FONT_SMALL + 0.5),
                                t.dim,
                                card.width() - 16.0,
                            );
                            p.galley(egui::pos2(card.left() + 8.0, card.top() + 6.0), hg, t.dim);
                            // Line 2: swatch strip — the changed tokens first (hover shows
                            // key and before → after), padded with bg/fg/palette to ten.
                            let sy = card.bottom() - 8.0 - 14.0;
                            let mut sx = card.left() + 8.0;
                            let mut shown = 0usize;
                            for ch in prop.changes.iter().take(10) {
                                let s = egui::Rect::from_min_size(
                                    egui::pos2(sx, sy),
                                    egui::vec2(14.0, 14.0),
                                );
                                // Left half before, right half after: the change at a glance.
                                let half = egui::Rect::from_min_max(
                                    s.min,
                                    egui::pos2(s.center().x, s.max.y),
                                );
                                p.rect_filled(s, egui::CornerRadius::same(2), ch.after);
                                p.rect_filled(half, egui::CornerRadius::ZERO, ch.before);
                                p.rect_stroke(
                                    s,
                                    egui::CornerRadius::same(2),
                                    egui::Stroke::new(1.0, t.border_focus),
                                    egui::StrokeKind::Inside,
                                );
                                ui.interact(
                                    s,
                                    egui::Id::new(("theme_expert_change", ti, shown)),
                                    egui::Sense::hover(),
                                )
                                .on_hover_ui(|ui| {
                                    ui.label(format!(
                                        "{}: {} → {}",
                                        ch.key,
                                        crate::theme::color_hex::to_hex(ch.before),
                                        crate::theme::color_hex::to_hex(ch.after)
                                    ));
                                });
                                sx += 17.0;
                                shown += 1;
                            }
                            let filler = [prop.theme.bg, prop.theme.fg]
                                .into_iter()
                                .chain(prop.theme.palette[1..=8].iter().copied());
                            for c in filler.take(10usize.saturating_sub(shown)) {
                                let s = egui::Rect::from_min_size(
                                    egui::pos2(sx, sy),
                                    egui::vec2(14.0, 14.0),
                                );
                                p.rect_filled(s, egui::CornerRadius::same(2), c);
                                p.rect_stroke(
                                    s,
                                    egui::CornerRadius::same(2),
                                    egui::Stroke::new(1.0, t.border),
                                    egui::StrokeKind::Inside,
                                );
                                sx += 17.0;
                            }
                            // Chips: Apply (right), Preview/Previewing (left of it).
                            let aw = chip_width(ui, "Apply");
                            let ar = egui::Rect::from_min_size(
                                egui::pos2(
                                    card.right() - 6.0 - aw,
                                    sy + 7.0 - (CHIP_H - 2.0) / 2.0,
                                ),
                                egui::vec2(aw, CHIP_H - 2.0),
                            );
                            if n == 0 {
                                // Nothing to apply: disabled look, no interaction.
                                p.rect_stroke(
                                    ar,
                                    egui::CornerRadius::same(4),
                                    egui::Stroke::new(1.0, t.border),
                                    egui::StrokeKind::Inside,
                                );
                                p.text(
                                    ar.center(),
                                    egui::Align2::CENTER_CENTER,
                                    "Apply",
                                    egui::FontId::proportional(FONT_CHIP),
                                    t.dim,
                                );
                            } else if chip(ui, ar, ("theme_expert_apply", ti), "Apply", t, false)
                                .on_hover_text("Save this proposal as the active theme")
                                .clicked()
                            {
                                select = Some(Some(i));
                                apply = true;
                            }
                            let pl = if is_sel { "Previewing" } else { "Preview" };
                            let pw = chip_width(ui, pl);
                            let pr = egui::Rect::from_min_size(
                                egui::pos2(ar.left() - 6.0 - pw, ar.top()),
                                egui::vec2(pw, ar.height()),
                            );
                            if pr.left() > sx
                                && chip(ui, pr, ("theme_expert_preview", ti), pl, t, is_sel)
                                    .clicked()
                            {
                                select = Some(if is_sel { None } else { Some(i) });
                            }
                        });
                    }
                }
                if busy {
                    let dots = ((ui.input(|i| i.time) * 2.5) as usize % 3) + 1;
                    row(ui, width, 24.0, |ui, r| {
                        ui.painter().text(
                            egui::pos2(r.left() + 2.0, r.center().y),
                            egui::Align2::LEFT_CENTER,
                            format!("Thinking{}", ".".repeat(dots)),
                            egui::FontId::proportional(FONT_CHIP),
                            t.dim,
                        );
                    });
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(400));
                }
                if let Some(err) = error {
                    let short = err.lines().next().unwrap_or("").to_string();
                    row(ui, width, 24.0, |ui, r| {
                        let g = truncated(
                            ui,
                            &short,
                            egui::FontId::proportional(FONT_SMALL + 0.5),
                            t.danger,
                            r.width() - 4.0,
                        );
                        let gr = egui::Rect::from_min_size(
                            egui::pos2(r.left() + 2.0, r.center().y - g.size().y / 2.0),
                            g.size(),
                        );
                        ui.painter().galley(gr.min, g, t.danger);
                        ui.interact(
                            gr,
                            egui::Id::new("theme_expert_error"),
                            egui::Sense::hover(),
                        )
                        .on_hover_text(err);
                    });
                }
            });
        if let Some(s) = starter {
            self.expert.input = s.to_string();
        }
        if let Some(sel) = select {
            self.expert.selected = sel;
        }

        // --- input ---
        let send_w = chip_width(&panel, "Send");
        let te_rect = egui::Rect::from_min_max(
            input.min,
            egui::pos2(input.right() - send_w - 8.0, input.bottom()),
        );
        p.rect_filled(te_rect, egui::CornerRadius::same(4), t.desk_bg);
        p.rect_stroke(
            te_rect,
            egui::CornerRadius::same(4),
            egui::Stroke::new(1.0, t.border),
            egui::StrokeKind::Inside,
        );
        panel.visuals_mut().selection.bg_fill = t.selection_text_bg;
        let te = panel.put(
            te_rect.shrink2(egui::vec2(2.0, 2.0)),
            egui::TextEdit::multiline(&mut self.expert.input)
                .font(egui::FontId::proportional(FONT_CHIP))
                .text_color(t.text)
                .hint_text("Describe or refine your theme…  (Enter sends, Shift+Enter newline)")
                .desired_rows(2)
                .frame(egui::Frame::NONE)
                .margin(egui::Margin::symmetric(6, 4))
                .return_key(Some(egui::KeyboardShortcut::new(
                    egui::Modifiers::SHIFT,
                    egui::Key::Enter,
                )))
                .desired_width(te_rect.width()),
        );
        let can_send = !self.expert.busy() && !self.expert.input.trim().is_empty();
        let enter = te.lost_focus()
            && panel.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
        let send_rect = egui::Rect::from_min_size(
            egui::pos2(input.right() - send_w, input.center().y - CHIP_H / 2.0),
            egui::vec2(send_w, CHIP_H),
        );
        let send_clicked = chip(
            &mut panel,
            send_rect,
            "theme_expert_send",
            "Send",
            t,
            can_send,
        )
        .clicked();
        if (enter || send_clicked) && can_send {
            self.expert.send(&self.working, panel.ctx());
            if enter {
                te.request_focus();
            }
        }
        apply
    }
}

// ------------------------------------------------------------- row helpers

/// Allocate one `h`-tall row spanning `width` inside a child Ui so that any
/// `put`/`scope_builder` placed within it (the colour picker) cannot drag the
/// parent's cursor back up — the child's min_rect is the full row, and that is
/// what the parent advances past.
fn row<R>(
    ui: &mut egui::Ui,
    width: f32,
    h: f32,
    f: impl FnOnce(&mut egui::Ui, egui::Rect) -> R,
) -> R {
    let top_left = ui.cursor().min;
    let rect = egui::Rect::from_min_size(top_left, egui::vec2(width, h));
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        let (r, _) = ui.allocate_exact_size(egui::vec2(width, h), egui::Sense::hover());
        f(ui, r)
    })
    .inner
}

/// A section header: small bold dim caps with a hairline beneath.
fn section_header(ui: &mut egui::Ui, width: f32, label: &str, t: &Theme) {
    row(ui, width, HEADER_H, |ui, r| {
        ui.painter().text(
            egui::pos2(r.left() + PAD, r.bottom() - 10.0),
            egui::Align2::LEFT_CENTER,
            label.to_uppercase(),
            egui::FontId::proportional(FONT_SMALL),
            t.dim,
        );
        ui.painter().hline(
            egui::Rangef::new(r.left() + PAD, r.right() - PAD),
            r.bottom() - 1.0,
            egui::Stroke::new(1.0, t.border),
        );
    });
}

/// Paint the label (13pt) + description (11pt dim) of a row, truncated to
/// `max_w` so a narrow pane never runs text under the controls.
fn row_text(ui: &egui::Ui, r: egui::Rect, label: &str, desc: &str, max_w: f32, t: &Theme) {
    let g = truncated(
        ui,
        label,
        egui::FontId::proportional(FONT_LABEL),
        t.text,
        max_w,
    );
    ui.painter().galley(
        egui::pos2(r.left() + PAD, r.top() + 16.0 - g.size().y / 2.0),
        g,
        t.text,
    );
    if !desc.is_empty() {
        let g = truncated(
            ui,
            desc,
            egui::FontId::proportional(FONT_SMALL),
            t.dim,
            max_w,
        );
        ui.painter().galley(
            egui::pos2(r.left() + PAD, r.top() + 32.0 - g.size().y / 2.0),
            g,
            t.dim,
        );
    }
}

/// The font-size stepper (a `Settings` field riding the Ctrl+Scroll zoom seam,
/// not a theme token — it lives here because it is the one non-colour thing
/// people reach for in Appearance).
fn font_size_row(ui: &mut egui::Ui, width: f32, t: &Theme) {
    row(ui, width, ROW_H, |ui, r| {
        let anchor_x = r.right() - PAD;
        let cy = r.center().y;
        row_text(
            ui,
            r,
            "Font size",
            "Also Ctrl+Scroll · saved in settings",
            (r.width() - 180.0).max(60.0),
            t,
        );
        let mut fs = crate::terminal::font_size(ui.ctx());
        let plus = egui::Rect::from_min_size(
            egui::pos2(anchor_x - 20.0, cy - 10.0),
            egui::vec2(20.0, 20.0),
        );
        let vw = 56.0;
        let value_rect =
            egui::Rect::from_min_size(egui::pos2(plus.min.x - vw, cy - 10.0), egui::vec2(vw, 20.0));
        let minus = egui::Rect::from_min_size(
            egui::pos2(value_rect.min.x - 20.0, cy - 10.0),
            egui::vec2(20.0, 20.0),
        );
        let id = egui::Id::new("appearance_font_size");
        let rp = ui.interact(plus, id.with("plus"), egui::Sense::click());
        let rm = ui.interact(minus, id.with("minus"), egui::Sense::click());
        if rp.clicked() {
            fs = (fs + 1.0).min(crate::config::MAX_FONT_SIZE);
        }
        if rm.clicked() {
            fs = (fs - 1.0).max(crate::config::MIN_FONT_SIZE);
        }
        crate::terminal::set_font_size(ui.ctx(), fs);
        for (rc, sym, hov) in [(minus, "−", rm.hovered()), (plus, "+", rp.hovered())] {
            ui.painter().rect_stroke(
                rc,
                egui::CornerRadius::same(4),
                egui::Stroke::new(1.0, if hov { t.border_focus } else { t.border }),
                egui::StrokeKind::Inside,
            );
            ui.painter().text(
                rc.center(),
                egui::Align2::CENTER_CENTER,
                sym,
                egui::FontId::proportional(14.0),
                if hov { t.text } else { t.dim },
            );
        }
        ui.painter().text(
            egui::pos2(value_rect.max.x - 4.0, cy),
            egui::Align2::RIGHT_CENTER,
            format!("{fs:.0} pt"),
            egui::FontId::proportional(FONT_CHIP),
            t.text,
        );
    });
}

/// One token row: label + description left; the JSON key (dim mono) and the
/// swatch chip right. Returns true when the swatch changed the theme.
fn token_row(
    ui: &mut egui::Ui,
    width: f32,
    spec: &TokenSpec,
    working: &mut Theme,
    t: &Theme,
) -> bool {
    row(ui, width, ROW_H, |ui, r| {
        let cy = r.center().y;
        let sw = egui::Rect::from_min_size(
            egui::pos2(r.right() - PAD - SWATCH_W, cy - SWATCH_H / 2.0),
            egui::vec2(SWATCH_W, SWATCH_H),
        );
        let key_w = (r.width() * 0.3).clamp(60.0, 150.0);
        let kg = truncated(
            ui,
            spec.key,
            egui::FontId::monospace(FONT_SMALL),
            t.dim,
            key_w,
        );
        ui.painter().galley(
            egui::pos2(sw.left() - 10.0 - kg.size().x, cy - kg.size().y / 2.0),
            kg.clone(),
            t.dim,
        );
        let text_w = (sw.left() - 10.0 - kg.size().x - 12.0 - (r.left() + PAD)).max(40.0);
        row_text(ui, r, spec.label, spec.desc, text_w, t);
        let mut c = (spec.get)(working);
        let hover = move || {
            format!(
                "{} · {}\n{}",
                spec.key,
                crate::theme::color_hex::to_hex(c),
                spec.desc
            )
        };
        if swatch(ui, sw, spec.key, &mut c, spec.alpha, t, hover) {
            (spec.set)(working, c);
            true
        } else {
            false
        }
    })
}

/// The 16-colour ANSI palette: a caption row, then `Base` and `Bright` swatch
/// rows. Returns true if any slot changed.
fn palette_rows(
    ui: &mut egui::Ui,
    width: f32,
    palette: &mut [egui::Color32; 16],
    t: &Theme,
) -> bool {
    let mut changed = false;
    row(ui, width, 36.0, |ui, r| {
        row_text(
            ui,
            egui::Rect::from_min_size(r.min, egui::vec2(r.width(), ROW_H)),
            "Palette",
            "ANSI colours 0–15 used by programs in the terminal",
            r.width() - 2.0 * PAD,
            t,
        );
    });
    let label_w = 48.0;
    let gap = 5.0;
    let avail = width - 2.0 * PAD - label_w;
    let sw = ((avail - 7.0 * gap) / 8.0).clamp(16.0, 40.0);
    let sh = (sw * 0.6).clamp(14.0, 22.0);
    for (base, label) in [(0usize, "Base"), (8usize, "Bright")] {
        row(ui, width, sh + 8.0, |ui, r| {
            ui.painter().text(
                egui::pos2(r.left() + PAD, r.center().y),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional(FONT_SMALL),
                t.dim,
            );
            let mut x = r.left() + PAD + label_w;
            for cc in 0..8usize {
                let i = base + cc;
                let s = egui::Rect::from_min_size(
                    egui::pos2(x, r.center().y - sh / 2.0),
                    egui::vec2(sw, sh),
                );
                let mut c = palette[i];
                let hover = move || {
                    format!(
                        "palette[{i}] · {}\n{}",
                        crate::theme::color_hex::to_hex(c),
                        PALETTE_NAMES[i]
                    )
                };
                if swatch(ui, s, ("palette", i), &mut c, false, t, hover) {
                    palette[i] = c;
                    changed = true;
                }
                x += sw + gap;
            }
        });
    }
    changed
}

/// The six chat member colours as one swatch row (with its caption above).
fn member_colour_row(
    ui: &mut egui::Ui,
    width: f32,
    colours: &mut [egui::Color32; 6],
    t: &Theme,
) -> bool {
    let mut changed = false;
    row(ui, width, 36.0, |ui, r| {
        row_text(
            ui,
            egui::Rect::from_min_size(r.min, egui::vec2(r.width(), ROW_H)),
            "Member colours",
            CHAT_COLOR_DESC,
            r.width() - 2.0 * PAD,
            t,
        );
    });
    let gap = 5.0;
    let sw = ((width - 2.0 * PAD - 5.0 * gap) / 6.0).clamp(16.0, 44.0);
    let sh = (sw * 0.55).clamp(14.0, 22.0);
    row(ui, width, sh + 10.0, |ui, r| {
        let mut x = r.left() + PAD;
        for i in 0..6usize {
            let s = egui::Rect::from_min_size(
                egui::pos2(x, r.center().y - sh / 2.0),
                egui::vec2(sw, sh),
            );
            let mut c = colours[i];
            let hover =
                move || format!("chat_colors[{i}] · {}", crate::theme::color_hex::to_hex(c));
            if swatch(ui, s, ("chat_colors", i), &mut c, false, t, hover) {
                colours[i] = c;
                changed = true;
            }
            x += sw + gap;
        }
    });
    changed
}

// ------------------------------------------------------------- primitives

/// A colour swatch chip at `rect`: egui's colour-picker button is placed there
/// (so its popup works) and our chip — checker under alpha, the colour, a
/// bordered edge that brightens on hover — is painted over it. Returns true on
/// change. Opaque tokens edit RGB; alpha tokens edit straight (un-premultiplied)
/// RGBA, the egui path without low-alpha round-trip drift.
fn swatch(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    salt: impl std::hash::Hash,
    c: &mut egui::Color32,
    alpha: bool,
    t: &Theme,
    hover: impl FnOnce() -> String,
) -> bool {
    let mut changed = false;
    let resp = ui
        .push_id(salt, |ui| {
            ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                ui.spacing_mut().interact_size = rect.size();
                if alpha {
                    let mut a = c.to_srgba_unmultiplied();
                    let r = ui.color_edit_button_srgba_unmultiplied(&mut a);
                    if r.changed() {
                        *c = egui::Color32::from_rgba_unmultiplied(a[0], a[1], a[2], a[3]);
                        changed = true;
                    }
                    r
                } else {
                    let mut rgb = [c.r(), c.g(), c.b()];
                    let r = ui.color_edit_button_srgb(&mut rgb);
                    if r.changed() {
                        *c = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                        changed = true;
                    }
                    r
                }
            })
            .inner
        })
        .inner;
    let p = ui.painter();
    let cr = egui::CornerRadius::same(4);
    if alpha {
        checker(p, rect, t);
    }
    p.rect_filled(rect, cr, *c);
    p.rect_stroke(
        rect,
        cr,
        egui::Stroke::new(
            1.0,
            if resp.hovered() {
                t.border_focus
            } else {
                t.border
            },
        ),
        egui::StrokeKind::Inside,
    );
    // Built only while hovered — ~65 swatches would otherwise format a string
    // (with a hex encode) every frame the pane is open.
    resp.on_hover_ui(|ui| {
        ui.label(hover());
    });
    changed
}

/// Two-tone checker (window / title fills) so a translucent swatch reads as
/// translucent instead of as a darker opaque colour.
fn checker(p: &egui::Painter, rect: egui::Rect, t: &Theme) {
    p.rect_filled(rect, egui::CornerRadius::same(4), t.win_bg);
    let s = 5.0;
    let cols = (rect.width() / s).ceil() as i32;
    let rows = (rect.height() / s).ceil() as i32;
    let p = p.with_clip_rect(rect);
    for y in 0..rows {
        for x in 0..cols {
            if (x + y) % 2 == 0 {
                p.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(rect.left() + x as f32 * s, rect.top() + y as f32 * s),
                        egui::vec2(s, s),
                    ),
                    egui::CornerRadius::ZERO,
                    t.title_bg,
                );
            }
        }
    }
}

/// Width a chip needs for `label` (text + padding).
fn chip_width(ui: &egui::Ui, label: &str) -> f32 {
    let g = ui.painter().layout_no_wrap(
        label.to_string(),
        egui::FontId::proportional(FONT_CHIP),
        egui::Color32::PLACEHOLDER,
    );
    g.size().x + 20.0
}

/// A bordered chip button (the house `Kind::Choice`/`Kind::Action` look). An
/// `active` chip gets the focus border and the focused-title fill.
fn chip(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    salt: impl std::hash::Hash,
    label: &str,
    t: &Theme,
    active: bool,
) -> egui::Response {
    let resp = ui.interact(
        rect,
        egui::Id::new(("appearance_chip", salt)),
        egui::Sense::click(),
    );
    let p = ui.painter();
    let cr = egui::CornerRadius::same(4);
    if active {
        p.rect_filled(rect, cr, t.title_bg_focus);
    } else if resp.hovered() {
        p.rect_filled(rect, cr, t.sel_bg);
    }
    p.rect_stroke(
        rect,
        cr,
        egui::Stroke::new(
            1.0,
            if active || resp.hovered() {
                t.border_focus
            } else {
                t.border
            },
        ),
        egui::StrokeKind::Inside,
    );
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(FONT_CHIP),
        t.text,
    );
    resp
}

/// Wrap the scope chips into rows that fit `width` (each entry is the scope
/// and its chip width).
fn scope_chip_rows(ui: &egui::Ui, width: f32) -> Vec<Vec<(crate::theme_expert::Scope, f32)>> {
    let mut rows: Vec<Vec<(crate::theme_expert::Scope, f32)>> = vec![Vec::new()];
    let mut x = 0.0;
    for scope in crate::theme_expert::Scope::ALL {
        let w = chip_width(ui, scope.label()) - 6.0;
        if x + w > width && !rows.last().unwrap().is_empty() {
            rows.push(Vec::new());
            x = 0.0;
        }
        rows.last_mut().unwrap().push((scope, w));
        x += w + 6.0;
    }
    rows
}

/// `base` if free, else `base-2`, `base-3`, … — the first slug not in `existing`.
fn unique_slug(base: &str, existing: &std::collections::HashSet<String>) -> String {
    if !existing.contains(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|c| !existing.contains(c))
        .unwrap_or_else(|| base.to_string())
}

/// Lay out one line of text truncated with an ellipsis at `max_w`.
fn truncated(
    ui: &egui::Ui,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
    max_w: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_w.max(1.0));
    ui.painter().layout_job(job)
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
    fn resync_fires_on_name_change_or_foreign_live_theme_but_not_on_own_edit() {
        let mut v = AppearanceView::new();
        v.set_active("mine", Theme::foreman_warm());
        assert!(!v.needs_resync("mine", &Theme::foreman_warm()));
        assert!(
            v.needs_resync("other", &Theme::foreman_warm()),
            "name change"
        );
        let mut disk = Theme::foreman_warm();
        disk.bg = egui::Color32::from_rgb(9, 9, 9);
        assert!(v.needs_resync("mine", &disk), "disk changed under us");
        v.working_mut().bg = disk.bg; // our own edit, already published as live
        assert!(
            !v.needs_resync("mine", &disk),
            "live == working after our edit"
        );
    }

    #[test]
    fn expert_name_becomes_the_fork_slug_with_collision_fallback() {
        let existing: std::collections::HashSet<String> =
            ["ember-night".to_string(), "ember-night-2".to_string()]
                .into_iter()
                .collect();
        assert_eq!(unique_slug("ember-night", &existing), "ember-night-3");
        assert_eq!(unique_slug("fresh", &existing), "fresh");
        let v = AppearanceView::new();
        // A usable suggestion is slugged; an unusable one falls back to "<name> copy".
        assert!(
            v.fork_name_from(Some("Ember Night"))
                .starts_with("ember-night")
        );
        assert!(
            v.fork_name_from(Some("!!!"))
                .starts_with("foreman-warm-copy")
        );
        assert!(v.fork_name_from(None).starts_with("foreman-warm-copy"));
    }

    #[test]
    fn builtin_is_active_by_default() {
        let v = AppearanceView::new();
        assert!(v.active_is_builtin());
        assert_eq!(v.active_name(), BUILTIN);
    }
}
