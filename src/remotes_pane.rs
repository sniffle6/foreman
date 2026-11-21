//! The Remotes pane of the settings menu: list, add, edit and delete Remotes,
//! with reachability and Test per row. A custom body like the Keybindings
//! pane. The model half (`Draft`, `commit`, `RemotesView::key`) is pure and
//! tested; the egui half below only draws it and feeds it keys.

use std::collections::HashMap;
use std::sync::mpsc;

use eframe::egui;

use crate::remote::{self, Remote, RemoteShell};
use crate::remote_probe::{self, Reach, TestOutcome};

/// What the pane wants the settings menu to do after a frame.
pub enum Outcome {
    Pending,
    /// `Settings::remotes` changed: the menu publishes it and the debounced
    /// write saves it.
    Changed,
    /// Esc while browsing: back to the rail.
    Close,
}

pub fn shell_label(s: RemoteShell) -> &'static str {
    match s {
        // The names Settings > General and the launcher use for local shells.
        RemoteShell::Pwsh => "PowerShell",
        RemoteShell::WindowsPowerShell => "Windows PowerShell",
        RemoteShell::Posix => "SH",
    }
}

const SHELLS: [RemoteShell; 3] = [
    RemoteShell::Posix,
    RemoteShell::Pwsh,
    RemoteShell::WindowsPowerShell,
];

pub fn cycle_shell(s: RemoteShell, forward: bool) -> RemoteShell {
    let i = SHELLS.iter().position(|x| *x == s).unwrap_or(0);
    let n = SHELLS.len();
    SHELLS[if forward {
        (i + 1) % n
    } else {
        (i + n - 1) % n
    }]
}

/// The edit form's fields, in Tab order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    Name,
    Target,
    Shell,
}

impl Field {
    fn step(self, forward: bool) -> Field {
        const ORDER: [Field; 3] = [Field::Name, Field::Target, Field::Shell];
        let i = ORDER.iter().position(|f| *f == self).unwrap_or(0);
        ORDER[if forward { (i + 1) % 3 } else { (i + 2) % 3 }]
    }
}

/// An add or edit in progress.
#[derive(Clone, Debug, PartialEq)]
pub struct Draft {
    /// The entry being edited; `None` adds a new one.
    pub index: Option<usize>,
    pub name: String,
    pub target: String,
    pub shell: RemoteShell,
    pub field: Field,
    /// The highlighted ssh-config suggestion, while the Target field is active.
    pub pick: Option<usize>,
    /// Why the last save was refused, drawn in the form. Dropped with the
    /// draft, so a cancel leaves nothing behind.
    pub error: Option<String>,
}

/// Why a draft cannot be saved, and the field to fix.
#[derive(Clone, Debug, PartialEq)]
pub struct Invalid {
    pub field: Field,
    pub text: String,
}

fn invalid(field: Field, text: impl Into<String>) -> Invalid {
    Invalid {
        field,
        text: text.into(),
    }
}

impl Draft {
    pub fn add() -> Self {
        Self {
            index: None,
            name: String::new(),
            target: String::new(),
            shell: RemoteShell::default(),
            field: Field::Name,
            pick: None,
            error: None,
        }
    }

    pub fn edit(index: usize, r: &Remote) -> Self {
        Self {
            index: Some(index),
            name: r.name.clone(),
            target: r.target.clone(),
            shell: r.shell,
            field: Field::Name,
            pick: None,
            error: None,
        }
    }

    /// The Remote this draft would store, or a plain reason it cannot be
    /// saved and the field to fix. Same rules as `remote::sanitize`, so a
    /// saved entry survives the next load. Editing keeps the entry's
    /// directory mappings.
    pub fn validate(&self, remotes: &[Remote]) -> Result<Remote, Invalid> {
        use Field::{Name, Target};
        let name = self.name.trim();
        let target = self.target.trim();
        if name.is_empty() {
            return Err(invalid(Name, "Give the Remote a name"));
        }
        if name.eq_ignore_ascii_case(remote::RESERVED_NAME) {
            return Err(invalid(
                Name,
                "\"local\" is the name for this PC; pick another",
            ));
        }
        if name.len() > remote::MAX_NAME {
            return Err(invalid(
                Name,
                format!("Names are at most {} characters", remote::MAX_NAME),
            ));
        }
        if !remote::valid_name(name) {
            return Err(invalid(Name, "Names use letters, digits, '.' and '-'"));
        }
        if remotes
            .iter()
            .enumerate()
            .any(|(i, r)| Some(i) != self.index && r.name.eq_ignore_ascii_case(name))
        {
            return Err(invalid(
                Name,
                format!("There is already a Remote named {name}"),
            ));
        }
        if target.is_empty() {
            return Err(invalid(
                Target,
                "Give the SSH target: user@pc or an ssh-config Host",
            ));
        }
        if target.starts_with('-') {
            return Err(invalid(Target, "A target cannot start with '-'"));
        }
        if !remote::valid_target(target) {
            return Err(invalid(Target, "A target has no spaces"));
        }
        let dirs = match self.index {
            Some(i) => match remotes.get(i) {
                Some(r) => r.dirs.clone(),
                None => {
                    return Err(invalid(Name, "That Remote was removed while you edited it"));
                }
            },
            None if remotes.len() >= remote::MAX_REMOTES => {
                return Err(invalid(
                    Name,
                    format!("At most {} Remotes", remote::MAX_REMOTES),
                ));
            }
            None => Default::default(),
        };
        Ok(Remote {
            name: name.to_string(),
            target: target.to_string(),
            shell: self.shell,
            dirs,
        })
    }

    /// Fill the target from a suggestion, and the name too while it is blank
    /// and the Host name is a usable one.
    fn accept(&mut self, host: &str, remotes: &[Remote]) {
        self.target = host.to_string();
        self.pick = None;
        if self.name.trim().is_empty()
            && remote::valid_name(host)
            && !remotes.iter().any(|r| r.name.eq_ignore_ascii_case(host))
        {
            self.name = host.to_string();
        }
    }
}

