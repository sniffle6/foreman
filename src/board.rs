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
/// The inline picker's "wt on/off" chip, left of the agent buttons.
const WT_CHIP_W: f32 = 36.0;
/// The worktree strip along the bottom of the board (logical px).
const STRIP_H: f32 = 22.0;
/// Width of the Worktrees page's owner column (logical px). Fixed because a
/// `Grid` sizes a column from its first frame, when a wrapping label has no
/// width to wrap at and folds a few characters per line.
const OWNER_W: f32 = 260.0;

/// Done header controls (spec: kanban-cut §Board UI): the version dropdown
/// and, in Current, the Cut button — right-anchored, own hit regions.
const DD_W: f32 = 120.0;
const CUT_W: f32 = 40.0;
/// Floors, in logical px. The title always wins the Done header: the
/// controls shrink into their own floor and then drop out entirely rather
/// than clip "Done  (NN)" to a sliver or spill left over Blocked.
const TITLE_MIN_W: f32 = 80.0;
const DD_MIN_W: f32 = 56.0;
const CUT_MIN_W: f32 = 30.0;

/// Lay a Done header out as `[title][gap][dropdown][gap][Cut][pad]`,
/// right-anchored. Pure, and the single source of that geometry — the board
/// draws through it and the tests aim clicks through it, so the two cannot
/// drift.
///
/// The title keeps at least [`TITLE_MIN_W`]; whatever is left over goes to
/// the controls, dropdown shrinking first, each dropped once it would fall
/// under its own floor. Nothing is ever placed left of `header.min.x`: the
/// board paints with one board-wide painter and Done registers its
/// `interact` after Blocked's cards, so a control that spilled left would
/// both paint over Blocked and swallow its clicks.
///
/// `want_cut` is false inside a Version, where no Cut button is drawn — the
/// dropdown then gets that space back instead of a hole (and, in a narrow
/// column, survives where it would otherwise have been dropped; it is the
/// only way back to Current).
fn done_header_rects(
    header: egui::Rect,
    scale: f32,
    want_cut: bool,
) -> (egui::Rect, Option<egui::Rect>, Option<egui::Rect>) {
    let (pad, gap, inset) = (PAD * scale, BTN_GAP * scale, 4.0 * scale);
    // What the controls may spend, once the title's floor and the right
    // margin are taken out.
    let room = (header.width() - TITLE_MIN_W * scale - pad).max(0.0);
    let cost = |dd: f32, cut: f32| {
        (if dd > 0.0 { gap + dd } else { 0.0 }) + (if cut > 0.0 { gap + cut } else { 0.0 })
    };
    let mut cut_w = if want_cut { CUT_W * scale } else { 0.0 };
    let mut dd_w = DD_W * scale;
    if cost(dd_w, cut_w) > room {
        dd_w = (room - cost(0.0, cut_w) - gap).max(0.0);
    }
    if dd_w < DD_MIN_W * scale {
        dd_w = 0.0;
        if cost(0.0, cut_w) > room {
            cut_w = (room - gap).max(0.0);
        }
        if cut_w < CUT_MIN_W * scale {
            cut_w = 0.0;
        }
    }
    let h = (header.height() - inset * 2.0).max(0.0);
    let top = header.min.y + inset;
    let mut right = header.max.x - pad;
    let cut = (cut_w > 0.0).then(|| {
        let r = egui::Rect::from_min_size(egui::pos2(right - cut_w, top), egui::vec2(cut_w, h));
        right -= cut_w + gap;
        r
    });
    let dd = (dd_w > 0.0).then(|| {
        let r = egui::Rect::from_min_size(egui::pos2(right - dd_w, top), egui::vec2(dd_w, h));
        right -= dd_w + gap;
        r
    });
    let mut title = header;
    title.max.x = right.max(header.min.x);
    (title, dd, cut)
}

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
    /// `worktree` is the per-dispatch choice (spec: dispatch-worktrees; the
    /// global setting is only its default): true = bring up a worktree.
    Dispatch {
        id: String,
        agent: String,
        worktree: bool,
    },
    Done(String),
    Release(String),
    Rm(String),
    JumpTo(String),
    /// Human-only forcing teardown of a card's worktree (spec:
    /// dispatch-worktrees §Teardown). The manager opens a confirm first.
    DiscardWorktree(String),
    /// Non-forcing teardown of a worktree no card owns (Worktrees page).
    /// git refuses a dirty or unmerged tree; the manager toasts why.
    RemoveStray(crate::kanban::Worktree),
    /// Forcing teardown of a worktree no card owns; confirm first.
    DiscardStray(crate::kanban::Worktree),
    /// Submit (or resubmit) a card's committed worktree to the repository
    /// integration queue (spec: worktree-integration-queue).
    Integrate(String),
    /// Withdraw a card's integration request.
    CancelIntegration(String),
    /// Cut the ungrouped Done cards into a Version (spec: kanban-cut §Cut).
    /// The manager owns the worktree probe and the trailer walk.
    Cut(String),
    /// Return a Version's cards to Current Done (spec §Uncut).
    Uncut(String),
    /// The Cut field just opened empty; the manager answers with the latest
    /// unused `v*` tag via `BoardView::prefill_cut`, or with nothing.
    CutPrefill,
}

pub struct BoardView {
    store: std::rc::Rc<std::cell::RefCell<crate::kanban::CardStore>>,
    quick_add: String,
    picker: Option<String>,
    /// The per-dispatch "into a worktree?" choice as (card id, value). Seeded
    /// from the global setting the first time a card's picker or detail page
    /// asks, so one card's override never leaks onto the next card.
    worktree_choice: Option<(String, bool)>,
    scroll: [f32; 4],
    collapsed: [bool; 4],
    selected: Option<String>,
    /// The Worktrees page is showing (below `selected`: a card opened from
    /// the page returns to it on Back). View state, never persisted.
    worktrees_open: bool,
    scale: f32,
    /// Which Done view is showing: `None` = Current, `Some(name)` = that
    /// Version (spec §Board UI). View state, never persisted — same rule as
    /// `collapsed`.
    pub(crate) version: Option<String>,
    /// The open Cut name field's buffer; `None` = closed.
    pub(crate) cut_field: Option<String>,
    /// True once the human typed into the field, so a late prefill never
    /// overwrites text.
    cut_touched: bool,
    /// Focus the Cut field on the first frame after it opens.
    cut_focus_pending: bool,
    /// Test probes: what the last frame drew for Done.
    #[cfg(test)]
    pub(crate) offered_cut: bool,
    #[cfg(test)]
    pub(crate) drew_banner: bool,
    #[cfg(test)]
    pub(crate) done_listed: Vec<String>,
    #[cfg(test)]
    pub(crate) detail_commits: Vec<String>,
    /// Test probe: `(version name, row rect)` for each row the version
    /// dropdown's popup drew last frame, so a test can click a real row
    /// instead of guessing where egui put the popup.
    #[cfg(test)]
    pub(crate) dropdown_rows: Vec<(String, egui::Rect)>,
    /// Test probe: the version dropdown's own button rect, for the
    /// does-it-fit-its-slot assertions at other font scales.
    #[cfg(test)]
    pub(crate) dropdown_btn: Option<egui::Rect>,
    /// Test probe: the archive banner's Uncut rect as actually drawn, after
    /// the clamp to the column.
    #[cfg(test)]
    pub(crate) uncut_rect: Option<egui::Rect>,
    /// Test probe: the text last painted on a collapsed Done rail.
    #[cfg(test)]
    pub(crate) rail_text: Option<String>,
    /// Test probe: the worktree strip's text last frame (`None` = no strip).
    #[cfg(test)]
    pub(crate) strip_text: Option<String>,
    /// Test probe: the row names the Worktrees page drew last frame.
    #[cfg(test)]
    pub(crate) wt_rows_drawn: Vec<String>,
    /// Test probe: `(row name, button label, rect)` for each button the
    /// Worktrees page drew last frame, so a test clicks the real thing.
    #[cfg(test)]
    pub(crate) wt_btn_rects: Vec<(String, &'static str, egui::Rect)>,
    pub acts: Vec<BoardAct>,
    /// Test probe: true when the last `show_details` frame drew the Discard
    /// button (Done/Blocked/orphaned card with a worktree).
    #[cfg(test)]
    pub(crate) offered_discard: bool,
    /// Test probe: the integration buttons the last `show_details` frame
    /// drew (`submit`, `resubmit`, `cancel`).
    #[cfg(test)]
    pub(crate) offered_integration: Vec<&'static str>,
}

impl BoardView {
    pub fn new(store: std::rc::Rc<std::cell::RefCell<crate::kanban::CardStore>>) -> Self {
        Self {
            store,
            quick_add: String::new(),
            picker: None,
            worktree_choice: None,
            scroll: [0.0; 4],
            collapsed: [false; 4],
            selected: None,
            worktrees_open: false,
            scale: 1.0,
            acts: Vec::new(),
            version: None,
            cut_field: None,
            cut_touched: false,
            cut_focus_pending: false,
            #[cfg(test)]
            offered_cut: false,
            #[cfg(test)]
            drew_banner: false,
            #[cfg(test)]
            done_listed: Vec::new(),
            #[cfg(test)]
            detail_commits: Vec::new(),
            #[cfg(test)]
            dropdown_rows: Vec::new(),
            #[cfg(test)]
            dropdown_btn: None,
            #[cfg(test)]
            uncut_rect: None,
            #[cfg(test)]
            rail_text: None,
            #[cfg(test)]
            strip_text: None,
            #[cfg(test)]
            wt_rows_drawn: Vec::new(),
            #[cfg(test)]
            wt_btn_rects: Vec::new(),
            #[cfg(test)]
            offered_discard: false,
            #[cfg(test)]
            offered_integration: Vec::new(),
        }
    }

