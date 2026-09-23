//! The per-project Plan window (`Content::Plan`).
//!
//! Read seam: the shared [`crate::kanban::CardStore`], borrowed once per
//! frame and folded into plans by [`crate::kanban::plans`] — no whole-`Card`
//! clones, so a project with no plans pays one scan and nothing else. Write
//! seam: every click is recorded as a [`PlanAct`] onto `acts` and drained by
//! the window manager after `apply_acts`, so content can never mutate the
//! manager mid-draw. Same shape as the board viewer
//! (`docs/kanban-board.md`) and the task-manager panel.
//!
//! Read-only by design (spec: plan-view): ordering is authored with
//! `foreman kanban edit --plan/--wave` and dispatch stays on the board.
//! There is no Start button and no scheduler behind this window.

use eframe::egui;

const PLAN_H: f32 = 26.0;
const WAVE_H: f32 = 22.0;
const ROW_H: f32 = 20.0;
const PAD: f32 = 8.0;
/// Narrow windows drop the per-card state word first; the title is what a
/// reader scans, so it keeps the space.
const STATE_MIN_W: f32 = 200.0;
/// Right-hand slot the state word and the wave/plan summaries live in.
const STATE_W: f32 = 84.0;

/// One user intent recorded during the draw; drained by the window manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanAct {
    /// Show this card's detail page on the project's board window.
    OpenCard(String),
}

/// Per-window view state. Nothing here is persisted — the window restores
/// scrolled to the top with the current wave of each plan open, the same
/// rule the board follows for its collapsed columns.
pub struct PlanView {
    store: std::rc::Rc<std::cell::RefCell<crate::kanban::CardStore>>,
    /// Waves whose default open/closed state the viewer flipped by hand,
    /// keyed by (folded plan name, wave number). See [`Self::expanded`].
    toggled: std::collections::HashSet<(String, u32)>,
    scroll: f32,
    scale: f32,
    pub acts: Vec<PlanAct>,
    /// Test probe: one string per row the last frame drew, top to bottom.
    #[cfg(test)]
    pub(crate) drawn_rows: Vec<String>,
    /// Test probe: `(card id, row rect)` for each card row drawn, so a test
    /// clicks where the view actually put it.
    #[cfg(test)]
    pub(crate) card_rects: Vec<(String, egui::Rect)>,
    /// Test probe: `((folded plan, wave), header rect)` per wave header.
    #[cfg(test)]
    pub(crate) wave_rects: Vec<((String, u32), egui::Rect)>,
}

impl PlanView {
    pub fn new(store: std::rc::Rc<std::cell::RefCell<crate::kanban::CardStore>>) -> Self {
        Self {
            store,
            toggled: std::collections::HashSet::new(),
            scroll: 0.0,
            scale: 1.0,
            acts: Vec::new(),
            #[cfg(test)]
            drawn_rows: Vec::new(),
            #[cfg(test)]
            card_rects: Vec::new(),
            #[cfg(test)]
            wave_rects: Vec::new(),
        }
    }

    /// Test-only identity accessor: confirms a restored/opened view shares
    /// the project's own `CardStore` Rc rather than a fresh one.
    #[cfg(test)]
    pub(crate) fn store(&self) -> &std::rc::Rc<std::cell::RefCell<crate::kanban::CardStore>> {
        &self.store
    }

    /// Whether wave `number` of `plan` is expanded. Only the current wave
    /// opens by default: finished waves are history and later waves are not
    /// in play, so a long plan still fits a screen. A click toggles that
    /// default either way, which is how a future wave gets inspected.
    fn expanded(&self, plan: &str, number: u32, current: Option<u32>) -> bool {
        let key = (fold(plan), number);
        (current == Some(number)) != self.toggled.contains(&key)
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        _active: bool,
        resp: &egui::Response,
        base: egui::Id,
    ) {
        // Arms the store's staleness re-read while this window is actually
        // rendered, the same way the board does: a plan tagged from a CLI in
        // another Session has to show up here without the board being open.
        self.store
            .borrow_mut()
            .mark_shown(std::time::Instant::now());

        let next_scale = crate::terminal::font_size(ui.ctx()) / crate::config::DEFAULT_FONT_SIZE;
        if next_scale != self.scale {
            self.scroll *= next_scale / self.scale;
            self.scale = next_scale;
        }
        let th = crate::theme::live(ui.ctx());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 0.0, th.bg);
        #[cfg(test)]
        {
            self.drawn_rows.clear();
            self.card_rects.clear();
            self.wave_rects.clear();
        }

