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

        // Raw pointer position (not `resp.hover_pos()`, which is gated on
        // `resp.hovered()` and so goes `None` the instant a nested widget
        // wins hover away from `resp` — see `gated_by_pointer`) plus whether
        // the pointer genuinely belongs to this window at all this frame.
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let over = resp.hovered() || resp.contains_pointer();

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
                    over,
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
        over: bool,
        base: egui::Id,
        th: &crate::theme::Theme,
        picker_click_consumed: &mut bool,
    ) {
        let hovered = gated_by_pointer(over, pointer, card_rect)
            && pointer.is_some_and(|pp| body_rect.contains(pp));
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
                    None => {
                        // Opening the picker is itself a click this frame:
                        // `show`'s end-of-frame dismiss check also sees
                        // `any_click() == true` this same frame and would
                        // otherwise immediately clear what we just set —
                        // consume it here exactly like an agent pick does.
                        self.picker = Some(card.id.clone());
                        *picker_click_consumed = true;
                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn store_at(dir: &std::path::Path) -> Rc<RefCell<crate::kanban::CardStore>> {
        let mut s = crate::kanban::CardStore::default();
        s.set_dir(Some(dir));
        Rc::new(RefCell::new(s))
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
        let del_min_x = card_rect.max.x - PAD - BTN_W;
        let go_max_x = del_min_x - BTN_GAP;
        let go_min_x = go_max_x - BTN_W;
        let btn_y = card_rect.max.y - BTN_H - 4.0;
        egui::pos2((go_min_x + go_max_x) / 2.0, btn_y + BTN_H / 2.0)
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