/// Store a draft. Returns the entry's index.
pub fn commit(remotes: &mut Vec<Remote>, d: &Draft) -> Result<usize, Invalid> {
    let r = d.validate(remotes)?;
    match d.index {
        Some(i) => {
            remotes[i] = r; // validate checked the index
            Ok(i)
        }
        None => {
            remotes.push(r);
            Ok(remotes.len() - 1)
        }
    }
}

/// Most suggestions shown under the Target field.
const MAX_SUGGESTIONS: usize = 6;

/// ssh-config Host names worth offering for `typed`: a case-insensitive
/// prefix match, not already another Remote's target, and not exactly what
/// is typed.
pub fn suggestions<'a>(hosts: &'a [String], remotes: &[Remote], d: &Draft) -> Vec<&'a str> {
    let typed = d.target.trim().to_ascii_lowercase();
    hosts
        .iter()
        .map(String::as_str)
        .filter(|h| {
            let lower = h.to_ascii_lowercase();
            lower.starts_with(&typed) && lower != typed
        })
        .filter(|h| {
            !remotes
                .iter()
                .enumerate()
                .any(|(i, r)| Some(i) != d.index && r.target.eq_ignore_ascii_case(h))
        })
        .take(MAX_SUGGESTIONS)
        .collect()
}

/// Copy a successful Test's shell onto every Remote with that target.
/// Returns whether anything changed.
pub fn apply_test(remotes: &mut [Remote], target: &str, outcome: &TestOutcome) -> bool {
    let TestOutcome::Ok(report) = outcome else {
        return false;
    };
    let mut changed = false;
    for r in remotes.iter_mut().filter(|r| r.target == target) {
        if r.shell != report.shell {
            r.shell = report.shell;
            changed = true;
        }
    }
    changed
}

/// The pane's keys, decoupled from egui so the model can be driven in tests.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Esc,
    Tab,
    BackTab,
    Space,
    Delete,
    Add,
    Test,
}

/// What a key asks of the caller beyond the model's own state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effect {
    None,
    Changed,
    Close,
    /// Start Test on the Remote at this index.
    Test(usize),
}

#[derive(Clone, Debug, PartialEq)]
enum Mode {
    Browse,
    Edit(Draft),
    ConfirmDelete(usize),
}

#[derive(Debug)]
enum TestState {
    Running(mpsc::Receiver<TestOutcome>),
    Done(TestOutcome),
}

/// The pane's UI state. `Settings::remotes` lives in the caller.
#[derive(Debug)]
pub struct RemotesView {
    /// Row cursor; `remotes.len()` is the Add row.
    selected: usize,
    mode: Mode,
    /// Footer line: the last save or delete. A refused save is the draft's
    /// `error`, drawn in the form instead.
    message: Option<String>,
    /// ssh-config Host names, read when an add form opens.
    hosts: Option<Vec<String>>,
    /// Test runs and results, by target.
    tests: HashMap<String, TestState>,
    /// The keyboard moved the selection, or a form opened; scroll it into
    /// view once.
    scroll_to: bool,
    /// Last frame's widget rects, for the layout test.
    laid: Laid,
}

/// Where the last frame put things, in screen points.
#[derive(Debug)]
pub struct Laid {
    pub pane: egui::Rect,
    /// The edit form's fields and buttons.
    pub form: Vec<egui::Rect>,
    /// Per row: its painted text, and its status and buttons.
    pub rows: Vec<(egui::Rect, egui::Rect)>,
}

impl Default for RemotesView {
    fn default() -> Self {
        Self::new()
    }
}

impl RemotesView {
    pub fn new() -> Self {
        Self {
            selected: 0,
            mode: Mode::Browse,
            message: None,
            hosts: None,
            tests: HashMap::new(),
            scroll_to: false,
            laid: Laid {
                pane: egui::Rect::NOTHING,
                form: Vec::new(),
                rows: Vec::new(),
            },
        }
    }

    /// True while the edit form is open, so the menu leaves Tab to it.
    pub fn is_editing(&self) -> bool {
        matches!(self.mode, Mode::Edit(_))
    }

    #[cfg(test)]
    pub fn laid(&self) -> &Laid {
        &self.laid
    }

    /// Open the add form below the list: select it and scroll it into view.
    fn begin_add(&mut self, len: usize) {
        self.selected = len;
        self.begin_edit(Draft::add());
        self.hosts = None; // re-read: the user may have just edited it
    }

    fn begin_edit(&mut self, d: Draft) {
        self.mode = Mode::Edit(d);
        self.message = None;
        self.scroll_to = true;
    }

