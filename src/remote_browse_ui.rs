//! The folder list under a Directory field: one widget for the directory
//! prompt and the Remotes pane edit form. The field text stays the source
//! of truth ([`crate::remote_browse`] reads and writes it); this only draws
//! the list, moves its selection and starts listings off the frame path.
//!
//! The area has a fixed height whatever it shows, so a listing arriving
//! never moves what is below it. The field and the list never both look
//! focused: while the list has the keyboard its frame and selected row are
//! drawn focused and the caller drops the field's focus.

use std::time::{Duration, Instant};

use eframe::egui;

use crate::remote::RemoteShell;
use crate::remote_browse::{self as model, Entry, Key, Listed, Shared, State, Style};

/// How long the directory part of the field must stay put while typing
/// before a listing starts. Navigation lists at once.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// The list frame's inner margin.
const PAD: f32 = 4.0;
/// Room left of each row's name for the selected-row marker.
const MARK_W: f32 = 12.0;

/// Who reads the keyboard: the caller's text field, or the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Field,
    List,
}

/// The list's keys, decoupled from egui so callers' models can drive it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nav {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Backspace,
    Tab,
}

/// One row of the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    /// `..`: one level up.
    Up,
    /// The listing's entry at this index.
    Entry(usize),
}

/// What the list shows for the current field text.
struct View {
    state: std::sync::Arc<State>,
    rows: Vec<Row>,
    /// Where `..` goes, and the folder it leaves.
    up: Option<(String, String)>,
}

impl View {
    fn listed(&self) -> Option<&Listed> {
        match &*self.state {
            State::Listed(l) => Some(l),
            _ => None,
        }
    }

    fn first_folder(&self) -> usize {
        self.rows
            .iter()
            .position(|r| matches!(r, Row::Entry(_)))
            .unwrap_or(0)
    }
}

/// Where the last frame put things, for the layout tests.
#[derive(Clone, Debug)]
pub struct Laid {
    /// The whole reserved area.
    pub area: egui::Rect,
    pub crumb: egui::Rect,
    pub rows: Vec<egui::Rect>,
    /// The Loading / Failed / empty / cut words, while shown.
    pub message: Option<egui::Rect>,
}

impl Default for Laid {
    fn default() -> Self {
        Self {
            area: egui::Rect::NOTHING,
            crumb: egui::Rect::NOTHING,
            rows: Vec::new(),
            message: None,
        }
    }
}

#[derive(Debug)]
pub struct Browser {
    /// Listings for the life of this widget (the prompt, or the edit form).
    cache: Shared,
    focus: Focus,
    /// The selected row while the list has the keyboard.
    sel: usize,
    /// The text just changed under the list: pick a row again once the new
    /// listing is there, `want` if it names one.
    reseat: bool,
    want: Option<String>,
    /// The directory last asked for, and when it changed (the debounce).
    last_dir: Option<String>,
    changed_at: Instant,
    /// The next ask skips the debounce: the user navigated, not typed.
    immediate: bool,
    scroll_to: bool,
    caret_to_end: bool,
    source: Option<(String, RemoteShell)>,
    pub laid: Laid,
}

impl Default for Browser {
    fn default() -> Self {
        Self::new()
    }
}

impl Browser {
    pub fn new() -> Self {
        Self {
            cache: Shared::default(),
            focus: Focus::Field,
            sel: 0,
            reseat: false,
            want: None,
            last_dir: None,
            changed_at: Instant::now(),
            immediate: true,
            scroll_to: false,
            caret_to_end: false,
            source: None,
            laid: Laid::default(),
        }
    }

    /// The Remote to list. A change hands the keyboard back to the field.
    pub fn set_source(&mut self, target: &str, shell: RemoteShell) {
        let target = target.trim();
        if self
            .source
            .as_ref()
            .is_none_or(|(t, s)| t != target || *s != shell)
        {
            self.source = Some((target.to_string(), shell));
            self.focus = Focus::Field;
            self.immediate = true;
        }
    }

