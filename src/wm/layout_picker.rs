//! The Project header's **Layout ▾** picker and **Save** button: the menu of
//! this Project's saved layouts, the naming and delete dialogs, and the
//! post-Apply summary. Spec:
//! `docs/superpowers/specs/2026-10-16-project-layouts-design.md` (Picker and
//! Save, Applying saved layouts, Revert). Feature doc: `docs/project-layouts.md`.
//!
//! State lives on the Project's child `WindowManager`, one per instance. The
//! UI turns clicks into [`PickerAction`]s and `run_layout_action` does the
//! work, so tests drive the same bindings without a pointer. Disk is touched
//! only on a click: the library is read once on first draw, when the menu
//! opens, and after each write, and that copy is kept (`LayoutStore` re-reads
//! the file on every call by design, so it must never run per frame).

use super::layouts::{LayoutApplied, LayoutRequest, RevertStatus};
use super::*;
use crate::layout_library::{LayoutStore, OnConflict, StoreError};
use crate::project_layout::{
    self as pl, ItemKind, LayoutDefinition, LayoutItemId, LayoutSelection, TreeSpec, WindowState,
};

/// How long "Saved …" stays up.
pub(super) const SAVED_NOTICE_SECS: f64 = 2.5;
const MENU_W: f32 = 292.0;
const NOTICE_W: f32 = 300.0;

/// Per-instance picker UI state. Runtime-only: nothing here is persisted.
#[derive(Default)]
pub(super) struct PickerUi {
    /// This Project's saved layouts as last read, or the read error.
    library: Option<Result<Vec<LayoutDefinition>, String>>,
    /// The New Project layout as last read with the library, to mark the
    /// row it was copied from while the two still match.
    new_project: Option<LayoutDefinition>,
    pub(super) menu_open: bool,
    pub(super) dialog: Option<Dialog>,
    pub(super) notice: Option<Notice>,
    /// The Create Layout… editor, while open.
    pub(super) create: Option<super::layout_create::CreateEditor>,
    /// What the queued request was, to title its outcome.
    pub(super) requested: Option<Requested>,
    /// Test seam: the store to use instead of the user's.
    #[cfg(test)]
    pub(super) store: Option<LayoutStore>,
}

impl PickerUi {
    /// An open menu or dialog owns the keyboard (Escape, Enter, the name
    /// field): this Project's terminals and the leader must not read keys
    /// meant for it.
    pub(super) fn captures_keyboard(&self) -> bool {
        self.dialog.is_some() || self.menu_open || self.create.is_some()
    }
}