    /// Apply one key to the model.
    pub fn key(&mut self, k: Key, remotes: &mut Vec<Remote>) -> Effect {
        let len = remotes.len();
        self.selected = self.selected.min(len);
        match std::mem::replace(&mut self.mode, Mode::Browse) {
            Mode::Browse => match k {
                Key::Up => {
                    self.selected = self.selected.saturating_sub(1);
                    self.scroll_to = true;
                }
                Key::Down => {
                    self.selected = (self.selected + 1).min(len);
                    self.scroll_to = true;
                }
                Key::Enter if self.selected < len => {
                    self.begin_edit(Draft::edit(self.selected, &remotes[self.selected]));
                }
                Key::Enter | Key::Add => self.begin_add(len),
                Key::Delete if self.selected < len => {
                    self.mode = Mode::ConfirmDelete(self.selected);
                }
                Key::Test if self.selected < len => return Effect::Test(self.selected),
                Key::Esc => return Effect::Close,
                _ => {}
            },
            Mode::ConfirmDelete(i) => match k {
                Key::Enter => return self.delete(i, remotes),
                Key::Esc => {}
                _ => self.mode = Mode::ConfirmDelete(i),
            },
            Mode::Edit(mut d) => {
                let hosts = self.hosts.as_deref().unwrap_or_default();
                let offered: Vec<String> = if d.field == Field::Target {
                    suggestions(hosts, remotes, &d)
                        .into_iter()
                        .map(String::from)
                        .collect()
                } else {
                    Vec::new()
                };
                match k {
                    Key::Esc if d.pick.is_some() => d.pick = None,
                    Key::Esc => {
                        // Cancel: mode is already Browse, and the draft's
                        // error went with it.
                        self.message = None;
                        return Effect::None;
                    }
                    Key::Enter => {
                        if let Some(host) = d.pick.and_then(|p| offered.get(p)) {
                            d.accept(host, remotes);
                        } else {
                            match commit(remotes, &d) {
                                Ok(i) => {
                                    self.selected = i;
                                    self.message = Some(format!("Saved {}", remotes[i].name));
                                    return Effect::Changed;
                                }
                                Err(e) => {
                                    d.field = e.field;
                                    d.pick = None;
                                    d.error = Some(e.text);
                                }
                            }
                        }
                    }
                    Key::Tab => {
                        d.field = d.field.step(true);
                        d.pick = None;
                    }
                    Key::BackTab => {
                        d.field = d.field.step(false);
                        d.pick = None;
                    }
                    Key::Down if !offered.is_empty() => {
                        d.pick = Some(d.pick.map_or(0, |p| (p + 1).min(offered.len() - 1)));
                        self.scroll_to = true;
                    }
                    Key::Up if d.pick.is_some() => {
                        d.pick = d.pick.and_then(|p| p.checked_sub(1));
                        self.scroll_to = true;
                    }
                    Key::Down => d.field = d.field.step(true),
                    Key::Up => d.field = d.field.step(false),
                    Key::Left | Key::Right | Key::Space if d.field == Field::Shell => {
                        d.shell = cycle_shell(d.shell, k != Key::Left);
                    }
                    _ => {}
                }
                self.mode = Mode::Edit(d);
            }
        }
        Effect::None
    }

    fn delete(&mut self, i: usize, remotes: &mut Vec<Remote>) -> Effect {
        if i >= remotes.len() {
            return Effect::None;
        }
        let gone = remotes.remove(i);
        if !remotes.iter().any(|r| r.target == gone.target) {
            self.tests.remove(&gone.target);
        }
        self.selected = self.selected.min(remotes.len());
        self.message = Some(format!(
            "Deleted {}. Sessions already open on it keep running.",
            gone.name
        ));
        Effect::Changed
    }

