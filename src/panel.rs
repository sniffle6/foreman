//! Task-manager panel: plain-data model + shallow view.
//!
//! Read seam vocabulary (`TargetPath`, `PanelModel`) is built by
//! `WindowManager::panel_model()`. The view paints rows and records clicks into
//! fields drained after the draw pass (same deferred-Act pattern as chat).

use crate::theme::*;
use crate::wm::WinId;
use eframe::egui;

/// Expanded panel width target (px).
pub const PANEL_W: f32 = 260.0;
/// Collapsed rail width (px).
pub const RAIL_W: f32 = 36.0;

/// Address of a row in the panel / `surface_target` write seam.
///
/// Semantics depend on the manager that interprets the path:
/// - **Desktop:** `project` is a desktop window id. When `window` is `None`,
///   optional `tab` selects a project tab on that window. When `window` is
///   `Some`, it is a child window inside a nested project manager, `ptab` is
///   the project tab owning it, and `tab` is the child's tab index.
/// - **Project manager (crew board):** `project` is a local window id, `window`
///   is `None`, and `tab` selects a tab on that window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TargetPath {
    pub project: WinId,
    /// Owning project-tab index on `project` when `window` is `Some`. Nested
    /// managers number child windows independently (each starts at 1), so
    /// tabbed projects collide under a bare child-id scan — this pins the tab.
    pub ptab: Option<usize>,
    pub window: Option<WinId>,
    pub tab: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowKind {
    Terminal(crate::icons::IconKind),
    Chat,
}

#[derive(Clone, Debug)]
pub struct TabEntry {
    pub path: TargetPath,
    pub title: String,
    pub kind: RowKind,
    /// The child *window* holding this tab is minimized inside its project.
    pub minimized: bool,
    /// This tab is its window's active tab (background tabs render dimmer).
    pub active_tab: bool,
    /// This tab is the focused leaf of the focused project.
    pub focused: bool,
    /// Terminal child process has exited (chat rows: always false).
    pub exited: bool,
}

#[derive(Clone, Debug)]
pub struct ProjectEntry {
    pub path: TargetPath,
    pub title: String,
    pub minimized: bool,
    pub focused: bool,
    pub tabs: Vec<TabEntry>,
}

#[derive(Clone, Debug, Default)]
pub struct PanelModel {
    pub projects: Vec<ProjectEntry>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PanelBtn {
    Min,
    Close,
}

/// Per-window view state for the task-manager panel (shallow view).
/// `model` is stashed by the desktop each frame before the draw pass;
/// `click` / `hover_act` / `toggle_collapse` are drained after it.
pub struct PanelView {
    pub model: PanelModel,
    pub collapsed: bool,
    pub expanded_width: f32,
    pub scroll: f32,
    pub click: Option<TargetPath>,
    pub hover_act: Option<(TargetPath, PanelBtn)>,
    pub toggle_collapse: bool,
}

impl PanelView {
    pub fn new(collapsed: bool, expanded_width: f32) -> Self {
        Self {
            model: PanelModel::default(),
            collapsed,
            expanded_width: expanded_width.clamp(RAIL_W + 40.0, 600.0),
            scroll: 0.0,
            click: None,
            hover_act: None,
            toggle_collapse: false,
        }
    }

    /// Paint the panel body (below the window title band). Records row
    /// interactions into `click` / `hover_act` / scroll; does not mutate the tree.
    pub fn show(&mut self, ui: &mut egui::Ui, rect: egui::Rect, base: egui::Id) {
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 0.0, BG);

        if self.collapsed {
            self.paint_rail(ui, rect, base);
            return;
        }

        let row_h = 22.0;
        let mut y = rect.min.y + 4.0 - self.scroll;
        let start_y = y;