pub(super) enum Requested {
    Apply { name: String },
    Revert,
    NewProject { name: String },
    /// A Create candidate; `saved` names the definition Save & Apply wrote.
    Create { saved: Option<String> },
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum NamePurpose {
    /// Save with no saved layout selected.
    Save,
    SaveAs,
    Rename {
        id: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Dialog {
    Name {
        purpose: NamePurpose,
        buf: String,
        /// Another layout already holds the name: `(id, name)`. The primary
        /// button becomes Replace; editing the name clears it.
        conflict: Option<(String, String)>,
        error: Option<String>,
        /// Focus the field on the next frame.
        focus: bool,
    },
    Delete {
        id: String,
        name: String,
        error: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Notice {
    /// After an Apply (or Revert) that was not an exact match.
    Summary {
        title: String,
        lines: Vec<(&'static str, String)>,
    },
    Error(String),
    /// Brief confirmation after a successful write; expires at `until`.
    Saved {
        text: String,
        until: f64,
    },
}

/// Everything a click can ask the picker to do.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum PickerAction {
    ToggleMenu,
    CloseMenu,
    /// Apply a saved layout by id, selecting it.
    Apply(String),
    /// Update the selected layout, or ask for a name if there is none.
    Save,
    SaveAs,
    Rename(String),
    Delete(String),
    /// Copy a saved layout into the New Project slot.
    UseForNewProjects(String),
    /// Open the Create Layout… editor.
    OpenCreate,
    /// A control inside the Create editor.
    Create(super::layout_create::CreateAction),
    Revert,
    /// The dialog's primary button: save, rename, replace, or delete.
    Submit,
    Cancel,
    /// The name field changed: the user is choosing another name.
    EditName,
    Dismiss,
}

impl WindowManager {
    /// The directory that keys this Project's library. `None` outside a
    /// Project, where the controls are disabled.
    fn layout_project_dir(&self) -> Option<std::path::PathBuf> {
        if self.desktop {
            return None;
        }
        self.cwd.clone()
    }

    pub(super) fn layout_store(&self) -> Result<LayoutStore, StoreError> {
        #[cfg(test)]
        {
            // Never the user's file from a test.
            Ok(self.layout_ui.store.clone().unwrap_or_else(|| {
                LayoutStore::at(std::env::temp_dir().join("foreman-layout-picker-unset"))
            }))
        }
        #[cfg(not(test))]
        LayoutStore::user()
    }

    /// Re-read this Project's saved layouts and keep the copy.
    pub(super) fn refresh_layouts(&mut self) {
        let Some(dir) = self.layout_project_dir() else {
            self.layout_ui.library = Some(Ok(Vec::new()));
            return;
        };
        let read = self
            .layout_store()
            .and_then(|store| store.project_layouts(&dir))
            .map_err(|e| match e {
                StoreError::Io(e) => format!("Saved layouts could not be read: {e}"),
                e => e.to_string(),
            });
        self.layout_ui.library = Some(read);
        self.layout_ui.new_project = self
            .layout_store()
            .and_then(|store| store.new_project_layout())
            .ok()
            .flatten();
    }

    /// `child` is a Project this (desktop) manager just opened from a
    /// directory. Queue the New Project layout on it, if one is set, for its
    /// first frame. Unset, or unreadable, leaves today's single terminal; an
    /// unreadable slot says so in a toast. One small read, on the open.
    pub(super) fn queue_new_project_layout(&self, child: &mut WindowManager, ctx: &egui::Context) {
        let def = match self.layout_store().and_then(|s| s.new_project_layout()) {
            Ok(Some(def)) => def,
            Ok(None) => return,
            Err(e) => {
                crate::notify::queue(
                    ctx,
                    crate::notify::Level::Warning,
                    format!("New Project layout not used: {e}"),
                );
                return;
            }
        };
        child.layout_ui.requested = Some(Requested::NewProject {
            name: def.name.clone(),
        });
        child.request_layout(LayoutRequest::NewProject { def });
    }

    fn cached_layouts(&self) -> &[LayoutDefinition] {
        match &self.layout_ui.library {
            Some(Ok(list)) => list,
            _ => &[],
        }
    }

    /// The selected definition, `None` for Custom (nothing selected, or it
    /// was deleted elsewhere).
    fn selected_layout(&self) -> Option<&LayoutDefinition> {
        crate::layout_library::resolve_selection(
            self.cached_layouts(),
            self.layout.selected.as_deref(),
        )
    }

    /// Picker label: the selected layout's name, or Custom.
    pub(super) fn layout_picker_label(&self) -> &str {
        self.selected_layout().map_or("Custom", |d| d.name.as_str())
    }

    /// Close the menu and cancel any dialog, changing nothing: the Project
    /// lost focus or is not drawn this frame.
    /// The Create editor goes too, and with it any pending request:
    /// nothing resumes.
    pub(super) fn close_layout_popups(&mut self) {
        self.layout_ui.menu_open = false;
        self.layout_ui.dialog = None;
        self.layout_ui.create = None;
    }

    /// Run one picker action. `ctx` supplies the clock and repaints.
    /// Writes and reads happen here, on the GUI thread, but only on a
    /// click: one small JSON file, never in the per-frame path.
    pub(super) fn run_layout_action(&mut self, action: PickerAction, ctx: &egui::Context) {
        let Some(dir) = self.layout_project_dir() else {
            self.layout_ui.menu_open = false;
            return;
        };
        let now = ctx.input(|i| i.time);
        match action {
            PickerAction::ToggleMenu => {
                self.layout_ui.menu_open = !self.layout_ui.menu_open;
                if self.layout_ui.menu_open {
                    self.refresh_layouts();
                    self.layout_ui.notice = None;
                    self.layout_ui.dialog = None;
                    self.layout_ui.create = None;
                }
            }
            PickerAction::CloseMenu => self.layout_ui.menu_open = false,
            PickerAction::Apply(id) => {
                self.layout_ui.menu_open = false;
                let Some(def) = self.cached_layouts().iter().find(|d| d.id == id).cloned() else {
                    self.refresh_layouts();
                    self.layout_ui.notice =
                        Some(Notice::Error(StoreError::NotFound(id).to_string()));
                    return;
                };
                self.layout_ui.notice = None;
                self.layout_ui.requested = Some(Requested::Apply {
                    name: def.name.clone(),
                });
                self.request_layout(LayoutRequest::Apply { def, select: true });
                ctx.request_repaint();
            }
            PickerAction::UseForNewProjects(id) => {
                self.layout_ui.menu_open = false;
                let Some(def) = self.cached_layouts().iter().find(|d| d.id == id).cloned() else {
                    self.refresh_layouts();
                    self.layout_ui.notice =
                        Some(Notice::Error(StoreError::NotFound(id).to_string()));
                    return;
                };
                let written = self
                    .layout_store()
                    .and_then(|store| store.set_new_project_layout(Some(&def)));
                self.layout_ui.notice = Some(match written {
                    Ok(()) => {
                        let text = format!("New Projects open with \u{201c}{}\u{201d}.", def.name);
                        self.layout_ui.new_project = Some(def);
                        Notice::Saved {
                            text,
                            until: now + SAVED_NOTICE_SECS,
                        }
                    }
                    Err(e) => Notice::Error(e.to_string()),
                });
            }
            PickerAction::Revert => {
                self.layout_ui.menu_open = false;
                match self.layout_revert_status() {
                    RevertStatus::Ready => {
                        self.layout_ui.notice = None;
                        self.layout_ui.requested = Some(Requested::Revert);
                        self.request_layout(LayoutRequest::Revert);
                        ctx.request_repaint();
                    }
                    RevertStatus::Invalidated => {
                        self.layout_ui.notice = Some(Notice::Error(
                            super::layouts::LayoutApplyError::RevertInvalidated.to_string(),
                        ));
                    }
                    RevertStatus::Unavailable => {}
                }
            }
            PickerAction::Save => {
                self.layout_ui.menu_open = false;
                // One popup at a time: Save's name dialog cannot draw
                // under the Create editor.
                self.layout_ui.create = None;
                if self.layout_ui.library.is_none() {
                    self.refresh_layouts();
                }
                match self
                    .selected_layout()
                    .map(|d| (d.id.clone(), d.name.clone()))
                {
                    Some((id, name)) => self.save_over(&dir, id, name, now),
                    None => self.open_name_dialog(NamePurpose::Save, String::new(), None),
                }
            }
            PickerAction::SaveAs => {
                self.layout_ui.menu_open = false;
                self.layout_ui.create = None;
                let suggested = self
                    .selected_layout()
                    .map(|d| format!("{} copy", d.name))
                    .unwrap_or_default();
                self.open_name_dialog(NamePurpose::SaveAs, suggested, None);
            }
            PickerAction::Rename(id) => {
                self.layout_ui.menu_open = false;
                let Some(name) = self
                    .cached_layouts()
                    .iter()
                    .find(|d| d.id == id)
                    .map(|d| d.name.clone())
                else {
                    return;
                };
                self.open_name_dialog(NamePurpose::Rename { id }, name, None);
            }
            PickerAction::Delete(id) => {
                self.layout_ui.menu_open = false;
                let Some(name) = self
                    .cached_layouts()
                    .iter()
                    .find(|d| d.id == id)
                    .map(|d| d.name.clone())
                else {
                    return;
                };
                self.layout_ui.notice = None;
                self.layout_ui.dialog = Some(Dialog::Delete {
                    id,
                    name,
                    error: None,
                });
            }
            PickerAction::OpenCreate => self.open_create_editor(),
            PickerAction::Create(action) => self.run_create_action(action, &dir, ctx),
            PickerAction::Submit => self.submit_dialog(&dir, now),
            PickerAction::Cancel => self.layout_ui.dialog = None,
            PickerAction::EditName => {
                if let Some(Dialog::Name {
                    conflict, error, ..
                }) = &mut self.layout_ui.dialog
                {
                    *conflict = None;
                    *error = None;
                }
            }
            PickerAction::Dismiss => self.layout_ui.notice = None,
        }
    }

    fn open_name_dialog(&mut self, purpose: NamePurpose, buf: String, error: Option<String>) {
        self.layout_ui.notice = None;
        self.layout_ui.dialog = Some(Dialog::Name {
            purpose,
            buf,
            conflict: None,
            error,
            focus: true,
        });
    }

    /// The arrangement Save writes. Zoom is ignored (`capture_arrangement`).
    fn capture_for_save(
        &self,
    ) -> Result<(Vec<pl::ItemRef>, pl::Arrangement), super::layouts::LayoutApplyError> {
        let area = self.last_area;
        if area.x < 1.0 || area.y < 1.0 {
            return Err(super::layouts::LayoutApplyError::NotLaidOut);
        }
        Ok(self.capture_arrangement(area))
    }

    /// Save with a layout selected: update it in place.
    fn save_over(&mut self, dir: &std::path::Path, id: String, name: String, now: f64) {
        let (items, arrangement) = match self.capture_for_save() {
            Ok(captured) => captured,
            Err(e) => {
                self.layout_ui.notice = Some(Notice::Error(e.to_string()));
                return;
            }
        };
        let result = self
            .layout_store()
            .and_then(|store| store.update(dir, &id, items, arrangement));
        match result {
            Ok(def) => self.saved(def, now),
            Err(StoreError::NotFound(_)) => {
                // Deleted elsewhere: the picker falls back to Custom and
                // Save asks for a name instead.
                self.refresh_layouts();
                let modified = self.layout.modified;
                self.set_layout_selection(LayoutSelection {
                    selected: None,
                    modified,
                });
                self.open_name_dialog(
                    NamePurpose::Save,
                    name,
                    Some(StoreError::NotFound(id).to_string()),
                );
            }
            Err(e) => self.layout_ui.notice = Some(Notice::Error(e.to_string())),
        }
    }

    /// A write succeeded: select `def`, clear Modified, re-read.
    fn saved(&mut self, def: LayoutDefinition, now: f64) {
        self.mark_layout_saved(def.id.clone());
        self.refresh_layouts();
        self.layout_ui.dialog = None;
        self.layout_ui.notice = Some(Notice::Saved {
            text: format!("Saved \u{201c}{}\u{201d}.", def.name),
            until: now + SAVED_NOTICE_SECS,
        });
    }

    fn submit_dialog(&mut self, dir: &std::path::Path, now: f64) {
        let Some(dialog) = self.layout_ui.dialog.clone() else {
            return;
        };
        let store = match self.layout_store() {
            Ok(store) => store,
            Err(e) => {
                self.set_dialog_error(e.to_string());
                return;
            }
        };
        match dialog {
            Dialog::Name {
                purpose,
                buf,
                conflict,
                ..
            } => {
                let on_conflict = if conflict.is_some() {
                    OnConflict::Replace
                } else {
                    OnConflict::Reject
                };
                let result = match &purpose {
                    NamePurpose::Save | NamePurpose::SaveAs => match self.capture_for_save() {
                        Ok((items, arrangement)) => {
                            store.create(dir, &buf, items, arrangement, on_conflict)
                        }
                        Err(e) => {
                            self.set_dialog_error(e.to_string());
                            return;
                        }
                    },
                    NamePurpose::Rename { id } => store.rename(dir, id, &buf, on_conflict),
                };
                match result {
                    Ok(def) => match purpose {
                        NamePurpose::Rename { .. } => {
                            // A replaced definition is gone; if it was the
                            // selection, the picker shows Custom.
                            if let Some((other, _)) = conflict
                                && self.layout.selected.as_deref() == Some(other.as_str())
                            {
                                self.set_layout_selection(LayoutSelection::default());
                            }
                            self.refresh_layouts();
                            self.layout_ui.dialog = None;
                        }
                        _ => self.saved(def, now),
                    },
                    Err(StoreError::NameTaken { id, name }) => {
                        if let Some(Dialog::Name {
                            conflict, error, ..
                        }) = &mut self.layout_ui.dialog
                        {
                            *conflict = Some((id, name));
                            *error = None;
                        }
                    }
                    Err(e) => {
                        if matches!(e, StoreError::NotFound(_)) {
                            self.refresh_layouts();
                        }
                        self.set_dialog_error(e.to_string());
                    }
                }
            }
            Dialog::Delete { id, .. } => match store.delete(dir, &id) {
                Ok(()) => {
                    // The live arrangement stays; the picker shows Custom.
                    if self.layout.selected.as_deref() == Some(id.as_str()) {
                        self.set_layout_selection(LayoutSelection::default());
                    }
                    self.refresh_layouts();
                    self.layout_ui.dialog = None;
                }
                Err(e) => self.set_dialog_error(e.to_string()),
            },
        }
    }

    fn set_dialog_error(&mut self, message: String) {
        match &mut self.layout_ui.dialog {
            Some(Dialog::Name { error, .. } | Dialog::Delete { error, .. }) => {
                *error = Some(message)
            }
            None => self.layout_ui.notice = Some(Notice::Error(message)),
        }
    }

    /// Turn the last processed request into the summary or an error.
    pub(super) fn poll_layout_outcome(&mut self, now: f64) {
        let Some(outcome) = self.take_layout_outcome() else {
            return;
        };
        let requested = self.layout_ui.requested.take();
        if let Some(Requested::Create { saved }) = requested {
            self.layout_ui.notice = Some(Self::create_outcome_notice(
                saved,
                outcome.map(|_| ()),
                now,
            ));
            return;
        }
        self.layout_ui.notice = match outcome {
            Ok(applied) if applied.is_exact() => None,
            Ok(applied) => {
                let title = match requested {
                    Some(Requested::Apply { name }) => {
                        format!("Applied \u{201c}{name}\u{201d} with changes")
                    }
                    Some(Requested::Revert) => "Reverted the layout".to_string(),
                    Some(Requested::NewProject { name }) => {
                        format!("Opened with \u{201c}{name}\u{201d}, with changes")
                    }
                    Some(Requested::Create { .. }) => unreachable!("handled above"),
                    None => "Layout applied with changes".to_string(),
                };
                let lines = summary_lines(&applied, |id| self.layout_item_title(id));
                Some(Notice::Summary { title, lines })
            }
            Err(e) => Some(Notice::Error(e.to_string())),
        };
    }

    pub(super) fn layout_item_title(&self, id: LayoutItemId) -> Option<String> {
        self.windows
            .iter()
            .flat_map(|w| w.tabs.iter())
            .find(|t| t.layout_id() == id)
            .map(|t| t.title.clone())
    }

    /// Draw the header's layout controls for this Project and their popups.
    /// `project` is the Project window's screen rect, `screen` the area
    /// popups stay inside. Returns whether the user interacted, so the
    /// caller can focus this Project.
    pub(super) fn show_layout_controls(
        &mut self,
        ui: &mut egui::Ui,
        ctl: LayoutCtlLayout,
        project: egui::Rect,
        screen: egui::Rect,
        focused: bool,
        id: egui::Id,
    ) -> bool {
        let th = crate::theme::live(ui.ctx());
        let now = ui.input(|i| i.time);
        self.poll_layout_outcome(now);
        if self.layout_ui.library.is_none() && self.layout_project_dir().is_some() {
            self.refresh_layouts();
        }
        if let Some(Notice::Saved { until, .. }) = &self.layout_ui.notice {
            if now >= *until {
                self.layout_ui.notice = None;
            } else {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_secs_f64(*until - now));
            }
        }
        let disabled = if self.layout_project_dir().is_none() {
            Some(super::layouts::LayoutApplyError::NotAProject.to_string())
        } else if self.app_modal {
            Some(super::layouts::LayoutApplyError::Busy.to_string())
        } else {
            None
        };
        // Popups belong to the focused Project: keys typed into another
        // one must never confirm this one's dialog.
        if disabled.is_some() || !focused {
            self.close_layout_popups();
        }
        let mut actions = Vec::new();
        let fg = if disabled.is_some() {
            th.dim_unfocused
        } else if focused {
            th.text
        } else {
            th.dim
        };

        // --- Layout ▾ ---
        let presp = ui.interact(ctl.picker, id.with("picker"), egui::Sense::click());
        let open = self.layout_ui.menu_open;
        let bg = if open {
            th.sel_bg
        } else if presp.hovered() && disabled.is_none() {
            th.win_btn_hover
        } else {
            egui::Color32::TRANSPARENT
        };
        let p = ui.painter();
        p.rect_filled(ctl.picker, egui::CornerRadius::same(4), bg);
        if !ctl.compact {
            p.rect_stroke(
                ctl.picker,
                egui::CornerRadius::same(4),
                egui::Stroke::new(1.0, th.border),
                egui::StrokeKind::Inside,
            );
        }
        let custom = self.selected_layout().is_none();
        let modified = !custom && self.layout.modified;
        let glyph = egui::Rect::from_center_size(
            egui::pos2(ctl.picker.min.x + 11.0, ctl.picker.center().y),
            egui::vec2(10.0, 9.0),
        );
        paint_layout_glyph(p, glyph, fg);
        let chevron = egui::pos2(ctl.picker.max.x - 8.0, ctl.picker.center().y);
        paint_chevron(p, chevron, fg);
        if ctl.compact {
            if modified {
                p.circle_filled(
                    egui::pos2(ctl.picker.max.x - 4.0, ctl.picker.min.y + 4.0),
                    2.5,
                    th.caret,
                );
            }
        } else {
            let font = egui::FontId::proportional(11.5);
            let text_min = glyph.max.x + 6.0;
            let dot_w = if modified { 9.0 } else { 0.0 };
            let text_max = chevron.x - 8.0 - dot_w;
            let galley = p.layout_no_wrap(
                self.layout_picker_label().to_string(),
                font,
                if custom { th.dim } else { fg },
            );
            let text_w = galley.size().x.min(text_max - text_min);
            let clip = egui::Rect::from_min_max(
                egui::pos2(text_min, ctl.picker.min.y),
                egui::pos2(text_max, ctl.picker.max.y),
            );
            p.with_clip_rect(clip).galley(
                egui::pos2(text_min, ctl.picker.center().y - galley.size().y / 2.0),
                galley,
                fg,
            );
            if modified {
                p.circle_filled(
                    egui::pos2(text_min + text_w + 6.0, ctl.picker.center().y),
                    2.5,
                    th.caret,
                );
            }
        }
        // Tooltips are built only while hovered, not every frame.
        let presp = presp.on_hover_ui(|ui| {
            let label = self.layout_picker_label();
            ui.label(match &disabled {
                Some(why) => why.clone(),
                None if modified => {
                    format!("Layout: {label} (Modified since it was applied or saved)")
                }
                None => format!("Layout: {label}"),
            });
        });
        if presp.clicked() && disabled.is_none() {
            actions.push(PickerAction::ToggleMenu);
        }

        // --- Save ---
        let sresp = ui.interact(ctl.save, id.with("save"), egui::Sense::click());
        let p = ui.painter();
        if sresp.hovered() && disabled.is_none() {
            p.rect_filled(ctl.save, egui::CornerRadius::same(4), th.win_btn_hover);
        }
        if ctl.compact {
            paint_save_glyph(p, ctl.save.center(), fg);
        } else {
            p.text(
                ctl.save.center(),
                egui::Align2::CENTER_CENTER,
                "Save",
                egui::FontId::proportional(11.5),
                fg,
            );
        }
        let sresp = sresp.on_hover_ui(|ui| {
            ui.label(match (&disabled, self.selected_layout()) {
                (Some(why), _) => why.clone(),
                (None, Some(def)) => format!(
                    "Save the current arrangement to \u{201c}{}\u{201d}",
                    def.name
                ),
                (None, None) => "Save the current arrangement as a named layout".to_string(),
            });
        });
        if sresp.clicked() && disabled.is_none() {
            actions.push(PickerAction::Save);
        }

        if self.layout_ui.menu_open {
            self.show_layout_menu(ui, &th, ctl, screen, id, &mut actions);
        }
        if self.layout_ui.create.is_some() {
            self.show_create_editor(ui, &th, project, screen, id, &mut actions);
        } else if self.layout_ui.dialog.is_some() {
            self.show_layout_dialog(ui, &th, project, id, &mut actions);
        } else if !self.layout_ui.menu_open && self.layout_ui.notice.is_some() {
            self.show_layout_notice(ui, &th, ctl, screen, id, &mut actions);
        }

        let interacted = presp.clicked() || sresp.clicked() || !actions.is_empty();
        for action in actions {
            self.run_layout_action(action, ui.ctx());
        }
        interacted
    }

    fn show_layout_menu(
        &self,
        ui: &mut egui::Ui,
        th: &crate::theme::Theme,
        ctl: LayoutCtlLayout,
        screen: egui::Rect,
        id: egui::Id,
        actions: &mut Vec<PickerAction>,
    ) {
        let menu_id = id.with("menu");
        let layouts = self.cached_layouts();
        let selected = self.layout.selected.as_deref();
        let revert = self.layout_revert_status();
        let x = (ctl.save.max.x - MENU_W).max(screen.min.x);
        let pos = egui::pos2(x, ctl.picker.max.y + 3.0);
        let small = egui::FontId::proportional(10.5);
        let font = egui::FontId::proportional(12.0);
        let shown = egui::Area::new(menu_id)
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .constrain_to(screen)
            .movable(false)
            .show(ui.ctx(), |mui| {
                egui::Frame::NONE
                    .fill(th.bg)
                    .stroke(egui::Stroke::new(1.0, th.border))
                    .corner_radius(egui::CornerRadius::same(4))
                    .inner_margin(egui::Margin::symmetric(0, 4))
                    .show(mui, |mui| {
                        mui.set_width(MENU_W);
                        mui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                        let header = |mui: &mut egui::Ui, text: &str| {
                            let (rect, _) = mui.allocate_exact_size(
                                egui::vec2(MENU_W, 20.0),
                                egui::Sense::hover(),
                            );
                            mui.painter().text(
                                egui::pos2(rect.min.x + 10.0, rect.center().y),
                                egui::Align2::LEFT_CENTER,
                                text,
                                small.clone(),
                                th.dim,
                            );
                        };
                        header(mui, "Layouts for this Project");
                        match &self.layout_ui.library {
                            Some(Err(e)) => wrapped(mui, e, th.danger),
                            _ if layouts.is_empty() => wrapped(
                                mui,
                                "No saved layouts yet. Layouts are saved for this Project: \
                                 arrange its windows, then Save.",
                                th.dim,
                            ),
                            _ => {}
                        }
                        for (ri, def) in layouts.iter().enumerate() {
                            let is_sel = selected == Some(def.id.as_str());
                            let (rect, _) = mui.allocate_exact_size(
                                egui::vec2(MENU_W, 34.0),
                                egui::Sense::hover(),
                            );
                            let rename = egui::Rect::from_center_size(
                                egui::pos2(rect.max.x - 44.0, rect.center().y),
                                egui::vec2(20.0, 20.0),
                            );
                            let delete = egui::Rect::from_center_size(
                                egui::pos2(rect.max.x - 20.0, rect.center().y),
                                egui::vec2(20.0, 20.0),
                            );
                            let for_new = egui::Rect::from_center_size(
                                egui::pos2(rect.max.x - 68.0, rect.center().y),
                                egui::vec2(20.0, 20.0),
                            );
                            let row = mui.interact(
                                egui::Rect::from_min_max(
                                    rect.min,
                                    egui::pos2(for_new.min.x - 2.0, rect.max.y),
                                ),
                                menu_id.with(("row", ri)),
                                egui::Sense::click(),
                            );
                            let rn = mui.interact(
                                rename,
                                menu_id.with(("rename", ri)),
                                egui::Sense::click(),
                            );
                            let del = mui.interact(
                                delete,
                                menu_id.with(("delete", ri)),
                                egui::Sense::click(),
                            );
                            let nw = mui.interact(
                                for_new,
                                menu_id.with(("for-new", ri)),
                                egui::Sense::click(),
                            );
                            // The slot is a copy: marked only while it still
                            // matches this definition.
                            let is_new = self.layout_ui.new_project.as_ref() == Some(def);
                            let mp = mui.painter();
                            if row.hovered() || rn.hovered() || del.hovered() || nw.hovered() {
                                mp.rect_filled(rect, 0.0, th.sel_bg);
                            }
                            if is_sel {
                                paint_check(
                                    mp,
                                    egui::pos2(rect.min.x + 12.0, rect.center().y),
                                    th.caret,
                                );
                            }
                            let diagram = egui::Rect::from_min_size(
                                egui::pos2(rect.min.x + 24.0, rect.center().y - 12.0),
                                egui::vec2(40.0, 24.0),
                            );
                            paint_diagram(mp, def, diagram, th);
                            let name_min = diagram.max.x + 10.0;
                            let tag = (is_sel && self.layout.modified).then_some("Modified");
                            let name_max = for_new.min.x - 6.0;
                            let galley = mp.layout_no_wrap(def.name.clone(), font.clone(), th.text);
                            let name_y = if tag.is_some() {
                                rect.center().y - 6.0
                            } else {
                                rect.center().y
                            };
                            mp.with_clip_rect(egui::Rect::from_min_max(
                                egui::pos2(name_min, rect.min.y),
                                egui::pos2(name_max, rect.max.y),
                            ))
                            .galley(
                                egui::pos2(name_min, name_y - galley.size().y / 2.0),
                                galley,
                                th.text,
                            );
                            if let Some(tag) = tag {
                                mp.text(
                                    egui::pos2(name_min, rect.center().y + 7.0),
                                    egui::Align2::LEFT_CENTER,
                                    tag,
                                    small.clone(),
                                    th.caret,
                                );
                            }
                            let icon =
                                |r: &egui::Response| if r.hovered() { th.text } else { th.dim };
                            if rn.hovered() {
                                mp.rect_filled(
                                    rename,
                                    egui::CornerRadius::same(3),
                                    th.win_btn_hover,
                                );
                            }
                            if del.hovered() {
                                mp.rect_filled(
                                    delete,
                                    egui::CornerRadius::same(3),
                                    th.win_btn_danger_hover,
                                );
                            }
                            if nw.hovered() {
                                mp.rect_filled(
                                    for_new,
                                    egui::CornerRadius::same(3),
                                    th.win_btn_hover,
                                );
                            }
                            paint_new_project(
                                mp,
                                for_new.center(),
                                if is_new { th.caret } else { icon(&nw) },
                            );
                            paint_pencil(mp, rename.center(), icon(&rn));
                            paint_cross(mp, delete.center(), icon(&del));
                            let nw = nw.on_hover_text(if is_new {
                                "New Projects open with this layout"
                            } else {
                                "Use for new Projects"
                            });
                            let rn = rn.on_hover_text("Rename\u{2026}");
                            let del = del.on_hover_text("Delete\u{2026}");
                            if row.clicked() {
                                actions.push(PickerAction::Apply(def.id.clone()));
                            }
                            if nw.clicked() {
                                actions.push(PickerAction::UseForNewProjects(def.id.clone()));
                            }
                            if rn.clicked() {
                                actions.push(PickerAction::Rename(def.id.clone()));
                            }
                            if del.clicked() {
                                actions.push(PickerAction::Delete(def.id.clone()));
                            }
                        }
                        divider(mui, th);
                        if menu_item(
                            mui,
                            th,
                            menu_id.with("create"),
                            "Create Layout\u{2026}",
                            Some("describe it"),
                            true,
                        )
                        .on_hover_text(
                            "Describe an arrangement of this Project's windows; a provider proposes it",
                        )
                        .clicked()
                        {
                            actions.push(PickerAction::OpenCreate);
                        }
                        let save_hint = self
                            .selected_layout()
                            .map(|d| format!("updates \u{201c}{}\u{201d}", d.name))
                            .unwrap_or_else(|| "asks for a name".to_string());
                        if menu_item(
                            mui,
                            th,
                            menu_id.with("save"),
                            "Save",
                            Some(&save_hint),
                            true,
                        )
                        .clicked()
                        {
                            actions.push(PickerAction::Save);
                        }
                        if menu_item(
                            mui,
                            th,
                            menu_id.with("saveas"),
                            "Save As\u{2026}",
                            None,
                            true,
                        )
                        .clicked()
                        {
                            actions.push(PickerAction::SaveAs);
                        }
                        if revert != RevertStatus::Unavailable {
                            divider(mui, th);
                            let ready = revert == RevertStatus::Ready;
                            if menu_item(
                                mui,
                                th,
                                menu_id.with("revert"),
                                "Revert Layout",
                                None,
                                ready,
                            )
                            .clicked()
                                && ready
                            {
                                actions.push(PickerAction::Revert);
                            }
                            if !ready {
                                wrapped(
                                    mui,
                                    &super::layouts::LayoutApplyError::RevertInvalidated
                                        .to_string(),
                                    th.dim,
                                );
                            }
                        }
                    });
            });
        // Close on a press outside the menu and the picker (the picker's
        // own click toggles it), or Escape.
        let menu_rect = shown.response.rect;
        let outside = ui.input(|i| {
            i.pointer.any_pressed()
                && i.pointer
                    .interact_pos()
                    .is_some_and(|p| !menu_rect.contains(p) && !ctl.picker.contains(p))
        });
        let escape = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if (outside || escape) && actions.is_empty() {
            actions.push(PickerAction::CloseMenu);
        }
    }

    fn show_layout_dialog(
        &mut self,
        ui: &mut egui::Ui,
        th: &crate::theme::Theme,
        project: egui::Rect,
        id: egui::Id,
        actions: &mut Vec<PickerAction>,
    ) {
        let selected_name = self.selected_layout().map(|d| d.name.clone());
        let Some(dialog) = &mut self.layout_ui.dialog else {
            return;
        };
        // Consumed so neither reaches anything else. Enter only submits a
        // name: Delete and Replace each need an explicit click.
        let (enter, escape) = ui.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        let enter = enter && matches!(dialog, Dialog::Name { conflict: None, .. });
        let field_id = id.with("name-field");
        let shown = egui::Window::new("Layout")
            .id(id.with("dialog"))
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::CENTER_CENTER)
            .fixed_pos(project.center())
            .show(ui.ctx(), |dui| {
                dui.set_width(320.0);
                match dialog {
                    Dialog::Name {
                        purpose,
                        buf,
                        conflict,
                        error,
                        focus,
                    } => {
                        let (title, lead, primary) = match purpose {
                            NamePurpose::Save => (
                                "Save layout",
                                "Name this arrangement. It is saved for this Project.".to_string(),
                                "save",
                            ),
                            NamePurpose::SaveAs => (
                                "Save layout as",
                                match &selected_name {
                                    Some(n) => format!(
                                        "Save a new layout from this arrangement. \u{201c}{n}\u{201d} stays as it is."
                                    ),
                                    None => "Save a new layout from this arrangement.".to_string(),
                                },
                                "save",
                            ),
                            NamePurpose::Rename { .. } => (
                                "Rename layout",
                                "The arrangement is unchanged.".to_string(),
                                "rename",
                            ),
                        };
                        dui.label(egui::RichText::new(title).strong().color(th.text));
                        dui.label(egui::RichText::new(lead).color(th.dim));
                        dui.add_space(4.0);
                        let resp = dui.add(
                            egui::TextEdit::singleline(buf)
                                .id(field_id)
                                .hint_text("Layout name")
                                .char_limit(200)
                                .desired_width(f32::INFINITY),
                        );
                        if std::mem::take(focus) {
                            resp.request_focus();
                        }
                        if resp.changed() {
                            actions.push(PickerAction::EditName);
                        }
                        if let Some((_, name)) = conflict {
                            dui.label(
                                egui::RichText::new(format!(
                                    "A layout named \u{201c}{name}\u{201d} already exists in this Project. Replace it, or type another name."
                                ))
                                .color(th.caret),
                            );
                        }
                        if let Some(e) = error {
                            dui.label(egui::RichText::new(e.as_str()).color(th.danger));
                        }
                        dui.add_space(4.0);
                        let primary = if conflict.is_some() { "replace" } else { primary };
                        dui.horizontal(|dui| {
                            if dui.button("cancel").clicked() {
                                actions.push(PickerAction::Cancel);
                            }
                            if dui
                                .button(egui::RichText::new(primary).color(th.caret).strong())
                                .clicked()
                            {
                                actions.push(PickerAction::Submit);
                            }
                        });
                    }
                    Dialog::Delete { name, error, .. } => {
                        dui.label(egui::RichText::new("Delete layout").strong().color(th.text));
                        dui.label(
                            egui::RichText::new(format!(
                                "Delete \u{201c}{name}\u{201d} from this Project's layouts? Windows stay as they are."
                            ))
                            .color(th.dim),
                        );
                        if let Some(e) = error {
                            dui.label(egui::RichText::new(e.as_str()).color(th.danger));
                        }
                        dui.add_space(4.0);
                        dui.horizontal(|dui| {
                            if dui.button("cancel").clicked() {
                                actions.push(PickerAction::Cancel);
                            }
                            if dui
                                .button(egui::RichText::new("delete").color(th.danger).strong())
                                .clicked()
                            {
                                actions.push(PickerAction::Submit);
                            }
                        });
                    }
                }
            });
        if actions.is_empty() {
            if escape {
                actions.push(PickerAction::Cancel);
            } else if enter {
                actions.push(PickerAction::Submit);
            } else if let Some(shown) = shown {
                let rect = shown.response.rect;
                let outside = ui.input(|i| {
                    i.pointer.any_pressed()
                        && i.pointer.interact_pos().is_some_and(|p| !rect.contains(p))
                });
                if outside {
                    actions.push(PickerAction::Cancel);
                }
            }
        }
    }

    fn show_layout_notice(
        &self,
        ui: &mut egui::Ui,
        th: &crate::theme::Theme,
        ctl: LayoutCtlLayout,
        screen: egui::Rect,
        id: egui::Id,
        actions: &mut Vec<PickerAction>,
    ) {
        let Some(notice) = &self.layout_ui.notice else {
            return;
        };
        let revert = self.layout_revert_status();
        let x = (ctl.save.max.x - NOTICE_W).max(screen.min.x);
        egui::Area::new(id.with("notice"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(x, ctl.picker.max.y + 3.0))
            .constrain_to(screen)
            .movable(false)
            .show(ui.ctx(), |nui| {
                egui::Frame::NONE
                    .fill(th.bg)
                    .stroke(egui::Stroke::new(1.0, th.border))
                    .corner_radius(egui::CornerRadius::same(4))
                    .inner_margin(egui::Margin::same(10))
                    .show(nui, |nui| {
                        nui.set_width(NOTICE_W - 20.0);
                        let dismissible = !matches!(notice, Notice::Saved { .. });
                        let mut headline = |nui: &mut egui::Ui, text: egui::RichText| {
                            nui.horizontal(|nui| {
                                nui.add(egui::Label::new(text).wrap());
                                if dismissible {
                                    nui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Min),
                                        |nui| {
                                            if nui
                                                .small_button("\u{2715}")
                                                .on_hover_text("Dismiss")
                                                .clicked()
                                            {
                                                actions.push(PickerAction::Dismiss);
                                            }
                                        },
                                    );
                                }
                            });
                        };
                        match notice {
                            Notice::Summary { title, lines } => {
                                headline(nui, egui::RichText::new(title).strong().color(th.text));
                                for (heading, items) in lines {
                                    nui.add(
                                        egui::Label::new(
                                            egui::RichText::new(format!("{heading}: {items}"))
                                                .color(th.dim)
                                                .size(11.5),
                                        )
                                        .wrap(),
                                    );
                                }
                                if revert != RevertStatus::Unavailable {
                                    nui.add_space(4.0);
                                    let ready = revert == RevertStatus::Ready;
                                    let resp = nui.add_enabled(
                                        ready,
                                        egui::Button::new(
                                            egui::RichText::new("Revert Layout").color(th.caret),
                                        ),
                                    );
                                    let resp = resp.on_disabled_hover_text(
                                        super::layouts::LayoutApplyError::RevertInvalidated
                                            .to_string(),
                                    );
                                    if resp.clicked() {
                                        actions.push(PickerAction::Revert);
                                    }
                                }
                            }
                            Notice::Error(e) => {
                                headline(nui, egui::RichText::new(e.as_str()).color(th.danger));
                            }
                            Notice::Saved { text, .. } => {
                                headline(nui, egui::RichText::new(text.as_str()).color(th.dim));
                            }
                        }
                    });
            });
    }
}

/// A user-facing name for a Content kind.
pub(super) fn kind_label(kind: ItemKind) -> &'static str {
    crate::layout_prompt::kind_name(kind)
}

/// Up to four names, then "+N more".
fn list_names(names: Vec<String>) -> String {
    const SHOWN: usize = 4;
    let extra = names.len().saturating_sub(SHOWN);
    let mut out: Vec<String> = names
        .into_iter()
        .take(SHOWN)
        .map(|n| {
            if n.chars().count() > 28 {
                format!("{}\u{2026}", n.chars().take(27).collect::<String>())
            } else {
                n
            }
        })
        .collect();
    if extra > 0 {
        out.push(format!("+{extra} more"));
    }
    out.join(", ")
}

/// The post-Apply summary, from the same validated reconciliation Apply
/// used: what was substituted, opened, moved to Additional windows, and left
/// unfilled. `title_of` names a live item; kinds stand in for the rest.
/// Empty sections are left out.
pub(super) fn summary_lines(
    applied: &LayoutApplied,
    title_of: impl Fn(LayoutItemId) -> Option<String>,
) -> Vec<(&'static str, String)> {
    let rec = &applied.reconciliation;
    let named = |id: LayoutItemId, kind: ItemKind| {
        title_of(id)
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| kind_label(kind).to_string())
    };
    let mut lines = Vec::new();
    let mut push = |heading, names: Vec<String>| {
        if !names.is_empty() {
            lines.push((heading, list_names(names)));
        }
    };
    push(
        "Stood in for saved windows",
        rec.substituted
            .iter()
            .map(|s| named(s.current, s.kind))
            .collect(),
    );
    push(
        "Opened",
        rec.opened
            .iter()
            .map(|s| kind_label(s.kind).to_string())
            .collect(),
    );
    push(
        "Moved to Additional windows",
        rec.additional.iter().map(|i| named(i.id, i.kind)).collect(),
    );
    let failed: Vec<pl::ItemRef> = applied.failed_opens.iter().map(|f| f.saved).collect();
    push(
        "Could not fill",
        rec.unfilled
            .iter()
            .filter(|i| !failed.contains(i))
            .map(|i| kind_label(i.kind).to_string())
            .collect(),
    );
    // Errors are not shortened: they say what to fix.
    let failures: Vec<String> = applied
        .failed_opens
        .iter()
        .map(|f| format!("{} ({})", kind_label(f.saved.kind), f.error))
        .collect();
    if !failures.is_empty() {
        lines.push(("Failed to open", failures.join(", ")));
    }
    lines
}