    /// Collect finished Tests, copying a found shell into the settings and
    /// into an open draft for the same target. Returns whether settings
    /// changed.
    fn poll_tests(&mut self, remotes: &mut [Remote]) -> bool {
        let mut changed = false;
        for (target, state) in self.tests.iter_mut() {
            let TestState::Running(rx) = state else {
                continue;
            };
            let outcome = match rx.try_recv() {
                Ok(o) => o,
                Err(mpsc::TryRecvError::Empty) => continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    TestOutcome::Failed("the probe thread stopped".into())
                }
            };
            changed |= apply_test(remotes, target, &outcome);
            if let (Mode::Edit(d), TestOutcome::Ok(report)) = (&mut self.mode, &outcome)
                && d.target.trim() == target
            {
                d.shell = report.shell;
            }
            *state = TestState::Done(outcome);
        }
        changed
    }

    fn start_test(&mut self, ctx: &egui::Context, target: &str) {
        if matches!(self.tests.get(target), Some(TestState::Running(_))) {
            return;
        }
        self.tests.insert(
            target.to_string(),
            TestState::Running(remote_probe::start_test(ctx, target)),
        );
    }

    /// This frame's key hint. Browsing offers only what the selection can
    /// do: the Add row cannot be edited, tested or deleted.
    fn hint(&self, len: usize) -> &'static str {
        match self.mode {
            Mode::Browse if self.selected < len => {
                "j/k or ↑/↓ select · Enter edit · A add · T test · Del delete · Esc back"
            }
            Mode::Browse if len == 0 => "Enter or A add · Esc back",
            Mode::Browse => "j/k or ↑/↓ select · Enter add · Esc back",
            Mode::Edit(_) => "Tab next field · ↓ suggestions · ←/→ shell · Enter save · Esc cancel",
            Mode::ConfirmDelete(_) => "Enter delete · Esc keep",
        }
    }

    /// Render one frame. `remotes` is the live list, mutated in place.
    /// `active` gates keyboard input and the form's focus, as in the
    /// Keybindings pane.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        active: bool,
        remotes: &mut Vec<Remote>,
    ) -> Outcome {
        let ctx = ui.ctx().clone();
        let mut changed = self.poll_tests(remotes);
        let mut close = false;

        if active {
            for k in read_keys(ui, &self.mode) {
                match self.key(k, remotes) {
                    Effect::Changed => changed = true,
                    Effect::Close => close = true,
                    Effect::Test(i) => {
                        let target = remotes[i].target.clone();
                        self.start_test(&ctx, &target);
                    }
                    Effect::None => {}
                }
            }
        }
        if let Mode::Edit(d) = &self.mode
            && d.index.is_none()
            && self.hosts.is_none()
        {
            self.hosts = Some(remote_probe::config_hosts());
        }
        self.laid.pane = rect;
        self.laid.form.clear();
        self.laid.rows.clear();

        let th = crate::theme::live(ui.ctx());
        ui.painter_at(rect).rect_filled(rect, 0.0, th.win_bg);
        // The Keybindings pane's left edge: the child spans the pane, rows
        // inset their text by ROW_PAD.
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        let ui = &mut child;
        ui.set_clip_rect(rect);
        ui.visuals_mut().override_text_color = Some(th.text);
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new("Remotes")
                .color(th.border_focus)
                .size(16.0)
                .strong(),
        );
        ui.label(
            egui::RichText::new(self.hint(remotes.len()))
                .color(th.dim)
                .size(11.5),
        );
        ui.add_space(8.0);

        let mut acts: Vec<Act> = Vec::new();
        egui::ScrollArea::vertical()
            .id_salt("remotes_pane")
            .auto_shrink([false, false])
            .max_height((rect.height() - 110.0).max(60.0))
            .show(ui, |ui| self.render_rows(ui, remotes, active, &mut acts));

        ui.add_space(6.0);
        ui.separator();
        if let Some(msg) = &self.message {
            ui.label(egui::RichText::new(msg).color(th.dim).size(11.5));
        }

        for a in acts {
            match a {
                Act::Select(i) => {
                    if self.mode == Mode::Browse {
                        self.selected = i;
                    }
                }
                Act::Edit(i) => {
                    self.selected = i;
                    self.begin_edit(Draft::edit(i, &remotes[i]));
                }
                Act::Add => self.begin_add(remotes.len()),
                Act::AskDelete(i) => {
                    self.selected = i;
                    self.mode = Mode::ConfirmDelete(i);
                }
                Act::Key(k) => {
                    if self.key(k, remotes) == Effect::Changed {
                        changed = true;
                    }
                }
                Act::Test(i) => {
                    let target = remotes[i].target.clone();
                    self.start_test(&ctx, &target);
                }
                Act::Field(f) => {
                    if let Mode::Edit(d) = &mut self.mode {
                        d.field = f;
                        d.pick = None;
                    }
                }
                Act::Accept(host) => {
                    if let Mode::Edit(d) = &mut self.mode {
                        d.accept(&host, remotes);
                    }
                }
            }
        }

        if close {
            Outcome::Close
        } else if changed {
            Outcome::Changed
        } else {
            Outcome::Pending
        }
    }

    fn render_rows(
        &mut self,
        ui: &mut egui::Ui,
        remotes: &[Remote],
        active: bool,
        acts: &mut Vec<Act>,
    ) {
        let th = crate::theme::live(ui.ctx());
        let scroll_to = std::mem::take(&mut self.scroll_to);
        let editing = match &self.mode {
            Mode::Edit(d) => Some(d.index),
            _ => None,
        };
        for (i, r) in remotes.iter().enumerate() {
            if editing == Some(Some(i)) {
                self.render_form(ui, remotes, active, scroll_to, acts);
                continue;
            }
            let selected = i == self.selected && editing.is_none();
            let (rect, resp) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), ROW_H),
                egui::Sense::click(),
            );
            if resp.clicked() {
                acts.push(Act::Select(i));
            }
            if selected {
                ui.painter()
                    .rect_filled(rect, egui::CornerRadius::same(4), th.sel_bg);
                if scroll_to {
                    ui.scroll_to_rect(rect, None);
                }
            }
            let reach = remote_probe::reach(ui.ctx(), &r.target);
            let (dot, reach_text, why) = match &reach {
                Reach::Checking => (th.dim, "checking…", None),
                Reach::Reachable => (th.border_focus, "reachable", None),
                Reach::Offline => (th.dim, "offline", Some(OFFLINE_WHY.to_string())),
                Reach::Unknown(why) => (th.dim, "unknown", Some(why.clone())),
            };

            // Status and buttons first, right to left; the text gets the rest.
            let mut row = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect.shrink2(egui::vec2(ROW_PAD, 0.0)))
                    .layout(egui::Layout::right_to_left(egui::Align::Center)),
            );
            if let Mode::ConfirmDelete(d) = self.mode
                && d == i
            {
                if row.small_button("keep").clicked() {
                    acts.push(Act::Key(Key::Esc));
                }
                if row
                    .small_button(egui::RichText::new("delete").color(th.danger))
                    .clicked()
                {
                    acts.push(Act::Key(Key::Enter));
                }
                row.label(egui::RichText::new(format!("Delete {}?", r.name)).color(th.danger));
            } else {
                if row.small_button("delete").clicked() {
                    acts.push(Act::AskDelete(i));
                }
                if row.small_button("edit").clicked() {
                    acts.push(Act::Edit(i));
                }
                let running = matches!(self.tests.get(&r.target), Some(TestState::Running(_)));
                if row
                    .add_enabled(!running, egui::Button::new("test").small())
                    .clicked()
                {
                    acts.push(Act::Test(i));
                }
                row.add_space(8.0);
                let label = row.label(egui::RichText::new(reach_text).color(dot).size(11.5));
                if let Some(why) = &why {
                    label.on_hover_text(why);
                }
            }
            let controls = row.min_rect();

            let name_color = if reach == Reach::Offline {
                th.dim
            } else {
                th.text
            };
            let sub_text = format!("{} · {}", r.target, shell_label(r.shell));
            let left = rect.min.x + ROW_PAD;
            let room = (controls.min.x - 8.0 - left).max(0.0);
            let name = one_line(ui, &r.name, egui::FontId::proportional(13.0), room);
            let sub = one_line(ui, &sub_text, egui::FontId::monospace(11.0), room);
            let name_at = egui::pos2(left, rect.min.y + 14.0 - name.size().y / 2.0);
            let sub_at = egui::pos2(left, rect.min.y + 30.0 - sub.size().y / 2.0);
            let text = egui::Rect::from_min_size(name_at, name.size())
                .union(egui::Rect::from_min_size(sub_at, sub.size()));
            let elided = name.elided || sub.elided;
            ui.painter().galley(name_at, name, name_color);
            ui.painter().galley(sub_at, sub, th.dim);
            self.laid.rows.push((text, controls));
            if elided
                && ui
                    .ctx()
                    .pointer_hover_pos()
                    .is_some_and(|p| text.contains(p))
            {
                resp.on_hover_text(format!("{}\n{sub_text}", r.name));
            }

            let status = match self.tests.get(&r.target) {
                Some(TestState::Running(_)) => Some(("Testing…".to_string(), th.dim)),
                Some(TestState::Done(o)) => {
                    let color = if matches!(o, TestOutcome::Ok(_)) {
                        th.dim
                    } else {
                        th.danger
                    };
                    Some((o.summary(), color))
                }
                // The reach reason is a tooltip; a keyboard user reads it here.
                None if selected => why.map(|w| (w, th.dim)),
                None => None,
            };
            status_line(ui, status);
            ui.add_space(2.0);
        }

        if editing == Some(None) {
            self.render_form(ui, remotes, active, scroll_to, acts);
        } else {
            let selected = self.selected >= remotes.len() && self.mode == Mode::Browse;
            let (rect, resp) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::click());
            if selected {
                ui.painter()
                    .rect_filled(rect, egui::CornerRadius::same(4), th.sel_bg);
                if scroll_to {
                    ui.scroll_to_rect(rect, None);
                }
            }
            let color = if selected || resp.hovered() {
                th.text
            } else {
                th.dim
            };
            ui.painter().text(
                egui::pos2(rect.min.x + ROW_PAD, rect.center().y),
                egui::Align2::LEFT_CENTER,
                "+ Add Remote",
                egui::FontId::proportional(12.5),
                color,
            );
            if resp.clicked() {
                acts.push(Act::Add);
            }
        }
    }

    /// The inline add/edit form, in place of its row (or after the list):
    /// one label/field row each, as wide as the pane allows.
    fn render_form(
        &mut self,
        ui: &mut egui::Ui,
        remotes: &[Remote],
        active: bool,
        scroll: bool,
        acts: &mut Vec<Act>,
    ) {
        let th = crate::theme::live(ui.ctx());
        let Mode::Edit(d) = &mut self.mode else {
            return;
        };
        let laid = &mut self.laid.form;
        let hosts = self.hosts.as_deref().unwrap_or_default();
        let name_id = egui::Id::new("remotes_edit_name");
        let target_id = egui::Id::new("remotes_edit_target");
        // The model owns which field is active and egui focus follows it,
        // but only while this Win reads the keyboard: typing in a terminal
        // must never land in a field.
        ui.memory_mut(|m| match (active, d.field) {
            (true, Field::Name) => m.request_focus(name_id),
            (true, Field::Target) => m.request_focus(target_id),
            _ => {
                m.surrender_focus(name_id);
                m.surrender_focus(target_id);
            }
        });
        let frame = egui::Frame::new()
            .fill(th.sel_bg)
            .corner_radius(egui::CornerRadius::same(4))
            .inner_margin(egui::Margin::same(8));
        let shown = frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing = egui::vec2(FORM_GAP, 6.0);
            let r = form_row(ui, "Name", th.dim, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut d.name)
                        .id(name_id)
                        .lock_focus(true) // Tab is ours: it moves fields
                        .hint_text("desk")
                        .desired_width(f32::INFINITY),
                )
            });
            laid.push(r.rect);
            if r.clicked() {
                acts.push(Act::Field(Field::Name));
            }
            if r.changed() {
                d.error = None;
            }
            let r = form_row(ui, "Target", th.dim, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut d.target)
                        .id(target_id)
                        .lock_focus(true)
                        .hint_text("user@pc or ssh-config Host")
                        .desired_width(f32::INFINITY),
                )
            });
            laid.push(r.rect);
            if r.clicked() {
                acts.push(Act::Field(Field::Target));
            }
            if r.changed() {
                d.pick = None;
                d.error = None;
            }
            if !hosts.is_empty() {
                // One fixed line whether or not Target is active, so the
                // form does not grow when focus moves.
                let offered = if d.field == Field::Target {
                    suggestions(hosts, remotes, d)
                } else {
                    Vec::new()
                };
                ui.horizontal(|ui| {
                    ui.set_min_height(LINE_H);
                    ui.add_space(LABEL_W + FORM_GAP);
                    egui::ScrollArea::horizontal()
                        .id_salt("remotes_hosts")
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let small =
                                    |t: &str| egui::RichText::new(t).color(th.dim).size(11.0);
                                if offered.is_empty() {
                                    ui.label(small(if d.field == Field::Target {
                                        "No ~/.ssh/config Host starts with that"
                                    } else {
                                        "~/.ssh/config Hosts are offered in Target"
                                    }));
                                }
                                if !offered.is_empty() {
                                    ui.label(small("~/.ssh/config:"));
                                }
                                for (i, host) in offered.iter().enumerate() {
                                    let picked = d.pick == Some(i);
                                    let text = egui::RichText::new(*host).monospace().size(11.0);
                                    let r = ui.selectable_label(picked, text);
                                    if picked && scroll {
                                        ui.scroll_to_rect(r.rect, None);
                                    }
                                    if r.clicked() {
                                        acts.push(Act::Accept(host.to_string()));
                                    }
                                }
                            });
                        });
                });
            }
            let r = form_row(ui, "Shell", th.dim, |ui| {
                let mut chip = egui::Button::new(shell_label(d.shell)).small();
                if d.field == Field::Shell {
                    chip = chip.stroke(egui::Stroke::new(1.0, th.border_focus));
                }
                ui.add(chip).on_hover_text("Test fills this in")
            });
            laid.push(r.rect);
            if r.clicked() {
                acts.push(Act::Field(Field::Shell));
                acts.push(Act::Key(Key::Right));
            }
            // A refused save, in the danger colour like a Keybindings
            // conflict, under the fields; focus is already on the bad one.
            if let Some(e) = &d.error {
                ui.horizontal_top(|ui| {
                    ui.add_space(LABEL_W + FORM_GAP);
                    ui.add(
                        egui::Label::new(egui::RichText::new(e).color(th.danger).size(11.5)).wrap(),
                    );
                });
            }
            ui.horizontal(|ui| {
                ui.add_space(LABEL_W + FORM_GAP);
                let save = if d.index.is_some() { "save" } else { "add" };
                let r = ui.small_button(save);
                laid.push(r.rect);
                if r.clicked() {
                    acts.push(Act::Key(Key::Enter));
                }
                let r = ui.small_button("cancel");
                laid.push(r.rect);
                if r.clicked() {
                    acts.push(Act::Key(Key::Esc));
                }
            });
        });
        // After the hosts strip's own scroll, so this one wins the list's.
        if scroll {
            ui.scroll_to_rect(shown.response.rect, None);
        }
        ui.add_space(4.0);
    }
}

