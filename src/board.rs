//! The per-project kanban board window (`Content::Board`).
//!
//! Read seam: the shared [`crate::kanban::CardStore`] snapshot, borrowed once
//! per frame into locals here (`Card::clone` is cheap; the store's own
//! reload/orphan-derivation cadence is Task 3's `kanban_tick`, not this
//! view's job). Write seam: every user action is recorded as a [`BoardAct`]
//! onto `acts` and drained by the window manager after `apply_acts` — content
//! can never mutate the manager mid-draw. This mirrors the chat viewer's
//! `click`/`pending_post` fields and the task-manager panel's act/drain
//! pattern (`docs/task-manager-panel.md`). Card footers have fixed geometry;
//! collapse and detail selection are local, transient view state.

use eframe::egui;

/// Agents foreman already detects for tab icons and skill installs. No
/// persisted default (spec) — the dispatch picker always starts blank.
pub const AGENTS: &[&str] = &["claude", "codex", "grok"];

const CARD_H: f32 = 96.0;
const CARD_GAP: f32 = 6.0;
const HEADER_H: f32 = 30.0;
const QUICK_ADD_H: f32 = 24.0;
const PAD: f32 = 6.0;
const BTN_W: f32 = 46.0;
const BTN_H: f32 = 22.0;
const BTN_GAP: f32 = 4.0;

/// Draw the disclosure marker rather than depending on font glyph coverage.
fn disclosure(
    p: &egui::Painter,
    center: egui::Pos2,
    scale: f32,
    collapsed: bool,
    color: egui::Color32,
) {
    let offsets = if collapsed {
        [
            egui::vec2(-2.0, -4.0),
            egui::vec2(2.0, 0.0),
            egui::vec2(-2.0, 4.0),
        ]
    } else {
        [
            egui::vec2(-4.0, -2.0),
            egui::vec2(0.0, 2.0),
            egui::vec2(4.0, -2.0),
        ]
    };
    p.add(egui::Shape::line(
        offsets.map(|v| center + v * scale).to_vec(),
        egui::Stroke::new(scale, color),
    ));
}

/// Rails retain their width; expanded lanes share the space left over.
fn column_widths(width: f32, collapsed: [bool; 4]) -> [f32; 4] {
    let width = width.max(0.0);
    let rail = 64.0_f32.min(width / 4.0);
    let count = collapsed.iter().filter(|&&c| c).count();
    let expanded = if count == 4 {
        0.0
    } else {
        (width - rail * count as f32) / (4 - count) as f32
    };
    collapsed.map(|c| if c { rail } else { expanded })
}

fn detail_text(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(text).wrap().selectable(true));
    // Keep multiline text intact and offer explicit HTTP(S) references below it.
    let mut seen = std::collections::HashSet::new();
    for word in text.split_whitespace() {
        let url = word.trim_matches(|c: char| {
            matches!(
                c,
                '(' | ')' | '[' | ']' | '<' | '>' | '"' | '\'' | ',' | '.' | '`'
            )
        });
        if (url.starts_with("https://") || url.starts_with("http://")) && seen.insert(url) {
            ui.hyperlink(url);
        }
    }
}

/// Fixed column order the board always renders, left to right.
const COLUMNS: [crate::kanban::CardState; 4] = [
    crate::kanban::CardState::Backlog,
    crate::kanban::CardState::InProgress,
    crate::kanban::CardState::Blocked,
    crate::kanban::CardState::Done,
];

fn column_title(state: crate::kanban::CardState) -> &'static str {
    use crate::kanban::CardState::*;
    match state {
        Backlog => "Backlog",
        InProgress => "In Progress",
        Blocked => "Blocked",
        Done => "Done",
    }
}

/// Pointer-in-sub-rect gate for a nested region (a column body, a card).
/// `over` must be `resp.hovered() || resp.contains_pointer()`, never
/// `hovered()` alone: same-layer children registered later in the same draw
/// (a card's own buttons, its jump rect, its title-hover rect) win
/// `hovered()` away from the containing response the moment the pointer sits
/// over them, which made the hover-action row and the column's wheel gate
/// flicker and drop input the instant the mouse reached what it was trying
/// to click — `contains_pointer()` doesn't get defeated by same-layer
/// children (see the identical fix and rationale in `panel.rs`). Extracted
/// so the gate itself is unit-testable without a live egui frame.
fn gated_by_pointer(over: bool, pointer: Option<egui::Pos2>, sub_rect: egui::Rect) -> bool {
    over && pointer.is_some_and(|p| sub_rect.contains(p))
}

/// One user intent recorded during the draw; drained by the window manager
/// after `apply_acts` (content cannot mutate the manager mid-loop).
pub enum BoardAct {
    QuickAdd(String),
    Dispatch {
        id: String,
        agent: String,
    },
    Done(String),
    Release(String),
    Rm(String),
    JumpTo(String),
    /// Human-only forcing teardown of a card's worktree (spec:
    /// dispatch-worktrees §Teardown). The manager opens a confirm first.
    DiscardWorktree(String),
}