/// One cell of an arrangement thumbnail.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct DiagramCell {
    pub(super) rect: egui::Rect,
    /// Index into the definition's windows.
    pub(super) window: usize,
    pub(super) floating: bool,
    /// The kind of the stack's active tab.
    pub(super) kind: Option<ItemKind>,
}

/// The thumbnail of a definition inside `rect`: tiled windows from the tree
/// and ratios, then floating windows back to front at their normalized
/// rects. Minimized windows are not drawn. Arrangement data only.
pub(super) fn diagram_cells(def: &LayoutDefinition, rect: egui::Rect) -> Vec<DiagramCell> {
    let kind_of = |window: usize| {
        let w = def.arrangement.windows.get(window)?;
        let item = *w.tabs.get(w.active).or(w.tabs.first())?;
        def.items.iter().find(|i| i.id == item).map(|i| i.kind)
    };
    let mut cells = Vec::new();
    fn walk(spec: &TreeSpec, rect: egui::Rect, depth: usize, out: &mut Vec<(usize, egui::Rect)>) {
        match spec {
            TreeSpec::Leaf { window } => out.push((*window, rect)),
            TreeSpec::Split {
                dir,
                ratios,
                children,
            } => {
                if depth > pl::MAX_TREE_DEPTH {
                    return;
                }
                let sum: f32 = ratios
                    .iter()
                    .copied()
                    .filter(|r| r.is_finite() && *r > 0.0)
                    .sum();
                if sum <= 0.0 {
                    return;
                }
                let mut at = 0.0;
                for (child, r) in children.iter().zip(ratios) {
                    let share = if r.is_finite() && *r > 0.0 {
                        r / sum
                    } else {
                        0.0
                    };
                    let sub = match dir {
                        pl::SplitAxis::H => egui::Rect::from_min_max(
                            egui::pos2(rect.min.x + rect.width() * at, rect.min.y),
                            egui::pos2(rect.min.x + rect.width() * (at + share), rect.max.y),
                        ),
                        pl::SplitAxis::V => egui::Rect::from_min_max(
                            egui::pos2(rect.min.x, rect.min.y + rect.height() * at),
                            egui::pos2(rect.max.x, rect.min.y + rect.height() * (at + share)),
                        ),
                    };
                    walk(child, sub, depth + 1, out);
                    at += share;
                }
            }
        }
    }
    let mut tiled = Vec::new();
    if let Some(tree) = &def.arrangement.tree {
        walk(tree, rect, 0, &mut tiled);
    }
    for (window, r) in tiled {
        cells.push(DiagramCell {
            rect: r.shrink(0.5),
            window,
            floating: false,
            kind: kind_of(window),
        });
    }
    for (window, w) in def.arrangement.windows.iter().enumerate() {
        if let WindowState::Floating { rect: n } = &w.state {
            let local = n.to_local(
                pl::Size::new(rect.width(), rect.height()),
                pl::Size::new(6.0, 4.0),
            );
            cells.push(DiagramCell {
                rect: egui::Rect::from_min_size(
                    egui::pos2(rect.min.x + local.x, rect.min.y + local.y),
                    egui::vec2(local.w, local.h),
                ),
                window,
                floating: true,
                kind: kind_of(window),
            });
        }
    }
    cells
}