    pub fn list_focused(&self) -> bool {
        self.focus == Focus::List
    }

    /// Hand the keyboard back to the field, caret at the end.
    pub fn focus_field(&mut self) {
        if self.focus == Focus::List {
            self.caret_to_end = true;
        }
        self.focus = Focus::Field;
    }

    /// Whether Down from the field would enter the list.
    pub fn has_rows(&self, text: &str) -> bool {
        self.view(text).is_some_and(|v| !v.rows.is_empty())
    }

    fn style(&self) -> Style {
        Style::of(self.source.as_ref().map_or(RemoteShell::Posix, |s| s.1))
    }

    fn key_for(&self, text: &str) -> Option<Key> {
        let (target, shell) = self.source.as_ref()?;
        let dir = model::split(text, self.style()).dir;
        Some(Key::new(target, *shell, dir))
    }

    fn view(&self, text: &str) -> Option<View> {
        let key = self.key_for(text)?;
        let state = self
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .peek(&key)
            .unwrap_or_else(|| std::sync::Arc::new(State::Loading));
        Some(self.view_of(text, state))
    }

    fn view_of(&self, text: &str, state: std::sync::Arc<State>) -> View {
        let style = self.style();
        let filter = model::split(text, style).filter.to_lowercase();
        let mut rows = Vec::new();
        let mut up = None;
        if let State::Listed(l) = &*state {
            up = model::up(text, style, Some(&l.resolved));
            if up.is_some() {
                rows.push(Row::Up);
            }
            rows.extend(
                l.entries
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.matches(&filter))
                    .map(|(i, _)| Row::Entry(i)),
            );
        }
        View { state, rows, up }
    }

    /// After the text changed under the list: select `want` once listed,
    /// else the first folder; and keep the selection on a row that exists.
    fn settle(&mut self, view: &View) {
        if self.reseat && view.listed().is_some() {
            self.reseat = false;
            let want = self.want.take();
            let listed = view.listed().expect("checked");
            self.sel = want
                .and_then(|w| {
                    view.rows.iter().position(|r| match r {
                        Row::Entry(i) => listed.entries[*i].name == w,
                        Row::Up => false,
                    })
                })
                .unwrap_or_else(|| view.first_folder());
            self.scroll_to = true;
        }
        self.sel = self.sel.min(view.rows.len().saturating_sub(1));
    }

    /// Write `next` into the field after a navigation: list it at once, and
    /// pick a row again when it is listed.
    fn navigate(&mut self, text: &mut String, next: String, want: Option<String>) {
        *text = next;
        self.reseat = true;
        self.want = want;
        self.immediate = true;
        self.caret_to_end = true;
    }

    fn open_row(&mut self, text: &mut String, view: &View, row: Row) {
        match row {
            Row::Up => self.go_up(text, view),
            Row::Entry(i) => {
                if let Some(e) = view.listed().and_then(|l| l.entries.get(i)) {
                    let next = model::descend(text, self.style(), e);
                    self.navigate(text, next, None);
                }
            }
        }
    }

    fn go_up(&mut self, text: &mut String, view: &View) {
        let resolved = view.listed().map(|l| l.resolved.as_str());
        if let Some((parent, child)) = view
            .up
            .clone()
            .or_else(|| model::up(text, self.style(), resolved))
        {
            self.navigate(text, parent, Some(child));
        }
    }

    /// Apply one key. Returns whether the list took it; the caller handles
    /// the rest (in the field, everything but Down when there are rows).
    pub fn key(&mut self, k: Nav, text: &mut String) -> bool {
        let Some(view) = self.view(text) else {
            return false;
        };
        self.settle(&view);
        match self.focus {
            Focus::Field => {
                if k == Nav::Down && !view.rows.is_empty() {
                    self.focus = Focus::List;
                    self.sel = view.first_folder();
                    self.scroll_to = true;
                    return true;
                }
                false
            }
            Focus::List => {
                let n = view.rows.len();
                match k {
                    Nav::Tab => self.focus_field(),
                    Nav::Up if self.sel == 0 || n == 0 => self.focus_field(),
                    Nav::Up => {
                        self.sel -= 1;
                        self.scroll_to = true;
                    }
                    Nav::Down => {
                        self.sel = (self.sel + 1).min(n.saturating_sub(1));
                        self.scroll_to = true;
                    }
                    Nav::Enter | Nav::Right => {
                        if let Some(&row) = view.rows.get(self.sel) {
                            self.open_row(text, &view, row);
                        }
                    }
                    Nav::Left => self.go_up(text, &view),
                    Nav::Backspace => {
                        if model::split(text, self.style()).filter.is_empty() {
                            self.go_up(text, &view);
                        } else {
                            text.pop();
                            self.reseat = true;
                        }
                    }
                }
                // A navigation already listed (cached): select in it now.
                if let Some(v) = self.view(text) {
                    self.settle(&v);
                }
                true
            }
        }
    }

    /// Put the field's caret at the end after the list rewrote its text.
    /// Call before the field draws.
    pub fn pin_caret(&mut self, ctx: &egui::Context, id: egui::Id, text: &str) {
        if std::mem::take(&mut self.caret_to_end) {
            let end = egui::text::CCursor::new(text.chars().count());
            let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(end)));
            egui::TextEdit::store_state(ctx, id, state);
        }
    }

    /// The area's full height for `body_h` of rows.
    pub fn height(ui: &egui::Ui, body_h: f32) -> f32 {
        crumb_h(ui) + body_h + 2.0 * PAD
    }

    /// Test hook: a listing, as if it had run.
    #[cfg(test)]
    pub fn put(&self, target: &str, shell: RemoteShell, dir: &str, state: State) {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .finish(Key::new(target, shell, dir), state);
    }

    /// Draw the list for `text` in a reserved area `body_h` tall (plus the
    /// breadcrumb). Lists only while `active`. Returns whether a click
    /// rewrote `text`; a click also hands the keyboard to the field.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        text: &mut String,
        body_h: f32,
        active: bool,
    ) -> bool {
        let Some(key) = self.key_for(text) else {
            idle(ui, body_h, "No Remote to list", &mut self.laid);
            return false;
        };
        let ctx = ui.ctx().clone();
        let now = Instant::now();
        if self.last_dir.as_deref() != Some(key.dir.as_str()) {
            self.last_dir = Some(key.dir.clone());
            self.changed_at = now;
        }
        let waited = now.saturating_duration_since(self.changed_at);
        let may_start = active && (self.immediate || waited >= DEBOUNCE);
        if may_start {
            self.immediate = false;
        } else if active {
            ctx.request_repaint_after(DEBOUNCE.saturating_sub(waited));
        }
        let state = model::list(&ctx, &self.cache, &key, may_start);
        let view = self.view_of(text, state);
        self.settle(&view);

        let th = crate::theme::live(&ctx);
        let style = self.style();
        // Like the form's fields: nothing looks focused while another Win
        // reads the keyboard.
        let focused = self.focus == Focus::List && active;
        let mut clicked = None;
        let scroll_to = std::mem::take(&mut self.scroll_to);
        let sel = self.sel;
        let dir = model::split(text, style).dir;
        let crumb = match style {
            Style::Windows if model::is_home(dir) => "~ and drives".to_string(),
            _ => model::breadcrumb(dir),
        };
        let filter = model::split(text, style).filter.to_string();
        let laid = &mut self.laid;
        laid.rows.clear();
        laid.message = None;
        area(ui, body_h, focused, laid, &crumb, |ui, laid| {
            let row_h = ui.spacing().interact_size.y;
            let listed = view.listed();
            for (i, row) in view.rows.iter().enumerate() {
                let (rect, resp) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), row_h),
                    egui::Sense::click(),
                );
                laid.rows.push(rect);
                let selected = focused && i == sel;
                let p = ui.painter();
                if selected {
                    p.rect_filled(rect, egui::CornerRadius::same(3), th.sel_bg);
                    p.text(
                        egui::pos2(rect.min.x + 2.0, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        "›",
                        name_font(ui),
                        th.border_focus,
                    );
                    if scroll_to {
                        ui.scroll_to_rect(rect, None);
                    }
                } else if resp.hovered() {
                    p.rect_stroke(
                        rect,
                        egui::CornerRadius::same(3),
                        egui::Stroke::new(1.0, th.border),
                        egui::StrokeKind::Inside,
                    );
                }
                let (name, note) = match *row {
                    Row::Up => ("..".to_string(), Some("up one level")),
                    Row::Entry(k) => {
                        let e: &Entry = &listed.expect("rows come from a listing").entries[k];
                        (e.name.clone(), e.drive.then_some("drive"))
                    }
                };
                let left = rect.min.x + MARK_W;
                let room = (rect.max.x - left - PAD).max(0.0);
                let note_g = note.map(|n| {
                    ui.painter().layout_no_wrap(
                        format!("  {n}"),
                        egui::FontId::proportional(11.5),
                        th.dim,
                    )
                });
                let note_w = note_g.as_ref().map_or(0.0, |g| g.size().x);
                let mut job =
                    egui::text::LayoutJob::simple_singleline(name.clone(), name_font(ui), th.text);
                job.wrap = egui::text::TextWrapping::truncate_at_width((room - note_w).max(0.0));
                let g = ui.painter().layout_job(job);
                let at = egui::pos2(left, rect.center().y - g.size().y / 2.0);
                let name_w = g.size().x;
                let elided = g.elided;
                ui.painter().galley(at, g, th.text);
                if let Some(ng) = note_g {
                    let at = egui::pos2(left + name_w, rect.center().y - ng.size().y / 2.0);
                    ui.painter().galley(at, ng, th.dim);
                }
                let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                let resp = if elided {
                    resp.on_hover_text(name)
                } else {
                    resp
                };
                if resp.clicked() {
                    clicked = Some(*row);
                }
            }
            let words: Option<(String, egui::Color32)> = match &*view.state {
                State::Loading => Some(("Listing folders…".into(), th.dim)),
                State::Failed(why) => Some((why.clone(), th.danger)),
                State::Listed(l) if l.entries.is_empty() => {
                    Some(("No folders here".into(), th.dim))
                }
                State::Listed(_) if !view.rows.iter().any(|r| matches!(r, Row::Entry(_))) => {
                    Some((format!("No folder here starts with “{filter}”"), th.dim))
                }
                State::Listed(l) if l.truncated => Some((
                    format!(
                        "Only the first {} folders are listed: type to narrow",
                        model::CAP
                    ),
                    th.dim,
                )),
                State::Listed(_) => None,
            };
            if let Some((words, color)) = words {
                let r = ui.horizontal_wrapped(|ui| {
                    ui.add_space(MARK_W);
                    ui.add(
                        egui::Label::new(egui::RichText::new(words).color(color).size(11.5)).wrap(),
                    );
                });
                laid.message = Some(r.response.rect);
            }
        });
        match clicked {
            Some(row) => {
                self.open_row(text, &view, row);
                self.focus = Focus::Field;
                true
            }
            None => false,
        }
    }
}

