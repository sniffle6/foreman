//! The per-project kanban board window (`Content::Board`).
//!
//! Read seam: the shared [`crate::kanban::CardStore`] snapshot, borrowed once
//! per frame into locals here (`Card::clone` is cheap; the store's own
//! reload/orphan-derivation cadence is Task 3's `kanban_tick`, not this
//! view's job). Write seam: every user action is recorded as a [`BoardAct`]
//! onto `acts` and drained by the window manager after `apply_acts` — content
//! can never mutate the manager mid-draw. This mirrors the chat viewer's
//! `click`/`pending_post` fields and the task-manager panel's act/drain
//! pattern (`docs/task-manager-panel.md`); the panel's row-paint idiom
//! (truncate-with-hover-tooltip, reserve space for hover buttons) is reused
//! for the card rows below.

use eframe::egui;

/// Agents foreman already detects for tab icons and skill installs. No
/// persisted default (spec) — the dispatch picker always starts blank.
pub const AGENTS: &[&str] = &["claude", "codex", "grok"];

const CARD_H: f32 = 60.0;
const CARD_GAP: f32 = 4.0;
const HEADER_H: f32 = 22.0;
const QUICK_ADD_H: f32 = 24.0;
const PAD: f32 = 6.0;
const BTN_W: f32 = 32.0;
const BTN_H: f32 = 16.0;
const BTN_GAP: f32 = 4.0;

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

/// One user intent recorded during the draw; drained by the window manager
/// after `apply_acts` (content cannot mutate the manager mid-loop).
pub enum BoardAct {
    QuickAdd(String),
    Dispatch { id: String, agent: String },
    Done(String),
    Release(String),
    Rm(String),
    JumpTo(String),
}

pub struct BoardView {
    store: std::rc::Rc<std::cell::RefCell<crate::kanban::CardStore>>,
    quick_add: String,
    picker: Option<String>,
    scroll: [f32; 4],
    pub acts: Vec<BoardAct>,
}

impl BoardView {
    pub fn new(store: std::rc::Rc<std::cell::RefCell<crate::kanban::CardStore>>) -> Self {
        Self {
            store,
            quick_add: String::new(),
            picker: None,
            scroll: [0.0; 4],
            acts: Vec::new(),
        }
    }

    /// Test-only identity accessor: confirms a restored/opened view shares
    /// the project's own `CardStore` Rc rather than a fresh one.
    #[cfg(test)]
    pub(crate) fn store(&self) -> &std::rc::Rc<std::cell::RefCell<crate::kanban::CardStore>> {
        &self.store
    }