/// A row's height: name over target · shell.
const ROW_H: f32 = 42.0;
/// Row text inset, the Keybindings pane's.
const ROW_PAD: f32 = 6.0;
/// The line reserved under each row, and for the hosts strip.
const LINE_H: f32 = 18.0;
/// The form's label column.
const LABEL_W: f32 = 52.0;
const FORM_GAP: f32 = 8.0;

/// The offline tooltip, and the selected row's reason line.
const OFFLINE_WHY: &str = "No TCP answer on its SSH port. This check cannot see past a \
     ProxyJump, so Test it or open a Remote Session to be sure.";

/// `text` on one line, cut with "…" to `width`.
fn one_line(
    ui: &egui::Ui,
    text: &str,
    font: egui::FontId,
    width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job =
        egui::text::LayoutJob::simple_singleline(text.to_owned(), font, egui::Color32::PLACEHOLDER);
    job.wrap = egui::text::TextWrapping::truncate_at_width(width);
    ui.painter().layout_job(job)
}

/// A form row: a dim label in a fixed column, then the field.
fn form_row<R>(
    ui: &mut egui::Ui,
    label: &str,
    color: egui::Color32,
    field: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.horizontal(|ui| {
        let h = ui.spacing().interact_size.y;
        let (r, _) = ui.allocate_exact_size(egui::vec2(LABEL_W, h), egui::Sense::hover());
        ui.painter().text(
            r.left_center(),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(11.5),
            color,
        );
        field(ui)
    })
    .inner
}