fn paint_diagram(
    p: &egui::Painter,
    def: &LayoutDefinition,
    rect: egui::Rect,
    th: &crate::theme::Theme,
) {
    p.rect_filled(rect, egui::CornerRadius::same(2), th.win_bg);
    p.rect_stroke(
        rect,
        egui::CornerRadius::same(2),
        egui::Stroke::new(1.0, th.border),
        egui::StrokeKind::Inside,
    );
    let inner = rect.shrink(2.0);
    for cell in diagram_cells(def, inner) {
        let (fill, stroke) = if cell.floating {
            (th.bg, th.text)
        } else if cell.kind == Some(ItemKind::Terminal) {
            (th.tab_bg_hover, th.dim)
        } else {
            (th.sel_bg, th.dim)
        };
        p.rect_filled(cell.rect, egui::CornerRadius::same(1), fill);
        p.rect_stroke(
            cell.rect,
            egui::CornerRadius::same(1),
            egui::Stroke::new(0.8, stroke),
            egui::StrokeKind::Inside,
        );
        // A one-letter kind mark where it fits, so a mixed terminal/viewer
        // layout reads at a glance.
        if let Some(kind) = cell.kind
            && cell.rect.width() >= 9.0
            && cell.rect.height() >= 9.0
        {
            p.text(
                cell.rect.center(),
                egui::Align2::CENTER_CENTER,
                kind_glyph(kind),
                egui::FontId::proportional(7.5),
                th.text,
            );
        }
    }
}