        // Read seam: one borrow, folded into owned plans, dropped before any
        // intent is recorded below.
        let plans = {
            let store = self.store.borrow();
            crate::kanban::plans(store.cards())
        };
        if plans.is_empty() {
            self.scroll = 0.0;
            p.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "No plans yet — tag a card with `kanban edit <id> --plan NAME --wave N`",
                egui::FontId::proportional(11.0 * self.scale),
                th.dim,
            );
            return;
        }

        // Scroll before layout so the content height is known; the wheel is
        // read from the whole body, like the board's columns.
        let body = ui.interact(
            rect.intersect(p.clip_rect()),
            base.with("plans"),
            egui::Sense::hover(),
        );
        let mut content_h = 0.0;
        for plan in &plans {
            content_h += PLAN_H * self.scale;
            if plan.card_count() == 1 {
                content_h += ROW_H * self.scale;
                continue;
            }
            let current = plan.current();
            for wave in &plan.waves {
                content_h += WAVE_H * self.scale;
                if self.expanded(&plan.name, wave.number, current) {
                    content_h += wave.cards.len() as f32 * ROW_H * self.scale;
                }
            }
        }
        let max_scroll = (content_h - rect.height()).max(0.0);
        if body.hovered() || resp.contains_pointer() {
            let dy = ui.ctx().input(|i| i.smooth_scroll_delta.y);
            if dy != 0.0 {
                self.scroll = (self.scroll - dy).clamp(0.0, max_scroll);
            }
        }
        self.scroll = self.scroll.clamp(0.0, max_scroll);

        let show_state = rect.width() >= STATE_MIN_W * self.scale;
        let mut y = rect.min.y - self.scroll;
        for plan in &plans {
            y = self.show_plan(ui, &p, rect, y, plan, show_state, base, &th);
        }
    }

    /// One plan: its header, then either its single card (a one-card plan is
    /// drawn flat) or a wave outline. Returns the next free `y`.
    #[allow(clippy::too_many_arguments)]
    fn show_plan(
        &mut self,
        ui: &mut egui::Ui,
        p: &egui::Painter,
        rect: egui::Rect,
        top: f32,
        plan: &crate::kanban::Plan,
        show_state: bool,
        base: egui::Id,
        th: &crate::theme::Theme,
    ) -> f32 {
        let current = plan.current();
        // A plan of one card is almost always a plan name that missed an
        // existing one by a space or a hyphen — `same_name` folds case and
        // outer whitespace and nothing else. Saying so costs five lines and
        // is honest either way: a real plan of one card is not much of a
        // plan, and a typo must not read as a peer of the real ones.
        let lone = plan.card_count() == 1;
        let summary = if lone {
            "1 card".to_string()
        } else {
            match current {
                Some(n) => format!("wave {n} of {}", plan.waves.len()),
                None => "complete".to_string(),
            }
        };
        let head = egui::Rect::from_min_max(
            egui::pos2(rect.min.x, top),
            egui::pos2(rect.max.x, top + PLAN_H * self.scale),
        );
        let mut y = head.max.y;
        if head.max.y > rect.min.y && head.min.y < rect.max.y {
            p.line_segment(
                [
                    egui::pos2(head.min.x, head.max.y - 0.5),
                    egui::pos2(head.max.x, head.max.y - 0.5),
                ],
                egui::Stroke::new(1.0, th.border),
            );
            let right = head.max.x - PAD * self.scale;
            p.text(
                egui::pos2(right, head.center().y),
                egui::Align2::RIGHT_CENTER,
                elide(&summary, STATE_W * self.scale, 10.0 * self.scale),
                egui::FontId::proportional(10.0 * self.scale),
                th.dim,
            );
            // The name starts one PAD in and must stop one PAD short of the
            // summary slot, so both pads come out of its width.
            let name_left = head.min.x + PAD * self.scale;
            let name_w = (right - (STATE_W + PAD) * self.scale - name_left).max(0.0);
            p.text(
                egui::pos2(name_left, head.center().y),
                egui::Align2::LEFT_CENTER,
                elide(&plan.name, name_w, 11.5 * self.scale),
                egui::FontId::proportional(11.5 * self.scale),
                if lone { th.dim } else { th.fg },
            );
            #[cfg(test)]
            self.drawn_rows
                .push(format!("plan {}: {summary}", plan.name));
        }

        if lone {
            for wave in &plan.waves {
                for card in &wave.cards {
                    y = self.show_card(ui, p, rect, y, card, show_state, base, th);
                }
            }
            return y;
        }
        for wave in &plan.waves {
            let expanded = self.expanded(&plan.name, wave.number, current);
            y = self.show_wave(ui, p, rect, y, plan, wave, expanded, current, base, th);
            if !expanded {
                continue;
            }
            for card in &wave.cards {
                y = self.show_card(ui, p, rect, y, card, show_state, base, th);
            }
        }
        y
    }

    /// One wave header. Returns the next free `y`.
    #[allow(clippy::too_many_arguments)]
    fn show_wave(
        &mut self,
        ui: &mut egui::Ui,
        p: &egui::Painter,
        rect: egui::Rect,
        top: f32,
        plan: &crate::kanban::Plan,
        wave: &crate::kanban::Wave,
        expanded: bool,
        current: Option<u32>,
        base: egui::Id,
        th: &crate::theme::Theme,
    ) -> f32 {
        let head = egui::Rect::from_min_max(
            egui::pos2(rect.min.x, top),
            egui::pos2(rect.max.x, top + WAVE_H * self.scale),
        );
        if head.max.y <= rect.min.y || head.min.y >= rect.max.y {
            return head.max.y;
        }
        let is_current = current == Some(wave.number);
        let resp = ui.interact(
            head.intersect(p.clip_rect()),
            base.with(("wave", plan.name.as_str(), wave.number)),
            egui::Sense::click(),
        );
        if resp.hovered() {
            p.rect_filled(head, 0.0, th.tab_bg);
        }
        if resp.clicked() {
            let key = (fold(&plan.name), wave.number);
            if !self.toggled.remove(&key) {
                self.toggled.insert(key);
            }
        }
        let done = wave
            .cards
            .iter()
            .filter(|c| c.state == crate::kanban::CardState::Done)
            .count();
        let status = if done == wave.cards.len() {
            "done".to_string()
        } else {
            format!("{done}/{} done", wave.cards.len())
        };
        p.text(
            egui::pos2(head.max.x - PAD * self.scale, head.center().y),
            egui::Align2::RIGHT_CENTER,
            elide(&status, STATE_W * self.scale, 9.5 * self.scale),
            egui::FontId::proportional(9.5 * self.scale),
            th.dim,
        );
        let label = format!(
            "{} wave {}{}",
            if expanded { "▾" } else { "▸" },
            wave.number,
            if is_current { " · current" } else { "" }
        );
        // Indented 1.5 PAD, stopping one PAD short of the status slot.
        let label_left = head.min.x + PAD * 1.5 * self.scale;
        let label_w = (head.max.x - (STATE_W + PAD * 2.0) * self.scale - label_left).max(0.0);
        p.text(
            egui::pos2(label_left, head.center().y),
            egui::Align2::LEFT_CENTER,
            elide(&label, label_w, 10.5 * self.scale),
            egui::FontId::proportional(10.5 * self.scale),
            if is_current { th.text } else { th.dim },
        );
        #[cfg(test)]
        {
            self.drawn_rows.push(format!("{label}  {status}"));
            self.wave_rects
                .push(((fold(&plan.name), wave.number), head));
        }
        head.max.y
    }

    /// One card row. Returns the next free `y`.
    #[allow(clippy::too_many_arguments)]
    fn show_card(
        &mut self,
        ui: &mut egui::Ui,
        p: &egui::Painter,
        rect: egui::Rect,
        top: f32,
        card: &crate::kanban::PlanCard,
        show_state: bool,
        base: egui::Id,
        th: &crate::theme::Theme,
    ) -> f32 {
        let row = egui::Rect::from_min_max(
            egui::pos2(rect.min.x, top),
            egui::pos2(rect.max.x, top + ROW_H * self.scale),
        );
        if row.max.y <= rect.min.y || row.min.y >= rect.max.y {
            return row.max.y;
        }
        let resp = ui.interact(
            row.intersect(p.clip_rect()),
            base.with(("card", card.id.as_str())),
            egui::Sense::click(),
        );
        if resp.hovered() {
            p.rect_filled(row, 0.0, th.tab_bg);
        }
        if resp.clicked() {
            self.acts.push(PlanAct::OpenCard(card.id.clone()));
        }
        let mut right = row.max.x - PAD * self.scale;
        if show_state {
            let (word, colour) = state_word(card.state, th);
            p.text(
                egui::pos2(right, row.center().y),
                egui::Align2::RIGHT_CENTER,
                elide(word, STATE_W * self.scale, 9.5 * self.scale),
                egui::FontId::proportional(9.5 * self.scale),
                colour,
            );
            right -= (STATE_W + PAD) * self.scale;
        }
        let left = row.min.x + PAD * 3.0 * self.scale;
        p.text(
            egui::pos2(left, row.center().y),
            egui::Align2::LEFT_CENTER,
            elide(&card.title, (right - left).max(0.0), 10.5 * self.scale),
            egui::FontId::proportional(10.5 * self.scale),
            if card.state == crate::kanban::CardState::Done {
                th.dim
            } else {
                th.text
            },
        );
        #[cfg(test)]
        {
            self.drawn_rows
                .push(format!("card {}: {}", card.id, card.title));
            self.card_rects.push((card.id.clone(), row));
        }
        row.max.y
    }
}