pub struct BoardView {
    store: std::rc::Rc<std::cell::RefCell<crate::kanban::CardStore>>,
    quick_add: String,
    picker: Option<String>,
    scroll: [f32; 4],
    collapsed: [bool; 4],
    selected: Option<String>,
    scale: f32,
    pub acts: Vec<BoardAct>,
    /// Test probe: true when the last `show_details` frame drew the Discard
    /// button (Done/Blocked/orphaned card with a worktree).
    #[cfg(test)]
    pub(crate) offered_discard: bool,
}

impl BoardView {
    pub fn new(store: std::rc::Rc<std::cell::RefCell<crate::kanban::CardStore>>) -> Self {
        Self {
            store,
            quick_add: String::new(),
            picker: None,
            scroll: [0.0; 4],
            collapsed: [false; 4],
            selected: None,
            scale: 1.0,
            acts: Vec::new(),
            #[cfg(test)]
            offered_discard: false,
        }
    }

    /// Test-only identity accessor: confirms a restored/opened view shares
    /// the project's own `CardStore` Rc rather than a fresh one.
    #[cfg(test)]
    pub(crate) fn store(&self) -> &std::rc::Rc<std::cell::RefCell<crate::kanban::CardStore>> {
        &self.store
    }

    /// Paint the board into `rect` (screen coordinates — same space `resp`
    /// senses). Expanded columns share available space, cards sorted by `created` (the
    /// store already keeps `cards()` in that order; filtering per column
    /// preserves it).
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        _active: bool,
        resp: &egui::Response,
        base: egui::Id,
    ) {
        // Arms Task 3's staleness poll only while the board is actually
        // rendered this frame (spec: nothing while hidden).
        self.store
            .borrow_mut()
            .mark_shown(std::time::Instant::now());

        let next_scale = crate::terminal::font_size(ui.ctx()) / crate::config::DEFAULT_FONT_SIZE;
        if next_scale != self.scale {
            for scroll in &mut self.scroll {
                *scroll *= next_scale / self.scale;
            }
            self.scale = next_scale;
        }
        let th = crate::theme::live(ui.ctx());
        let p = ui.painter_at(rect);
        // The board and its detail page share the terminal's base surface.
        p.rect_filled(rect, 0.0, th.bg);

        // Read seam: one borrow, into locals, dropped before any intent is
        // recorded below.
        let (cards, orphans) = {
            let store = self.store.borrow();
            (store.cards().to_vec(), store.orphans().clone())
        };

        if let Some(id) = self.selected.clone() {
            if let Some(card) = cards.iter().find(|c| c.id == id) {
                self.show_details(ui, rect, base, card, orphans.contains(&id), &th);
                return;
            }
            self.selected = None;
        }
        // Focus belongs to the window frame, not column dividers.
        let border_col = th.border;
        let widths =
            column_widths(rect.width() / self.scale, self.collapsed).map(|w| w * self.scale);
        let mut left = rect.min.x;
        let mut picker_click_consumed = false;

        // Raw pointer position (not `resp.hover_pos()`, which is gated on
        // `resp.hovered()` and so goes `None` the instant a nested widget
        // wins hover away from `resp` — see `gated_by_pointer`) plus whether
        // the pointer genuinely belongs to this window at all this frame.
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let over = resp.hovered() || resp.contains_pointer();

        for (i, &state) in COLUMNS.iter().enumerate() {
            let col_rect = egui::Rect::from_min_size(
                egui::pos2(left, rect.min.y),
                egui::vec2(widths[i], rect.height()),
            );
            left += widths[i];
            if i > 0 {
                p.line_segment(
                    [col_rect.min, egui::pos2(col_rect.min.x, col_rect.max.y)],
                    egui::Stroke::new(1.0, border_col),
                );
            }
            self.show_column(
                ui,
                &p,
                col_rect,
                i,
                state,
                &cards,
                &orphans,
                pointer,
                over,
                base,
                &th,
                &mut picker_click_consumed,
            );
        }

        // Picker dismiss: any click this frame that wasn't one of the
        // picker's own agent buttons closes it ("clicking elsewhere closes
        // it" — spec).
        if self.picker.is_some() && !picker_click_consumed && ui.input(|i| i.pointer.any_click()) {
            self.picker = None;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn show_column(
        &mut self,
        ui: &mut egui::Ui,
        p: &egui::Painter,
        col_rect: egui::Rect,
        col_idx: usize,
        state: crate::kanban::CardState,
        cards: &[crate::kanban::Card],
        orphans: &std::collections::HashSet<String>,
        pointer: Option<egui::Pos2>,
        over: bool,
        base: egui::Id,
        th: &crate::theme::Theme,
        picker_click_consumed: &mut bool,
    ) {
        let matching: Vec<&crate::kanban::Card> =
            cards.iter().filter(|c| c.state == state).collect();

        if self.collapsed[col_idx] {
            let rail = ui.interact(
                col_rect,
                base.with((col_idx, "expand")),
                egui::Sense::click(),
            );
            p.rect_filled(
                col_rect,
                0.0,
                if rail.hovered() {
                    th.sel_bg
                } else {
                    th.title_bg
                },
            );
            disclosure(
                &p.with_clip_rect(col_rect),
                col_rect.min + egui::vec2(10.0, HEADER_H / 2.0) * self.scale,
                self.scale,
                true,
                th.dim,
            );
            let text = format!(
                "{}\n{}",
                column_title(state).replace(' ', "\n"),
                matching.len()
            );
            let galley = p.layout(
                text,
                egui::FontId::proportional(11.5 * self.scale),
                th.dim,
                col_rect.width(),
            );
            p.galley(
                col_rect.min + egui::vec2(4.0 * self.scale, HEADER_H * self.scale),
                galley,
                th.dim,
            );
            if rail
                .on_hover_text(format!("Expand {}", column_title(state)))
                .clicked()
            {
                self.collapsed[col_idx] = false;
            }
            return;
        }

        let header_rect = egui::Rect::from_min_size(
            col_rect.min,
            egui::vec2(col_rect.width(), HEADER_H * self.scale),
        );
        disclosure(
            &p.with_clip_rect(header_rect),
            egui::pos2(
                header_rect.min.x + 10.0 * self.scale,
                header_rect.center().y,
            ),
            self.scale,
            false,
            th.dim,
        );
        p.with_clip_rect(header_rect).text(
            egui::pos2(
                header_rect.min.x + 20.0 * self.scale,
                header_rect.center().y,
            ),
            egui::Align2::LEFT_CENTER,
            format!("{}  ({})", column_title(state), matching.len()),
            egui::FontId::proportional(11.5 * self.scale),
            th.dim,
        );
        if ui
            .interact(
                header_rect,
                base.with((col_idx, "collapse")),
                egui::Sense::click(),
            )
            .on_hover_text("Collapse column")
            .clicked()
        {
            self.collapsed[col_idx] = true;
            self.picker = None;
        }

        let mut body_top = header_rect.max.y;
        if state == crate::kanban::CardState::Backlog {
            let qa_rect = egui::Rect::from_min_size(
                egui::pos2(col_rect.min.x + PAD * self.scale, body_top),
                egui::vec2(
                    (col_rect.width() - PAD * self.scale * 2.0).max(0.0),
                    QUICK_ADD_H * self.scale - 4.0 * self.scale,
                ),
            );
            ui.visuals_mut().selection.bg_fill = th.selection_text_bg;
            let te = ui.put(
                qa_rect,
                egui::TextEdit::singleline(&mut self.quick_add)
                    .id(base.with((col_idx, "quick-add")))
                    .font(egui::FontId::proportional(11.5 * self.scale))
                    .text_color(th.text)
                    .hint_text("+ new card…")
                    .vertical_align(egui::Align::Center)
                    .frame(egui::Frame::NONE)
                    .margin(egui::Margin::symmetric(4, 0))
                    .desired_width(qa_rect.width()),
            );
            if te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let title = std::mem::take(&mut self.quick_add);
                let title = title.trim().to_string();
                if !title.is_empty() {
                    self.acts.push(BoardAct::QuickAdd(title));
                }
                te.request_focus(); // keep typing; multi-add is the norm
            }
            body_top += QUICK_ADD_H * self.scale;
        }

        let body_rect =
            egui::Rect::from_min_max(egui::pos2(col_rect.min.x, body_top), col_rect.max);

        let content_h = matching.len() as f32 * (CARD_H * self.scale + CARD_GAP * self.scale);
        let max_scroll = (content_h - body_rect.height()).max(0.0);
        let wheel = if gated_by_pointer(over, pointer, body_rect) {
            ui.input(|i| i.smooth_scroll_delta.y)
        } else {
            0.0
        };
        self.scroll[col_idx] = (self.scroll[col_idx] - wheel).clamp(0.0, max_scroll);

        let cp = ui.painter_at(body_rect);
        let mut y = body_rect.min.y - self.scroll[col_idx];
        for card in matching {
            let card_rect = egui::Rect::from_min_size(
                egui::pos2(body_rect.min.x + PAD * self.scale, y),
                egui::vec2(
                    (body_rect.width() - PAD * self.scale * 2.0).max(0.0),
                    CARD_H * self.scale,
                ),
            );
            // Cull cards scrolled fully out of the visible body — keeps
            // interact() hit-regions from bleeding above/below the column.
            if card_rect.max.y >= body_rect.min.y && card_rect.min.y <= body_rect.max.y {
                self.show_card(
                    ui,
                    &cp,
                    card_rect,
                    body_rect,
                    card,
                    orphans.contains(&card.id),
                    pointer,
                    over,
                    base,
                    th,
                    picker_click_consumed,
                );
            }
            y += CARD_H * self.scale + CARD_GAP * self.scale;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn show_card(
        &mut self,
        ui: &mut egui::Ui,
        cp: &egui::Painter,
        card_rect: egui::Rect,
        body_rect: egui::Rect,
        card: &crate::kanban::Card,
        orphaned: bool,
        pointer: Option<egui::Pos2>,
        over: bool,
        base: egui::Id,
        th: &crate::theme::Theme,
        picker_click_consumed: &mut bool,
    ) {
        let hovered = gated_by_pointer(over, pointer, card_rect)
            && pointer.is_some_and(|pp| body_rect.contains(pp));
        let is_picked = self.picker.as_deref() == Some(card.id.as_str());

        cp.rect_filled(card_rect, 4.0, th.bg);
        if hovered {
            cp.rect_filled(card_rect, 4.0, th.sel_bg);
        }
        cp.rect_stroke(
            card_rect,
            4.0,
            egui::Stroke::new(1.0, th.border),
            egui::StrokeKind::Inside,
        );
        // Card content and footer have disjoint hit regions. Hover never changes text width.
        let text_w = (card_rect.width() - PAD * self.scale * 2.0).max(1.0);
        let main_rect = egui::Rect::from_min_max(
            card_rect.min,
            egui::pos2(
                card_rect.max.x,
                card_rect.max.y - BTN_H * self.scale - 8.0 * self.scale,
            ),
        );
        if ui
            .interact(
                main_rect.intersect(body_rect),
                base.with((card.id.as_str(), "details")),
                egui::Sense::click(),
            )
            .on_hover_text("View task details")
            .clicked()
        {
            self.selected = Some(card.id.clone());
            self.picker = None;
        }
        let mut job = egui::text::LayoutJob::simple(
            card.title.clone(),
            egui::FontId::proportional(12.0 * self.scale),
            th.text,
            text_w,
        );
        job.wrap.max_rows = 2;
        job.wrap.break_anywhere = false;
        let title = cp.layout_job(job);
        cp.galley(
            card_rect.min + egui::vec2(PAD * self.scale, PAD * self.scale),
            title,
            th.text,
        );
        let status = if orphaned {
            "Session ended — restart or return to backlog".to_owned()
        } else if let Some(reason) = &card.blocked_reason {
            reason.clone()
        } else if let Some(claim) = &card.claim {
            match &claim.agent {
                Some(agent) => format!("{agent} · {}", claim.terminal),
                None => claim.terminal.clone(),
            }
        } else if card.state == crate::kanban::CardState::Done {
            "Completed".to_owned()
        } else {
            "Ready to start".to_owned()
        };
        let status_color = if orphaned || card.blocked_reason.is_some() {
            th.danger
        } else {
            th.dim
        };
        // A worktree card keeps one row for the status and one for the
        // worktree line; the card's height never changes (spec: nothing on
        // cards without a worktree).
        let mut job = egui::text::LayoutJob::simple(
            status,
            egui::FontId::proportional(10.5 * self.scale),
            status_color,
            text_w,
        );
        job.wrap.max_rows = if card.worktree.is_some() { 1 } else { 2 };
        job.wrap.break_anywhere = false;
        cp.galley(
            card_rect.min + egui::vec2(PAD * self.scale, 40.0 * self.scale),
            cp.layout_job(job),
            status_color,
        );
        if let Some(wt) = &card.worktree {
            let st = self.store.borrow().worktree_status(&card.id);
            // Ahead of base is normal while the worker is still going; it
            // is attention once the card has left In Progress.
            let attention = st.is_some_and(|s| {
                s.dirty || (s.ahead > 0 && card.state != crate::kanban::CardState::InProgress)
            });
            let color = if st.is_some_and(|s| s.missing) {
                th.dim
            } else if attention {
                th.danger
            } else {
                th.text
            };
            let mut job = egui::text::LayoutJob::simple(
                crate::kanban::worktree_summary(wt, st.as_ref()),
                egui::FontId::monospace(10.0 * self.scale),
                color,
                text_w,
            );
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            cp.galley(
                card_rect.min + egui::vec2(PAD * self.scale, 54.0 * self.scale),
                cp.layout_job(job),
                color,
            );
        }

        if is_picked {
            if card_rect.width() < (PAD * 2.0 + BTN_W * 3.0 + BTN_GAP * 2.0) * self.scale {
                self.selected = Some(card.id.clone());
                self.picker = None;
                return;
            }
            self.show_picker(ui, cp, card_rect, card, base, th, picker_click_consumed);
            return;
        }
        let can_dispatch = matches!(
            card.state,
            crate::kanban::CardState::Backlog | crate::kanban::CardState::Blocked
        ) || orphaned;
        let label = if orphaned {
            "Restart…"
        } else if can_dispatch {
            "Start…"
        } else if card.claim.is_some() {
            "Open terminal"
        } else {
            "Details"
        };
        let footer_y = card_rect.max.y - BTN_H * self.scale - 4.0 * self.scale;
        let primary = egui::Rect::from_min_size(
            egui::pos2(card_rect.min.x + PAD * self.scale, footer_y),
            egui::vec2(
                (text_w - 32.0 * self.scale).clamp(1.0, 100.0 * self.scale),
                BTN_H * self.scale,
            ),
        );
        let more = egui::Rect::from_min_size(
            egui::pos2(
                card_rect.max.x - PAD * self.scale - 26.0 * self.scale,
                footer_y,
            ),
            egui::vec2(26.0 * self.scale, BTN_H * self.scale),
        );
        for (r, label, role) in [(primary, label, "primary"), (more, "…", "more")] {
            let r = r.intersect(body_rect);
            if !r.is_positive() {
                continue;
            }
            let response =
                ui.interact(r, base.with((card.id.as_str(), role)), egui::Sense::click());
            cp.rect_filled(r, 3.0, if response.hovered() { th.sel_bg } else { th.bg });
            cp.with_clip_rect(r).text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(10.5 * self.scale),
                th.text,
            );
            if response.clicked() {
                if role == "more" || (!can_dispatch && card.claim.is_none()) {
                    self.selected = Some(card.id.clone());
                } else if can_dispatch {
                    self.picker = Some(card.id.clone());
                    *picker_click_consumed = true;
                } else if let Some(claim) = &card.claim {
                    self.acts.push(BoardAct::JumpTo(claim.terminal.clone()));
                }
            }
        }
    }

    /// A pane-local detail page: no modal can steal another terminal's focus.
    /// Read the current snapshot every frame, so close-out and external removal stay live.
    fn show_details(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        base: egui::Id,
        card: &crate::kanban::Card,
        orphaned: bool,
        th: &crate::theme::Theme,
    ) {
        #[cfg(test)]
        {
            self.offered_discard = false;
        }
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(base.with("detail-page"))
                .max_rect(rect.shrink(12.0 * self.scale))
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        child.set_clip_rect(rect);
        for font in child.style_mut().text_styles.values_mut() {
            font.size *= self.scale;
        }
        child.spacing_mut().interact_size *= self.scale;
        child.spacing_mut().button_padding *= self.scale;
        child.spacing_mut().item_spacing *= self.scale;
        child.visuals_mut().override_text_color = Some(th.text);
        if child.button("‹ Back to board").clicked() {
            self.selected = None;
            self.picker = None;
        }
        child.separator();
        egui::ScrollArea::vertical()
            .id_salt(base.with((card.id.as_str(), "detail-scroll")))
            .show(&mut child, |ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(&card.title).size(18.0 * self.scale))
                        .wrap(),
                );
                ui.label(
                    egui::RichText::new(format!("{} · {}", card.id, column_title(card.state)))
                        .color(th.dim),
                );
                if orphaned {
                    ui.colored_label(th.danger, "Session ended");
                }
                if let Some(claim) = &card.claim {
                    ui.label(format!(
                        "Agent: {}",
                        claim.agent.as_deref().unwrap_or("Unspecified")
                    ));
                    ui.label(format!("Terminal: {}", claim.terminal));
                    if !orphaned && ui.button("Open terminal").clicked() {
                        self.acts.push(BoardAct::JumpTo(claim.terminal.clone()));
                    }
                }
                if let Some(wt) = &card.worktree {
                    ui.add_space(12.0 * self.scale);
                    ui.strong("Worktree");
                    ui.add(
                        egui::Label::new(format!("Path: {}", wt.path))
                            .wrap()
                            .selectable(true),
                    );
                    ui.label(format!("Branch: {}   base: {}", wt.branch, wt.base));
                    let st = self.store.borrow().worktree_status(&card.id);
                    let line = match st {
                        None => "Status: not polled yet".to_owned(),
                        Some(s) if s.missing => {
                            format!("Status: directory missing · +{} -{}", s.ahead, s.behind)
                        }
                        Some(s) => format!(
                            "Status: +{} ahead, -{} behind{}",
                            s.ahead,
                            s.behind,
                            if s.dirty { ", uncommitted changes" } else { "" }
                        ),
                    };
                    let attention = st.is_some_and(|s| s.dirty || s.ahead > 0);
                    ui.label(egui::RichText::new(line).color(if attention {
                        th.danger
                    } else {
                        th.dim
                    }));
                    // Discard is human-only and only where no live worker
                    // could be mid-write: Done, Blocked, or orphaned.
                    let discardable = orphaned
                        || matches!(
                            card.state,
                            crate::kanban::CardState::Done | crate::kanban::CardState::Blocked
                        );
                    if discardable {
                        #[cfg(test)]
                        {
                            self.offered_discard = true;
                        }
                        if ui
                            .button(egui::RichText::new("Discard worktree").color(th.danger))
                            .on_hover_text(
                                "Force-removes the tree and branch, including unmerged work",
                            )
                            .clicked()
                        {
                            self.acts.push(BoardAct::DiscardWorktree(card.id.clone()));
                        }
                    }
                }
                ui.add_space(12.0 * self.scale);
                ui.strong("Description");
                detail_text(
                    ui,
                    card.body.as_deref().unwrap_or("No description provided."),
                );
                if let Some(reason) = &card.blocked_reason {
                    ui.add_space(12.0 * self.scale);
                    ui.colored_label(th.danger, "Blocker");
                    detail_text(ui, reason);
                }
                ui.add_space(12.0 * self.scale);
                ui.label(
                    egui::RichText::new(format!(
                        "Created: {}\nUpdated: {}",
                        card.created, card.updated
                    ))
                    .color(th.dim),
                );
                ui.separator();
                if orphaned
                    || matches!(
                        card.state,
                        crate::kanban::CardState::Backlog | crate::kanban::CardState::Blocked
                    )
                {
                    ui.label(if orphaned {
                        "Restart with"
                    } else {
                        "Start with"
                    });
                    ui.horizontal_wrapped(|ui| {
                        for agent in AGENTS {
                            if ui.button(*agent).clicked() {
                                self.acts.push(BoardAct::Dispatch {
                                    id: card.id.clone(),
                                    agent: (*agent).to_owned(),
                                });
                            }
                        }
                    });
                }
                if orphaned || card.state == crate::kanban::CardState::Blocked {
                    if ui.button("Return to backlog").clicked() {
                        self.acts.push(BoardAct::Release(card.id.clone()));
                    }
                }
                if card.state == crate::kanban::CardState::InProgress && !orphaned {
                    if ui.button("Mark done").clicked() {
                        self.acts.push(BoardAct::Done(card.id.clone()));
                    }
                }
                ui.add_space(12.0 * self.scale);
                if ui
                    .button(egui::RichText::new("Delete card").color(th.danger))
                    .clicked()
                {
                    self.acts.push(BoardAct::Rm(card.id.clone()));
                }
            });
    }

    /// Inline three-button agent row shown on the picked card in place of the
    /// normal hover-action row. Picking an agent pushes `Dispatch` and closes
    /// the picker; the "clicking elsewhere closes it" half lives in `show`.
    #[allow(clippy::too_many_arguments)]
    fn show_picker(
        &mut self,
        ui: &mut egui::Ui,
        cp: &egui::Painter,
        card_rect: egui::Rect,
        card: &crate::kanban::Card,
        base: egui::Id,
        th: &crate::theme::Theme,
        picker_click_consumed: &mut bool,
    ) {
        let mut bx = card_rect.max.x - PAD * self.scale;
        for agent in AGENTS.iter().rev() {
            let btn_rect = egui::Rect::from_min_size(
                egui::pos2(
                    bx - BTN_W * self.scale,
                    card_rect.max.y - BTN_H * self.scale - 4.0 * self.scale,
                ),
                egui::vec2(BTN_W * self.scale, BTN_H * self.scale),
            );
            let id = base.with((card.id.as_str(), "pick", *agent));
            let btn_resp =
                ui.interact(btn_rect.intersect(cp.clip_rect()), id, egui::Sense::click());
            cp.rect_filled(
                btn_rect,
                2.0,
                if btn_resp.hovered() { th.sel_bg } else { th.bg },
            );
            cp.text(
                btn_rect.center(),
                egui::Align2::CENTER_CENTER,
                *agent,
                egui::FontId::proportional(9.0 * self.scale),
                th.text,
            );
            if btn_resp.clicked() {
                *picker_click_consumed = true;
                self.acts.push(BoardAct::Dispatch {
                    id: card.id.clone(),
                    agent: agent.to_string(),
                });
                self.picker = None;
            }
            bx -= BTN_W * self.scale + BTN_GAP * self.scale;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn live_font_changes_scale_card_action_hit_regions() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        let id = store.borrow_mut().add("Scaled task", None).unwrap();
        let mut board = BoardView::new(store);
        let ctx = egui::Context::default();
        let base = egui::Id::new("font-scaling");
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(2400.0, 1400.0));
        for font_size in [6.0, 13.0, 26.0, 40.0, 13.0] {
            crate::terminal::set_font_size(&ctx, font_size);
            board.picker = None;
            let scale = font_size / crate::config::DEFAULT_FONT_SIZE;
            let pos = egui::pos2(
                (PAD * 2.0 + 40.0) * scale,
                (HEADER_H + QUICK_ADD_H + CARD_H - BTN_H / 2.0 - 4.0) * scale,
            );
            run_frame(&ctx, &mut board, rect, base, vec![moved(pos)]);
            run_frame(&ctx, &mut board, rect, base, vec![moved(pos)]);
            run_frame(&ctx, &mut board, rect, base, vec![button(pos, true)]);
            run_frame(&ctx, &mut board, rect, base, vec![button(pos, false)]);
            assert_eq!(
                board.picker.as_deref(),
                Some(id.as_str()),
                "font size {font_size}"
            );
            assert!(
                board.selected.is_none(),
                "footer must not overlap the card details hit region"
            );
        }
    }

    #[test]
    fn collapsed_lanes_redistribute_space_and_stay_in_bounds() {
        assert_eq!(column_widths(800.0, [false; 4]), [200.0; 4]);
        assert_eq!(
            column_widths(800.0, [true, false, true, true]),
            [64.0, 608.0, 64.0, 64.0]
        );
        for mask in 0..16 {
            let collapsed = std::array::from_fn(|i| mask & (1 << i) != 0);
            for width in [0.0, 80.0, 800.0] {
                let widths = column_widths(width, collapsed);
                assert!(widths.iter().all(|w| w.is_finite() && *w >= 0.0));
                assert!(widths.iter().sum::<f32>() <= width + 0.01);
            }
        }
    }

    #[test]
    fn headers_toggle_independently_and_card_click_opens_current_details() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        let id = store
            .borrow_mut()
            .add("Full title", Some("Multiline\nagent brief".into()))
            .unwrap();
        let mut board = BoardView::new(Rc::clone(&store));
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 400.0));
        let ctx = egui::Context::default();
        let base = egui::Id::new("toggle-and-details");
        let click = |board: &mut BoardView, pos| {
            run_frame(&ctx, board, rect, base, vec![moved(pos)]);
            run_frame(&ctx, board, rect, base, vec![moved(pos)]);
            run_frame(&ctx, board, rect, base, vec![button(pos, true)]);
            run_frame(&ctx, board, rect, base, vec![button(pos, false)]);
        };
        click(&mut board, egui::pos2(210.0, 12.0));
        assert_eq!(board.collapsed, [false, true, false, false]);
        click(&mut board, egui::pos2(250.0, 12.0));
        assert_eq!(board.collapsed, [false; 4]);
        click(&mut board, card_body_pos(rect));
        assert_eq!(board.selected.as_deref(), Some(id.as_str()));
        assert!(
            board.acts.is_empty(),
            "reading a card must not dispatch or mutate it"
        );
        store.borrow_mut().rm(&id).unwrap();
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert!(
            board.selected.is_none(),
            "removed cards return to the board"
        );
    }

    fn store_at(dir: &std::path::Path) -> Rc<RefCell<crate::kanban::CardStore>> {
        let mut s = crate::kanban::CardStore::default();
        s.set_dir(Some(dir));
        Rc::new(RefCell::new(s))
    }

    #[test]
    fn detail_page_offers_discard_only_for_done_blocked_or_orphaned_worktree_cards() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        let id = store.borrow_mut().add("card", None).unwrap();
        let wt = crate::kanban::Worktree {
            path: "H:/repo/.foreman/worktrees/x".into(),
            branch: "card/x".into(),
            base: "main".into(),
        };
        store
            .borrow_mut()
            .claim_for_dispatch(
                &id,
                "t1",
                "claude",
                crate::kanban::run_nonce(),
                crate::kanban::TermState::Missing,
                Some(wt),
            )
            .unwrap();
        let mut board = BoardView::new(Rc::clone(&store));
        board.selected = Some(id.clone());
        let ctx = egui::Context::default();
        let base = egui::Id::new("discard");
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        // In Progress with a live claim: no Discard button rendered.
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert!(!board.offered_discard);
        store.borrow_mut().done(&id).unwrap();
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert!(board.offered_discard, "Done + worktree must offer Discard");
        // Back on the board, the card face renders the summary line (no
        // status polled yet) without recording any act.
        board.selected = None;
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert!(board.acts.is_empty());
    }

    #[test]
    fn gated_by_pointer_requires_over_window_and_containment() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        let inside = egui::pos2(50.0, 50.0);
        let outside = egui::pos2(500.0, 500.0);

        assert!(gated_by_pointer(true, Some(inside), rect));
        // `over` (hovered() || contains_pointer()) must be required even
        // when the raw pointer position is inside the rect — a same-layer
        // child widget can be topmost there without the pointer having left
        // the window at all, but if `over` is false the pointer belongs to
        // a DIFFERENT window entirely (occluded), and the gate must not fire.
        assert!(!gated_by_pointer(false, Some(inside), rect));
        assert!(!gated_by_pointer(true, Some(outside), rect));
        assert!(!gated_by_pointer(true, None, rect));
    }

    // Replicates `show_card`'s button-row layout for the sole card in an
    // empty Backlog column, so the click-survival test below can land a real
    // pointer click on the "Go" button without reaching into private layout
    // internals from outside a `show()` call. Kept in lock-step with
    // `show_card`'s button loop: Backlog + not-orphaned => `[Go, Del]`, Del
    // rightmost.
    fn go_button_center(rect: egui::Rect) -> egui::Pos2 {
        let col_w = rect.width() / COLUMNS.len() as f32; // Backlog is column 0
        let card_rect = egui::Rect::from_min_size(
            egui::pos2(rect.min.x + PAD, rect.min.y + HEADER_H + QUICK_ADD_H),
            egui::vec2(col_w - PAD * 2.0, CARD_H),
        );
        egui::pos2(
            card_rect.min.x + PAD + 40.0,
            card_rect.max.y - BTN_H / 2.0 - 4.0,
        )
    }

    // Anywhere on the same card's body, away from the button row — where the
    // pointer starts before it moves onto the Go button.
    fn card_body_pos(rect: egui::Rect) -> egui::Pos2 {
        egui::pos2(
            rect.min.x + PAD + 4.0,
            rect.min.y + HEADER_H + QUICK_ADD_H + 6.0,
        )
    }

    fn run_frame(
        ctx: &egui::Context,
        board: &mut BoardView,
        rect: egui::Rect,
        base: egui::Id,
        events: Vec<egui::Event>,
    ) {
        let mut input = egui::RawInput::default();
        input.events = events;
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                // Sense::click(), matching the real `content_rect` response a
                // project-hosted `Content::Board` is actually painted with in
                // wm.rs (project content senses clicks only, not drags).
                let resp =
                    ui.interact(rect, egui::Id::new("test-board-resp"), egui::Sense::click());
                board.show(ui, rect, true, &resp, base);
            });
        });
    }

    fn moved(pos: egui::Pos2) -> egui::Event {
        egui::Event::PointerMoved(pos)
    }

    fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        }
    }

    /// Drives a real click through egui frame-by-frame — pointer settles on
    /// the card (registering the hover action row for the first time), then
    /// moves onto the Go button and clicks it. Pins both Criticals from the
    /// review: (1) the picker must survive the SAME frame it opened on, not
    /// get wiped by the end-of-frame "clicking elsewhere" dismiss check that
    /// also sees this frame's click; (2) the Go button must still be
    /// clickable at all once the pointer sits exactly over it — a bare
    /// `resp.hovered()` gate would already have gone false by then (the
    /// button, registered last frame at that spot, wins hover away from the
    /// containing response), so the hover row would never repaint there and
    /// the click would land on nothing.
    #[test]
    fn dispatch_picker_survives_the_frame_it_opened_on() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        let id = store.borrow_mut().add("card", None).unwrap();
        let mut board = BoardView::new(Rc::clone(&store));

        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 400.0));
        let base = egui::Id::new("test-board");
        let ctx = egui::Context::default();
        let card_pos = card_body_pos(rect);
        let go_pos = go_button_center(rect);

        // Frame 0: warm-up. egui's hover/contains_pointer hit-testing for a
        // widget is resolved against the PRIOR frame's finalized paint order
        // (there is none yet on a brand-new Context), so every response
        // reads `hovered() == false`, `contains_pointer() == false` on the
        // very first frame no matter where the pointer is. One throwaway
        // frame establishes that order for frame 1 onward.
        run_frame(&ctx, &mut board, rect, base, vec![moved(card_pos)]);

        // Frame 1: pointer settles on the card body — this is what makes the
        // hover action row (including Go) exist at its fixed spot at all.
        run_frame(&ctx, &mut board, rect, base, vec![moved(card_pos)]);

        // Frame 2: pointer moves onto the Go button and presses down.
        run_frame(
            &ctx,
            &mut board,
            rect,
            base,
            vec![moved(go_pos), button(go_pos, true)],
        );

        // Frame 3: release over the same spot — "the open click's frame":
        // `any_click()` is true here, and this is exactly the frame the
        // dismiss-check bug fired the picker closed in.
        run_frame(
            &ctx,
            &mut board,
            rect,
            base,
            vec![moved(go_pos), button(go_pos, false)],
        );
        assert_eq!(
            board.picker.as_deref(),
            Some(id.as_str()),
            "the Go click must open the picker and survive its own frame's dismiss check"
        );

        // Frame 4: no new click — the picker must still be showing.
        run_frame(&ctx, &mut board, rect, base, vec![moved(go_pos)]);
        assert_eq!(
            board.picker.as_deref(),
            Some(id.as_str()),
            "the picker must survive into the next frame too"
        );
    }
}
