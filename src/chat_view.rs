//! Chat viewer paint: crew board, message log, and human input strip.
//!
//! Pure room/model logic stays in [`crate::chat`] (std-only). This module is
//! the egui adapter for [`crate::chat::ChatView`].

use crate::chat::{self, ChatBlock, ChatView};
use crate::theme::*;
use eframe::egui;

const CHAT_BOARD_W: f32 = 160.0;
const CHAT_BOARD_MIN_W: f32 = 480.0; // window narrower than this hides the board

fn chat_color(id: &str) -> egui::Color32 {
    if id == "you" {
        return CHAT_COLORS[0];
    }
    let n: u64 = id.trim_start_matches('t').parse().unwrap_or(0);
    CHAT_COLORS[(n as usize) % CHAT_COLORS.len()]
}

impl ChatView {
    /// Paint the chat viewer into `rect`.
    ///
    /// **Preconditions**
    /// - `resp` must be a `Sense::click_and_drag` (or equivalent) `Response`
    ///   sensing the **same screen rect** as `rect`. Crew-row hit tests use
    ///   `resp.hover_pos()` against rows laid out in `rect` space; a mismatch
    ///   silently kills clicks. Wheel handling assumes the host has not already
    ///   consumed scroll for that rect.
    /// - `id` is the stable egui Id for the message `TextEdit` (Content builds
    ///   it from the window base; this module never sees `WinId`).
    ///
    /// **Deferred outcomes** (host drains after the draw loop, after
    /// `apply_acts` — that ordering is load-bearing so a crew click focuses the
    /// member terminal, not the chat viewer):
    /// - [`ChatView::click`] — crew row Member id
    /// - [`ChatView::pending_post`] — submitted human line
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        active: bool,
        resp: &egui::Response,
        id: egui::Id,
    ) {
        // Reserve the input strip up front and shrink the working rect so the
        // board/log lay out above it. The painter keeps the FULL rect (it must
        // draw the strip chrome too).
        const INPUT_H: f32 = 32.0;
        let input_rect = egui::Rect::from_min_max(
            egui::pos2(rect.min.x, rect.max.y - INPUT_H),
            rect.max,
        );
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 0.0, WIN_BG);
        let rect = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, input_rect.min.y));
        let pad = 8.0;
        let meta_font = egui::FontId::proportional(11.0);
        let body_font = egui::FontId::proportional(12.5);
        let compact = rect.width() < CHAT_BOARD_MIN_W;

        // ---- crew board (comfortable widths only) ----
        let mut log_left = rect.min.x;
        if !compact {
            let board = egui::Rect::from_min_max(
                rect.min,
                egui::pos2(rect.min.x + CHAT_BOARD_W, rect.max.y),
            );
            log_left = board.max.x;
            p.line_segment(
                [
                    egui::pos2(board.max.x, rect.min.y),
                    egui::pos2(board.max.x, rect.max.y),
                ],
                egui::Stroke::new(1.0, BORDER),
            );
            p.text(
                egui::pos2(board.min.x + pad, board.min.y + pad),
                egui::Align2::LEFT_TOP,
                "CREW · BY LAST HEARD",
                egui::FontId::proportional(9.5),
                DIM,
            );
            let now = std::time::SystemTime::now();
            let row_h = 20.0;
            let mut y = board.min.y + pad + 16.0;
            // Pull crew rows from the room; the borrow is dropped before the
            // paint loop (it borrows nothing else from the room).
            let crew = self.room.borrow().crew(std::time::Instant::now());
            for r in &crew {
                let row = egui::Rect::from_min_size(
                    egui::pos2(board.min.x + 4.0, y),
                    egui::vec2(board.width() - 8.0, row_h),
                );
                let hovered = resp.hovered() && resp.hover_pos().is_some_and(|p| row.contains(p));
                if hovered {
                    p.rect_filled(row, 3.0, TITLE_BG);
                }
                if hovered && resp.clicked() {
                    self.click = Some(r.id.clone());
                }
                let dot = if r.exited { BORDER } else { CHAT_LIVE };
                p.circle_filled(egui::pos2(row.min.x + 7.0, row.center().y), 3.0, dot);
                let name_col = if r.exited { DIM } else { chat_color(&r.id) };
                // The pane identity has name == id ("you") — a bare label beats
                // the silly-looking "you · you".
                let label = if r.name == r.id {
                    r.name.clone()
                } else {
                    format!("{} · {}", r.name, r.id)
                };
                let (age, stale) = if r.exited {
                    ("exited".to_string(), false)
                } else {
                    match r.last.and_then(|t| now.duration_since(t).ok()) {
                        Some(d) => chat::age_label(d),
                        None => ("—".to_string(), false),
                    }
                };
                // Age paints first so the label can truncate into the space
                // that's left — an unconstrained p.text label runs straight
                // under the age column on long tab titles.
                let age_rect = p.text(
                    egui::pos2(row.max.x - 4.0, row.center().y),
                    egui::Align2::RIGHT_CENTER,
                    age,
                    egui::FontId::proportional(10.5),
                    if stale { CHAT_STALE } else { DIM },
                );
                let label_x = row.min.x + 16.0;
                let mut job = egui::text::LayoutJob::simple_singleline(
                    label,
                    egui::FontId::proportional(11.5),
                    name_col,
                );
                job.wrap =
                    egui::text::TextWrapping::truncate_at_width((age_rect.min.x - 6.0 - label_x).max(0.0));
                let g = p.layout_job(job);
                p.galley(
                    egui::pos2(label_x, row.center().y - g.size().y * 0.5),
                    g,
                    name_col,
                );
                y += row_h;
                if y + row_h > board.max.y {
                    break; // board overflow: clip; the log is the priority
                }
            }
        }

        // ---- log: layout pass (galleys + heights), then paint ----
        let log_rect = egui::Rect::from_min_max(
            egui::pos2(log_left + pad, rect.min.y + pad),
            egui::pos2(rect.max.x - pad, rect.max.y - pad),
        );
        let wrap = (log_rect.width() - 10.0).max(40.0);
        // Borrow stays scoped — never held across recursion into other
        // windows' show() (post paths borrow_mut the room).
        let blocks = self.room.borrow().blocks(self.last_seen, compact);
        enum Painted {
            Galley(
                std::sync::Arc<egui::Galley>,
                egui::Color32,
                f32,  /*indent*/
                bool, /*edge*/
            ),
            Centered(std::sync::Arc<egui::Galley>),
            MetaPair(std::sync::Arc<egui::Galley>, std::sync::Arc<egui::Galley>),
            Rule(Option<std::sync::Arc<egui::Galley>>),
            Gap(f32),
        }
        let mut items: Vec<Painted> = Vec::new();
        let mut total = 0.0f32;
        for b in &blocks {
            match b {
                ChatBlock::Sys(s) => {
                    let g = p.layout(s.clone(), meta_font.clone(), DIM, wrap);
                    total += g.size().y + 6.0;
                    items.push(Painted::Centered(g));
                    items.push(Painted::Gap(6.0));
                }
                ChatBlock::Divider => {
                    let g = p.layout(
                        "NEW".into(),
                        egui::FontId::proportional(9.0),
                        CHAT_STALE,
                        wrap,
                    );
                    total += 14.0;
                    items.push(Painted::Rule(Some(g)));
                }
                ChatBlock::Header { name, id, meta } => {
                    let gn = p.layout_no_wrap(
                        name.clone(),
                        egui::FontId::proportional(12.0),
                        chat_color(id),
                    );
                    let gm = p.layout_no_wrap(meta.clone(), meta_font.clone(), DIM);
                    total += gn.size().y + 2.0 + 4.0; // header + breathing room above
                    items.push(Painted::Gap(4.0));
                    items.push(Painted::MetaPair(gn, gm));
                }
                ChatBlock::Text { text, to } => {
                    // Mention chips: lay the body out as a LayoutJob so
                    // @tokens get their own colored sections inline.
                    let mut job = egui::text::LayoutJob::default();
                    job.wrap.max_width = wrap;
                    for (i, word) in text.split(' ').enumerate() {
                        let lead = if i == 0 { "" } else { " " };
                        let (col, bg) = if word.starts_with('@') && word.len() > 1 {
                            (CHAT_COLORS[0], CHAT_MENTION_BG)
                        } else {
                            (TEXT, egui::Color32::TRANSPARENT)
                        };
                        job.append(
                            &format!("{lead}{word}"),
                            0.0,
                            egui::text::TextFormat {
                                font_id: body_font.clone(),
                                color: col,
                                background: bg,
                                ..Default::default()
                            },
                        );
                    }
                    let g = p.layout_job(job);
                    total += g.size().y + 2.0;
                    items.push(Painted::Galley(
                        g,
                        TEXT,
                        if to.is_empty() { 0.0 } else { 10.0 },
                        !to.is_empty(),
                    ));
                    items.push(Painted::Gap(2.0));
                }
            }
        }

        // Scroll: stick-to-bottom by default; the math lives in
        // `chat::scroll_step` (pure + tested). Only read the wheel when the
        // log is hovered.
        let wheel_dy = if resp.hovered() {
            ui.input(|i| i.smooth_scroll_delta.y)
        } else {
            0.0
        };
        let (scroll, stick, offset) =
            chat::scroll_step(total, log_rect.height(), wheel_dy, self.scroll, self.stick);
        self.scroll = scroll;
        self.stick = stick;
        let mut y = log_rect.min.y - offset;
        for it in items {
            match it {
                Painted::Gap(h) => y += h,
                Painted::Centered(g) => {
                    let h = g.size().y;
                    let x = log_rect.center().x - g.size().x / 2.0;
                    p.galley(egui::pos2(x, y), g, DIM);
                    y += h;
                }
                Painted::Rule(label) => {
                    let mid = y + 7.0;
                    // The rule is intentionally dim (mockup: 1px #45402f); the
                    // amber NEW label is the affordance.
                    p.line_segment(
                        [
                            egui::pos2(log_rect.min.x, mid),
                            egui::pos2(log_rect.max.x, mid),
                        ],
                        egui::Stroke::new(1.0, CHAT_MENTION_BG),
                    );
                    if let Some(g) = label {
                        let w = g.size().x;
                        let lx = log_rect.center().x - w / 2.0;
                        p.rect_filled(
                            egui::Rect::from_min_size(
                                egui::pos2(lx - 4.0, y),
                                egui::vec2(w + 8.0, 14.0),
                            ),
                            0.0,
                            WIN_BG,
                        );
                        p.galley(egui::pos2(lx, y + 1.0), g, CHAT_STALE);
                    }
                    y += 14.0;
                }
                Painted::MetaPair(gn, gm) => {
                    let h = gn.size().y;
                    let nw = gn.size().x;
                    p.galley(egui::pos2(log_rect.min.x, y), gn, TEXT);
                    p.galley(egui::pos2(log_rect.min.x + nw + 6.0, y + 1.5), gm, DIM);
                    y += h + 2.0;
                }
                Painted::Galley(g, col, indent, edge) => {
                    let h = g.size().y;
                    if edge {
                        p.line_segment(
                            [
                                egui::pos2(log_rect.min.x + 2.0, y),
                                egui::pos2(log_rect.min.x + 2.0, y + h),
                            ],
                            egui::Stroke::new(2.0, CHAT_EDGE),
                        );
                    }
                    p.galley(egui::pos2(log_rect.min.x + indent, y), g, col);
                    y += h;
                }
            }
        }
        self.on_frame(active);

        // ---- input strip: the human posts from here ----
        // Repaint the strip ground first: the painter's clip spans the full
        // window, so a partially-scrolled log line can bleed under the strip.
        p.rect_filled(input_rect, 0.0, WIN_BG);
        p.line_segment(
            [
                input_rect.min,
                egui::pos2(input_rect.max.x, input_rect.min.y),
            ],
            egui::Stroke::new(1.0, BORDER),
        );
        let te_rect = input_rect.shrink2(egui::vec2(8.0, 5.0));
        p.rect_filled(te_rect, egui::CornerRadius::same(3), DESK_BG);
        p.rect_stroke(
            te_rect,
            egui::CornerRadius::same(3),
            egui::Stroke::new(1.0, BORDER),
            egui::StrokeKind::Inside,
        );
        ui.visuals_mut().selection.bg_fill = SELECTION_TEXT_BG;
        let te = ui.put(
            te_rect,
            egui::TextEdit::singleline(&mut self.input)
                .id(id)
                .font(egui::FontId::proportional(12.5))
                .text_color(TEXT)
                .hint_text("Message…")
                .vertical_align(egui::Align::Center)
                .frame(egui::Frame::NONE)
                .margin(egui::Margin::symmetric(6, 0))
                .desired_width(te_rect.width()),
        );
        if te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            self.pending_post = Some(std::mem::take(&mut self.input));
            te.request_focus(); // keep typing; multi-post sessions are the norm
                                 // Escape defocuses the field at frame start (egui Focus::begin_pass),
                                 // so detect it as lost_focus + Escape — has_focus() is already false.
        } else if te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.input.clear();
        }
    }
}