/// The word a card's state gets here, plus the colour it earns. Blocked is
/// the only attention state a card reaches on its own, and attention is
/// `danger` — never `bell`, which the board reserves for terminals.
fn state_word(
    state: crate::kanban::CardState,
    th: &crate::theme::Theme,
) -> (&'static str, egui::Color32) {
    use crate::kanban::CardState as S;
    match state {
        S::Backlog => ("backlog", th.dim),
        S::InProgress => ("in progress", th.text),
        S::Blocked => ("blocked", th.danger),
        S::Done => ("done", th.dim),
    }
}

/// The plan-name fold used for view-state keys: the same trimmed,
/// lower-cased comparison [`crate::kanban::same_name`] makes, so two
/// spellings of one plan share one set of expanded waves.
fn fold(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Cut `text` to fit `width` at `size`, ending in `…`. Approximate on
/// purpose: egui's exact width needs a font atlas, and this runs per row per
/// frame. The estimate errs narrow, so text never overruns its slot.
fn elide(text: &str, width: f32, size: f32) -> String {
    let per = size * 0.58;
    if per <= 0.0 || width <= 0.0 {
        return String::new();
    }
    let fits = (width / per).floor() as usize;
    if text.chars().count() <= fits {
        return text.to_string();
    }
    if fits <= 1 {
        return "…".to_string();
    }
    let mut out: String = text.chars().take(fits - 1).collect();
    out.push('…');
    out
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

    /// Add a card and tag it into `plan` at `wave`, leaving it in `state`.
    fn plan_card(
        store: &Rc<RefCell<crate::kanban::CardStore>>,
        title: &str,
        plan: &str,
        wave: u32,
        state: crate::kanban::CardState,
    ) -> String {
        let id = store.borrow_mut().add(title, None).unwrap();
        store
            .borrow_mut()
            .edit(&id, None, None, Some(plan), Some(wave))
            .unwrap();
        if state != crate::kanban::CardState::Backlog {
            store
                .borrow_mut()
                .start(
                    &id,
                    "t1",
                    crate::kanban::run_nonce(),
                    crate::kanban::TermState::Missing,
                )
                .unwrap();
        }
        match state {
            crate::kanban::CardState::Done => store.borrow_mut().done(&id).unwrap(),
            crate::kanban::CardState::Blocked => store.borrow_mut().block(&id, "held").unwrap(),
            _ => {}
        }
        id
    }

    fn run_frame(
        ctx: &egui::Context,
        view: &mut PlanView,
        rect: egui::Rect,
        base: egui::Id,
        events: Vec<egui::Event>,
    ) {
        let mut input = egui::RawInput::default();
        input.events = events;
        // `run_ui`, not the deprecated `run` + `CentralPanel::show` the older
        // view tests use - same Ui, and it keeps the warning baseline flat.
        let _ = ctx.run_ui(input, |ui| {
            // Sense::click(), matching the real content response a
            // project-hosted `Content::Plan` is painted with in wm.rs.
            let resp = ui.interact(rect, egui::Id::new("test-plan-resp"), egui::Sense::click());
            view.show(ui, rect, true, &resp, base);
        });
    }

    fn click_at(
        ctx: &egui::Context,
        view: &mut PlanView,
        rect: egui::Rect,
        base: egui::Id,
        pos: egui::Pos2,
    ) {
        let moved = egui::Event::PointerMoved(pos);
        let btn = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        run_frame(ctx, view, rect, base, vec![moved.clone()]);
        run_frame(ctx, view, rect, base, vec![moved]);
        run_frame(ctx, view, rect, base, vec![btn(true)]);
        run_frame(ctx, view, rect, base, vec![btn(false)]);
        // One settle frame: `expanded` is read before `show_wave` processes
        // the click, so the release frame still paints the old state.
        run_frame(ctx, view, rect, base, vec![]);
    }

    #[test]
    fn elide_fits_shortens_and_never_exceeds_the_width() {
        assert_eq!(elide("hi", 100.0, 10.0), "hi");
        let out = elide("a very long plan name indeed", 40.0, 10.0);
        assert!(out.ends_with('…'), "{out}");
        assert!(out.chars().count() < "a very long plan name indeed".len());
        assert_eq!(elide("anything", 0.0, 10.0), "");
        assert_eq!(elide("anything", 3.0, 10.0), "…");
    }

    #[test]
    fn only_the_current_wave_opens_by_default_and_a_toggle_flips_either_way() {
        let tmp = tempfile::tempdir().unwrap();
        let mut v = PlanView::new(store_at(tmp.path()));
        assert!(v.expanded("P", 2, Some(2)), "the current wave is open");
        assert!(!v.expanded("P", 1, Some(2)), "a finished wave is collapsed");
        assert!(!v.expanded("P", 5, Some(2)), "a future wave is collapsed");
        assert!(
            !v.expanded("P", 1, None),
            "a complete plan collapses every wave"
        );
        v.toggled.insert(("p".into(), 2));
        assert!(!v.expanded("P", 2, Some(2)), "toggling closes the current");
        assert!(
            v.expanded("Other", 2, Some(2)),
            "another plan keeps the default"
        );
        assert!(!v.expanded("P", 5, Some(2)), "and so does another wave");
        assert!(
            !v.expanded(" p ", 2, Some(2)),
            "two spellings of one plan share one toggle"
        );
        v.toggled.insert(("p".into(), 5));
        assert!(v.expanded("P", 5, Some(2)), "toggling opens a future wave");
    }

    #[test]
    fn plans_group_by_wave_with_the_current_one_open_and_the_rest_collapsed() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        plan_card(
            &store,
            "done one",
            "Terminal",
            1,
            crate::kanban::CardState::Done,
        );
        plan_card(
            &store,
            "working",
            "Terminal",
            2,
            crate::kanban::CardState::InProgress,
        );
        plan_card(
            &store,
            "waiting",
            "Terminal",
            2,
            crate::kanban::CardState::Backlog,
        );
        plan_card(
            &store,
            "later",
            "Terminal",
            3,
            crate::kanban::CardState::Backlog,
        );

        let mut view = PlanView::new(Rc::clone(&store));
        let ctx = egui::Context::default();
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 400.0));
        let base = egui::Id::new("plan-grouping");
        run_frame(&ctx, &mut view, rect, base, vec![]);

        let rows = view.drawn_rows.join("\n");
        assert!(rows.contains("plan Terminal: wave 2 of 3"), "{rows}");
        assert!(rows.contains("wave 2 · current"), "{rows}");
        assert!(rows.contains("0/2 done"), "{rows}");
        assert!(rows.contains("▸ wave 1  done"), "{rows}");
        // Only the current wave lists its cards.
        assert!(rows.contains("card") && rows.contains("working"), "{rows}");
        assert!(!rows.contains("done one"), "a finished wave stays shut");
        assert!(!rows.contains("later"), "a future wave stays shut");

        // Clicking wave 1's header opens it.
        let head = view
            .wave_rects
            .iter()
            .find(|((p, n), _)| p == "terminal" && *n == 1)
            .map(|(_, r)| *r)
            .expect("wave 1 header was drawn");
        click_at(&ctx, &mut view, rect, base, head.center());
        assert!(
            view.drawn_rows.join("\n").contains("done one"),
            "{:?}",
            view.drawn_rows
        );
    }

    #[test]
    fn a_card_click_records_open_card_and_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        let id = plan_card(
            &store,
            "click me",
            "Terminal",
            1,
            crate::kanban::CardState::Backlog,
        );
        plan_card(
            &store,
            "sibling",
            "Terminal",
            1,
            crate::kanban::CardState::Backlog,
        );

        let mut view = PlanView::new(Rc::clone(&store));
        let ctx = egui::Context::default();
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 400.0));
        let base = egui::Id::new("plan-click");
        run_frame(&ctx, &mut view, rect, base, vec![]);
        let row = view
            .card_rects
            .iter()
            .find(|(c, _)| *c == id)
            .map(|(_, r)| *r)
            .expect("the card row was drawn");
        click_at(&ctx, &mut view, rect, base, row.center());
        assert_eq!(view.acts, vec![PlanAct::OpenCard(id.clone())]);
        assert_eq!(
            store.borrow().get(&id).unwrap().state,
            crate::kanban::CardState::Backlog,
            "reading a plan must never mutate a card"
        );
    }

    #[test]
    fn a_one_card_plan_is_drawn_flat_and_named_as_one_card() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        // The classic typo: `same_name` folds case and outer whitespace
        // only, so a hyphen makes a second plan out of nothing.
        plan_card(
            &store,
            "real one",
            "Terminal work",
            1,
            crate::kanban::CardState::Backlog,
        );
        plan_card(
            &store,
            "real two",
            "terminal WORK",
            2,
            crate::kanban::CardState::Backlog,
        );
        plan_card(
            &store,
            "typo",
            "terminal-work",
            1,
            crate::kanban::CardState::Backlog,
        );

        let mut view = PlanView::new(Rc::clone(&store));
        let ctx = egui::Context::default();
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 400.0));
        run_frame(&ctx, &mut view, rect, egui::Id::new("plan-lone"), vec![]);

        let rows = view.drawn_rows.join("\n");
        // Which spelling of the folded plan wins is card order, not a
        // property worth pinning; that there are exactly two plans is.
        let headers: Vec<&String> = view
            .drawn_rows
            .iter()
            .filter(|r| r.starts_with("plan "))
            .collect();
        assert_eq!(headers.len(), 2, "{headers:?}");
        assert!(
            headers.iter().any(|h| h.ends_with(": wave 1 of 2")),
            "{headers:?}"
        );
        assert!(
            headers.iter().any(|h| h.ends_with("terminal-work: 1 card")),
            "{headers:?}"
        );
        // A one-card plan draws no wave header at all: its card sits
        // directly under the name, so it cannot be mistaken for a real plan.
        assert!(
            !view
                .wave_rects
                .iter()
                .any(|((p, _), _)| p == "terminal-work"),
            "{:?}",
            view.wave_rects
        );
        assert!(rows.contains("card") && rows.contains("typo"), "{rows}");
    }

    #[test]
    fn an_empty_store_draws_the_hint_and_no_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        store.borrow_mut().add("unplanned", None).unwrap();
        let mut view = PlanView::new(store);
        let ctx = egui::Context::default();
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 400.0));
        run_frame(&ctx, &mut view, rect, egui::Id::new("plan-empty"), vec![]);
        assert!(view.drawn_rows.is_empty(), "{:?}", view.drawn_rows);
        assert!(view.acts.is_empty());
    }

    #[test]
    fn live_font_changes_scale_every_row_and_narrow_widths_drop_the_state_word() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        plan_card(&store, "one", "P", 1, crate::kanban::CardState::Backlog);
        plan_card(&store, "two", "P", 1, crate::kanban::CardState::Backlog);
        let mut view = PlanView::new(Rc::clone(&store));
        let ctx = egui::Context::default();
        let base = egui::Id::new("plan-scale");
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 700.0));
        for font_size in [6.0, 13.0, 26.0, 40.0, 13.0] {
            crate::terminal::set_font_size(&ctx, font_size);
            run_frame(&ctx, &mut view, rect, base, vec![]);
            let scale = font_size / crate::config::DEFAULT_FONT_SIZE;
            let row = view.card_rects.first().map(|(_, r)| *r).expect("a row");
            assert!(
                (row.height() - ROW_H * scale).abs() < 0.01,
                "font {font_size}: row {} vs {}",
                row.height(),
                ROW_H * scale
            );
            assert!(rect.contains_rect(row), "font {font_size}: {row:?}");
        }
        crate::terminal::set_font_size(&ctx, crate::config::DEFAULT_FONT_SIZE);
        // Narrow: the state word drops so the title keeps the space, and no
        // row escapes the window.
        let narrow = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(150.0, 400.0));
        run_frame(&ctx, &mut view, narrow, base, vec![]);
        for (_, r) in &view.card_rects {
            assert!(narrow.contains_rect(*r), "{r:?}");
        }
    }
}