        // Collect paint specs first so we can mutate self (click/hover) without
        // holding a borrow on model.
        let mut specs: Vec<(egui::Rect, egui::Id, RowPaintOwned)> = Vec::new();
        for (pi, proj) in self.model.projects.iter().enumerate() {
            let row = egui::Rect::from_min_size(
                egui::pos2(rect.min.x + 4.0, y),
                egui::vec2((rect.width() - 8.0).max(0.0), row_h),
            );
            if row_visible(row, rect) {
                specs.push((
                    row,
                    base.with(("prow", pi, proj.path.project)),
                    RowPaintOwned {
                        path: proj.path,
                        title: proj.title.clone(),
                        kind: None,
                        focused: proj.focused,
                        minimized: proj.minimized,
                        background_tab: false,
                        exited: false,
                        project_row: true,
                    },
                ));
            }
            y += row_h;

            for (ti, t) in proj.tabs.iter().enumerate() {
                let row = egui::Rect::from_min_size(
                    egui::pos2(rect.min.x + 16.0, y),
                    egui::vec2((rect.width() - 20.0).max(0.0), row_h),
                );
                if row_visible(row, rect) {
                    specs.push((
                        row,
                        base.with(("trow", pi, ti, t.path.window, t.path.tab)),
                        RowPaintOwned {
                            path: t.path,
                            title: t.title.clone(),
                            kind: Some(t.kind),
                            focused: t.focused,
                            minimized: t.minimized,
                            background_tab: !t.active_tab,
                            exited: t.exited,
                            project_row: false,
                        },
                    ));
                }
                y += row_h;
            }
            y += 4.0;
        }
        for (row, id, rp) in specs {
            self.paint_row(ui, row, id, rect, rp);
        }

        let content_h = (y - start_y) + self.scroll;
        let resp = ui.interact(rect, base.with("panel-scroll"), egui::Sense::hover());
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if resp.hovered() && scroll != 0.0 {
            let max_scroll = (content_h - rect.height()).max(0.0);
            self.scroll = (self.scroll - scroll).clamp(0.0, max_scroll);
        }
    }

