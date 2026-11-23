//! The folder list under a Directory field: one widget for the directory
//! prompt and the Remotes pane edit form. The field text stays the source
//! of truth ([`crate::remote_browse`] reads and writes it); this only draws
//! the list, moves its selection and starts listings off the frame path.
//!
//! The area has a fixed height whatever it shows, so a listing arriving
//! never moves what is below it. The field and the list never both look
//! focused: while the list has the keyboard its frame and selected row are
//! drawn focused and the caller drops the field's focus. Dropping the
//! widget kills its listings' ssh.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use eframe::egui;

use crate::remote::RemoteShell;
use crate::remote_browse::{self as model, Key, Listed, Shared, State, Style};

/// How long the directory part of the field must stay put while typing
/// before a listing starts. Navigation lists at once.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// How long a listing runs before the list says it is still trying.
const STILL_TRYING: Duration = Duration::from_secs(3);
/// One row: the launcher menu's item height, so the lists read alike.
pub const ROW_H: f32 = 22.0;
/// The list frame's inner margin.
const PAD: f32 = 4.0;
/// Room left of each row's name for the selected-row marker.
const MARK_W: f32 = 12.0;
const SMALL: f32 = 11.5;

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

/// What Down from the field does, for the field's key hint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Enters {
    /// Nothing: Down is the caller's.
    None,
    /// Enter the folders (now, or as soon as they are listed).
    Folders,
    /// Ask the Remote again, and enter the list.
    Retry,
}

/// What the list shows while it has the keyboard, for its key hint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shows {
    Rows,
    /// Listing: the first folder is selected when it arrives.
    Waiting,
    /// The Remote did not answer: one retry row.
    Retry,
}

/// One row of the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    /// `..`: one level up.
    Up,
    /// The listing's entry at this index.
    Entry(usize),
    /// Ask an unreachable Remote again.
    Retry,
}

/// What the list shows for one field text and one cached state. Built once
/// per (text, state) and reused by the frame's every reader.
#[derive(Debug)]
struct View {
    state: Arc<State>,
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

    /// Whether the keyboard may stop on `row`: not on a name that cannot
    /// be chosen.
    fn selectable(&self, row: Row) -> bool {
        match row {
            Row::Up | Row::Retry => true,
            Row::Entry(i) => self
                .listed()
                .and_then(|l| l.entries.get(i))
                .is_some_and(|e| e.blocked.is_none()),
        }
    }

    /// The row Down from the field lands on: the first folder that can be
    /// chosen, else the first row that can.
    fn first(&self) -> usize {
        let ok = |r: &Row| self.selectable(*r);
        self.rows
            .iter()
            .position(|r| matches!(r, Row::Entry(_)) && ok(r))
            .or_else(|| self.rows.iter().position(ok))
            .unwrap_or(0)
    }

    /// The next selectable row from `from` in the direction `step`.
    fn step(&self, from: usize, down: bool) -> Option<usize> {
        let mut i = from;
        loop {
            i = if down {
                i.checked_add(1)?
            } else {
                i.checked_sub(1)?
            };
            let row = *self.rows.get(i)?;
            if self.selectable(row) {
                return Some(i);
            }
        }
    }
}

/// Where the last frame put things, for the layout tests.
#[derive(Clone, Debug)]
pub struct Laid {
    /// The whole reserved area.
    pub area: egui::Rect,
    pub crumb: egui::Rect,
    /// The breadcrumb line's words, lead and path and cut note together.
    pub crumb_text: String,
    /// The rows drawn (only those scrolled into view).
    pub rows: Vec<egui::Rect>,
    /// The Loading / Failed / empty words, while shown.
    pub message: Option<egui::Rect>,
    pub message_text: Option<String>,
}

impl Default for Laid {
    fn default() -> Self {
        Self {
            area: egui::Rect::NOTHING,
            crumb: egui::Rect::NOTHING,
            crumb_text: String::new(),
            rows: Vec::new(),
            message: None,
            message_text: None,
        }
    }
}

#[derive(Debug)]
struct Source {
    target: String,
    shell: RemoteShell,
    /// The Remote's name, for the words.
    name: String,
}

#[derive(Debug)]
pub struct Browser {
    /// Listings for the life of this widget (the prompt, or the edit form).
    cache: Shared,
    /// Set on drop: kills this widget's running listings.
    cancel: Arc<AtomicBool>,
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
    /// The rows' scroll offset last frame.
    offset: f32,
    caret_to_end: bool,
    source: Option<Source>,
    memo: RefCell<Option<(String, Arc<View>)>>,
    pub laid: Laid,
}