pub(super) fn kind_glyph(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Terminal => ">",
        ItemKind::Chat => "C",
        ItemKind::Board => "B",
        ItemKind::Plan => "P",
        ItemKind::GitHistory => "H",
        ItemKind::GitChanges => "G",
        ItemKind::GitDiff => "D",
        ItemKind::Image => "I",
        ItemKind::Project => "#",
    }
}

fn wrapped(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    egui::Frame::NONE
        .inner_margin(egui::Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.set_width(MENU_W - 20.0);
            ui.add(egui::Label::new(egui::RichText::new(text).color(color).size(11.5)).wrap());
        });
}

fn divider(ui: &mut egui::Ui, th: &crate::theme::Theme) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(MENU_W, 9.0), egui::Sense::hover());
    ui.painter().line_segment(
        [
            egui::pos2(rect.min.x + 10.0, rect.center().y),
            egui::pos2(rect.max.x - 10.0, rect.center().y),
        ],
        egui::Stroke::new(1.0, th.border),
    );
}

fn menu_item(
    ui: &mut egui::Ui,
    th: &crate::theme::Theme,
    id: egui::Id,
    label: &str,
    hint: Option<&str>,
    enabled: bool,
) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(MENU_W, 24.0), egui::Sense::hover());
    let resp = ui.interact(rect, id, egui::Sense::click());
    if resp.hovered() && enabled {
        ui.painter().rect_filled(rect, 0.0, th.sel_bg);
    }
    ui.painter().text(
        egui::pos2(rect.min.x + 24.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(12.0),
        if enabled { th.text } else { th.dim },
    );
    if let Some(hint) = hint {
        ui.painter().text(
            egui::pos2(rect.max.x - 10.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            hint,
            egui::FontId::proportional(10.5),
            th.dim,
        );
    }
    resp
}

/// Two side-by-side panes with a split: the "layout" mark.
fn paint_layout_glyph(p: &egui::Painter, r: egui::Rect, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.2, color);
    p.rect_stroke(
        r,
        egui::CornerRadius::same(1),
        stroke,
        egui::StrokeKind::Inside,
    );
    let x = r.min.x + r.width() * 0.45;
    p.line_segment([egui::pos2(x, r.min.y), egui::pos2(x, r.max.y)], stroke);
    let y = r.center().y;
    p.line_segment([egui::pos2(x, y), egui::pos2(r.max.x, y)], stroke);
}

fn paint_chevron(p: &egui::Painter, c: egui::Pos2, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.3, color);
    p.line_segment(
        [egui::pos2(c.x - 3.0, c.y - 1.5), egui::pos2(c.x, c.y + 1.5)],
        stroke,
    );
    p.line_segment(
        [egui::pos2(c.x, c.y + 1.5), egui::pos2(c.x + 3.0, c.y - 1.5)],
        stroke,
    );
}

/// A down arrow into a tray.
fn paint_save_glyph(p: &egui::Painter, c: egui::Pos2, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.3, color);
    p.line_segment(
        [egui::pos2(c.x, c.y - 5.0), egui::pos2(c.x, c.y + 1.5)],
        stroke,
    );
    p.line_segment(
        [egui::pos2(c.x - 3.0, c.y - 1.5), egui::pos2(c.x, c.y + 1.5)],
        stroke,
    );
    p.line_segment(
        [egui::pos2(c.x + 3.0, c.y - 1.5), egui::pos2(c.x, c.y + 1.5)],
        stroke,
    );
    p.line_segment(
        [
            egui::pos2(c.x - 5.0, c.y + 1.5),
            egui::pos2(c.x - 5.0, c.y + 4.5),
        ],
        stroke,
    );
    p.line_segment(
        [
            egui::pos2(c.x - 5.0, c.y + 4.5),
            egui::pos2(c.x + 5.0, c.y + 4.5),
        ],
        stroke,
    );
    p.line_segment(
        [
            egui::pos2(c.x + 5.0, c.y + 4.5),
            egui::pos2(c.x + 5.0, c.y + 1.5),
        ],
        stroke,
    );
}

fn paint_check(p: &egui::Painter, c: egui::Pos2, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.6, color);
    p.line_segment(
        [egui::pos2(c.x - 4.0, c.y), egui::pos2(c.x - 1.0, c.y + 3.0)],
        stroke,
    );
    p.line_segment(
        [
            egui::pos2(c.x - 1.0, c.y + 3.0),
            egui::pos2(c.x + 4.0, c.y - 3.5),
        ],
        stroke,
    );
}

fn paint_pencil(p: &egui::Painter, c: egui::Pos2, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.3, color);
    p.line_segment(
        [
            egui::pos2(c.x - 4.0, c.y + 4.0),
            egui::pos2(c.x + 3.0, c.y - 3.0),
        ],
        stroke,
    );
    p.line_segment(
        [
            egui::pos2(c.x + 1.5, c.y - 4.5),
            egui::pos2(c.x + 4.5, c.y - 1.5),
        ],
        stroke,
    );
    p.line_segment(
        [
            egui::pos2(c.x - 4.0, c.y + 4.0),
            egui::pos2(c.x - 4.0, c.y + 1.5),
        ],
        stroke,
    );
    p.line_segment(
        [
            egui::pos2(c.x - 4.0, c.y + 4.0),
            egui::pos2(c.x - 1.5, c.y + 4.0),
        ],
        stroke,
    );
}

/// A window with a plus: "new Projects".
fn paint_new_project(p: &egui::Painter, c: egui::Pos2, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.3, color);
    p.rect_stroke(
        egui::Rect::from_center_size(c, egui::vec2(11.0, 10.0)),
        egui::CornerRadius::same(1),
        stroke,
        egui::StrokeKind::Inside,
    );
    p.line_segment(
        [egui::pos2(c.x - 2.5, c.y), egui::pos2(c.x + 2.5, c.y)],
        stroke,
    );
    p.line_segment(
        [egui::pos2(c.x, c.y - 2.5), egui::pos2(c.x, c.y + 2.5)],
        stroke,
    );
}