    /// Show `id`'s detail page. The plan view's click-through comes in this
    /// way: the plan window records the intent, the manager opens or focuses
    /// the board, then points it here. An id that names no live card is
    /// dropped by `show`, which snaps back to the columns.
    pub fn open_card(&mut self, id: &str) {
        self.selected = Some(id.to_string());
        self.worktrees_open = false;
        self.picker = None;
    }

    /// Manager's answer to `BoardAct::CutPrefill`. Fills the open Cut field
    /// only while it is still empty and untouched.
    pub fn prefill_cut(&mut self, name: &str) {
        if self.cut_touched {
            return;
        }
        if let Some(buf) = &mut self.cut_field
            && buf.is_empty()
        {
            *buf = name.to_string();
        }
    }

    /// Test-only identity accessor: confirms a restored/opened view shares
    /// the project's own `CardStore` Rc rather than a fresh one.
    #[cfg(test)]
    pub(crate) fn store(&self) -> &std::rc::Rc<std::cell::RefCell<crate::kanban::CardStore>> {
        &self.store
    }

    /// Test-only probe for the card whose detail page is showing, so the
    /// manager's plan-view click-through can be asserted from `wm`.
    #[cfg(test)]
    pub(crate) fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// The dispatch-time worktree choice for `card`: forced on for a card
    /// that already carries a worktree (Restart resumes in it), otherwise
    /// the remembered per-card toggle, seeded from the global setting.
    fn worktree_choice_for(&mut self, ctx: &egui::Context, card: &crate::kanban::Card) -> bool {
        if card.worktree.is_some() {
            return true;
        }
        match &self.worktree_choice {
            Some((id, v)) if *id == card.id => *v,
            _ => {
                let v = crate::config::live(ctx).dispatch_worktrees;
                self.worktree_choice = Some((card.id.clone(), v));
                v
            }
        }
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
        let (cards, orphans, rows) = {
            let store = self.store.borrow();
            let rows = crate::kanban::worktree_rows(
                store.cards(),
                store.orphans(),
                |id| store.worktree_status(id),
                store.strays(),
            );
            (store.cards().to_vec(), store.orphans().clone(), rows)
        };
        // A selected Version that no longer exists (Uncut, `rm` of its last
        // card, a pull) snaps back to Current (spec §Uncut).
        if let Some(v) = self.version.clone()
            && !crate::kanban::versions(&cards)
                .iter()
                .any(|x| crate::kanban::same_name(&x.name, &v))
        {
            self.version = None;
        }
        #[cfg(test)]
        {
            self.offered_cut = false;
            self.drew_banner = false;
            self.done_listed.clear();
            self.detail_commits.clear();
            self.dropdown_rows.clear();
            self.dropdown_btn = None;
            self.uncut_rect = None;
            self.rail_text = None;
            self.strip_text = None;
            self.wt_rows_drawn.clear();
            self.wt_btn_rects.clear();
        }

        if let Some(id) = self.selected.clone() {
            if let Some(card) = cards.iter().find(|c| c.id == id) {
                self.show_details(ui, rect, base, card, orphans.contains(&id), &th);
                return;
            }
            self.selected = None;
        }
        if self.worktrees_open {
            self.show_worktrees(ui, rect, base, &rows, &th);
            return;
        }
        // The worktree strip takes the bottom row only when there is
        // something to show, so a project without worktrees sees no change.
        let mut rect = rect;
        if let Some((text, attention)) = crate::kanban::worktree_strip(&rows) {
            let strip = egui::Rect::from_min_max(
                egui::pos2(rect.min.x, rect.max.y - STRIP_H * self.scale),
                rect.max,
            );
            rect.max.y = strip.min.y;
            self.show_strip(ui, &p, strip, base, &text, attention, &th);
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
        let is_done = state == crate::kanban::CardState::Done;
        let matching: Vec<&crate::kanban::Card> = cards
            .iter()
            .filter(|c| {
                c.state == state
                    && (!is_done
                        || match &self.version {
                            None => c.shipped.is_none(),
                            Some(v) => c
                                .shipped
                                .as_ref()
                                .is_some_and(|s| crate::kanban::same_name(&s.name, v)),
                        })
            })
            .collect();
        #[cfg(test)]
        if is_done {
            self.done_listed = matching.iter().map(|c| c.id.clone()).collect();
        }

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
            let text = match (&self.version, is_done) {
                (Some(v), true) => format!("Done\n{v}\n{}", matching.len()),
                _ => format!(
                    "{}\n{}",
                    column_title(state).replace(' ', "\n"),
                    matching.len()
                ),
            };
            #[cfg(test)]
            if is_done {
                self.rail_text = Some(text.clone());
            }
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
        // Done carries right-anchored controls; the collapse click target is
        // only the title to their left (spec §Board UI). Every rect comes
        // from the one pure layout function, and is clipped to the column as
        // a belt-and-braces guard against ever reaching into Blocked.
        let (title_rect, dd_rect, cut_rect) = if is_done {
            let (t, dd, cut) = done_header_rects(header_rect, self.scale, self.version.is_none());
            (
                t.intersect(col_rect),
                dd.map(|r| r.intersect(col_rect)),
                cut.map(|r| r.intersect(col_rect)),
            )
        } else {
            (header_rect, None, None)
        };
        disclosure(
            &p.with_clip_rect(title_rect),
            egui::pos2(title_rect.min.x + 10.0 * self.scale, title_rect.center().y),
            self.scale,
            false,
            th.dim,
        );
        p.with_clip_rect(title_rect).text(
            egui::pos2(title_rect.min.x + 20.0 * self.scale, title_rect.center().y),
            egui::Align2::LEFT_CENTER,
            format!("{}  ({})", column_title(state), matching.len()),
            egui::FontId::proportional(11.5 * self.scale),
            th.dim,
        );
        if ui
            .interact(
                title_rect,
                base.with((col_idx, "collapse")),
                egui::Sense::click(),
            )
            .on_hover_text("Collapse column")
            .clicked()
        {
            self.collapsed[col_idx] = true;
            self.picker = None;
        }

        if let Some(dd) = dd_rect {
            // Version dropdown: Current pinned, then Versions newest first.
            let versions = crate::kanban::versions(cards);
            let selected_text = self
                .version
                .clone()
                .unwrap_or_else(|| crate::kanban::CURRENT.to_string());
            let mut pick: Option<Option<String>> = None;
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt(base.with((col_idx, "version-ui")))
                    .max_rect(dd),
            );
            // The combo sizes itself from the style, which is in unscaled
            // px: without this it keeps its default height and overflows the
            // slot into the first card row whenever the font is zoomed out.
            child.spacing_mut().interact_size.y = dd.height();
            child.spacing_mut().button_padding *= self.scale;
            child.spacing_mut().icon_width *= self.scale;
            child.spacing_mut().icon_width_inner *= self.scale;
            for font in child.style_mut().text_styles.values_mut() {
                font.size *= self.scale;
            }
            let combo = egui::ComboBox::from_id_salt(base.with((col_idx, "version")))
                .width(dd.width())
                .truncate()
                .selected_text(selected_text)
                .show_ui(&mut child, |ui| {
                    let r = ui.selectable_label(self.version.is_none(), crate::kanban::CURRENT);
                    #[cfg(test)]
                    self.dropdown_rows
                        .push((crate::kanban::CURRENT.to_string(), r.rect));
                    if r.clicked() {
                        pick = Some(None);
                    }
                    for v in &versions {
                        let on = self
                            .version
                            .as_deref()
                            .is_some_and(|s| crate::kanban::same_name(s, &v.name));
                        let r = ui.selectable_label(on, format!("{} ({})", v.name, v.count));
                        #[cfg(test)]
                        self.dropdown_rows.push((v.name.clone(), r.rect));
                        if r.clicked() {
                            pick = Some(Some(v.name.clone()));
                        }
                    }
                });
            #[cfg(test)]
            {
                self.dropdown_btn = Some(combo.response.rect);
            }
            #[cfg(not(test))]
            let _ = combo;
            if let Some(choice) = pick {
                self.version = choice;
                self.cut_field = None;
                self.picker = None;
            }
        }
        if let Some(cut) = cut_rect {
            if self.version.is_none() {
                let enabled = !matching.is_empty();
                #[cfg(test)]
                {
                    self.offered_cut = enabled;
                }
                let sense = if enabled {
                    egui::Sense::click()
                } else {
                    egui::Sense::hover()
                };
                let r = ui.interact(cut, base.with((col_idx, "cut")), sense);
                p.rect_filled(
                    cut,
                    3.0,
                    if enabled && r.hovered() {
                        th.sel_bg
                    } else {
                        th.bg
                    },
                );
                p.rect_stroke(
                    cut,
                    3.0,
                    egui::Stroke::new(1.0, th.border),
                    egui::StrokeKind::Inside,
                );
                p.text(
                    cut.center(),
                    egui::Align2::CENTER_CENTER,
                    "Cut",
                    egui::FontId::proportional(10.5 * self.scale),
                    if enabled { th.text } else { th.dim },
                );
                let r = r.on_hover_text(if enabled {
                    "Cut Done into a named Version"
                } else {
                    "Nothing in Done to cut"
                });
                if enabled && r.clicked() && self.cut_field.is_none() {
                    self.cut_field = Some(String::new());
                    self.cut_touched = false;
                    self.cut_focus_pending = true;
                    self.acts.push(BoardAct::CutPrefill);
                }
            }
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

        if is_done && self.version.is_none() && self.cut_field.is_some() {
            // The Cut name field: same inline shape as quick-add, no modal.
            let field_rect = egui::Rect::from_min_size(
                egui::pos2(col_rect.min.x + PAD * self.scale, body_top),
                egui::vec2(
                    (col_rect.width() - PAD * self.scale * 2.0).max(0.0),
                    QUICK_ADD_H * self.scale - 4.0 * self.scale,
                ),
            );
            ui.visuals_mut().selection.bg_fill = th.selection_text_bg;
            let buf = self.cut_field.as_mut().expect("checked above");
            let te = ui.put(
                field_rect,
                egui::TextEdit::singleline(buf)
                    .id(base.with((col_idx, "cut-name")))
                    .font(egui::FontId::proportional(11.5 * self.scale))
                    .text_color(th.text)
                    .hint_text("version name…  Enter cuts, Esc cancels")
                    .vertical_align(egui::Align::Center)
                    .frame(egui::Frame::NONE)
                    .margin(egui::Margin::symmetric(4, 0))
                    .desired_width(field_rect.width()),
            );
            if std::mem::take(&mut self.cut_focus_pending) {
                te.request_focus();
            }
            if te.changed() {
                self.cut_touched = true;
            }
            let (enter, esc) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::Enter),
                    i.key_pressed(egui::Key::Escape),
                )
            });
            if te.lost_focus() && esc {
                self.cut_field = None;
            } else if te.lost_focus() && enter {
                let name = self.cut_field.take().unwrap_or_default();
                let name = name.trim().to_string();
                if name.is_empty() {
                    // Empty is a no-op, not a toast: nothing was asked for.
                } else {
                    self.acts.push(BoardAct::Cut(name));
                }
            }
            body_top += QUICK_ADD_H * self.scale;
        }
        if is_done && let Some(v) = self.version.clone() {
            // Archive banner: the signal that Done is not Current.
            #[cfg(test)]
            {
                self.drew_banner = true;
            }
            let banner = egui::Rect::from_min_size(
                egui::pos2(col_rect.min.x, body_top),
                egui::vec2(col_rect.width(), QUICK_ADD_H * self.scale),
            );
            p.rect_filled(banner, 0.0, th.title_bg);
            p.with_clip_rect(banner).text(
                egui::pos2(banner.min.x + PAD * self.scale, banner.center().y),
                egui::Align2::LEFT_CENTER,
                format!("Archived · {v}"),
                egui::FontId::proportional(11.0 * self.scale),
                th.dim,
            );
            // Clamped to the column for the same reason the header controls
            // are: `BTN_W + PAD` is a fixed cost, so in a narrow Done (a
            // small pane, or a zoomed font) the unclamped rect starts left of
            // the column, paints over Blocked and — Done interacting after
            // Blocked's cards on one board-wide painter — takes its clicks.
            let uncut = egui::Rect::from_min_size(
                egui::pos2(
                    banner.max.x - PAD * self.scale - BTN_W * self.scale,
                    banner.min.y + 2.0 * self.scale,
                ),
                egui::vec2(BTN_W * self.scale, banner.height() - 4.0 * self.scale),
            )
            .intersect(col_rect);
            #[cfg(test)]
            {
                self.uncut_rect = uncut.is_positive().then_some(uncut);
            }
            if uncut.is_positive() {
                let r = ui.interact(uncut, base.with((col_idx, "uncut")), egui::Sense::click());
                p.rect_filled(uncut, 3.0, if r.hovered() { th.sel_bg } else { th.bg });
                p.rect_stroke(
                    uncut,
                    3.0,
                    egui::Stroke::new(1.0, th.border),
                    egui::StrokeKind::Inside,
                );
                p.text(
                    uncut.center(),
                    egui::Align2::CENTER_CENTER,
                    "Uncut",
                    egui::FontId::proportional(10.5 * self.scale),
                    th.text,
                );
                if r.on_hover_text("Return these cards to Current Done")
                    .clicked()
                {
                    self.acts.push(BoardAct::Uncut(v));
                }
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
        // The integration substate (spec: worktree-integration-queue) is
        // the card's status while a request stands — queued, integrating,
        // and needs resolution are what the human wants to see, not the
        // claim. An ended Session is still said first: a submitted request
        // survives its worker, and the human should know both.
        let integration = self.store.borrow().integration(&card.id);
        let status = match (&integration, orphaned) {
            (Some(v), true) => format!("Session ended · integration {}", v.summary()),
            (Some(v), false) => {
                let s = v.summary();
                let mut c = s.chars();
                match c.next() {
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    None => s,
                }
            }
            (None, true) => "Session ended — restart or return to backlog".to_owned(),
            (None, false) => {
                if let Some(reason) = &card.blocked_reason {
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
                }
            }
        };
        let needs_resolution = integration
            .as_ref()
            .is_some_and(|v| v.phase == crate::integrate::Phase::NeedsResolution);
        let status_color = if orphaned || card.blocked_reason.is_some() || needs_resolution {
            th.danger
        } else if integration.is_some() {
            th.text
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
    /// The one-row summary along the bottom of the board; the whole row is
    /// the button that opens the Worktrees page.
    #[allow(clippy::too_many_arguments)]
    fn show_strip(
        &mut self,
        ui: &mut egui::Ui,
        p: &egui::Painter,
        strip: egui::Rect,
        base: egui::Id,
        text: &str,
        attention: bool,
        th: &crate::theme::Theme,
    ) {
        #[cfg(test)]
        {
            self.strip_text = Some(text.to_owned());
        }
        let r = ui.interact(strip, base.with("wt-strip"), egui::Sense::click());
        p.rect_filled(strip, 0.0, if r.hovered() { th.sel_bg } else { th.bg });
        p.line_segment(
            [strip.min, egui::pos2(strip.max.x, strip.min.y)],
            egui::Stroke::new(1.0, th.border),
        );
        p.text(
            egui::pos2(strip.min.x + PAD * self.scale * 2.0, strip.center().y),
            egui::Align2::LEFT_CENTER,
            text,
            egui::FontId::proportional(10.5 * self.scale),
            if attention { th.danger } else { th.dim },
        );
        p.text(
            egui::pos2(strip.max.x - PAD * self.scale * 2.0, strip.center().y),
            egui::Align2::RIGHT_CENTER,
            "Worktrees ›",
            egui::FontId::proportional(10.5 * self.scale),
            th.text,
        );
        if r.on_hover_text("Every worktree foreman made for this project")
            .clicked()
        {
            self.worktrees_open = true;
            self.picker = None;
        }
    }

    /// The Worktrees page: one row per foreman worktree, card-owned rows
    /// first. Card rows navigate (Open card, Open terminal); stray rows
    /// clean up (Remove, Discard). Swaps in for the columns like the card
    /// detail page.
    fn show_worktrees(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        base: egui::Id,
        rows: &[crate::kanban::WorktreeRow],
        th: &crate::theme::Theme,
    ) {
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(base.with("worktrees-page"))
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
            self.worktrees_open = false;
        }
        child.separator();
        child.add(
            egui::Label::new(
                egui::RichText::new(format!("Worktrees ({})", rows.len())).size(18.0 * self.scale),
            )
            .wrap(),
        );
        if rows.is_empty() {
            child.label(egui::RichText::new("No worktrees.").color(th.dim));
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt(base.with("worktrees-scroll"))
            .show(&mut child, |ui| {
                egui::Grid::new(base.with("worktrees-grid"))
                    .num_columns(4)
                    .striped(true)
                    .spacing(egui::vec2(16.0 * self.scale, 6.0 * self.scale))
                    .show(ui, |ui| {
                        for row in rows {
                            self.show_worktree_row(ui, row, th);
                            ui.end_row();
                        }
                    });
            });
    }

    fn show_worktree_row(
        &mut self,
        ui: &mut egui::Ui,
        row: &crate::kanban::WorktreeRow,
        th: &crate::theme::Theme,
    ) {
        use crate::kanban::RowOwner;
        #[cfg(test)]
        {
            self.wt_rows_drawn.push(row.name());
        }
        let missing = row.status.is_some_and(|s| s.missing);
        let color = if missing {
            th.dim
        } else if row.attention {
            th.danger
        } else {
            th.text
        };
        ui.add(egui::Label::new(
            egui::RichText::new(&row.wt.branch).monospace().color(color),
        ))
        .on_hover_text(&row.wt.path);
        ui.vertical(|ui| {
            ui.set_min_width(OWNER_W * self.scale);
            ui.set_max_width(OWNER_W * self.scale);
            match &row.owner {
                RowOwner::Card {
                    id,
                    state,
                    title,
                    version,
                    ..
                } => {
                    let mut head = format!("{id} · {}", column_title(*state));
                    if let Some(v) = version {
                        head.push_str(&format!(" · {v}"));
                    }
                    ui.add(egui::Label::new(egui::RichText::new(head).color(th.dim)).truncate());
                    ui.add(egui::Label::new(title.as_str()).truncate());
                }
                RowOwner::None => {
                    ui.label(egui::RichText::new("no card").color(th.danger));
                    ui.add(
                        egui::Label::new(egui::RichText::new(row.name()).color(th.dim)).truncate(),
                    );
                }
            }
        });
        let line = match row.status {
            None => "not polled yet".to_owned(),
            Some(s) => {
                let mut l = format!("+{} -{}", s.ahead, s.behind);
                if s.dirty {
                    l.push_str(" · uncommitted changes");
                }
                if s.missing {
                    l.push_str(" · directory missing");
                }
                l
            }
        };
        ui.label(
            egui::RichText::new(line)
                .monospace()
                .color(if row.attention { th.danger } else { th.dim }),
        );
        let name = row.name();
        #[cfg(test)]
        let mut probe = |label: &'static str, r: &egui::Response| {
            self.wt_btn_rects.push((name.clone(), label, r.rect));
        };
        #[cfg(not(test))]
        let mut probe = |_: &'static str, _: &egui::Response| {};
        ui.horizontal(|ui| match &row.owner {
            RowOwner::Card { id, terminal, .. } => {
                let r = ui.button("Open card");
                probe("Open card", &r);
                if r.clicked() {
                    self.selected = Some(id.clone());
                    self.picker = None;
                }
                if let Some(t) = terminal {
                    let r = ui.button("Open terminal");
                    probe("Open terminal", &r);
                    if r.clicked() {
                        self.acts.push(BoardAct::JumpTo(t.clone()));
                    }
                }
            }
            RowOwner::None => {
                let r = ui
                    .button("Remove")
                    .on_hover_text("git worktree remove: refuses uncommitted or unmerged work");
                probe("Remove", &r);
                if r.clicked() {
                    self.acts.push(BoardAct::RemoveStray(row.wt.clone()));
                }
                let r = ui
                    .button(egui::RichText::new("Discard").color(th.danger))
                    .on_hover_text("Force-removes the tree and branch, including unmerged work");
                probe("Discard", &r);
                if r.clicked() {
                    self.acts.push(BoardAct::DiscardStray(row.wt.clone()));
                }
            }
        });
    }

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
            self.offered_integration.clear();
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
                #[cfg(test)]
                {
                    self.detail_commits = card
                        .shipped
                        .as_ref()
                        .map(|s| s.commits.clone())
                        .unwrap_or_default();
                }
                if let Some(s) = &card.shipped {
                    ui.label(
                        egui::RichText::new(format!("Version: {} · cut {}", s.name, s.at))
                            .color(th.dim),
                    );
                }
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
                    // The integration queue (spec: worktree-integration-
                    // queue): state, reason, next action, and the human's
                    // two levers — submit/resubmit and cancel. Discard and
                    // restart are refused by the manager while the queue
                    // owns the tree, so the buttons are hidden then.
                    let integration = self.store.borrow().integration(&card.id);
                    let owned = integration.as_ref().is_some_and(|v| v.phase.owns_worktree());
                    ui.add_space(12.0 * self.scale);
                    ui.strong("Integration");
                    match &integration {
                        None => {
                            ui.label(egui::RichText::new("Not submitted").color(th.dim));
                        }
                        Some(v) => {
                            let bad = v.phase == crate::integrate::Phase::NeedsResolution;
                            ui.label(
                                egui::RichText::new(v.summary())
                                    .color(if bad { th.danger } else { th.text }),
                            );
                            if let Some(d) = &v.detail {
                                detail_text(ui, d);
                            }
                            if let Some(n) = &v.next {
                                ui.label(
                                    egui::RichText::new(format!("Next: {n}")).color(th.dim),
                                );
                            }
                            if let Some(c) = &v.checks {
                                ui.label(
                                    egui::RichText::new(format!("Checks: {c}")).color(th.dim),
                                );
                            }
                            if let Some(n) = &v.note {
                                ui.label(egui::RichText::new(n.as_str()).color(th.dim));
                            }
                        }
                    }
                    ui.horizontal_wrapped(|ui| {
                        let submittable = card.state == crate::kanban::CardState::InProgress
                            && integration.as_ref().is_none_or(|v| {
                                v.phase == crate::integrate::Phase::NeedsResolution
                            });
                        if submittable {
                            let label = if integration.is_some() {
                                "Resubmit"
                            } else {
                                "Submit for integration"
                            };
                            #[cfg(test)]
                            self.offered_integration.push(if integration.is_some() {
                                "resubmit"
                            } else {
                                "submit"
                            });
                            if ui
                                .button(label)
                                .on_hover_text(
                                    "Queue the committed worktree: Foreman rebases, checks, fast-forwards, and marks Done",
                                )
                                .clicked()
                            {
                                self.acts.push(BoardAct::Integrate(card.id.clone()));
                            }
                        }
                        let cancellable = integration.as_ref().is_some_and(|v| {
                            v.phase != crate::integrate::Phase::Integrated
                        });
                        if cancellable {
                            #[cfg(test)]
                            self.offered_integration.push("cancel");
                            if ui
                                .button("Cancel integration")
                                .on_hover_text(
                                    "Withdraw the request; a running turn stops at its next checkpoint",
                                )
                                .clicked()
                            {
                                self.acts.push(BoardAct::CancelIntegration(card.id.clone()));
                            }
                        }
                    });
                    // Discard is human-only and only where no live worker
                    // could be mid-write: Done, Blocked, or orphaned — and
                    // never while the queue owns the tree.
                    let discardable = !owned
                        && (orphaned
                            || matches!(
                                card.state,
                                crate::kanban::CardState::Done | crate::kanban::CardState::Blocked
                            ));
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
                if let Some(s) = &card.shipped
                    && !s.commits.is_empty()
                {
                    ui.add_space(12.0 * self.scale);
                    ui.strong("Commits");
                    ui.add(
                        egui::Label::new(egui::RichText::new(s.commits.join("  ")).monospace())
                            .wrap()
                            .selectable(true),
                    );
                }
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
                    let mut use_worktree = self.worktree_choice_for(ui.ctx(), card);
                    if card.worktree.is_none() {
                        if ui
                            .checkbox(&mut use_worktree, "Dispatch into a git worktree")
                            .changed()
                        {
                            self.worktree_choice = Some((card.id.clone(), use_worktree));
                        }
                    }
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
                                    worktree: use_worktree,
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
        let use_worktree = self.worktree_choice_for(ui.ctx(), card);
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
                    worktree: use_worktree,
                });
                self.picker = None;
            }
            bx -= BTN_W * self.scale + BTN_GAP * self.scale;
        }
        // The worktree chip: only for a card without a worktree (one that
        // has one always restarts in it), and only when the card is wide
        // enough — a narrow card still dispatches with the default, and the
        // detail page always offers the checkbox.
        if card.worktree.is_some() {
            return;
        }
        let chip_rect = egui::Rect::from_min_size(
            egui::pos2(
                bx - WT_CHIP_W * self.scale,
                card_rect.max.y - BTN_H * self.scale - 4.0 * self.scale,
            ),
            egui::vec2(WT_CHIP_W * self.scale, BTN_H * self.scale),
        );
        if chip_rect.min.x < card_rect.min.x + PAD * self.scale {
            return;
        }
        let chip_resp = ui
            .interact(
                chip_rect.intersect(cp.clip_rect()),
                base.with((card.id.as_str(), "pick", "worktree")),
                egui::Sense::click(),
            )
            .on_hover_text(if use_worktree {
                "Dispatch into a git worktree (click for the project cwd)"
            } else {
                "Dispatch in the project cwd (click for a git worktree)"
            });
        cp.rect_filled(
            chip_rect,
            2.0,
            if chip_resp.hovered() {
                th.sel_bg
            } else {
                th.bg
            },
        );
        cp.text(
            chip_rect.center(),
            egui::Align2::CENTER_CENTER,
            if use_worktree { "wt on" } else { "wt off" },
            egui::FontId::proportional(9.0 * self.scale),
            if use_worktree { th.text } else { th.dim },
        );
        if chip_resp.clicked() {
            *picker_click_consumed = true;
            self.worktree_choice = Some((card.id.clone(), !use_worktree));
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

    /// Footer-row geometry for the sole card in Backlog (column 0) at scale
    /// 1: the worktree chip sits left of the three agent buttons, the
    /// rightmost agent button is the last entry of `AGENTS`.
    fn picker_points(rect: egui::Rect) -> (egui::Pos2, egui::Pos2) {
        let col_w = rect.width() / COLUMNS.len() as f32;
        let card_max_x = rect.min.x + col_w - PAD;
        let y = rect.min.y + HEADER_H + QUICK_ADD_H + CARD_H - BTN_H / 2.0 - 4.0;
        let chip_x = card_max_x - PAD - BTN_W * 3.0 - BTN_GAP * 3.0 - WT_CHIP_W / 2.0;
        let agent_x = card_max_x - PAD - BTN_W / 2.0;
        (egui::pos2(chip_x, y), egui::pos2(agent_x, y))
    }

    fn click_at(
        ctx: &egui::Context,
        board: &mut BoardView,
        rect: egui::Rect,
        base: egui::Id,
        pos: egui::Pos2,
    ) {
        run_frame(ctx, board, rect, base, vec![moved(pos)]);
        run_frame(ctx, board, rect, base, vec![moved(pos)]);
        run_frame(ctx, board, rect, base, vec![button(pos, true)]);
        run_frame(ctx, board, rect, base, vec![button(pos, false)]);
    }

    #[test]
    fn picker_worktree_chip_flips_the_choice_and_the_dispatch_act_carries_it() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        let id = store.borrow_mut().add("card", None).unwrap();
        let mut board = BoardView::new(Rc::clone(&store));
        let ctx = egui::Context::default(); // unseeded settings: default is worktree on
        let base = egui::Id::new("wt-chip");
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 400.0));
        let (chip, agent) = picker_points(rect);
        board.picker = Some(id.clone());
        run_frame(&ctx, &mut board, rect, base, vec![moved(chip)]);

        click_at(&ctx, &mut board, rect, base, chip);
        assert_eq!(
            board.worktree_choice,
            Some((id.clone(), false)),
            "one click flips the default (on) to off"
        );
        assert_eq!(
            board.picker.as_deref(),
            Some(id.as_str()),
            "the chip is a picker click, not a dismiss"
        );

        click_at(&ctx, &mut board, rect, base, agent);
        match board.acts.pop() {
            Some(BoardAct::Dispatch {
                id: aid,
                agent,
                worktree,
            }) => {
                assert_eq!(aid, id);
                assert_eq!(agent, *AGENTS.last().unwrap());
                assert!(!worktree, "the act carries the flipped choice");
            }
            _ => panic!("agent click must record a Dispatch act"),
        }
        assert!(board.picker.is_none());
    }

    #[test]
    fn picker_hides_the_chip_and_forces_worktree_on_for_a_card_that_has_one() {
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
        store.borrow_mut().block(&id, "paused").unwrap();
        let mut board = BoardView::new(Rc::clone(&store));
        let ctx = egui::Context::default();
        let base = egui::Id::new("wt-chip-hidden");
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 400.0));
        // Blocked cards live in column 2: shift the column-0 geometry over.
        let col_w = rect.width() / COLUMNS.len() as f32;
        let (chip, agent) = picker_points(rect);
        let chip = chip + egui::vec2(col_w * 2.0, -QUICK_ADD_H); // no quick-add row outside Backlog
        let agent = agent + egui::vec2(col_w * 2.0, -QUICK_ADD_H);
        board.picker = Some(id.clone());
        run_frame(&ctx, &mut board, rect, base, vec![moved(chip)]);

        click_at(&ctx, &mut board, rect, base, chip);
        assert!(
            board.worktree_choice.is_none(),
            "no chip: a card that already has a worktree always restarts in it"
        );
        assert!(
            board.picker.is_none(),
            "with no chip there, the click is 'elsewhere' and dismisses the picker"
        );
        board.picker = Some(id.clone());
        run_frame(&ctx, &mut board, rect, base, vec![moved(agent)]);
        click_at(&ctx, &mut board, rect, base, agent);
        match board.acts.pop() {
            Some(BoardAct::Dispatch { worktree, .. }) => assert!(worktree),
            _ => panic!("agent click must record a Dispatch act"),
        }
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

    fn integration_view(phase: crate::integrate::Phase) -> crate::integrate::IntegrationView {
        crate::integrate::IntegrationView {
            phase,
            commit: "c".into(),
            position: Some(1),
            stage: None,
            reason: Some(crate::integrate::Reason::Conflict),
            detail: Some("conflicts in: f.txt".into()),
            next: Some("resolve".into()),
            hold: None,
            prepared: None,
            checks: None,
            note: None,
            cancel_requested: false,
        }
    }

    #[test]
    fn detail_page_offers_submit_resubmit_and_cancel_by_integration_phase() {
        use crate::integrate::Phase;
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
        let base = egui::Id::new("integration");
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let set = |phase: Option<Phase>| {
            let mut map = std::collections::HashMap::new();
            if let Some(p) = phase {
                map.insert(id.clone(), integration_view(p));
            }
            store.borrow_mut().set_integration(map);
        };
        // In Progress, nothing submitted: Submit only
        set(None);
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert_eq!(board.offered_integration, vec!["submit"]);
        // queued / integrating: Cancel only (the queue owns the tree)
        for p in [Phase::Queued, Phase::Integrating] {
            set(Some(p));
            run_frame(&ctx, &mut board, rect, base, vec![]);
            assert_eq!(board.offered_integration, vec!["cancel"], "{p:?}");
            assert!(!board.offered_discard, "{p:?}: no Discard while owned");
        }
        // handed back: Resubmit and Cancel
        set(Some(Phase::NeedsResolution));
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert_eq!(board.offered_integration, vec!["resubmit", "cancel"]);
        // integrated (bookkeeping pending): nothing to press
        set(Some(Phase::Integrated));
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert!(board.offered_integration.is_empty());
        // Done: no submit either
        set(None);
        store.borrow_mut().done(&id).unwrap();
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert!(board.offered_integration.is_empty());
        assert!(board.offered_discard);
        // the card face paints the substate without recording any act
        set(Some(Phase::Queued));
        board.selected = None;
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert!(board.acts.is_empty());
    }

    /// Add a card, claim it as if dispatched, close it out.
    fn add_done(store: &Rc<RefCell<crate::kanban::CardStore>>, title: &str) -> String {
        let mut s = store.borrow_mut();
        let id = s.add(title, None).unwrap();
        s.claim_for_dispatch(
            &id,
            "t1",
            "claude",
            crate::kanban::run_nonce(),
            crate::kanban::TermState::Missing,
            None,
        )
        .unwrap();
        s.done(&id).unwrap();
        id
    }

    fn cut(store: &Rc<RefCell<crate::kanban::CardStore>>, name: &str) {
        store
            .borrow_mut()
            .cut(name, |_| None, |_| Default::default())
            .unwrap();
    }

    /// The Done header rect at scale 1. Done is the last column and
    /// `column_widths` gives every expanded column an equal share.
    fn done_header(rect: egui::Rect) -> egui::Rect {
        let col_w = rect.width() / COLUMNS.len() as f32;
        egui::Rect::from_min_size(
            egui::pos2(rect.max.x - col_w, rect.min.y),
            egui::vec2(col_w, HEADER_H),
        )
    }

    /// Centre of the Done header's Cut button at scale 1, read out of the
    /// same layout function the board draws with so the two cannot drift.
    fn cut_button_pos(rect: egui::Rect) -> egui::Pos2 {
        done_header_rects(done_header(rect), 1.0, true)
            .2
            .expect("Cut fits at this width")
            .center()
    }

    /// Centre of the Done header's version dropdown at scale 1.
    /// `in_current` mirrors `version.is_none()`: inside a Version no Cut
    /// button is reserved and the dropdown sits further right.
    fn dropdown_pos(rect: egui::Rect, in_current: bool) -> egui::Pos2 {
        done_header_rects(done_header(rect), 1.0, in_current)
            .1
            .expect("the dropdown fits at this width")
            .center()
    }

    /// Centre of the archive banner's Uncut button at scale 1: the banner is
    /// the first row under the Done header, Uncut right-anchored in it.
    fn uncut_button_pos(rect: egui::Rect) -> egui::Pos2 {
        egui::pos2(
            rect.max.x - PAD - BTN_W / 2.0,
            rect.min.y + HEADER_H + QUICK_ADD_H / 2.0,
        )
    }

    #[test]
    fn done_header_rects_floor_the_title_and_never_leave_the_column() {
        for scale in [1.0_f32, 1.5, 0.5] {
            for w in [40.0_f32, 80.0, 100.0, 120.0, 170.0, 200.0, 260.0, 400.0] {
                for want_cut in [true, false] {
                    let header = egui::Rect::from_min_size(
                        egui::pos2(600.0, 0.0),
                        egui::vec2(w * scale, HEADER_H * scale),
                    );
                    let (title, dd, cut) = done_header_rects(header, scale, want_cut);
                    let what = format!("w={w} scale={scale} want_cut={want_cut}");
                    assert_eq!(title.min.x, header.min.x, "{what}");
                    assert!(title.max.x >= title.min.x, "title inverted: {what}");
                    assert!(header.contains_rect(title), "title escapes: {what}");
                    // Right to left: Cut, then the dropdown, then the title.
                    let mut left = header.max.x;
                    for r in [cut, dd].into_iter().flatten() {
                        assert!(header.contains_rect(r), "control escapes: {what}");
                        assert!(r.max.x <= left, "controls overlap: {what}");
                        left = r.min.x;
                    }
                    assert!(title.max.x <= left, "title overlaps a control: {what}");
                    if dd.is_some() || cut.is_some() {
                        assert!(
                            title.width() >= TITLE_MIN_W * scale - 0.01,
                            "title under its floor: {what}"
                        );
                    }
                    assert!(
                        cut.is_none() || want_cut,
                        "Cut reserved in a Version: {what}"
                    );
                }
            }
        }
        // The nominal board: 800px over four columns is a 200px Done header.
        // Both controls fit and the title still clears its floor.
        let (title, dd, cut) = done_header_rects(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(200.0, HEADER_H)),
            1.0,
            true,
        );
        assert!(dd.is_some() && cut.is_some());
        assert!(title.width() >= TITLE_MIN_W, "{}", title.width());
        // Narrow: the dropdown goes before the title gives up a pixel.
        let (title, dd, cut) = done_header_rects(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(120.0, HEADER_H)),
            1.0,
            true,
        );
        assert!(dd.is_none(), "no room for a legible dropdown at 120px");
        assert!(cut.is_some());
        assert!(title.width() >= TITLE_MIN_W);
        // Narrower still: title only, and it never inverts.
        let (title, dd, cut) = done_header_rects(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(60.0, HEADER_H)),
            1.0,
            true,
        );
        assert!(dd.is_none() && cut.is_none());
        assert!(title.width() > 0.0);
    }

    /// Spec §Tests: the Version's last card being removed (an `rm`, a pull)
    /// is the other way a Version can vanish under a selection — the
    /// dropdown snaps back to Current exactly as it does for Uncut.
    #[test]
    fn rm_of_a_versions_last_card_snaps_the_dropdown_back_to_current() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        let id = add_done(&store, "a");
        cut(&store, "v1");
        let mut board = BoardView::new(Rc::clone(&store));
        board.version = Some("v1".into());
        let ctx = egui::Context::default();
        let base = egui::Id::new("rm-snap-back");
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 400.0));
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert!(board.drew_banner);
        assert_eq!(board.done_listed, vec![id.clone()]);

        store.borrow_mut().rm(&id).unwrap();
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert!(board.version.is_none(), "the Version no longer exists");
        assert!(!board.drew_banner);
        assert!(board.done_listed.is_empty());
        assert!(board.acts.is_empty(), "snapping back is not a user act");
    }

    /// Spec §Tests: a collapsed Done inside a Version says which Version it
    /// is showing and how many cards that is — otherwise the rail reads
    /// "Done 0" and looks like lost work.
    #[test]
    fn collapsed_done_rail_names_the_version_and_its_count() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        add_done(&store, "a");
        add_done(&store, "b");
        cut(&store, "v1");
        add_done(&store, "c");
        let mut board = BoardView::new(Rc::clone(&store));
        let ctx = egui::Context::default();
        let base = egui::Id::new("rail-text");
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 400.0));
        board.collapsed[3] = true;
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert_eq!(
            board.rail_text.as_deref(),
            Some("Done\n1"),
            "Current counts only the unshipped card"
        );
        board.version = Some("v1".into());
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert_eq!(board.rail_text.as_deref(), Some("Done\nv1\n2"));
        // Expanding again keeps the Version (the rail never clears it).
        board.collapsed[3] = false;
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert_eq!(board.version.as_deref(), Some("v1"));
        assert!(board.drew_banner);
    }

    /// Nothing Done draws may reach into Blocked, at any pane width or font
    /// zoom: the header controls are clamped by `done_header_rects` and the
    /// banner's Uncut by an explicit intersect, and the version combo has to
    /// fit the slot it was given rather than the unscaled default style.
    #[test]
    fn narrow_or_zoomed_done_controls_stay_inside_the_column() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        add_done(&store, "a");
        cut(&store, "v1");
        let mut board = BoardView::new(Rc::clone(&store));
        let ctx = egui::Context::default();
        let base = egui::Id::new("narrow-done");

        // A pane so narrow that an unclamped Uncut would start two columns
        // to the left of Done.
        let narrow = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(120.0, 400.0));
        board.version = Some("v1".into());
        run_frame(&ctx, &mut board, narrow, base, vec![]);
        let done_col = egui::Rect::from_min_size(
            egui::pos2(
                narrow.max.x - narrow.width() / COLUMNS.len() as f32,
                narrow.min.y,
            ),
            egui::vec2(narrow.width() / COLUMNS.len() as f32, narrow.height()),
        );
        let uncut = board
            .uncut_rect
            .expect("Uncut is still drawn, just clipped");
        assert!(
            done_col.contains_rect(uncut),
            "Uncut {uncut:?} escapes Done {done_col:?}"
        );

        // Zoomed out: the combo must shrink with the header, not keep the
        // style's unscaled height and spill into the first card row.
        crate::terminal::set_font_size(&ctx, crate::config::DEFAULT_FONT_SIZE * 0.5);
        board.version = None;
        let wide = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 400.0));
        run_frame(&ctx, &mut board, wide, base, vec![]);
        run_frame(&ctx, &mut board, wide, base, vec![]);
        let header = egui::Rect::from_min_size(
            egui::pos2(wide.max.x - wide.width() / COLUMNS.len() as f32, wide.min.y),
            egui::vec2(wide.width() / COLUMNS.len() as f32, HEADER_H * 0.5),
        );
        let (_, dd, _) = done_header_rects(header, 0.5, true);
        let slot = dd.expect("the dropdown fits");
        let btn = board.dropdown_btn.expect("the dropdown was drawn");
        assert!(
            btn.height() <= slot.height() + 0.5,
            "combo {} tall in a {} slot",
            btn.height(),
            slot.height()
        );
        assert!(
            header.contains_rect(btn.intersect(header)) && btn.max.y <= header.max.y + 0.5,
            "combo {btn:?} overflows the header {header:?}"
        );
    }

    #[test]
    fn long_version_name_stays_inside_the_dropdown_slot() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        add_done(&store, "a");
        let version = "release-with-a-very-long-name-that-must-not-expand-the-header";
        cut(&store, version);
        let mut board = BoardView::new(Rc::clone(&store));
        board.version = Some(version.into());
        let ctx = egui::Context::default();
        let base = egui::Id::new("long-version");
        for width in [600.0, 800.0, 1200.0] {
            let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 400.0));
            run_frame(&ctx, &mut board, rect, base, vec![]);
            run_frame(&ctx, &mut board, rect, base, vec![]);
            let header = egui::Rect::from_min_size(
                egui::pos2(width * 0.75, 0.0),
                egui::vec2(width * 0.25, HEADER_H),
            );
            let (_, slot, _) = done_header_rects(header, 1.0, false);
            let slot = slot.expect("dropdown fits");
            let button = board.dropdown_btn.expect("dropdown drawn");
            assert!(
                slot.expand(0.5).contains_rect(button),
                "{button:?} escapes {slot:?}"
            );
        }
    }

    #[test]
    fn uncut_from_the_archive_banner_records_the_act_without_collapsing_done() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        add_done(&store, "a");
        cut(&store, "v1");
        let mut board = BoardView::new(Rc::clone(&store));
        board.version = Some("v1".into());
        let ctx = egui::Context::default();
        let base = egui::Id::new("uncut-click");
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 400.0));
        click_at(&ctx, &mut board, rect, base, uncut_button_pos(rect));
        assert!(board.drew_banner);
        assert!(
            matches!(board.acts.as_slice(), [BoardAct::Uncut(n)] if n == "v1"),
            "{} acts recorded",
            board.acts.len()
        );
        assert_eq!(board.collapsed, [false; 4]);
        assert_eq!(
            board.version.as_deref(),
            Some("v1"),
            "the view waits for wm"
        );
    }

    #[test]
    fn picking_a_version_in_the_dropdown_switches_the_done_column() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        let a = add_done(&store, "a");
        cut(&store, "v1");
        let b = add_done(&store, "b");
        let mut board = BoardView::new(Rc::clone(&store));
        let ctx = egui::Context::default();
        let base = egui::Id::new("dd-pick");
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 400.0));
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert_eq!(board.done_listed, vec![b], "Current first");
        assert!(board.dropdown_rows.is_empty(), "the popup starts shut");
        // Open the popup, then click the v1 row exactly where the board drew
        // it last frame rather than guessing at egui's popup placement.
        click_at(&ctx, &mut board, rect, base, dropdown_pos(rect, true));
        run_frame(&ctx, &mut board, rect, base, vec![]);
        let row = board
            .dropdown_rows
            .iter()
            .find(|(n, _)| n == "v1")
            .expect("the open popup lists the Version")
            .1;
        click_at(&ctx, &mut board, rect, base, row.center());
        assert_eq!(board.version.as_deref(), Some("v1"));
        // The pick lands after that frame's column filter has already run, so
        // the Version's cards appear on the next frame.
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert_eq!(board.done_listed, vec![a], "Done now shows the Version");
        assert!(board.drew_banner);
        assert!(!board.offered_cut, "no Cut inside a Version");
        assert!(board.acts.is_empty(), "switching views is not an act");
    }

    #[test]
    fn done_header_offers_cut_only_in_current_with_ungrouped_done() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        let mut board = BoardView::new(Rc::clone(&store));
        let ctx = egui::Context::default();
        let base = egui::Id::new("cut-gating");
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 400.0));
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert!(!board.offered_cut, "empty Done: Cut disabled");
        assert!(!board.drew_banner);

        let a = add_done(&store, "a");
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert!(board.offered_cut);
        assert_eq!(board.done_listed, vec![a.clone()]);

        cut(&store, "v1");
        let b = add_done(&store, "b");
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert_eq!(
            board.done_listed,
            vec![b.clone()],
            "Current hides shipped cards"
        );

        board.version = Some("v1".into());
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert!(!board.offered_cut, "no Cut inside a Version");
        assert!(board.drew_banner);
        assert_eq!(board.done_listed, vec![a.clone()]);

        // Collapsing Done keeps the selection.
        board.collapsed[3] = true;
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert_eq!(board.version.as_deref(), Some("v1"));
        board.collapsed[3] = false;

        // The Version vanishing (uncut) snaps the dropdown back to Current.
        store.borrow_mut().uncut("v1").unwrap();
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert!(board.version.is_none());
        assert!(!board.drew_banner);
        let mut listed = board.done_listed.clone();
        listed.sort();
        let mut want = vec![a, b];
        want.sort();
        assert_eq!(listed, want);
    }

    #[test]
    fn cut_button_opens_the_field_prefill_lands_and_enter_records_the_act() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        add_done(&store, "a");
        let mut board = BoardView::new(Rc::clone(&store));
        let ctx = egui::Context::default();
        let base = egui::Id::new("cut-field");
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 400.0));
        click_at(&ctx, &mut board, rect, base, cut_button_pos(rect));
        assert_eq!(board.cut_field.as_deref(), Some(""));
        assert!(matches!(board.acts.pop(), Some(BoardAct::CutPrefill)));
        assert_eq!(
            board.collapsed, [false; 4],
            "the button must not collapse Done"
        );
        board.prefill_cut("v0.5.0");
        assert_eq!(board.cut_field.as_deref(), Some("v0.5.0"));
        // A late prefill never overwrites text.
        board.prefill_cut("v9.9.9");
        assert_eq!(board.cut_field.as_deref(), Some("v0.5.0"));
        let enter = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        };
        run_frame(&ctx, &mut board, rect, base, vec![enter]);
        assert!(
            matches!(board.acts.as_slice(), [BoardAct::Cut(n)] if n == "v0.5.0"),
            "{:?}",
            board.acts.len()
        );
        assert!(board.cut_field.is_none(), "Enter closes the field");
    }

    #[test]
    fn dropdown_click_does_not_collapse_done() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        add_done(&store, "a");
        cut(&store, "v1");
        let mut board = BoardView::new(Rc::clone(&store));
        let ctx = egui::Context::default();
        let base = egui::Id::new("dd-click");
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 400.0));
        click_at(&ctx, &mut board, rect, base, dropdown_pos(rect, true));
        assert_eq!(board.collapsed, [false; 4]);
        assert!(board.acts.is_empty());
    }

    #[test]
    fn detail_page_shows_version_and_commits_for_a_shipped_card() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        let id = add_done(&store, "a");
        let id2 = id.clone();
        store
            .borrow_mut()
            .cut(
                "v1",
                |_| None,
                move |_| {
                    let mut m: std::collections::HashMap<String, Vec<String>> = Default::default();
                    m.insert(id2.clone(), vec!["abc1234".into()]);
                    m
                },
            )
            .unwrap();
        let mut board = BoardView::new(Rc::clone(&store));
        board.selected = Some(id);
        let ctx = egui::Context::default();
        let base = egui::Id::new("detail-shipped");
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert_eq!(board.detail_commits, vec!["abc1234".to_string()]);
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

    /// The strip exists only when a foreman worktree does; clicking it opens
    /// the page; Open card opens the detail page; and once that page is
    /// left (Back clears `selected`) the Worktrees page is what shows.
    #[test]
    fn worktree_strip_opens_the_page_and_a_card_opened_from_it_returns_to_it() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        store.borrow_mut().add("plain", None).unwrap();
        let mut board = BoardView::new(Rc::clone(&store));
        let ctx = egui::Context::default();
        let base = egui::Id::new("wt-strip");
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert!(board.strip_text.is_none(), "no worktree: no strip");

        let id = store.borrow_mut().add("with tree", None).unwrap();
        let wt = crate::kanban::Worktree {
            path: format!("H:/repo/.foreman/worktrees/{id}"),
            branch: format!("card/{id}"),
            base: "main".into(),
        };
        store
            .borrow_mut()
            .claim_for_dispatch(
                &id,
                "t1",
                "claude",
                crate::kanban::run_nonce(),
                crate::kanban::TermState::Running,
                Some(wt),
            )
            .unwrap();
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert_eq!(board.strip_text.as_deref(), Some("1 worktree"));
        assert!(!board.worktrees_open);

        let strip_pos = egui::pos2(rect.center().x, rect.max.y - STRIP_H / 2.0);
        click_at(&ctx, &mut board, rect, base, strip_pos);
        assert!(board.worktrees_open, "clicking the strip opens the page");
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert_eq!(board.wt_rows_drawn, vec![id.clone()]);
        assert!(board.strip_text.is_none(), "the page draws no strip");

        let open = board
            .wt_btn_rects
            .iter()
            .find(|(n, l, _)| n == &id && *l == "Open card")
            .map(|(_, _, r)| r.center())
            .expect("Open card button drawn");
        click_at(&ctx, &mut board, rect, base, open);
        assert_eq!(board.selected.as_deref(), Some(id.as_str()));
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert!(
            board.wt_rows_drawn.is_empty(),
            "the detail page wins over the page"
        );
        // What Back does: clear `selected`; the page is still open beneath.
        board.selected = None;
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert!(board.worktrees_open);
        assert_eq!(board.wt_rows_drawn, vec![id.clone()]);
        assert!(board.acts.is_empty());
    }

    /// A stray (no card) shows on the strip and the page, and its two
    /// buttons record the two acts with the tree attached.
    #[test]
    fn stray_rows_offer_remove_and_discard_and_record_the_acts() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_at(tmp.path());
        let wt = crate::kanban::Worktree {
            path: "H:/repo/.foreman/worktrees/qq1".into(),
            branch: "card/qq1".into(),
            base: "main".into(),
        };
        store.borrow_mut().set_worktree_statuses(
            Default::default(),
            vec![crate::kanban::StrayWorktree {
                wt: wt.clone(),
                status: Some(crate::kanban::WorktreeStatus {
                    dirty: true,
                    ..Default::default()
                }),
            }],
        );
        let mut board = BoardView::new(Rc::clone(&store));
        let ctx = egui::Context::default();
        let base = egui::Id::new("stray");
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert_eq!(
            board.strip_text.as_deref(),
            Some("1 worktree · 1 dirty · 1 no card")
        );
        board.worktrees_open = true;
        run_frame(&ctx, &mut board, rect, base, vec![]);
        assert_eq!(board.wt_rows_drawn, vec!["qq1".to_string()]);
        let btn = |board: &BoardView, label: &str| {
            board
                .wt_btn_rects
                .iter()
                .find(|(n, l, _)| n == "qq1" && *l == label)
                .map(|(_, _, r)| r.center())
                .unwrap_or_else(|| panic!("{label} drawn"))
        };
        assert!(
            !board.wt_btn_rects.iter().any(|(_, l, _)| *l == "Open card"),
            "a stray has no card to open"
        );
        let remove = btn(&board, "Remove");
        click_at(&ctx, &mut board, rect, base, remove);
        match board.acts.pop() {
            Some(BoardAct::RemoveStray(got)) => assert_eq!(got, wt),
            _ => panic!("Remove records RemoveStray"),
        }
        let discard = btn(&board, "Discard");
        click_at(&ctx, &mut board, rect, base, discard);
        match board.acts.pop() {
            Some(BoardAct::DiscardStray(got)) => assert_eq!(got, wt),
            _ => panic!("Discard records DiscardStray"),
        }
        assert!(board.acts.is_empty());
    }
}