impl Default for Browser {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Browser {
    pub fn new() -> Self {
        Self {
            cache: Shared::default(),
            cancel: Arc::new(AtomicBool::new(false)),
            focus: Focus::Field,
            sel: 0,
            reseat: false,
            want: None,
            last_dir: None,
            changed_at: Instant::now(),
            immediate: true,
            scroll_to: false,
            offset: 0.0,
            caret_to_end: false,
            source: None,
            memo: RefCell::new(None),
            laid: Laid::default(),
        }
    }

    /// The Remote to list, and its name for the words. A change hands the
    /// keyboard back to the field and asks unreachable directories again.
    pub fn set_source(&mut self, target: &str, shell: RemoteShell, name: &str) {
        let target = target.trim();
        let name = if name.trim().is_empty() {
            target
        } else {
            name.trim()
        };
        match &mut self.source {
            Some(s) if s.target == target && s.shell == shell => {
                if s.name != name {
                    s.name = name.to_string();
                }
            }
            _ => {
                self.source = Some(Source {
                    target: target.to_string(),
                    shell,
                    name: name.to_string(),
                });
                self.focus = Focus::Field;
                self.immediate = true;
                self.reseat = false;
                self.want = None;
                self.lock().forget();
            }
        }
    }

    /// Whether a Remote was ever set: the widget has state worth dropping.
    pub fn used(&self) -> bool {
        self.source.is_some()
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

    /// What Down from the field would do for `text`.
    pub fn enters(&self, text: &str) -> Enters {
        let Some(view) = self.view(text) else {
            return Enters::None;
        };
        match &*view.state {
            State::Unreachable(_) => Enters::Retry,
            State::Loading => Enters::Folders,
            _ if !view.rows.is_empty() => Enters::Folders,
            _ => Enters::None,
        }
    }

    /// What the focused list shows for `text`.
    pub fn shows(&self, text: &str) -> Shows {
        match self.view(text).as_deref().map(|v| &*v.state) {
            Some(State::Unreachable(_)) => Shows::Retry,
            Some(State::Listed(_)) => Shows::Rows,
            _ => Shows::Waiting,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, model::Cache> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn style(&self) -> Style {
        Style::of(self.source.as_ref().map_or(RemoteShell::Posix, |s| s.shell))
    }

    fn key_for(&self, text: &str) -> Option<Key> {
        let s = self.source.as_ref()?;
        let dir = model::split(text, self.style()).dir;
        Some(Key::new(&s.target, s.shell, dir))
    }

    fn view(&self, text: &str) -> Option<Arc<View>> {
        let key = self.key_for(text)?;
        let state = self
            .lock()
            .peek(&key)
            .unwrap_or_else(|| Arc::new(State::Loading));
        Some(self.view_of(text, state))
    }

    /// The view for `text` over `state`, from the memo when neither changed.
    fn view_of(&self, text: &str, state: Arc<State>) -> Arc<View> {
        let mut memo = self.memo.borrow_mut();
        if let Some((t, v)) = &*memo {
            let same_state = Arc::ptr_eq(&v.state, &state)
                || (matches!(*v.state, State::Loading) && matches!(*state, State::Loading));
            if t == text && same_state {
                return v.clone();
            }
        }
        let style = self.style();
        let filter = model::split(text, style).filter.to_lowercase();
        let mut rows = Vec::new();
        let mut up = None;
        match &*state {
            State::Listed(l) => {
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
            State::Unreachable(_) => rows.push(Row::Retry),
            State::Loading | State::Failed(_) => {}
        }
        let v = Arc::new(View { state, rows, up });
        *memo = Some((text.to_string(), v.clone()));
        v
    }

    /// Keep the selection true to what the list now shows: after the text
    /// changed, `want` (else the first folder) once listed; a directory that
    /// cannot be listed hands the keyboard back; an unreachable one selects
    /// its retry row. A stale reseat never outlives a failed listing.
    fn settle(&mut self, view: &View) {
        match &*view.state {
            State::Listed(l) if self.reseat => {
                self.reseat = false;
                let want = self.want.take();
                self.sel = want
                    .and_then(|w| {
                        view.rows.iter().position(|r| match r {
                            Row::Entry(i) => l.entries[*i].name == w,
                            _ => false,
                        })
                    })
                    .unwrap_or_else(|| view.first());
                self.scroll_to = true;
            }
            State::Failed(_) => {
                self.reseat = false;
                self.want = None;
                self.focus_field();
            }
            State::Unreachable(_) => {
                self.reseat = false;
                self.want = None;
                self.sel = view.first();
            }
            _ => {}
        }
        self.sel = self.sel.min(view.rows.len().saturating_sub(1));
        if view
            .rows
            .get(self.sel)
            .is_some_and(|r| !view.selectable(*r))
        {
            self.sel = view.first();
        }
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

    /// Ask unreachable directories again, at once; select on arrival.
    fn retry(&mut self) {
        self.lock().forget();
        self.immediate = true;
        self.reseat = true;
        self.want = None;
    }

    fn open_row(&mut self, text: &mut String, view: &View, row: Row) {
        match row {
            Row::Up => self.go_up(text, view),
            Row::Retry => self.retry(),
            Row::Entry(i) => {
                if let Some(e) = view.listed().and_then(|l| l.entries.get(i))
                    && e.blocked.is_none()
                {
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
    /// the rest (in the field, everything but Down while [`Self::enters`]).
    pub fn key(&mut self, k: Nav, text: &mut String) -> bool {
        let Some(view) = self.view(text) else {
            return false;
        };
        self.settle(&view);
        match self.focus {
            Focus::Field => {
                if k != Nav::Down {
                    return false;
                }
                match &*view.state {
                    // Entering again is asking again.
                    State::Unreachable(_) => self.retry(),
                    // Down stays Down: wait in the list for the first row.
                    State::Loading => {
                        self.immediate = true;
                        self.reseat = true;
                        self.want = None;
                    }
                    _ if !view.rows.is_empty() => {
                        self.sel = view.first();
                        self.scroll_to = true;
                    }
                    _ => return false,
                }
                self.focus = Focus::List;
                true
            }
            Focus::List => {
                match k {
                    Nav::Tab => self.focus_field(),
                    Nav::Up => match view.step(self.sel, false) {
                        Some(i) if !view.rows.is_empty() => {
                            self.sel = i;
                            self.scroll_to = true;
                        }
                        _ => self.focus_field(),
                    },
                    Nav::Down => {
                        if let Some(i) = view.step(self.sel, true) {
                            self.sel = i;
                            self.scroll_to = true;
                        }
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

    /// Typing while the list has the keyboard: the text goes to the field,
    /// which takes the keyboard back. Call before the field draws. Returns
    /// whether `text` changed.
    pub fn route_typing(&mut self, ctx: &egui::Context, text: &mut String) -> bool {
        if self.focus != Focus::List {
            return false;
        }
        let typed = ctx.input_mut(|i| {
            let mut typed = String::new();
            i.events.retain(|e| match e {
                egui::Event::Text(t) | egui::Event::Paste(t) => {
                    typed.extend(t.chars().filter(|c| !c.is_control()));
                    false
                }
                _ => true,
            });
            typed
        });
        if typed.is_empty() {
            return false;
        }
        text.push_str(&typed);
        self.focus_field();
        true
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
        self.lock().finish(Key::new(target, shell, dir), state);
    }

    /// Test hook: what is cached for `dir`.
    #[cfg(test)]
    pub fn peek(&self, target: &str, shell: RemoteShell, dir: &str) -> Option<Arc<State>> {
        self.lock().peek(&Key::new(target, shell, dir))
    }

    /// Test hook: pretend the listing of `dir` started `ago`.
    #[cfg(test)]
    pub fn age(&self, target: &str, shell: RemoteShell, dir: &str, ago: Duration) {
        self.lock().age(&Key::new(target, shell, dir), ago);
    }

    /// Test hook: the cancel flag the listings watch.
    #[cfg(test)]
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    /// Draw the list for `text` in a reserved area `body_h` tall (plus the
    /// breadcrumb). Lists only while `active`. `project` names whose
    /// Directory this lists, where several share one area. Returns whether
    /// a click rewrote `text`; a click also hands the keyboard to the field.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        text: &mut String,
        body_h: f32,
        active: bool,
        project: Option<&str>,
    ) -> bool {
        let Some(key) = self.key_for(text) else {
            idle(ui, body_h, "No Remote to list", &mut self.laid);
            return false;
        };
        let ctx = ui.ctx().clone();
        let now = Instant::now();
        if self.last_dir.as_deref() != Some(key.dir.as_str()) {
            // Another directory: what could not be reached is asked again.
            if self.last_dir.is_some() {
                self.lock().forget();
            }
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
        let state = model::list(&ctx, &self.cache, &key, may_start, &self.cancel);
        let running_for = self.lock().running_for(&key);
        // Nothing runs during the debounce, so nothing says it does.
        let asked = running_for.is_some() || may_start;
        if let Some(t) = running_for
            && t < STILL_TRYING
        {
            ctx.request_repaint_after(STILL_TRYING - t);
        } else if may_start && running_for.is_none() && matches!(*state, State::Loading) {
            // No free slot yet: look again when one frees.
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        let view = self.view_of(text, state);
        self.settle(&view);

        let th = crate::theme::live(&ctx);
        let style = self.style();
        let name = self.source.as_ref().map_or("", |s| s.name.as_str());
        // Like the form's fields: nothing looks focused while another Win
        // reads the keyboard.
        let focused = self.focus == Focus::List && active;
        let dir = model::split(text, style).dir;
        let path = match style {
            Style::Windows if model::is_home(dir) => "~ and drives".to_string(),
            _ => model::breadcrumb(dir),
        };
        let lead = match project {
            Some(p) => format!("Folders for {p} on {name} in "),
            None => "Folders in ".to_string(),
        };
        let tail = match view.listed() {
            Some(l) if l.truncated => format!(" · first {}", model::CAP),
            _ => String::new(),
        };
        let filter = model::split(text, style).filter;
        let message: Option<(String, egui::Color32, Option<String>)> = match &*view.state {
            State::Loading if !asked => None,
            State::Loading if running_for.is_some_and(|t| t >= STILL_TRYING) => {
                Some((format!("Still trying to reach {name}…"), th.dim, None))
            }
            State::Loading => Some(("Listing folders…".into(), th.dim, None)),
            State::Failed(f) | State::Unreachable(f) => {
                Some((f.why.clone(), th.danger, f.detail.clone()))
            }
            State::Listed(l) if l.entries.is_empty() => {
                Some(("No folders here".into(), th.dim, None))
            }
            State::Listed(_) if !view.rows.iter().any(|r| matches!(r, Row::Entry(_))) => Some((
                format!("No folder here starts with “{filter}”"),
                th.dim,
                None,
            )),
            State::Listed(_) => None,
        };

        let scroll_to = std::mem::take(&mut self.scroll_to) && focused;
        let sel = self.sel;
        let mut offset = self.offset;
        let mut clicked = None;
        let laid = &mut self.laid;
        laid.rows.clear();
        laid.message = None;
        laid.message_text = message.as_ref().map(|m| m.0.clone());
        let crumb = Crumb {
            lead: &lead,
            path: &path,
            tail: &tail,
        };
        area(ui, body_h, focused, laid, Some(crumb), |ui, laid, body| {
            let n = view.rows.len();
            let view_h = body.height();
            if scroll_to {
                let y = sel as f32 * ROW_H;
                if y < offset {
                    offset = y;
                } else if y + ROW_H > offset + view_h {
                    offset = y + ROW_H - view_h;
                }
            }
            let mut sa = egui::ScrollArea::vertical()
                .id_salt("remote_browse_rows")
                .auto_shrink([false, false])
                .max_height(view_h);
            if scroll_to {
                sa = sa.vertical_scroll_offset(offset);
            }
            ui.spacing_mut().item_spacing.y = 0.0;
            let out = sa.show_rows(ui, ROW_H, n, |ui, range| {
                let listed = view.listed();
                let font = name_font(ui);
                for i in range {
                    let row = view.rows[i];
                    let ok = view.selectable(row);
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), ROW_H),
                        if ok {
                            egui::Sense::click()
                        } else {
                            egui::Sense::hover()
                        },
                    );
                    laid.rows.push(rect);
                    let p = ui.painter();
                    let selected = focused && i == sel;
                    if selected || (ok && resp.hovered()) {
                        p.rect_filled(rect, egui::CornerRadius::same(3), th.sel_bg);
                    }
                    if selected {
                        p.text(
                            egui::pos2(rect.min.x + 2.0, rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            "›",
                            font.clone(),
                            th.border_focus,
                        );
                    }
                    let (name, note, color, why) = match row {
                        Row::Up => ("..".into(), Some("up one level"), th.text, None),
                        Row::Retry => (
                            "retry".into(),
                            Some("ask the Remote again"),
                            th.border_focus,
                            None,
                        ),
                        Row::Entry(k) => {
                            let e = &listed.expect("rows come from a listing").entries[k];
                            let note = if e.blocked.is_some() {
                                Some("can't be chosen")
                            } else {
                                e.drive.then_some("drive")
                            };
                            let color = if e.blocked.is_some() { th.dim } else { th.text };
                            (e.shown().into_owned(), note, color, e.blocked)
                        }
                    };
                    let left = rect.min.x + MARK_W;
                    let room = (rect.max.x - left - PAD).max(0.0);
                    let note_g = note.map(|n| {
                        p.layout_no_wrap(
                            format!("  {n}"),
                            egui::FontId::proportional(SMALL),
                            th.dim,
                        )
                    });
                    let note_w = note_g.as_ref().map_or(0.0, |g| g.size().x);
                    let mut job =
                        egui::text::LayoutJob::simple_singleline(name.clone(), font.clone(), color);
                    job.wrap =
                        egui::text::TextWrapping::truncate_at_width((room - note_w).max(0.0));
                    let g = p.layout_job(job);
                    let at = egui::pos2(left, rect.center().y - g.size().y / 2.0);
                    let name_w = g.size().x;
                    let elided = g.elided;
                    p.galley(at, g, color);
                    if let Some(ng) = note_g {
                        let at = egui::pos2(left + name_w, rect.center().y - ng.size().y / 2.0);
                        p.galley(at, ng, th.dim);
                    }
                    let resp = match (why, elided) {
                        (Some(why), true) => resp.on_hover_text(format!("{name}\n{why}")),
                        (Some(why), false) => resp.on_hover_text(why),
                        (None, true) => resp.on_hover_text(name),
                        (None, false) => resp,
                    };
                    if ok {
                        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                        if resp.clicked() {
                            clicked = Some(row);
                        }
                    }
                }
            });
            offset = out.state.offset.y;
            if let Some((words, color, detail)) = message {
                // Under the rows (a ".." or retry at most), inside the body.
                let top = (body.min.y + n as f32 * ROW_H - offset).max(body.min.y);
                let r = egui::Rect::from_min_max(egui::pos2(body.min.x, top), body.max);
                let mut mui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(r)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                mui.set_clip_rect(body.intersect(ui.clip_rect()));
                let shown = mui.horizontal_wrapped(|ui| {
                    ui.add_space(MARK_W);
                    let l = ui.add(
                        egui::Label::new(egui::RichText::new(words).color(color).size(SMALL))
                            .wrap(),
                    );
                    if let Some(d) = detail {
                        l.on_hover_text(d);
                    }
                });
                laid.message = Some(shown.response.rect);
            }
        });
        self.offset = offset;
        match clicked {
            Some(row) => {
                let before = text.clone();
                self.open_row(text, &view, row);
                self.focus = Focus::Field;
                *text != before
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
    laid.message_text = Some(words.to_string());
    area(ui, body_h, false, laid, None, |ui, laid, _| {
        let r = ui.horizontal_wrapped(|ui| {
            ui.add_space(MARK_W);
            ui.add(egui::Label::new(egui::RichText::new(words).color(th.dim).size(SMALL)).wrap());
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

/// The breadcrumb line: `lead` and `tail` in small dim words, `path` in the
/// name font with its front cut to fit between them.
struct Crumb<'a> {
    lead: &'a str,
    path: &'a str,
    tail: &'a str,
}

/// The fixed frame: a breadcrumb line, then `body_h` of scrolling rows.
/// Allocated whole before anything is drawn in it, so its height is the
/// same in every state. `body` gets the body's rect.
fn area(
    ui: &mut egui::Ui,
    body_h: f32,
    focused: bool,
    laid: &mut Laid,
    crumb: Option<Crumb>,
    body: impl FnOnce(&mut egui::Ui, &mut Laid, egui::Rect),
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
    laid.crumb_text.clear();
    if let Some(c) = crumb {
        laid.crumb_text = format!("{}{}{}", c.lead, c.path, c.tail);
        let small = egui::FontId::proportional(SMALL);
        let x = crumb_rect.min.x + MARK_W;
        let room = (crumb_rect.max.x - x).max(0.0);
        let p = ui.painter();
        let tail_g = p.layout_no_wrap(c.tail.into(), small.clone(), th.dim);
        let tail_w = tail_g.size().x;
        // The lead gives way first, down to half the line; the path's front
        // is cut for the rest.
        let mut job = egui::text::LayoutJob::simple_singleline(c.lead.into(), small, th.dim);
        job.wrap = egui::text::TextWrapping::truncate_at_width(((room - tail_w) * 0.6).max(0.0));
        let lead_g = p.layout_job(job);
        let lead_cut = lead_g.elided;
        let lead_w = lead_g.size().x;
        let y = |h: f32| crumb_rect.center().y - h / 2.0;
        p.galley(egui::pos2(x, y(lead_g.size().y)), lead_g, th.dim);
        let font = name_font(ui);
        let path_room = (room - lead_w - tail_w).max(0.0);
        let (shown, cut) = crate::remotes_pane::elide_front(ui, c.path, &font, path_room);
        let g = ui.painter().layout_no_wrap(shown, font, th.text);
        let at = egui::pos2(x + lead_w, y(g.size().y));
        let path_w = g.size().x;
        ui.painter().galley(at, g, th.text);
        ui.painter().galley(
            egui::pos2(x + lead_w + path_w, y(tail_g.size().y)),
            tail_g,
            th.dim,
        );
        if cut || lead_cut {
            ui.interact(crumb_rect, ui.id().with("crumb"), egui::Sense::hover())
                .on_hover_text(laid.crumb_text.clone());
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
    body(&mut child, laid, body_rect);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_browse::{Entry, Failure, Listed};

    fn listing(resolved: &str, names: &[&str]) -> State {
        State::Listed(Listed {
            resolved: resolved.into(),
            entries: names.iter().map(|n| Entry::dir(n)).collect(),
            truncated: false,
        })
    }

    fn browser() -> Browser {
        let mut b = Browser::new();
        b.set_source("desk", RemoteShell::Posix, "desk");
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
        assert_eq!(b.enters(&text), Enters::Folders);
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

    /// While the focused directory is still listing, Down does not fall
    /// through to the caller (who would move to the next field): it enters
    /// the list, and the first folder is selected when the listing lands.
    #[test]
    fn down_while_loading_waits_in_the_list_and_selects_on_arrival() {
        let mut b = browser();
        let mut text = "/srv/".to_string();
        assert_eq!(b.enters(&text), Enters::Folders);
        assert!(b.key(Nav::Down, &mut text), "Down is the list's");
        assert!(b.list_focused());
        assert_eq!(b.shows(&text), Shows::Waiting);
        b.key(Nav::Down, &mut text); // nothing to move to: stays
        assert!(b.list_focused());
        b.put(
            "desk",
            RemoteShell::Posix,
            "/srv/",
            listing("/srv", &["app", "data"]),
        );
        let v = b.view(&text).unwrap();
        b.settle(&v);
        assert_eq!(b.shows(&text), Shows::Rows);
        assert_eq!(v.rows[b.sel], Row::Entry(0), "app, the first folder");
    }

    /// A directory that cannot be listed: Down stays the field's, and a
    /// list waiting on it hands the keyboard back when it fails. No stale
    /// reseat survives to move the selection on a later listing.
    #[test]
    fn a_failed_directory_leaves_every_key_to_the_field() {
        let mut b = browser();
        let mut text = "/nope/".to_string();
        assert!(b.key(Nav::Down, &mut text), "Loading: waits in the list");
        b.put(
            "desk",
            RemoteShell::Posix,
            "/nope/",
            State::Failed(Failure::new("No such")),
        );
        let v = b.view(&text).unwrap();
        b.settle(&v);
        assert!(!b.list_focused(), "the failure hands the keyboard back");
        assert!(!b.reseat && b.want.is_none());
        assert_eq!(b.enters(&text), Enters::None);
        assert!(!b.key(Nav::Down, &mut text));
        // No Remote at all.
        let mut b = Browser::new();
        assert!(!b.key(Nav::Down, &mut text));
        assert_eq!(b.enters(&text), Enters::None);
    }

    /// An unreachable directory offers a retry row; Down (entering again)
    /// and Enter on it both forget the answer so the next frame asks again.
    #[test]
    fn an_unreachable_directory_is_retried_by_entering_or_enter() {
        let mut b = browser();
        let mut text = "/srv/".to_string();
        let gone = || model::unreachable("ssh: connect to host desk port 22: Connection refused");
        b.put("desk", RemoteShell::Posix, "/srv/", gone());
        assert_eq!(b.enters(&text), Enters::Retry);
        assert_eq!(b.view(&text).unwrap().rows, [Row::Retry]);
        assert!(b.key(Nav::Down, &mut text));
        assert!(b.list_focused());
        assert_eq!(
            b.peek("desk", RemoteShell::Posix, "/srv/"),
            None,
            "asked again"
        );
        b.put("desk", RemoteShell::Posix, "/srv/", gone());
        let v = b.view(&text).unwrap();
        b.settle(&v);
        assert!(b.list_focused(), "unreachable keeps the list, on retry");
        assert_eq!(b.shows(&text), Shows::Retry);
        assert_eq!(v.rows[b.sel], Row::Retry);
        b.key(Nav::Enter, &mut text);
        assert_eq!(b.peek("desk", RemoteShell::Posix, "/srv/"), None);
        assert_eq!(text, "/srv/", "retry writes nothing");
    }

    /// A name that cannot be chosen is skipped by the keyboard and never
    /// opened.
    #[test]
    fn a_blocked_name_is_never_selected_or_opened() {
        let mut b = browser();
        b.put(
            "desk",
            RemoteShell::Posix,
            "/odd/",
            State::Listed(Listed {
                resolved: "/odd".into(),
                entries: vec![
                    Entry::from_bytes(b"\xffbad"),
                    Entry::dir("good"),
                    Entry::from_bytes(b"\xfeworse"),
                ],
                truncated: false,
            }),
        );
        let mut text = "/odd/".to_string();
        b.key(Nav::Down, &mut text);
        let v = b.view(&text).unwrap();
        let at = |b: &Browser| v.rows[b.sel];
        let good = v
            .rows
            .iter()
            .position(
                |r| matches!(r, Row::Entry(i) if v.listed().unwrap().entries[*i].name == "good"),
            )
            .unwrap();
        assert_eq!(b.sel, good, "the first choosable folder");
        b.key(Nav::Down, &mut text);
        assert_eq!(b.sel, good, "nothing choosable below");
        b.key(Nav::Up, &mut text);
        assert_eq!(at(&b), Row::Up, "skips the blocked one to ..");
        // Even asked directly (a click lands on it), it never opens.
        let row = v.rows.iter().copied().find(|r| !v.selectable(*r)).unwrap();
        b.open_row(&mut text, &v, row);
        assert_eq!(text, "/odd/", "never opened");
        // And a selection left on one moves off it.
        b.sel = v.rows.iter().position(|r| *r == row).unwrap();
        b.settle(&v);
        assert!(v.selectable(v.rows[b.sel]));
    }

    #[test]
    fn a_new_remote_hands_the_keyboard_back() {
        let mut b = browser();
        let mut text = String::new();
        b.key(Nav::Down, &mut text);
        b.set_source("desk", RemoteShell::Posix, "desk");
        assert!(b.list_focused(), "same Remote: unchanged");
        b.set_source("pi", RemoteShell::Posix, "pi");
        assert!(!b.list_focused());
    }

    /// Dropping the widget sets the flag its listings' ssh watch.
    #[test]
    fn dropping_the_widget_cancels_its_listings() {
        let b = browser();
        let flag = b.cancel_flag();
        assert!(!flag.load(Ordering::Relaxed));
        drop(b);
        assert!(flag.load(Ordering::Relaxed));
    }

    /// Typing while the list has the keyboard lands in the field.
    #[test]
    fn typing_in_the_list_goes_to_the_field() {
        let mut b = browser();
        let mut text = "~/".to_string();
        b.key(Nav::Down, &mut text);
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events: vec![
                egui::Event::Text("sr".into()),
                egui::Event::Key {
                    key: egui::Key::A,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        };
        let mut routed = false;
        let _ = ctx.run_ui(input, |ui| {
            routed = b.route_typing(ui.ctx(), &mut text);
            assert!(
                !ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Text(_)))),
                "consumed"
            );
        });
        assert!(routed);
        assert_eq!(text, "~/sr");
        assert!(!b.list_focused());
        // From the field it is the field's own.
        let _ = ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::Text("c".into())],
                ..Default::default()
            },
            |ui| assert!(!b.route_typing(ui.ctx(), &mut text)),
        );
    }
}