    fn paint_rail(&mut self, ui: &mut egui::Ui, rect: egui::Rect, base: egui::Id) {
        let p = ui.painter().with_clip_rect(rect);
        let rails: Vec<_> = self
            .model
            .projects
            .iter()
            .enumerate()
            .map(|(pi, proj)| {
                (
                    pi,
                    proj.path,
                    proj.title.clone(),
                    proj.focused,
                    proj.minimized,
                )
            })
            .collect();
        let mut y = rect.min.y + 6.0;
        for (pi, path, title, focused, minimized) in rails {
            let cell = egui::Rect::from_center_size(
                egui::pos2(rect.center().x, y + 14.0),
                egui::vec2(28.0, 28.0),
            );
            let resp = ui
                .interact(
                    cell,
                    base.with(("rail", pi, path.project)),
                    egui::Sense::click(),
                )
                .on_hover_text(&title);
            if focused || resp.hovered() {
                p.rect_filled(cell, egui::CornerRadius::same(5), SEL_BG);
            }
            if focused {
                p.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(rect.min.x + 2.0, cell.min.y + 6.0),
                        egui::vec2(2.0, cell.height() - 12.0),
                    ),
                    0.0,
                    BORDER_FOCUS,
                );
            }
            let tint = if minimized {
                DIM
            } else {
                crate::icons::IconKind::Folder.tint()
            };
            paint_icon(
                ui,
                &p,
                cell.center(),
                14.0,
                crate::icons::IconKind::Folder,
                tint,
            );
            if resp.clicked() {
                self.click = Some(path);
            }
            y += 32.0;
        }
    }

    fn paint_row(
        &mut self,
        ui: &mut egui::Ui,
        row: egui::Rect,
        id: egui::Id,
        clip: egui::Rect,
        rp: RowPaintOwned,
    ) {
        let p = ui.painter().with_clip_rect(clip);
        let resp = ui.interact(row, id, egui::Sense::click());
        // Geometric containment, not `hovered()`: the min/close buttons below
        // are registered on top of this row, so hovering them un-hovers the
        // row — gating on `hovered()` made the buttons flicker in and out of
        // existence and drop their clicks (they only existed on frames the
        // row itself was topmost).
        let over = resp.hovered() || resp.contains_pointer();
        if rp.focused || over {
            p.rect_filled(row, egui::CornerRadius::same(4), SEL_BG);
        }
        if rp.focused {
            p.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(row.min.x, row.min.y + 4.0),
                    egui::vec2(2.0, row.height() - 8.0),
                ),
                0.0,
                BORDER_FOCUS,
            );
        }

        let col = if rp.exited {
            DIM
        } else if rp.focused {
            TEXT
        } else if rp.minimized || rp.background_tab {
            DIM
        } else {
            TEXT
        };

        // Icon
        let icon_c = egui::pos2(row.min.x + 9.0, row.center().y);
        match rp.kind {
            None => {
                let tint = if rp.minimized {
                    DIM
                } else {
                    crate::icons::IconKind::Folder.tint()
                };
                paint_icon(ui, &p, icon_c, 12.0, crate::icons::IconKind::Folder, tint);
            }
            Some(RowKind::Terminal(k)) => {
                let tint = if rp.minimized || rp.background_tab || rp.exited {
                    DIM
                } else {
                    k.tint()
                };
                paint_icon(ui, &p, icon_c, 12.0, k, tint);
            }
            Some(RowKind::Chat) => {
                p.text(
                    icon_c,
                    egui::Align2::CENTER_CENTER,
                    "§",
                    egui::FontId::proportional(12.0),
                    col,
                );
            }
        }

        // Title, truncated with … to the space left of the buttons/labels.
        let font = egui::FontId::proportional(if rp.project_row { 12.5 } else { 12.0 });
        let text_x = row.min.x + 20.0;
        let reserve = if over {
            38.0 // min + close buttons
        } else if rp.minimized || rp.background_tab {
            26.0 // "min"/"tab" label
        } else {
            6.0
        };
        let avail = (row.max.x - reserve - text_x).max(0.0);
        let mut job = egui::text::LayoutJob::simple_singleline(rp.title.clone(), font, col);
        job.wrap = egui::text::TextWrapping::truncate_at_width(avail);
        let galley = p.layout_job(job);
        let text_size = galley.size();
        p.galley(
            egui::pos2(text_x, row.center().y - text_size.y / 2.0),
            galley,
            col,
        );
        if rp.exited {
            let y = row.center().y;
            p.line_segment(
                [egui::pos2(text_x, y), egui::pos2(text_x + text_size.x, y)],
                egui::Stroke::new(1.0, DIM),
            );
        }

        let mut btn_hit = false;
        if over {
            let btn_y = row.center().y;
            let min_c = egui::pos2(row.max.x - 26.0, btn_y);
            let close_c = egui::pos2(row.max.x - 10.0, btn_y);
            let min_r = egui::Rect::from_center_size(min_c, egui::vec2(16.0, 16.0));
            let close_r = egui::Rect::from_center_size(close_c, egui::vec2(16.0, 16.0));
            let min_resp = ui.interact(min_r, id.with("min"), egui::Sense::click());
            let close_resp = ui.interact(close_r, id.with("close"), egui::Sense::click());
            p.text(
                min_c,
                egui::Align2::CENTER_CENTER,
                if rp.minimized { "□" } else { "–" },
                egui::FontId::proportional(11.0),
                if min_resp.hovered() { TEXT } else { DIM },
            );
            if close_resp.hovered() {
                p.rect_filled(close_r, egui::CornerRadius::same(3), DANGER);
            }
            p.text(
                close_c,
                egui::Align2::CENTER_CENTER,
                "×",
                egui::FontId::proportional(12.0),
                if close_resp.hovered() { TEXT } else { DIM },
            );
            if min_resp.clicked() {
                if rp.minimized {
                    // □ on a minimized row restores; MinPath would re-minimize.
                    self.click = Some(rp.path);
                } else {
                    self.hover_act = Some((rp.path, PanelBtn::Min));
                }
                btn_hit = true;
            }
            if close_resp.clicked() {
                self.hover_act = Some((rp.path, PanelBtn::Close));
                btn_hit = true;
            }
        } else if rp.minimized {
            p.text(
                egui::pos2(row.max.x - 8.0, row.center().y),
                egui::Align2::RIGHT_CENTER,
                "min",
                egui::FontId::proportional(10.0),
                DIM,
            );
        } else if rp.background_tab {
            p.text(
                egui::pos2(row.max.x - 8.0, row.center().y),
                egui::Align2::RIGHT_CENTER,
                "tab",
                egui::FontId::proportional(10.0),
                DIM,
            );
        }

        if resp.clicked() && !btn_hit {
            self.click = Some(rp.path);
        }
    }
}

struct RowPaintOwned {
    path: TargetPath,
    title: String,
    kind: Option<RowKind>,
    focused: bool,
    minimized: bool,
    background_tab: bool,
    exited: bool,
    project_row: bool,
}

fn row_visible(row: egui::Rect, clip: egui::Rect) -> bool {
    row.max.y >= clip.min.y && row.min.y <= clip.max.y
}

fn paint_icon(
    ui: &mut egui::Ui,
    p: &egui::Painter,
    center: egui::Pos2,
    size: f32,
    kind: crate::icons::IconKind,
    tint: egui::Color32,
) {
    let rect = egui::Rect::from_center_size(center, egui::vec2(size, size));
    let px = (size * ui.ctx().pixels_per_point()).round().max(1.0) as u32;
    let tex = crate::icons::texture(ui.ctx(), kind, px);
    p.image(
        tex.id(),
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        tint,
    );
}