    /// Paint the board into `rect` (screen coordinates — same space `resp`
    /// senses). Four equal-width columns, cards sorted by `created` (the
    /// store already keeps `cards()` in that order; filtering per column
    /// preserves it).
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        active: bool,
        resp: &egui::Response,
        base: egui::Id,
    ) {
        // Arms Task 3's staleness poll only while the board is actually
        // rendered this frame (spec: nothing while hidden).
        self.store
            .borrow_mut()
            .mark_shown(std::time::Instant::now());

        let th = crate::theme::live(ui.ctx());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 0.0, th.win_bg);

        // Read seam: one borrow, into locals, dropped before any intent is
        // recorded below.
        let (cards, orphans) = {
            let store = self.store.borrow();
            (store.cards().to_vec(), store.orphans().clone())
        };

        let border_col = if active { th.border_focus } else { th.border };
        let col_w = rect.width() / COLUMNS.len() as f32;
        let mut picker_click_consumed = false;

        for (i, &state) in COLUMNS.iter().enumerate() {
            let col_rect = egui::Rect::from_min_size(
                egui::pos2(rect.min.x + col_w * i as f32, rect.min.y),
                egui::vec2(col_w, rect.height()),
            );
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
                resp,
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
        resp: &egui::Response,
        base: egui::Id,
        th: &crate::theme::Theme,
        picker_click_consumed: &mut bool,
    ) {
        let matching: Vec<&crate::kanban::Card> =
            cards.iter().filter(|c| c.state == state).collect();

        let header_rect =
            egui::Rect::from_min_size(col_rect.min, egui::vec2(col_rect.width(), HEADER_H));
        p.text(
            egui::pos2(header_rect.min.x + PAD, header_rect.center().y),
            egui::Align2::LEFT_CENTER,
            format!("{}  ({})", column_title(state), matching.len()),
            egui::FontId::proportional(11.5),
            th.dim,
        );

        let mut body_top = header_rect.max.y;
        if state == crate::kanban::CardState::Backlog {
            let qa_rect = egui::Rect::from_min_size(
                egui::pos2(col_rect.min.x + PAD, body_top),
                egui::vec2((col_rect.width() - PAD * 2.0).max(0.0), QUICK_ADD_H - 4.0),
            );
            ui.visuals_mut().selection.bg_fill = th.selection_text_bg;
            let te = ui.put(
                qa_rect,
                egui::TextEdit::singleline(&mut self.quick_add)
                    .id(base.with((col_idx, "quick-add")))
                    .font(egui::FontId::proportional(11.5))
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
            body_top += QUICK_ADD_H;
        }

        let body_rect =
            egui::Rect::from_min_max(egui::pos2(col_rect.min.x, body_top), col_rect.max);

        let content_h = matching.len() as f32 * (CARD_H + CARD_GAP);
        let max_scroll = (content_h - body_rect.height()).max(0.0);
        let pointer = resp.hover_pos();
        let hovering_body = pointer.is_some_and(|pp| body_rect.contains(pp));
        let wheel = if resp.hovered() && hovering_body {
            ui.input(|i| i.smooth_scroll_delta.y)
        } else {
            0.0
        };
        self.scroll[col_idx] = (self.scroll[col_idx] - wheel).clamp(0.0, max_scroll);

        let cp = ui.painter_at(body_rect);
        let mut y = body_rect.min.y - self.scroll[col_idx];
        for card in matching {
            let card_rect = egui::Rect::from_min_size(
                egui::pos2(body_rect.min.x + PAD, y),
                egui::vec2((body_rect.width() - PAD * 2.0).max(0.0), CARD_H),
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
                    resp,
                    base,
                    th,
                    picker_click_consumed,
                );
            }
            y += CARD_H + CARD_GAP;
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
        resp: &egui::Response,
        base: egui::Id,
        th: &crate::theme::Theme,
        picker_click_consumed: &mut bool,
    ) {
        let hovered = resp.hovered()
            && pointer.is_some_and(|pp| card_rect.contains(pp) && body_rect.contains(pp));
        let is_picked = self.picker.as_deref() == Some(card.id.as_str());

        if hovered || is_picked {
            cp.rect_filled(card_rect, 3.0, th.title_bg);
        }

        // Reserve space for the hover action row on the right (the panel's
        // `reserve` idiom — text truncates around it) whenever any action
        // row could show, whether or not this exact card is hovered right
        // now — keeps the title width stable as the mouse moves card to card.
        let reserve = if hovered || is_picked {
            BTN_W * 3.0 + BTN_GAP * 2.0 + 4.0
        } else {
            6.0
        };
        let text_w = (card_rect.width() - PAD * 2.0 - reserve).max(20.0);

        let title_pos = egui::pos2(card_rect.min.x + PAD, card_rect.min.y + 4.0);
        let mut job = egui::text::LayoutJob::simple_singleline(
            card.title.clone(),
            egui::FontId::proportional(12.0),
            th.text,
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(text_w);
        let galley = cp.layout_job(job);
        let elided = galley.elided;
        cp.galley(title_pos, galley, th.text);
        if elided {
            ui.interact(
                card_rect,
                base.with((card.id.as_str(), "title-hover")),
                egui::Sense::hover(),
            )
            .on_hover_text(card.title.clone());
        }

        cp.text(
            egui::pos2(card_rect.min.x + PAD, card_rect.min.y + 20.0),
            egui::Align2::LEFT_TOP,
            card.id.as_str(),
            egui::FontId::proportional(9.5),
            th.dim,
        );

        // Status line: orphaned marker beats a stale claim display; claimed
        // is clickable (-> JumpTo); blocked shows the reason. Backlog/Done
        // cards have neither a claim nor a reason, so the line stays blank.
        let status_pos = egui::pos2(card_rect.min.x + PAD, card_rect.min.y + 34.0);
        if orphaned {
            cp.text(
                status_pos,
                egui::Align2::LEFT_TOP,
                "ORPHANED",
                egui::FontId::proportional(10.5),
                th.danger,
            );
        } else if let Some(claim) = &card.claim {
            let label = match &claim.agent {
                Some(agent) => format!("{} · {agent}", claim.terminal),
                None => claim.terminal.clone(),
            };
            let status_rect = egui::Rect::from_min_size(status_pos, egui::vec2(text_w, 14.0));
            let jump_resp = ui.interact(
                status_rect,
                base.with((card.id.as_str(), "jump")),
                egui::Sense::click(),
            );
            let col = if jump_resp.hovered() { th.text } else { th.dim };
            cp.text(
                status_pos,
                egui::Align2::LEFT_TOP,
                label,
                egui::FontId::proportional(10.5),
                col,
            );
            if jump_resp.clicked() {
                self.acts.push(BoardAct::JumpTo(claim.terminal.clone()));
            }
        } else if let Some(reason) = &card.blocked_reason {
            cp.text(
                status_pos,
                egui::Align2::LEFT_TOP,
                reason.as_str(),
                egui::FontId::proportional(10.5),
                th.dim,
            );
        }

        if !(hovered || is_picked) {
            return;
        }
        if is_picked {
            self.show_picker(ui, cp, card_rect, card, base, th, picker_click_consumed);
            return;
        }

        // Hover action row, right-aligned. Per spec: orphaned cards get
        // re-dispatch (opens the picker) + release; Backlog/Blocked get
        // dispatch (picker); live InProgress gets Done; every card gets
        // delete. No block button (v1 has no card editor for a reason).
        let mut buttons: Vec<(&'static str, Option<BoardAct>)> = Vec::new();
        let can_dispatch = matches!(
            card.state,
            crate::kanban::CardState::Backlog | crate::kanban::CardState::Blocked
        ) || orphaned;
        if can_dispatch {
            buttons.push(("Go", None)); // opens the picker; no direct act
        }
        if orphaned {
            buttons.push(("Undo", Some(BoardAct::Release(card.id.clone()))));
        }
        if card.state == crate::kanban::CardState::InProgress && !orphaned {
            buttons.push(("Done", Some(BoardAct::Done(card.id.clone()))));
        }
        buttons.push(("Del", Some(BoardAct::Rm(card.id.clone()))));

        let mut bx = card_rect.max.x - PAD;
        for (label, act) in buttons.into_iter().rev() {
            let btn_rect = egui::Rect::from_min_size(
                egui::pos2(bx - BTN_W, card_rect.max.y - BTN_H - 4.0),
                egui::vec2(BTN_W, BTN_H),
            );
            let id = base.with((card.id.as_str(), "btn", label));
            let btn_resp = ui.interact(btn_rect, id, egui::Sense::click());
            let col = if btn_resp.hovered() { th.text } else { th.dim };
            cp.rect_filled(
                btn_rect,
                2.0,
                if btn_resp.hovered() { th.sel_bg } else { th.bg },
            );
            cp.text(
                btn_rect.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(10.0),
                col,
            );
            if btn_resp.clicked() {
                match act {
                    Some(a) => self.acts.push(a),
                    None => self.picker = Some(card.id.clone()),
                }
            }
            bx -= BTN_W + BTN_GAP;
        }
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
        let mut bx = card_rect.max.x - PAD;
        for agent in AGENTS.iter().rev() {
            let btn_rect = egui::Rect::from_min_size(
                egui::pos2(bx - BTN_W, card_rect.max.y - BTN_H - 4.0),
                egui::vec2(BTN_W, BTN_H),
            );
            let id = base.with((card.id.as_str(), "pick", *agent));
            let btn_resp = ui.interact(btn_rect, id, egui::Sense::click());
            cp.rect_filled(
                btn_rect,
                2.0,
                if btn_resp.hovered() { th.sel_bg } else { th.bg },
            );
            cp.text(
                btn_rect.center(),
                egui::Align2::CENTER_CENTER,
                *agent,
                egui::FontId::proportional(9.0),
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
            bx -= BTN_W + BTN_GAP;
        }
    }
}