fn paint_cross(p: &egui::Painter, c: egui::Pos2, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.3, color);
    let s = 3.5;
    p.line_segment(
        [egui::pos2(c.x - s, c.y - s), egui::pos2(c.x + s, c.y + s)],
        stroke,
    );
    p.line_segment(
        [egui::pos2(c.x - s, c.y + s), egui::pos2(c.x + s, c.y - s)],
        stroke,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_layout::{Arrangement, ItemRef, NormRect, SplitAxis, WindowSpec};

    const AREA: egui::Vec2 = egui::vec2(1000.0, 800.0);

    struct Fixture {
        _tmp: tempfile::TempDir,
        config: std::path::PathBuf,
        repo: std::path::PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        Fixture {
            _tmp: tmp,
            config,
            repo,
        }
    }

    impl Fixture {
        fn store(&self) -> LayoutStore {
            LayoutStore::at(&self.config)
        }

        /// A laid-out Project child manager on this fixture's directory and
        /// store, with no windows.
        fn project(&self) -> WindowManager {
            let mut wm = WindowManager::new();
            wm.cwd = Some(self.repo.clone());
            wm.tag = Some("p1".into());
            wm.last_area = AREA;
            wm.layout_ui.store = Some(self.store());
            wm
        }

        fn saved(&self) -> Vec<LayoutDefinition> {
            self.store().project_layouts(&self.repo).unwrap()
        }
    }

    /// A cheap layout item: an empty nested manager (kind Project).
    fn stub() -> Content {
        Content::Project(Box::new(WindowManager::new()))
    }

    fn tile(wm: &mut WindowManager, title: &str) -> LayoutItemId {
        let anchor = wm.focused;
        let (id, rect) = wm.next_slot(egui::vec2(400.0, 300.0));
        let tab = Tab::fixed(title, stub());
        let item = tab.layout_id();
        wm.push_win(id, tab, rect);
        wm.tile_new(id, anchor);
        wm.focused = Some(id);
        item
    }

    fn win_id_of(wm: &WindowManager, item: LayoutItemId) -> WinId {
        wm.windows
            .iter()
            .find(|w| w.tabs.iter().any(|t| t.layout_id() == item))
            .unwrap()
            .id
    }

    fn act(wm: &mut WindowManager, action: PickerAction) {
        let ctx = egui::Context::default();
        wm.run_layout_action(action, &ctx);
    }

    fn type_name(wm: &mut WindowManager, name: &str) {
        let Some(Dialog::Name { buf, .. }) = &mut wm.layout_ui.dialog else {
            panic!("expected the name dialog, got {:?}", wm.layout_ui.dialog);
        };
        *buf = name.to_string();
        act(wm, PickerAction::EditName);
    }

    /// Save with nothing selected, named `name`.
    fn save_new(wm: &mut WindowManager, name: &str) -> String {
        act(wm, PickerAction::Save);
        type_name(wm, name);
        act(wm, PickerAction::Submit);
        assert_eq!(wm.layout_ui.dialog, None, "saved and closed");
        wm.layout
            .selected
            .clone()
            .expect("Save selects the new layout")
    }

    fn arrangement(wm: &WindowManager) -> Arrangement {
        wm.capture_arrangement(wm.last_area).1
    }

    #[test]
    fn header_places_layout_controls_between_title_and_project_controls() {
        let tiny = egui::Rect::from_min_size(egui::pos2(30.0, 20.0), egui::vec2(139.0, 400.0));
        let hl = header_layout(tiny, true, false, HeaderSpec::Rename);
        assert!(
            hl.layout.is_none(),
            "no room: hidden, not drawn over the tabs"
        );
        for width in [140.0, 300.0, 519.0, 520.0, 800.0, 1600.0] {
            let scr = egui::Rect::from_min_size(egui::pos2(30.0, 20.0), egui::vec2(width, 400.0));
            for spec in [
                HeaderSpec::Title {
                    title_w: 5000.0,
                    has_icon: true,
                },
                HeaderSpec::Rename,
            ] {
                let hl = header_layout(scr, true, false, spec);
                let ctl = hl.layout.expect("projects carry the layout controls");
                assert_eq!(ctl.compact, width < LAYOUT_CTL_FULL_MIN_W, "w={width}");
                let ovf = hl
                    .controls
                    .iter()
                    .find(|(r, _)| *r == CtlRole::Ovf)
                    .unwrap()
                    .1;
                assert!(
                    ctl.save.max.x < ovf.min.x,
                    "Save clears the overflow (w={width})"
                );
                assert!(ctl.picker.max.x <= ctl.save.min.x, "w={width}");
                assert!(hl.avail_end <= ctl.picker.min.x, "fence (w={width})");
                assert!(
                    ctl.picker.min.x >= scr.min.x,
                    "inside the header (w={width})"
                );
                assert_eq!(ctl.picker.height(), ovf.height());
                if let Some(plus) = hl.plus {
                    assert!(plus.max.x <= ctl.picker.min.x, "+ (w={width})");
                }
                if let HeaderContentLayout::Title { label_rect, .. } = hl.content {
                    assert!(label_rect.max.x <= ctl.picker.min.x, "title (w={width})");
                }
            }
            let terminal = header_layout(
                scr,
                false,
                false,
                HeaderSpec::Title {
                    title_w: 10.0,
                    has_icon: false,
                },
            );
            assert!(terminal.layout.is_none(), "child windows get no picker");
        }
    }

    fn reconciliation() -> pl::Reconciliation {
        pl::Reconciliation {
            arrangement: Arrangement::default(),
            items: vec![],
            matched: vec![],
            substituted: vec![pl::Substitution {
                saved: 90,
                current: 1,
                kind: ItemKind::Terminal,
            }],
            to_open: vec![],
            opened: vec![pl::Substitution {
                saved: 91,
                current: 2,
                kind: ItemKind::Chat,
            }],
            additional: vec![
                ItemRef::new(3, ItemKind::Terminal),
                ItemRef::new(4, ItemKind::Image),
            ],
            kept_minimized: vec![ItemRef::new(5, ItemKind::Terminal)],
            unfilled: vec![
                ItemRef::new(92, ItemKind::GitDiff),
                ItemRef::new(93, ItemKind::Terminal),
            ],
        }
    }

    #[test]
    fn summary_lists_each_reconciliation_section_by_name() {
        let applied = LayoutApplied {
            reconciliation: reconciliation(),
            failed_opens: vec![super::super::layouts::FailedOpen {
                saved: ItemRef::new(93, ItemKind::Terminal),
                error: "terminal failed to start: boom".into(),
            }],
        };
        let titles = |id| match id {
            1 => Some("claude: fix login".to_string()),
            3 => Some("pwsh".to_string()),
            4 => Some("   ".to_string()),
            _ => None,
        };
        let lines = summary_lines(&applied, titles);
        assert_eq!(
            lines,
            vec![
                (
                    "Stood in for saved windows",
                    "claude: fix login".to_string()
                ),
                ("Opened", "Chat".to_string()),
                ("Moved to Additional windows", "pwsh, Image".to_string()),
                ("Could not fill", "Git diff".to_string()),
                (
                    "Failed to open",
                    "Terminal (terminal failed to start: boom)".to_string()
                ),
            ],
            "kept-minimized extras are not news: they stayed where they were"
        );
    }

    #[test]
    fn summary_truncates_long_lists() {
        let mut rec = reconciliation();
        rec.substituted.clear();
        rec.opened.clear();
        rec.unfilled.clear();
        rec.additional = (10..17)
            .map(|id| ItemRef::new(id, ItemKind::Terminal))
            .collect();
        let applied = LayoutApplied {
            reconciliation: rec,
            failed_opens: vec![],
        };
        let long = "x".repeat(40);
        let lines = summary_lines(&applied, |_| Some(long.clone()));
        let cut = format!("{}\u{2026}", "x".repeat(27));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].1, format!("{cut}, {cut}, {cut}, {cut}, +3 more"));
    }

    #[test]
    fn diagram_draws_tiles_by_ratio_then_floats_and_skips_minimized() {
        let def = LayoutDefinition {
            id: "d".into(),
            name: "d".into(),
            items: vec![
                ItemRef::new(1, ItemKind::Terminal),
                ItemRef::new(2, ItemKind::Chat),
                ItemRef::new(3, ItemKind::Board),
                ItemRef::new(4, ItemKind::Terminal),
            ],
            arrangement: Arrangement {
                windows: vec![
                    WindowSpec::tiled(vec![1], 0),
                    WindowSpec::tiled(vec![2], 0),
                    WindowSpec::floating(vec![3], 0, NormRect::new(0.5, 0.5, 0.25, 0.5)),
                    WindowSpec::minimized(vec![4], 0, true, NormRect::new(0.0, 0.0, 0.5, 0.5)),
                ],
                tree: Some(TreeSpec::Split {
                    dir: SplitAxis::H,
                    ratios: vec![3.0, 1.0],
                    children: vec![TreeSpec::Leaf { window: 0 }, TreeSpec::Leaf { window: 1 }],
                }),
            },
        };
        let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(40.0, 24.0));
        let cells = diagram_cells(&def, rect);
        assert_eq!(cells.len(), 3, "minimized windows are not drawn");
        assert_eq!(cells[0].kind, Some(ItemKind::Terminal));
        assert!(!cells[0].floating);
        assert!(
            (cells[0].rect.width() - 29.0).abs() < 0.01,
            "3:1 of 40, less the hairline"
        );
        assert_eq!(cells[1].kind, Some(ItemKind::Chat));
        assert!((cells[1].rect.min.x - 40.5).abs() < 0.01);
        assert!(cells[2].floating);
        assert_eq!(cells[2].kind, Some(ItemKind::Board));
        assert_eq!(cells[2].rect.min, egui::pos2(30.0, 22.0));
        for c in &cells {
            assert!(rect.contains_rect(c.rect));
        }
    }

    #[test]
    fn save_with_nothing_selected_asks_for_a_name_then_selects_it() {
        let fx = fixture();
        let mut wm = fx.project();
        tile(&mut wm, "a");
        tile(&mut wm, "b");
        assert_eq!(wm.layout_picker_label(), "Custom");

        act(&mut wm, PickerAction::Save);
        assert!(
            matches!(
                &wm.layout_ui.dialog,
                Some(Dialog::Name { purpose: NamePurpose::Save, buf, .. }) if buf.is_empty()
            ),
            "Save with Custom asks for a name: {:?}",
            wm.layout_ui.dialog
        );
        assert!(fx.saved().is_empty(), "nothing is written before a name");
        assert!(wm.layout_ui.captures_keyboard());
        assert!(!wm.terminal_owns_keyboard(), "the name field owns the keys");

        type_name(&mut wm, "  Coding  ");
        act(&mut wm, PickerAction::Submit);
        let saved = fx.saved();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].name, "Coding", "names are trimmed");
        assert_eq!(saved[0].arrangement, arrangement(&wm));
        assert_eq!(
            wm.layout,
            LayoutSelection {
                selected: Some(saved[0].id.clone()),
                modified: false
            }
        );
        assert_eq!(wm.layout_picker_label(), "Coding", "from the kept copy");
        assert_eq!(wm.layout_ui.dialog, None);
        assert!(
            matches!(&wm.layout_ui.notice, Some(Notice::Saved { text, .. }) if text.contains("Coding"))
        );
    }

    #[test]
    fn save_with_a_layout_selected_updates_it_without_asking() {
        let fx = fixture();
        let mut wm = fx.project();
        let a = tile(&mut wm, "a");
        tile(&mut wm, "b");
        let id = save_new(&mut wm, "Two");
        wm.watch_layout_edits(); // first frame: measure the baseline
        // A structural edit: minimize one window; the watch marks Modified.
        let win = win_id_of(&wm, a);
        wm.minimize(win);
        wm.watch_layout_edits();
        assert!(wm.layout.modified);

        act(&mut wm, PickerAction::Save);
        assert_eq!(
            wm.layout_ui.dialog, None,
            "no prompt when a layout is selected"
        );
        let saved = fx.saved();
        assert_eq!(saved.len(), 1, "updated in place");
        assert_eq!(saved[0].id, id);
        assert_eq!(saved[0].name, "Two");
        assert_eq!(saved[0].arrangement, arrangement(&wm));
        assert!(!wm.layout.modified, "Save clears Modified");
    }

    #[test]
    fn duplicate_names_offer_replace_or_another_name() {
        let fx = fixture();
        let mut wm = fx.project();
        tile(&mut wm, "a");
        let first = save_new(&mut wm, "Coding");
        tile(&mut wm, "b");

        act(&mut wm, PickerAction::SaveAs);
        assert!(matches!(
            &wm.layout_ui.dialog,
            Some(Dialog::Name { purpose: NamePurpose::SaveAs, buf, .. }) if buf == "Coding copy"
        ));
        type_name(&mut wm, "CODING");
        act(&mut wm, PickerAction::Submit);
        let Some(Dialog::Name { conflict, .. }) = &wm.layout_ui.dialog else {
            panic!("the dialog stays open on a duplicate");
        };
        assert_eq!(conflict, &Some((first.clone(), "Coding".to_string())));
        assert_eq!(fx.saved().len(), 1, "nothing written");
        assert_eq!(fx.saved()[0].arrangement.windows.len(), 1);

        // Choosing another name clears the offer.
        type_name(&mut wm, "Coding 2");
        assert!(matches!(
            &wm.layout_ui.dialog,
            Some(Dialog::Name { conflict: None, .. })
        ));

        // Back to the duplicate, then Replace: same id, new arrangement.
        type_name(&mut wm, "coding");
        act(&mut wm, PickerAction::Submit);
        act(&mut wm, PickerAction::Submit);
        let saved = fx.saved();
        assert_eq!(saved.len(), 1, "replaced in place");
        assert_eq!(saved[0].id, first);
        assert_eq!(saved[0].name, "coding");
        assert_eq!(saved[0].arrangement, arrangement(&wm));
        assert_eq!(wm.layout.selected.as_deref(), Some(first.as_str()));
    }

    #[test]
    fn invalid_names_and_write_failures_keep_everything_as_it_was() {
        let fx = fixture();
        let mut wm = fx.project();
        tile(&mut wm, "a");
        let before = arrangement(&wm);
        act(&mut wm, PickerAction::Save);
        type_name(&mut wm, "   ");
        act(&mut wm, PickerAction::Submit);
        assert!(
            matches!(&wm.layout_ui.dialog, Some(Dialog::Name { error: Some(e), .. }) if e.contains("empty")),
            "{:?}",
            wm.layout_ui.dialog
        );
        type_name(&mut wm, &"n".repeat(81));
        act(&mut wm, PickerAction::Submit);
        assert!(matches!(
            &wm.layout_ui.dialog,
            Some(Dialog::Name { error: Some(_), .. })
        ));
        assert!(fx.saved().is_empty());

        // The store's directory is a file: the write cannot happen.
        let blocked = fx.config.join("blocked");
        std::fs::write(&blocked, b"not a directory").unwrap();
        wm.layout_ui.store = Some(LayoutStore::at(&blocked));
        type_name(&mut wm, "Coding");
        act(&mut wm, PickerAction::Submit);
        let Some(Dialog::Name {
            error: Some(e),
            buf,
            ..
        }) = &wm.layout_ui.dialog
        else {
            panic!("a failed write keeps the dialog and says why");
        };
        assert!(e.starts_with("Layout not saved"), "{e}");
        assert_eq!(buf, "Coding", "the typed name survives");
        assert_eq!(wm.layout, LayoutSelection::default(), "no success reported");
        assert_eq!(arrangement(&wm), before);

        act(&mut wm, PickerAction::Cancel);
        assert_eq!(wm.layout_ui.dialog, None);
        assert!(!wm.layout_ui.captures_keyboard());
    }

    #[test]
    fn canceled_dialogs_change_neither_library_nor_arrangement() {
        let fx = fixture();
        let mut wm = fx.project();
        tile(&mut wm, "a");
        tile(&mut wm, "b");
        let id = save_new(&mut wm, "Keep");
        let library = fx.saved();
        let live = arrangement(&wm);
        let selection = wm.layout.clone();

        for open in [
            PickerAction::SaveAs,
            PickerAction::Rename(id.clone()),
            PickerAction::Delete(id.clone()),
        ] {
            act(&mut wm, open.clone());
            assert!(wm.layout_ui.dialog.is_some(), "{open:?} opens a dialog");
            if matches!(wm.layout_ui.dialog, Some(Dialog::Name { .. })) {
                type_name(&mut wm, "Something else");
            }
            act(&mut wm, PickerAction::Cancel);
            assert_eq!(wm.layout_ui.dialog, None);
            assert_eq!(fx.saved(), library, "{open:?}");
            assert_eq!(arrangement(&wm), live, "{open:?}");
            assert_eq!(wm.layout, selection, "{open:?}");
        }
    }

    #[test]
    fn rename_and_delete_follow_the_picker_contract() {
        let fx = fixture();
        let mut wm = fx.project();
        tile(&mut wm, "a");
        let one = save_new(&mut wm, "One");
        act(&mut wm, PickerAction::SaveAs);
        type_name(&mut wm, "Two");
        act(&mut wm, PickerAction::Submit);
        let two = wm.layout.selected.clone().unwrap();
        assert_ne!(one, two);

        // Rename One onto Two's name: offered, then Replace deletes Two,
        // which was selected, so the picker falls back to Custom.
        act(&mut wm, PickerAction::Rename(one.clone()));
        assert!(matches!(&wm.layout_ui.dialog, Some(Dialog::Name { buf, .. }) if buf == "One"));
        type_name(&mut wm, "two");
        act(&mut wm, PickerAction::Submit);
        assert!(matches!(
            &wm.layout_ui.dialog,
            Some(Dialog::Name {
                conflict: Some(_),
                ..
            })
        ));
        act(&mut wm, PickerAction::Submit);
        let saved = fx.saved();
        assert_eq!(saved.len(), 1);
        assert_eq!(
            (saved[0].id.as_str(), saved[0].name.as_str()),
            (one.as_str(), "two")
        );
        assert_eq!(wm.layout.selected, None);
        assert_eq!(wm.layout_picker_label(), "Custom");

        // Delete the selected layout: the picker shows Custom and the live
        // windows stay where they are.
        wm.mark_layout_saved(one.clone());
        let live = arrangement(&wm);
        act(&mut wm, PickerAction::Delete(one.clone()));
        assert!(matches!(&wm.layout_ui.dialog, Some(Dialog::Delete { name, .. }) if name == "two"));
        act(&mut wm, PickerAction::Submit);
        assert!(fx.saved().is_empty());
        assert_eq!(wm.layout, LayoutSelection::default(), "Custom");
        assert_eq!(arrangement(&wm), live, "deleting moves nothing");
        assert_eq!(wm.layout_ui.dialog, None);
    }

    #[test]
    fn use_for_new_projects_copies_the_layout_into_the_global_slot() {
        let fx = fixture();
        let mut wm = fx.project();
        tile(&mut wm, "a");
        let id = save_new(&mut wm, "Pair");
        let source = fx.saved()[0].clone();

        act(&mut wm, PickerAction::ToggleMenu);
        act(&mut wm, PickerAction::UseForNewProjects(id.clone()));
        assert!(!wm.layout_ui.menu_open);
        assert_eq!(
            fx.store().new_project_layout().unwrap(),
            Some(source.clone())
        );
        assert!(matches!(
            &wm.layout_ui.notice,
            Some(Notice::Saved { text, .. }) if text.contains("New Projects") && text.contains("Pair")
        ));
        assert_eq!(
            wm.layout_ui.new_project.as_ref(),
            Some(&source),
            "the row is marked"
        );
        assert_eq!(
            wm.layout.selected.as_deref(),
            Some(id.as_str()),
            "selection untouched"
        );

        // A copy, not a reference: updating, renaming, and deleting the
        // source leave the slot as it was.
        tile(&mut wm, "b");
        act(&mut wm, PickerAction::Save);
        assert_ne!(fx.saved()[0].arrangement, source.arrangement);
        act(&mut wm, PickerAction::Rename(id.clone()));
        type_name(&mut wm, "Renamed");
        act(&mut wm, PickerAction::Submit);
        act(&mut wm, PickerAction::ToggleMenu);
        assert_eq!(wm.layout_ui.new_project.as_ref(), Some(&source));
        assert_ne!(
            wm.cached_layouts().first(),
            Some(&source),
            "the edited source no longer matches, so its row is not marked"
        );
        act(&mut wm, PickerAction::Delete(id));
        act(&mut wm, PickerAction::Submit);
        assert!(fx.saved().is_empty());
        assert_eq!(fx.store().new_project_layout().unwrap(), Some(source));
    }

    #[test]
    fn use_for_new_projects_reports_a_failed_write() {
        let fx = fixture();
        let mut wm = fx.project();
        tile(&mut wm, "a");
        let id = save_new(&mut wm, "Pair");
        let blocked = fx.config.join("blocked");
        std::fs::write(&blocked, "").unwrap();
        wm.layout_ui.store = Some(LayoutStore::at(&blocked));
        act(&mut wm, PickerAction::UseForNewProjects(id));
        assert!(matches!(&wm.layout_ui.notice, Some(Notice::Error(_))));
        assert_eq!(wm.layout_ui.new_project, None);
        assert_eq!(fx.store().new_project_layout().unwrap(), None);

        // A row whose layout is gone says so instead of doing nothing.
        wm.layout_ui.store = Some(fx.store());
        act(&mut wm, PickerAction::UseForNewProjects("gone".into()));
        assert!(matches!(
            &wm.layout_ui.notice,
            Some(Notice::Error(e)) if e.contains("no longer exists")
        ));
        assert_eq!(fx.store().new_project_layout().unwrap(), None);
    }

    #[test]
    fn save_over_a_layout_deleted_elsewhere_asks_for_a_name() {
        let fx = fixture();
        let mut wm = fx.project();
        tile(&mut wm, "a");
        let id = save_new(&mut wm, "Gone");
        fx.store().delete(&fx.repo, &id).unwrap();

        act(&mut wm, PickerAction::Save);
        let Some(Dialog::Name {
            purpose,
            buf,
            error,
            ..
        }) = &wm.layout_ui.dialog
        else {
            panic!("expected the name dialog");
        };
        assert_eq!(purpose, &NamePurpose::Save);
        assert_eq!(buf, "Gone");
        assert!(error.as_deref().unwrap().contains("no longer exists"));
        assert_eq!(wm.layout.selected, None, "the picker shows Custom");
        assert_eq!(wm.layout_picker_label(), "Custom");
    }

    #[test]
    fn library_is_read_on_menu_open_and_after_writes_never_otherwise() {
        let fx = fixture();
        let mut a = fx.project();
        let mut b = fx.project();
        tile(&mut a, "a");
        tile(&mut b, "b");
        act(&mut b, PickerAction::ToggleMenu);
        act(&mut b, PickerAction::CloseMenu);
        assert!(b.cached_layouts().is_empty());

        // Instance A of the same directory saves; B's copy is stale until
        // its menu opens, and B's windows never move.
        let id = save_new(&mut a, "Shared");
        let b_live = arrangement(&b);
        assert!(b.cached_layouts().is_empty(), "no reads between clicks");
        act(&mut b, PickerAction::ToggleMenu);
        assert!(b.layout_ui.menu_open);
        assert_eq!(b.cached_layouts().len(), 1);
        assert_eq!(b.cached_layouts()[0].id, id);
        assert_eq!(b.layout.selected, None, "selection is per instance");
        assert_eq!(arrangement(&b), b_live);
    }

    #[test]
    fn selecting_applies_and_a_non_exact_apply_summarizes_with_revert() {
        let fx = fixture();
        let mut wm = fx.project();
        let a = tile(&mut wm, "alpha");
        tile(&mut wm, "beta");
        let ctx = egui::Context::default();
        // A saved layout with only "alpha" in it; "beta" will be an extra.
        let def = fx
            .store()
            .create(
                &fx.repo,
                "Solo",
                vec![ItemRef::new(a, ItemKind::Project)],
                Arrangement {
                    windows: vec![WindowSpec::tiled(vec![a], 0)],
                    tree: Some(TreeSpec::Leaf { window: 0 }),
                },
                OnConflict::Reject,
            )
            .unwrap();
        act(&mut wm, PickerAction::ToggleMenu);
        act(&mut wm, PickerAction::Apply(def.id.clone()));
        assert!(!wm.layout_ui.menu_open, "selecting closes the menu");
        // Nothing moved yet: the request runs at the next frame's boundary.
        assert_eq!(wm.layout.selected, None);
        wm.drain_layout_request(&ctx);
        wm.poll_layout_outcome(0.0);

        assert_eq!(wm.layout.selected.as_deref(), Some(def.id.as_str()));
        assert!(wm.layout.modified, "an extra went to Additional windows");
        let Some(Notice::Summary { title, lines }) = &wm.layout_ui.notice else {
            panic!("expected a summary, got {:?}", wm.layout_ui.notice);
        };
        assert!(title.contains("Solo"));
        assert_eq!(
            lines,
            &vec![("Moved to Additional windows", "beta".to_string())]
        );
        assert_eq!(wm.layout_revert_status(), RevertStatus::Ready);

        // Revert is one click from the summary.
        act(&mut wm, PickerAction::Revert);
        assert_eq!(wm.layout_ui.notice, None);
        wm.drain_layout_request(&ctx);
        wm.poll_layout_outcome(0.0);
        assert_eq!(wm.layout, LayoutSelection::default(), "selection restored");
        assert_eq!(wm.layout_ui.notice, None, "an exact Revert says nothing");
        assert_eq!(wm.windows.len(), 2);
    }

    #[test]
    fn an_exact_apply_shows_no_summary_and_errors_show_why() {
        let fx = fixture();
        let mut wm = fx.project();
        tile(&mut wm, "a");
        tile(&mut wm, "b");
        let ctx = egui::Context::default();
        let id = save_new(&mut wm, "Both");
        wm.layout_ui.notice = None;
        act(&mut wm, PickerAction::ToggleMenu);
        act(&mut wm, PickerAction::Apply(id));
        wm.drain_layout_request(&ctx);
        wm.poll_layout_outcome(0.0);
        assert_eq!(wm.layout_ui.notice, None);
        assert!(!wm.layout.modified);

        // A structural edit retires the revert point; Revert says why.
        let w = wm.windows[0].id;
        wm.minimize(w);
        wm.watch_layout_edits();
        assert_eq!(wm.layout_revert_status(), RevertStatus::Invalidated);
        act(&mut wm, PickerAction::Revert);
        assert!(
            matches!(&wm.layout_ui.notice, Some(Notice::Error(e)) if e.contains("Windows changed"))
        );

        // An entry the kept copy does not have: re-read and say so.
        act(&mut wm, PickerAction::ToggleMenu);
        act(&mut wm, PickerAction::Apply("not-there".into()));
        assert!(
            matches!(&wm.layout_ui.notice, Some(Notice::Error(e)) if e.contains("no longer exists")),
            "{:?}",
            wm.layout_ui.notice
        );
    }

    #[test]
    fn a_project_without_a_directory_disables_every_action() {
        let mut wm = WindowManager::new();
        wm.last_area = AREA;
        tile(&mut wm, "a");
        for action in [
            PickerAction::ToggleMenu,
            PickerAction::Save,
            PickerAction::SaveAs,
        ] {
            act(&mut wm, action);
        }
        assert!(!wm.layout_ui.menu_open);
        assert_eq!(wm.layout_ui.dialog, None);
        assert_eq!(wm.layout, LayoutSelection::default());
    }

    /// One frame of a desktop holding one Project.
    fn frame(
        ctx: &egui::Context,
        desk: &mut WindowManager,
        area: egui::Rect,
        t: f64,
        events: Vec<egui::Event>,
    ) {
        let input = egui::RawInput {
            screen_rect: Some(area),
            time: Some(t),
            events,
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| {
            desk.show(ui, area, true, egui::Id::new("picker-test"), false);
        });
    }

    fn click(
        ctx: &egui::Context,
        desk: &mut WindowManager,
        area: egui::Rect,
        t: &mut f64,
        pos: egui::Pos2,
    ) {
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        for events in [
            vec![egui::Event::PointerMoved(pos)],
            vec![button(true)],
            vec![button(false)],
            vec![],
        ] {
            *t += 0.05;
            frame(ctx, desk, area, *t, events);
        }
    }

    fn child(desk: &mut WindowManager) -> &mut WindowManager {
        match &mut desk.windows[0].tabs[0].content {
            Content::Project(inner) => inner,
            _ => panic!("expected a Project"),
        }
    }

    #[test]
    fn header_buttons_drive_the_picker_through_real_frames() {
        let fx = fixture();
        let mut project = fx.project();
        tile(&mut project, "a");
        tile(&mut project, "b");
        let mut desk = WindowManager::new();
        desk.desktop = true;
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 700.0));
        desk.push_win(
            1,
            Tab::fixed("repo", Content::Project(Box::new(project))),
            area,
        );
        desk.focused = Some(1);
        let ctx = egui::Context::default();
        let mut t = 0.0;
        frame(&ctx, &mut desk, area, t, vec![]);
        let scr = desk.windows[0].rect.translate(area.min.to_vec2());
        let ctl = header_layout(scr, true, false, HeaderSpec::Rename)
            .layout
            .unwrap();
        assert!(!ctl.compact);

        // Save with Custom opens the name dialog; typing and Enter save.
        click(&ctx, &mut desk, area, &mut t, ctl.save.center());
        assert!(matches!(
            child(&mut desk).layout_ui.dialog,
            Some(Dialog::Name {
                purpose: NamePurpose::Save,
                ..
            })
        ));
        t += 0.05;
        frame(
            &ctx,
            &mut desk,
            area,
            t,
            vec![egui::Event::Text("Grid".into())],
        );
        t += 0.05;
        frame(
            &ctx,
            &mut desk,
            area,
            t,
            vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        let saved = fx.saved();
        assert_eq!(
            saved.len(),
            1,
            "dialog: {:?}",
            child(&mut desk).layout_ui.dialog
        );
        assert_eq!(saved[0].name, "Grid");
        assert_eq!(child(&mut desk).layout_picker_label(), "Grid");
        assert_eq!(child(&mut desk).layout_ui.dialog, None);

        // The picker opens the menu; a press elsewhere closes it.
        click(&ctx, &mut desk, area, &mut t, ctl.picker.center());
        assert!(child(&mut desk).layout_ui.menu_open);
        assert!(desk.any_layout_capture(), "the open menu owns the keys");
        assert!(desk.overlay_blocks_close());
        click(&ctx, &mut desk, area, &mut t, egui::pos2(500.0, 500.0));
        assert!(!child(&mut desk).layout_ui.menu_open);
        assert!(!desk.any_layout_capture());
    }

    fn key(key: egui::Key) -> Vec<egui::Event> {
        vec![egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]
    }

    /// A desktop holding one laid-out Project on `fx`, after one frame.
    fn desk_with_project(fx: &Fixture) -> (egui::Context, WindowManager, egui::Rect, f64) {
        let mut project = fx.project();
        tile(&mut project, "a");
        tile(&mut project, "b");
        let mut desk = WindowManager::new();
        desk.desktop = true;
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 700.0));
        desk.push_win(
            1,
            Tab::fixed("repo", Content::Project(Box::new(project))),
            area,
        );
        desk.focused = Some(1);
        let ctx = egui::Context::default();
        frame(&ctx, &mut desk, area, 0.0, vec![]);
        (ctx, desk, area, 0.0)
    }

    #[test]
    fn enter_never_confirms_a_delete_or_a_replace() {
        let fx = fixture();
        let (ctx, mut desk, area, mut t) = desk_with_project(&fx);
        let id = save_new(child(&mut desk), "Keep");

        act(child(&mut desk), PickerAction::Delete(id.clone()));
        t += 0.05;
        frame(&ctx, &mut desk, area, t, key(egui::Key::Enter));
        assert!(matches!(
            child(&mut desk).layout_ui.dialog,
            Some(Dialog::Delete { .. })
        ));
        assert_eq!(fx.saved().len(), 1, "Delete needs a click");
        t += 0.05;
        frame(&ctx, &mut desk, area, t, key(egui::Key::Escape));
        assert_eq!(child(&mut desk).layout_ui.dialog, None, "Escape cancels");

        act(child(&mut desk), PickerAction::SaveAs);
        type_name(child(&mut desk), "keep");
        act(child(&mut desk), PickerAction::Submit);
        let before = fx.saved();
        t += 0.05;
        frame(&ctx, &mut desk, area, t, key(egui::Key::Enter));
        assert!(
            matches!(
                child(&mut desk).layout_ui.dialog,
                Some(Dialog::Name {
                    conflict: Some(_),
                    ..
                })
            ),
            "Replace needs a click"
        );
        assert_eq!(fx.saved(), before);
    }

    #[test]
    fn popups_close_when_their_project_loses_focus_or_is_not_drawn() {
        let fx = fixture();
        let (ctx, mut desk, area, mut t) = desk_with_project(&fx);
        let id = save_new(child(&mut desk), "Keep");

        // Focus leaves the Project: its Delete dialog is canceled, so an
        // Enter typed elsewhere can never confirm it.
        act(child(&mut desk), PickerAction::Delete(id));
        assert!(desk.any_layout_capture());
        desk.focused = None;
        t += 0.05;
        frame(&ctx, &mut desk, area, t, key(egui::Key::Enter));
        assert_eq!(child(&mut desk).layout_ui.dialog, None);
        assert_eq!(fx.saved().len(), 1, "canceled, not confirmed");
        assert!(!desk.any_layout_capture());

        // A minimized Project draws no header: its open menu closes rather
        // than keep its terminals frozen out of sight.
        desk.focused = Some(1);
        act(child(&mut desk), PickerAction::ToggleMenu);
        desk.windows[0].minimized = true;
        t += 0.05;
        frame(&ctx, &mut desk, area, t, vec![]);
        assert!(!child(&mut desk).layout_ui.menu_open);
        assert!(!desk.any_layout_capture());
    }
}