/// The line under a row for its Test or reach reason. Always there, so a
/// result arriving does not push the rows below down; a long one wraps,
/// indented by layout so its wrapped lines keep the row's text edge.
fn status_line(ui: &mut egui::Ui, status: Option<(String, egui::Color32)>) {
    ui.horizontal_top(|ui| {
        ui.set_min_height(LINE_H);
        ui.add_space(ROW_PAD);
        if let Some((text, color)) = status {
            ui.add(egui::Label::new(egui::RichText::new(text).color(color).size(11.0)).wrap());
        }
    });
}

/// A mouse action, applied after the rows are drawn so the draw never
/// mutates the list it iterates.
enum Act {
    Select(usize),
    Edit(usize),
    Add,
    AskDelete(usize),
    Test(usize),
    Field(Field),
    Accept(String),
    /// Same path as the keyboard (Save, Cancel, confirm delete).
    Key(Key),
}

/// A browse-mode letter's key. A letter with a modifier held is someone
/// else's shortcut: Ctrl+D must not ask to delete.
fn browse_letter(key: egui::Key, mods: egui::Modifiers) -> Option<Key> {
    if !mods.is_none() {
        return None;
    }
    Some(match key {
        egui::Key::K => Key::Up,
        egui::Key::J => Key::Down,
        egui::Key::A => Key::Add,
        egui::Key::D => Key::Delete,
        egui::Key::T => Key::Test,
        _ => return None,
    })
}