/// The same reserved area with only a sentence in it: no Remote to list,
/// or no Directory focused.
pub fn idle(ui: &mut egui::Ui, body_h: f32, words: &str, laid: &mut Laid) {
    let th = crate::theme::live(ui.ctx());
    laid.rows.clear();
    laid.message = None;
    area(ui, body_h, false, laid, "", |ui, laid| {
        let r = ui.horizontal_wrapped(|ui| {
            ui.add_space(MARK_W);
            ui.add(egui::Label::new(egui::RichText::new(words).color(th.dim).size(11.5)).wrap());
        });
        laid.message = Some(r.response.rect);
    });
}

fn name_font(ui: &egui::Ui) -> egui::FontId {
    egui::TextStyle::Monospace.resolve(ui.style())
}

fn crumb_h(ui: &egui::Ui) -> f32 {
    ui.spacing().interact_size.y
}

/// The fixed frame: a breadcrumb line, then `body_h` of scrolling rows.
/// Allocated whole before anything is drawn in it, so its height is the
/// same in every state.
fn area(
    ui: &mut egui::Ui,
    body_h: f32,
    focused: bool,
    laid: &mut Laid,
    crumb: &str,
    body: impl FnOnce(&mut egui::Ui, &mut Laid),
) {
    let th = crate::theme::live(ui.ctx());
    let ch = crumb_h(ui);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ch + body_h + 2.0 * PAD),
        egui::Sense::hover(),
    );
    laid.area = rect;
    let stroke = if focused { th.border_focus } else { th.border };
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(4), th.win_bg);
    ui.painter().rect_stroke(
        rect,
        egui::CornerRadius::same(4),
        egui::Stroke::new(1.0, stroke),
        egui::StrokeKind::Inside,
    );
    let inner = rect.shrink(PAD);
    let crumb_rect = egui::Rect::from_min_size(inner.min, egui::vec2(inner.width(), ch));
    laid.crumb = crumb_rect;
    if !crumb.is_empty() {
        let lead = "Folders in ";
        let small = egui::FontId::proportional(11.5);
        let lead_g = ui.painter().layout_no_wrap(lead.into(), small, th.dim);
        let lead_w = lead_g.size().x;
        let x = crumb_rect.min.x + MARK_W;
        ui.painter().galley(
            egui::pos2(x, crumb_rect.center().y - lead_g.size().y / 2.0),
            lead_g,
            th.dim,
        );
        let font = name_font(ui);
        let room = (crumb_rect.max.x - x - lead_w).max(0.0);
        let (shown, cut) = crate::remotes_pane::elide_front(ui, crumb, &font, room);
        let g = ui.painter().layout_no_wrap(shown, font, th.text);
        let at = egui::pos2(x + lead_w, crumb_rect.center().y - g.size().y / 2.0);
        let r = egui::Rect::from_min_size(at, g.size());
        ui.painter().galley(at, g, th.text);
        if cut {
            ui.interact(r, ui.id().with("crumb"), egui::Sense::hover())
                .on_hover_text(crumb);
        }
    }
    let body_rect = egui::Rect::from_min_max(egui::pos2(inner.min.x, crumb_rect.max.y), inner.max);
    // Top-down whatever the caller's layout: the prompt draws this inside a
    // horizontal row (for the field's indent).
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(body_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.set_clip_rect(body_rect.intersect(ui.clip_rect()));
    egui::ScrollArea::vertical()
        .id_salt("remote_browse_rows")
        .auto_shrink([false, false])
        .max_height(body_rect.height())
        .show(&mut child, |ui| body(ui, laid));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_browse::Listed;

    fn listing(resolved: &str, names: &[&str]) -> State {
        State::Listed(Listed {
            resolved: resolved.into(),
            entries: names.iter().map(|n| Entry::dir(n)).collect(),
            truncated: false,
        })
    }

    fn browser() -> Browser {
        let mut b = Browser::new();
        b.set_source("desk", RemoteShell::Posix);
        b.put(
            "desk",
            RemoteShell::Posix,
            "",
            listing("/home/andy", &["apps", "src"]),
        );
        b.put(
            "desk",
            RemoteShell::Posix,
            "~/src/",
            listing("/home/andy/src", &["foreman", "notes"]),
        );
        b.put(
            "desk",
            RemoteShell::Posix,
            "/home/",
            listing("/home", &["andy", "bob"]),
        );
        b
    }

    #[test]
    fn down_enters_the_list_on_the_first_folder_and_up_or_tab_leaves() {
        let mut b = browser();
        let mut text = String::new();
        assert!(b.has_rows(&text));
        assert!(!b.key(Nav::Up, &mut text), "the field keeps Up");
        assert!(b.key(Nav::Down, &mut text));
        assert!(b.list_focused());
        // Row 0 is "..": the first folder is selected instead.
        assert_eq!(b.sel, 1);
        b.key(Nav::Down, &mut text);
        b.key(Nav::Down, &mut text);
        assert_eq!(b.sel, 2, "clamps on the last row");
        b.key(Nav::Tab, &mut text);
        assert!(!b.list_focused());
        b.key(Nav::Down, &mut text);
        b.key(Nav::Up, &mut text);
        b.key(Nav::Up, &mut text);
        assert!(!b.list_focused(), "Up from the first row returns");
        assert_eq!(text, "", "moving never writes");
    }

    #[test]
    fn right_or_enter_descends_and_left_or_backspace_goes_up() {
        let mut b = browser();
        let mut text = String::new();
        b.key(Nav::Down, &mut text); // "apps"
        b.key(Nav::Down, &mut text); // "src"
        assert!(b.key(Nav::Right, &mut text));
        assert_eq!(text, "~/src/");
        assert!(b.list_focused(), "the list keeps the keyboard");
        assert_eq!(b.sel, 1, "first folder of the new level");
        b.key(Nav::Enter, &mut text);
        assert_eq!(text, "~/src/foreman/");
        b.key(Nav::Left, &mut text);
        assert_eq!(text, "~/src/");
        assert_eq!(b.sel, 1, "back on the folder left: foreman");
        b.key(Nav::Down, &mut text);
        b.key(Nav::Backspace, &mut text);
        assert_eq!(text, "~/");
        let v = b.view(&text).unwrap();
        assert_eq!(v.rows[b.sel], Row::Entry(1), "src is selected again");
        // Above home: where the listing says home is.
        b.key(Nav::Left, &mut text);
        assert_eq!(text, "/home/");
        assert_eq!(b.sel, 1, "andy");
        // Enter on ".." goes up too.
        b.sel = 0;
        b.key(Nav::Enter, &mut text);
        assert_eq!(text, "/");
    }

    #[test]
    fn backspace_trims_a_filter_before_it_goes_up() {
        let mut b = browser();
        let mut text = "~/src/no".to_string();
        assert!(b.key(Nav::Down, &mut text));
        let v = b.view(&text).unwrap();
        assert_eq!(v.rows, [Row::Up, Row::Entry(1)], "filtered to notes");
        b.key(Nav::Backspace, &mut text);
        assert_eq!(text, "~/src/n");
        b.key(Nav::Backspace, &mut text);
        b.key(Nav::Backspace, &mut text);
        assert_eq!(text, "~/");
    }

    #[test]
    fn nothing_listed_leaves_every_key_to_the_field() {
        let mut b = browser();
        let mut text = "/nope/".to_string();
        assert!(!b.has_rows(&text), "Loading");
        assert!(!b.key(Nav::Down, &mut text));
        b.put(
            "desk",
            RemoteShell::Posix,
            "/nope/",
            State::Failed("No such".into()),
        );
        assert!(!b.key(Nav::Down, &mut text));
        assert!(!b.list_focused());
        // No Remote at all.
        let mut b = Browser::new();
        assert!(!b.key(Nav::Down, &mut text));
    }

    #[test]
    fn a_new_remote_hands_the_keyboard_back() {
        let mut b = browser();
        let mut text = String::new();
        b.key(Nav::Down, &mut text);
        b.set_source("desk", RemoteShell::Posix);
        assert!(b.list_focused(), "same Remote: unchanged");
        b.set_source("pi", RemoteShell::Posix);
        assert!(!b.list_focused());
    }
}
