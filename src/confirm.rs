//! The close-confirm modal: a self-contained view over an already-grouped list
//! of doomed processes. Knows nothing about window ids or how a close is
//! performed — the owner (wm.rs / main.rs) maps the outcome to an action.

use crate::proc::ProcInfo;
use crate::theme::*;
use eframe::egui;

pub enum ConfirmOutcome {
    Pending,
    Cancelled,
    Confirmed,
}

/// One labelled cluster of doomed processes. `scope` is the optional dim suffix
/// on the header ("3 terminals" in the quit variant); None otherwise.
pub struct ProcGroup {
    pub label: String,
    pub scope: Option<String>,
    pub procs: Vec<ProcInfo>,
}

pub struct ConfirmClose {
    title: String,
    lead: String,
    confirm_label: String,
    groups: Vec<ProcGroup>,
}

impl ConfirmClose {
    pub fn new(
        title: impl Into<String>,
        lead: impl Into<String>,
        confirm_label: impl Into<String>,
        groups: Vec<ProcGroup>,
    ) -> Self {
        Self {
            title: title.into(),
            lead: lead.into(),
            confirm_label: confirm_label.into(),
            groups,
        }
    }

    /// Total processes across all groups.
    pub fn total(&self) -> usize {
        self.groups.iter().map(|g| g.procs.len()).sum()
    }

    /// The modal heading (read-only; used by the wm close-gate tests).
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The lead line under the heading (read-only; used by the wm close-gate tests).
    pub fn lead(&self) -> &str {
        &self.lead
    }

    /// True once there is more than one group — render terminal-name headers and
    /// indent the processes under them. A single group renders flat.
    fn grouped(&self) -> bool {
        self.groups.len() > 1
    }

    /// Render one frame over `area` (dim + centered panel) and report the
    /// outcome. Esc → Cancelled, Enter → Confirmed; buttons mirror the keys.
    /// Flat when a single group, grouped + indented otherwise.
    pub fn show(&mut self, ui: &mut egui::Ui, area: egui::Rect) -> ConfirmOutcome {
        let mut outcome = ConfirmOutcome::Pending;

        ui.input(|i| {
            if i.key_pressed(egui::Key::Enter) {
                outcome = ConfirmOutcome::Confirmed;
            }
            if i.key_pressed(egui::Key::Escape) {
                outcome = ConfirmOutcome::Cancelled;
            }
        });

        // Dim only the owning manager's area (desktop for a project/quit close,
        // the project's rect for a terminal close).
        ui.painter()
            .rect_filled(area, 0.0, egui::Color32::from_black_alpha(150));

        let grouped = self.grouped();
        egui::Window::new(&self.title)
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.set_min_width(360.0);
                ui.label(egui::RichText::new(&self.title).strong().color(TEXT));
                ui.label(egui::RichText::new(&self.lead).color(DIM));
                ui.separator();

                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for g in &self.groups {
                            if grouped {
                                let mut header = egui::RichText::new(&g.label).color(DIM).monospace();
                                if let Some(scope) = &g.scope {
                                    header = egui::RichText::new(format!("{}   {scope}", g.label))
                                        .color(DIM)
                                        .monospace();
                                }
                                ui.label(header);
                            }
                            for p in &g.procs {
                                ui.horizontal(|ui| {
                                    if grouped {
                                        ui.add_space(14.0);
                                    }
                                    ui.label(egui::RichText::new(&p.name).color(TEXT).monospace());
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.label(
                                                egui::RichText::new(p.pid.to_string())
                                                    .color(DIM)
                                                    .monospace(),
                                            );
                                        },
                                    );
                                });
                            }
                        }
                    });

                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("cancel").clicked() {
                        outcome = ConfirmOutcome::Cancelled;
                    }
                    if ui
                        .button(egui::RichText::new(&self.confirm_label).color(CARET).strong())
                        .clicked()
                    {
                        outcome = ConfirmOutcome::Confirmed;
                    }
                });
            });

        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grp(label: &str, pids: &[u32]) -> ProcGroup {
        ProcGroup {
            label: label.into(),
            scope: None,
            procs: pids.iter().map(|&pid| ProcInfo { pid, name: format!("p{pid}.exe") }).collect(),
        }
    }

    #[test]
    fn total_sums_all_groups() {
        let c = ConfirmClose::new("t", "l", "close anyway",
            vec![grp("a", &[1, 2]), grp("b", &[3])]);
        assert_eq!(c.total(), 3);
    }

    #[test]
    fn single_group_renders_flat() {
        let c = ConfirmClose::new("t", "l", "close anyway", vec![grp("a", &[1])]);
        assert!(!c.grouped());
    }

    #[test]
    fn multiple_groups_render_grouped() {
        let c = ConfirmClose::new("t", "l", "close anyway",
            vec![grp("a", &[1]), grp("b", &[2])]);
        assert!(c.grouped());
    }
}