/// This frame's keys for the current mode. Letters only count while
/// browsing: in the form they are typing.
fn read_keys(ui: &egui::Ui, mode: &Mode) -> Vec<Key> {
    use egui::Key as K;
    ui.input(|i| {
        let mut out = Vec::new();
        let p = |k: K| i.key_pressed(k);
        let shift = i.modifiers.shift;
        if p(K::ArrowUp) {
            out.push(Key::Up);
        }
        if p(K::ArrowDown) {
            out.push(Key::Down);
        }
        if p(K::Enter) {
            out.push(Key::Enter);
        }
        if p(K::Escape) {
            out.push(Key::Esc);
        }
        match mode {
            Mode::Browse => {
                for k in [K::K, K::J, K::A, K::D, K::T] {
                    if p(k)
                        && let Some(key) = browse_letter(k, i.modifiers)
                    {
                        out.push(key);
                    }
                }
                if p(K::Insert) {
                    out.push(Key::Add);
                }
                if p(K::Delete) {
                    out.push(Key::Delete);
                }
            }
            Mode::Edit(d) => {
                if p(K::Tab) {
                    out.push(if shift { Key::BackTab } else { Key::Tab });
                }
                if d.field == Field::Shell {
                    if p(K::ArrowLeft) {
                        out.push(Key::Left);
                    }
                    if p(K::ArrowRight) {
                        out.push(Key::Right);
                    }
                    if p(K::Space) {
                        out.push(Key::Space);
                    }
                }
            }
            Mode::ConfirmDelete(_) => {}
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_probe::TestReport;

    fn r(name: &str, target: &str) -> Remote {
        Remote {
            name: name.into(),
            target: target.into(),
            shell: RemoteShell::Posix,
            dirs: Default::default(),
        }
    }

    fn typed(view: &mut RemotesView, field: Field, text: &str) {
        if let Mode::Edit(d) = &mut view.mode {
            match field {
                Field::Name => d.name = text.into(),
                Field::Target => d.target = text.into(),
                Field::Shell => {}
            }
        }
    }

    #[test]
    fn add_by_keyboard_saves_a_new_remote() {
        let mut v = RemotesView::new();
        let mut list = vec![];
        assert_eq!(v.key(Key::Add, &mut list), Effect::None);
        assert!(v.is_editing());
        typed(&mut v, Field::Name, "desk");
        typed(&mut v, Field::Target, " andy@desk ");
        v.key(Key::Tab, &mut list);
        v.key(Key::Tab, &mut list); // Shell
        v.key(Key::Right, &mut list);
        assert_eq!(v.key(Key::Enter, &mut list), Effect::Changed);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].target, "andy@desk");
        assert_eq!(list[0].shell, RemoteShell::Pwsh);
        assert!(!v.is_editing());
        assert_eq!(v.selected, 0);
    }

    #[test]
    fn enter_on_the_add_row_opens_the_add_form() {
        let mut v = RemotesView::new();
        let mut list = vec![r("desk", "desk")];
        v.key(Key::Down, &mut list);
        assert_eq!(v.selected, 1);
        v.key(Key::Down, &mut list); // clamps on the Add row
        assert_eq!(v.selected, 1);
        v.key(Key::Enter, &mut list);
        assert!(matches!(&v.mode, Mode::Edit(d) if d.index.is_none()));
    }

    #[test]
    fn edit_keeps_directory_mappings_and_renames() {
        let mut v = RemotesView::new();
        let mut desk = r("desk", "andy@desk");
        desk.dirs.insert(r"H:\src".into(), "/home/andy/src".into());
        let mut list = vec![desk];
        v.key(Key::Enter, &mut list);
        typed(&mut v, Field::Name, "desk2");
        assert_eq!(v.key(Key::Enter, &mut list), Effect::Changed);
        assert_eq!(list[0].name, "desk2");
        assert_eq!(list[0].dirs[r"H:\src"], "/home/andy/src");
    }

    fn draft(v: &RemotesView) -> &Draft {
        let Mode::Edit(d) = &v.mode else {
            panic!("not editing: {:?}", v.mode)
        };
        d
    }

    #[test]
    fn invalid_draft_stays_open_with_the_error_in_the_form() {
        let mut v = RemotesView::new();
        let mut list = vec![r("desk", "desk")];
        v.key(Key::Add, &mut list);
        typed(&mut v, Field::Name, "DESK");
        typed(&mut v, Field::Target, "x");
        v.key(Key::Tab, &mut list); // focus on Target
        assert_eq!(v.key(Key::Enter, &mut list), Effect::None);
        let d = draft(&v);
        assert_eq!(
            d.error.as_deref(),
            Some("There is already a Remote named DESK")
        );
        assert_eq!(d.field, Field::Name, "focus moves to the bad field");
        assert_eq!(v.message, None, "the footer is for saves and deletes");
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn a_bad_target_focuses_target_and_esc_leaves_no_message() {
        let mut v = RemotesView::new();
        let mut list = vec![r("desk", "desk")];
        // A save leaves its footer line...
        v.key(Key::Add, &mut list);
        typed(&mut v, Field::Name, "laptop");
        typed(&mut v, Field::Target, "laptop");
        assert_eq!(v.key(Key::Enter, &mut list), Effect::Changed);
        assert_eq!(v.message.as_deref(), Some("Saved laptop"));
        // ...which a new form clears; a refused save stays in the form.
        v.key(Key::Add, &mut list);
        assert_eq!(v.message, None);
        typed(&mut v, Field::Name, "pi");
        typed(&mut v, Field::Target, "a b");
        v.key(Key::Enter, &mut list);
        let d = draft(&v);
        assert_eq!(d.field, Field::Target);
        assert_eq!(d.error.as_deref(), Some("A target has no spaces"));
        assert_eq!(v.key(Key::Esc, &mut list), Effect::None);
        assert!(!v.is_editing());
        assert_eq!(v.message, None);
        // Reopening starts clean.
        v.key(Key::Add, &mut list);
        assert_eq!(draft(&v).error, None);
    }

    #[test]
    fn a_letter_with_a_modifier_held_does_nothing() {
        use egui::{Key as K, Modifiers as M};
        for k in [K::A, K::D, K::T, K::J, K::K] {
            assert!(browse_letter(k, M::NONE).is_some(), "{k:?}");
            for m in [M::CTRL, M::ALT, M::SHIFT, M::COMMAND] {
                assert_eq!(browse_letter(k, m), None, "{m:?}+{k:?}");
            }
        }
        assert_eq!(browse_letter(K::D, M::NONE), Some(Key::Delete));
    }

    #[test]
    fn opening_the_add_form_selects_and_scrolls_to_it() {
        let mut v = RemotesView::new();
        let mut list: Vec<Remote> = (0..12).map(|i| r(&format!("r{i}"), "t")).collect();
        v.scroll_to = false;
        v.key(Key::Add, &mut list);
        assert_eq!(v.selected, list.len());
        assert!(v.scroll_to);
        v.key(Key::Esc, &mut list);
        assert_eq!(v.selected, list.len(), "cancel lands on the Add row");
    }

    #[test]
    fn the_browse_hint_offers_only_what_the_selection_can_do() {
        let mut v = RemotesView::new();
        assert!(!v.hint(0).contains("Del"), "{}", v.hint(0));
        assert!(v.hint(1).contains("T test"));
        v.selected = 1; // the Add row
        for h in [v.hint(1), v.hint(0)] {
            assert!(
                !h.contains("edit") && !h.contains("test") && !h.contains("Del"),
                "{h}"
            );
        }
    }

    #[test]
    fn validate_mirrors_the_sanitize_rules() {
        let list = vec![r("desk", "desk")];
        let draft = |name: &str, target: &str| Draft {
            name: name.into(),
            target: target.into(),
            ..Draft::add()
        };
        for (name, target) in [
            ("", "x"),
            ("local", "x"),
            ("has space", "x"),
            (&"n".repeat(remote::MAX_NAME + 1), "x"),
            ("ok", ""),
            ("ok", "-oProxyCommand=x"),
            ("ok", "a b"),
        ] {
            assert!(
                draft(name, target).validate(&list).is_err(),
                "{name:?} {target:?}"
            );
        }
        let ok = draft("laptop", "andy@laptop").validate(&list).unwrap();
        // Whatever validate accepts, sanitize keeps.
        assert_eq!(remote::sanitize(vec![ok.clone()]), vec![ok]);
        // Editing an entry may keep its own name.
        let mut same = Draft::edit(0, &list[0]);
        same.target = "andy@desk".into();
        assert!(same.validate(&list).is_ok());
    }

    #[test]
    fn add_refused_at_the_cap() {
        let list: Vec<Remote> = (0..remote::MAX_REMOTES)
            .map(|i| r(&format!("r{i}"), "t"))
            .collect();
        let d = Draft {
            name: "one-more".into(),
            target: "t".into(),
            ..Draft::add()
        };
        assert!(d.validate(&list).is_err());
    }

    #[test]
    fn esc_cancels_the_form_then_backs_out() {
        let mut v = RemotesView::new();
        let mut list = vec![r("desk", "desk")];
        v.key(Key::Enter, &mut list);
        typed(&mut v, Field::Name, "changed");
        assert_eq!(v.key(Key::Esc, &mut list), Effect::None);
        assert!(!v.is_editing());
        assert_eq!(list[0].name, "desk");
        assert_eq!(v.key(Key::Esc, &mut list), Effect::Close);
    }

    #[test]
    fn delete_asks_first_and_esc_keeps() {
        let mut v = RemotesView::new();
        let mut list = vec![r("desk", "desk"), r("laptop", "laptop")];
        v.key(Key::Delete, &mut list);
        v.key(Key::Down, &mut list); // ignored while confirming
        v.key(Key::Esc, &mut list);
        assert_eq!(list.len(), 2);
        v.key(Key::Down, &mut list);
        v.key(Key::Delete, &mut list);
        assert_eq!(v.key(Key::Enter, &mut list), Effect::Changed);
        assert_eq!(list, vec![r("desk", "desk")]);
        assert_eq!(v.selected, 1); // now the Add row
    }

    #[test]
    fn delete_and_test_do_nothing_on_the_add_row() {
        let mut v = RemotesView::new();
        let mut list = vec![];
        assert_eq!(v.key(Key::Delete, &mut list), Effect::None);
        assert_eq!(v.key(Key::Test, &mut list), Effect::None);
        let mut list = vec![r("desk", "desk")];
        assert_eq!(v.key(Key::Test, &mut list), Effect::Test(0));
    }

    #[test]
    fn suggestions_filter_by_prefix_and_skip_used_targets() {
        let hosts: Vec<String> = ["desk", "Deck", "laptop", "garage"]
            .map(String::from)
            .to_vec();
        let list = vec![r("g", "garage")];
        let mut d = Draft::add();
        assert_eq!(
            suggestions(&hosts, &list, &d),
            vec!["desk", "Deck", "laptop"]
        );
        d.target = "de".into();
        assert_eq!(suggestions(&hosts, &list, &d), vec!["desk", "Deck"]);
        d.target = "desk".into(); // exactly typed: nothing left to complete
        assert!(suggestions(&hosts, &list, &d).is_empty());
        // Editing the entry that uses garage offers it back.
        let d = Draft::edit(0, &list[0]);
        let mut d2 = d.clone();
        d2.target = "gar".into();
        assert_eq!(suggestions(&hosts, &list, &d2), vec!["garage"]);
    }

    #[test]
    fn picking_a_suggestion_by_keyboard_fills_target_and_blank_name() {
        let mut v = RemotesView::new();
        let mut list = vec![];
        v.key(Key::Add, &mut list);
        v.hosts = Some(vec!["desk".into(), "my_pc".into()]);
        v.key(Key::Tab, &mut list); // Target
        v.key(Key::Down, &mut list);
        assert_eq!(v.key(Key::Enter, &mut list), Effect::None); // accepts, does not save
        let Mode::Edit(d) = &v.mode else { panic!() };
        assert_eq!((d.name.as_str(), d.target.as_str()), ("desk", "desk"));
        // A Host name that is not a valid Remote name leaves the name blank.
        let mut d = Draft::add();
        d.accept("my_pc", &[]);
        assert_eq!((d.name.as_str(), d.target.as_str()), ("", "my_pc"));
        // Then Enter saves.
        assert_eq!(v.key(Key::Enter, &mut list), Effect::Changed);
        assert_eq!(list[0].name, "desk");
    }

    #[test]
    fn up_from_the_first_suggestion_returns_to_the_field() {
        let mut v = RemotesView::new();
        let mut list = vec![];
        v.key(Key::Add, &mut list);
        v.hosts = Some(vec!["desk".into()]);
        v.key(Key::Tab, &mut list);
        v.key(Key::Down, &mut list);
        v.key(Key::Up, &mut list);
        let Mode::Edit(d) = &v.mode else { panic!() };
        assert_eq!((d.field, d.pick), (Field::Target, None));
    }

    #[test]
    fn shell_cycles_both_ways_and_only_on_the_shell_field() {
        for s in SHELLS {
            assert_eq!(cycle_shell(cycle_shell(s, true), false), s);
        }
        let mut v = RemotesView::new();
        let mut list = vec![];
        v.key(Key::Add, &mut list);
        v.key(Key::Right, &mut list); // Name field: the TextEdit's cursor key
        let Mode::Edit(d) = &v.mode else { panic!() };
        assert_eq!(d.shell, RemoteShell::Posix);
        v.key(Key::BackTab, &mut list); // wraps to Shell
        v.key(Key::Left, &mut list);
        let Mode::Edit(d) = &v.mode else { panic!() };
        assert_eq!(d.shell, RemoteShell::WindowsPowerShell);
    }

    #[test]
    fn test_result_fills_in_the_shell_for_that_target() {
        let mut list = vec![r("desk", "andy@desk"), r("other", "x")];
        let ok = TestOutcome::Ok(TestReport {
            shell: RemoteShell::Pwsh,
            login_shell: None,
            curl: true,
            agents: vec![],
        });
        assert!(apply_test(&mut list, "andy@desk", &ok));
        assert_eq!(list[0].shell, RemoteShell::Pwsh);
        assert_eq!(list[1].shell, RemoteShell::Posix);
        assert!(!apply_test(&mut list, "andy@desk", &ok)); // already set
        assert!(!apply_test(&mut list, "x", &TestOutcome::NeedsLogin));
    }

    #[test]
    fn finished_test_is_collected_and_saved() {
        let mut v = RemotesView::new();
        let mut list = vec![r("desk", "andy@desk")];
        let (tx, rx) = mpsc::channel();
        v.tests.insert("andy@desk".into(), TestState::Running(rx));
        assert!(!v.poll_tests(&mut list));
        tx.send(TestOutcome::Ok(TestReport {
            shell: RemoteShell::WindowsPowerShell,
            login_shell: None,
            curl: false,
            agents: vec![],
        }))
        .unwrap();
        assert!(v.poll_tests(&mut list));
        assert_eq!(list[0].shell, RemoteShell::WindowsPowerShell);
        assert!(matches!(v.tests["andy@desk"], TestState::Done(_)));
    }
}
