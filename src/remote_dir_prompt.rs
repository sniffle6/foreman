//! The one-time "which directory on this Remote?" prompt. The first launch on
//! a Remote from a Project with no mapping asks here, and the answer is saved
//! to `Remote::dirs` so it never asks again. Spec: Launch > Directory in
//! `docs/superpowers/specs/2026-11-20-remote-sessions-design.md`.

use eframe::egui;

use crate::landing::SessionKind;
use crate::remote::RemoteShell;
use crate::remote_browse_ui::{Browser, Enters, Nav, Shows};
use crate::wm::Dir;

/// Longest answer kept. Far past any real path; it bounds what a stuck key
/// or a paste can put into settings.json.
pub const MAX_DIR: usize = 1024;

/// What the launch that asked will open once it has a directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum What {
    Agent(SessionKind),
    Shell,
}

/// Where that launch puts its Win.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// Beside the focused window, like any new terminal.
    Default,
    /// A directional split of the focused window.
    Split(Dir),
}

/// A launch held until the directory is known.
#[derive(Clone, Debug, PartialEq)]
pub struct Ask {
    pub remote: String,
    /// The Remote's ssh target, for listing its folders.
    pub target: String,
    pub shell: RemoteShell,
    /// The seat-PC Project directory: the `Remote::dirs` key.
    pub project_dir: String,
    /// The Project's tag (`p7`), to find it again when the answer comes.
    pub project: String,
    pub what: What,
    pub place: Place,
    /// Filled in when the launch came from a Session on the same Remote that
    /// knows its own directory.
    pub prefill: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Pending,
    /// Esc or cancel: nothing saved, nothing opened.
    Cancelled,
    /// The trimmed answer; blank means the Remote's home.
    Accepted(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// Opens from the field; goes into the selected folder from the list.
    Enter,
    /// Cancels from the field; from the list, returns to the field.
    Esc,
    /// A folder-list key ([`Browser::key`]).
    Nav(Nav),
}

/// Most and fewest folder rows the list shows; between them, what the
/// window has room for, so the buttons stay on screen.
const MAX_ROWS: f32 = 7.0;
const MIN_ROWS: f32 = 2.0;

pub struct DirPrompt {
    pub ask: Ask,
    pub text: String,
    /// Why the last Enter was refused, drawn in the form.
    pub error: Option<String>,
    /// The folder list under the field.
    pub browse: Browser,
    /// The prompt's height without the list, measured last frame: the list
    /// gets what the window has left.
    other_h: f32,
    /// Where the last frame put things, for the layout tests.
    pub laid: Laid,
}

#[derive(Clone, Debug)]
pub struct Laid {
    pub prompt: egui::Rect,
    pub field: egui::Rect,
    pub buttons: Vec<egui::Rect>,
    pub hint: egui::Rect,
    /// The hint fit on its line uncut.
    pub hint_fits: bool,
}

/// One line, bounded. Blank is a real answer (the Remote's home); anything
/// else the launch script quotes as a literal, so no other rule is needed.
pub fn validate(text: &str) -> Result<String, String> {
    let dir = text.trim();
    if dir.chars().any(char::is_control) {
        return Err("A directory is one line: no tabs or line breaks".into());
    }
    if dir.len() > MAX_DIR {
        return Err(format!(
            "Too long for a directory ({MAX_DIR} characters at most)"
        ));
    }
    Ok(dir.to_string())
}

/// The prompt's title: what it is about to open, and where. A prompt raised
/// by a keyboard split says so, or it would come out of nowhere.
pub fn title(ask: &Ask) -> String {
    let what = match ask.what {
        What::Agent(kind) => kind.label(),
        What::Shell => "a shell",
    };
    let on = &ask.remote;
    match ask.place {
        Place::Default => format!("Open {what} on {on}"),
        Place::Split(d) => {
            let side = match d {
                Dir::Left => "left",
                Dir::Right => "right",
                Dir::Up => "up",
                Dir::Down => "down",
            };
            format!("Split {side}: open {what} on {on}")
        }
    }
}

/// The line under the title: what the answer is for, that it is kept, and
/// where to change it, since a wrong answer otherwise sticks.
pub fn explain(ask: &Ask) -> String {
    format!(
        "Which directory on {on} do Sessions from {dir} start in? Blank is the home \
         directory. Saved for this Project; change it in Settings > Remotes.",
        on = ask.remote,
        dir = ask.project_dir,
    )
}

impl DirPrompt {
    pub fn new(ask: Ask) -> Self {
        let mut browse = Browser::new();
        browse.set_source(&ask.target, ask.shell, &ask.remote);
        Self {
            text: ask.prefill.clone().unwrap_or_default(),
            ask,
            error: None,
            browse,
            other_h: 200.0,
            laid: Laid {
                prompt: egui::Rect::NOTHING,
                field: egui::Rect::NOTHING,
                buttons: Vec::new(),
                hint: egui::Rect::NOTHING,
                hint_fits: true,
            },
        }
    }

    pub fn key(&mut self, k: Key) -> Outcome {
        match k {
            // The first Esc leaves the list, the second cancels.
            Key::Esc if self.browse.list_focused() => {
                self.browse.focus_field();
                Outcome::Pending
            }
            Key::Esc => Outcome::Cancelled,
            // One thing per state: the list's Enter never opens.
            Key::Enter if self.browse.list_focused() => self.nav(Nav::Enter),
            Key::Enter => match validate(&self.text) {
                Ok(dir) => Outcome::Accepted(dir),
                Err(e) => {
                    self.error = Some(e);
                    Outcome::Pending
                }
            },
            Key::Nav(n) => self.nav(n),
        }
    }

    fn nav(&mut self, n: Nav) -> Outcome {
        let before = self.text.clone();
        self.browse.key(n, &mut self.text);
        if self.text != before {
            self.error = None;
        }
        Outcome::Pending
    }

    /// The key hint drawn this frame.
    #[cfg(test)]
    pub fn hint(&self) -> &'static str {
        hint(&self.browse, &self.text)
    }

    fn placeholder(&self) -> &'static str {
        match self.ask.shell {
            RemoteShell::Posix => "~/src/project",
            RemoteShell::Pwsh | RemoteShell::WindowsPowerShell => r"C:\src\project",
        }
    }

    /// Draw the prompt as a desktop modal; it owns the keyboard while open.
    pub fn show(&mut self, ui: &mut egui::Ui) -> Outcome {
        let ctx = ui.ctx().clone();
        let th = crate::theme::live(&ctx);
        let mut outcome = Outcome::Pending;
        let list = self.browse.list_focused();
        let rows = !list && self.browse.enters(&self.text) != Enters::None;
        for k in ui.input_mut(|i| {
            let mut keys = Vec::new();
            if i.key_pressed(egui::Key::Escape) {
                keys.push(Key::Esc);
            }
            if i.key_pressed(egui::Key::Enter) {
                keys.push(Key::Enter);
            }
            // Consumed before the field draws, so none moves its caret.
            let none = egui::Modifiers::NONE;
            if list {
                for (key, nav) in [
                    (egui::Key::ArrowUp, Nav::Up),
                    (egui::Key::ArrowDown, Nav::Down),
                    (egui::Key::ArrowLeft, Nav::Left),
                    (egui::Key::ArrowRight, Nav::Right),
                    (egui::Key::Backspace, Nav::Backspace),
                    (egui::Key::Tab, Nav::Tab),
                ] {
                    if i.consume_key(none, key) {
                        keys.push(Key::Nav(nav));
                    }
                }
            } else if i.consume_key(none, egui::Key::ArrowDown) {
                // Always consumed: unread, egui moves the keyboard focus to
                // the next widget (the buttons) for a frame.
                if rows {
                    keys.push(Key::Nav(Nav::Down));
                }
            }
            keys
        }) {
            match self.key(k) {
                Outcome::Pending => {}
                done => return done,
            }
        }

        let screen = ctx.content_rect();
        ui.painter().rect_filled(screen, 0.0, th.scrim);
        let field_id = egui::Id::new("remote_dir_prompt_field");
        let placeholder = self.placeholder();
        let width = (screen.width() - 32.0).clamp(200.0, 440.0);
        let row_h = crate::remote_browse_ui::ROW_H;
        // What the window has left under the rest of the prompt, in whole
        // rows: the list scrolls, the buttons stay on screen.
        let room = screen.height() - 32.0 - self.other_h - Browser::height(ui, 0.0);
        let body_h = (room / row_h).floor().clamp(MIN_ROWS, MAX_ROWS) * row_h;
        if self.browse.route_typing(&ctx, &mut self.text) {
            self.error = None;
        }
        self.browse.pin_caret(&ctx, field_id, &self.text);
        let mut acts = Vec::new();
        let laid = &mut self.laid;
        laid.buttons.clear();
        let browse = &mut self.browse;
        let text = &mut self.text;
        let error = &mut self.error;
        let ask = &self.ask;
        let shown = egui::Area::new(egui::Id::new("remote_dir_prompt"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 16.0))
            .show(&ctx, |ui| {
                ui.set_width(width);
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(width);
                    ui.visuals_mut().override_text_color = Some(th.text);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(title(ask))
                                .color(th.border_focus)
                                .size(14.0)
                                .strong(),
                        )
                        .wrap(),
                    );
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(explain(ask)).color(th.dim).size(11.5),
                        )
                        .wrap(),
                    );
                    ui.add_space(6.0);
                    // The Remotes pane's form: same frame, label column and
                    // error placement.
                    egui::Frame::new()
                        .fill(th.sel_bg)
                        .corner_radius(egui::CornerRadius::same(4))
                        .inner_margin(egui::Margin::same(8))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.spacing_mut().item_spacing =
                                egui::vec2(crate::remotes_pane::FORM_GAP, 6.0);
                            let r = crate::remotes_pane::form_row(ui, "Directory", th.dim, |ui| {
                                ui.add(
                                    egui::TextEdit::singleline(text)
                                        .id(field_id)
                                        .hint_text(placeholder)
                                        .desired_width(f32::INFINITY),
                                )
                            });
                            laid.field = r.rect;
                            if r.changed() {
                                *error = None;
                            }
                            if r.clicked() {
                                browse.focus_field();
                            }
                            let indent =
                                crate::remotes_pane::LABEL_W + crate::remotes_pane::FORM_GAP;
                            ui.horizontal(|ui| {
                                ui.add_space(indent);
                                if browse.show(ui, text, body_h, true, None) {
                                    *error = None;
                                }
                            });
                            if let Some(e) = &*error {
                                ui.horizontal_top(|ui| {
                                    ui.add_space(indent);
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(e).color(th.danger).size(11.5),
                                        )
                                        .wrap(),
                                    );
                                });
                            }
                            ui.horizontal(|ui| {
                                ui.add_space(indent);
                                let open = ui.small_button("open");
                                if open.clicked() {
                                    acts.push(Key::Enter);
                                }
                                let cancel = ui.small_button("cancel");
                                if cancel.clicked() {
                                    acts.push(Key::Esc);
                                }
                                laid.buttons.extend([open.rect, cancel.rect]);
                            });
                        });
                    ui.add_space(4.0);
                    // Truncated, never wrapped, so a focus change moves
                    // nothing; `hint_fits` proves the copy needs no cut.
                    let hint = hint(browse, text);
                    laid.hint_fits = ui
                        .painter()
                        .layout_no_wrap(hint.into(), egui::FontId::proportional(11.5), th.dim)
                        .size()
                        .x
                        <= ui.available_width();
                    laid.hint = ui
                        .add(
                            egui::Label::new(egui::RichText::new(hint).color(th.dim).size(11.5))
                                .truncate(),
                        )
                        .rect;
                });
            });
        self.laid.prompt = shown.response.rect;
        self.other_h = (shown.response.rect.height() - self.browse.laid.area.height()).max(0.0);
        // The modal owns the keyboard: the field keeps it, including after a
        // refused Enter, except while the list has it, so the two never both
        // look focused.
        ctx.memory_mut(|m| {
            if self.browse.list_focused() {
                m.surrender_focus(field_id);
            } else if !m.has_focus(field_id) {
                // Only when it lacks it: a request resets the field's focus
                // lock, and egui would then move focus on an arrow key.
                m.request_focus(field_id);
            }
        });
        for k in acts {
            // A button means what it says, whoever had the keyboard: open
            // must not go into the selected folder.
            self.browse.focus_field();
            match self.key(k) {
                Outcome::Pending => {}
                done => outcome = done,
            }
        }
        outcome
    }
}

/// The key hint, true for who has the keyboard: the list's keys (and how
/// to get back to opening), or the field's (offering the list only when
/// Down enters it).
fn hint(browse: &Browser, text: &str) -> &'static str {
    if browse.list_focused() {
        match browse.shows(text) {
            Shows::Rows => "↑/↓ pick · Enter/→ go in · ← up · Esc, then Enter open",
            Shows::Waiting => "Listing folders · Esc, then Enter open",
            Shows::Retry => "Enter retry · Esc, then Enter open",
        }
    } else {
        match browse.enters(text) {
            Enters::Folders => "Enter open · ↓ folders · Esc cancel",
            Enters::Retry => "Enter open · ↓ retry the folders · Esc cancel",
            Enters::None => "Enter open · Esc cancel",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn ask(prefill: Option<&str>) -> Ask {
        Ask {
            remote: "desk".into(),
            target: "andy@desk".into(),
            shell: RemoteShell::Posix,
            project_dir: r"H:\src\app".into(),
            project: "p1".into(),
            what: What::Shell,
            place: Place::Default,
            prefill: prefill.map(str::to_string),
        }
    }

    #[test]
    fn enter_accepts_the_trimmed_answer_and_blank_means_home() {
        let mut p = DirPrompt::new(ask(None));
        p.text = "  /home/andy/app  ".into();
        assert_eq!(
            p.key(Key::Enter),
            Outcome::Accepted("/home/andy/app".into())
        );
        let mut p = DirPrompt::new(ask(None));
        assert_eq!(p.key(Key::Enter), Outcome::Accepted(String::new()));
    }

    #[test]
    fn a_bad_answer_stays_open_with_the_error() {
        let mut p = DirPrompt::new(ask(None));
        p.text = "/a\n/b".into();
        assert_eq!(p.key(Key::Enter), Outcome::Pending);
        assert!(p.error.as_deref().is_some_and(|e| e.contains("one line")));
        p.text = "x".repeat(MAX_DIR + 1);
        assert_eq!(p.key(Key::Enter), Outcome::Pending);
        assert!(p.error.as_deref().is_some_and(|e| e.contains("Too long")));
    }

    #[test]
    fn esc_cancels_even_with_an_error_showing() {
        let mut p = DirPrompt::new(ask(None));
        p.text = "/a\tb".into();
        assert_eq!(p.key(Key::Enter), Outcome::Pending);
        assert_eq!(p.key(Key::Esc), Outcome::Cancelled);
    }

    #[test]
    fn the_prompt_names_the_action_the_remote_and_where_to_change_it() {
        let mut a = ask(None);
        assert_eq!(title(&a), "Open a shell on desk");
        a.what = What::Agent(SessionKind::Claude);
        assert_eq!(title(&a), "Open Claude on desk");
        a.place = Place::Split(Dir::Right);
        assert_eq!(title(&a), "Split right: open Claude on desk");
        let e = explain(&a);
        assert!(e.contains(r"H:\src\app") && e.contains("desk"), "{e}");
        assert!(e.contains("Settings > Remotes"), "{e}");
    }

    #[test]
    fn the_directory_label_fits_the_form_label_column() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            for label in ["Directory", "Project", "Name", "Target", "Shell"] {
                let w = ui
                    .painter()
                    .layout_no_wrap(
                        label.into(),
                        crate::remotes_pane::label_font(),
                        egui::Color32::WHITE,
                    )
                    .size()
                    .x;
                assert!(w <= crate::remotes_pane::LABEL_W, "{label}: {w}");
            }
        });
    }

    #[test]
    fn a_known_directory_is_offered_first() {
        let p = DirPrompt::new(ask(Some("/srv/app")));
        assert_eq!(p.text, "/srv/app");
    }

    use crate::remote_browse::{Entry, Failure, Listed, State};

    fn listing(resolved: &str, names: &[&str]) -> State {
        State::Listed(Listed {
            resolved: resolved.into(),
            entries: names.iter().map(|n| Entry::dir(n)).collect(),
            truncated: false,
        })
    }

    /// One frame of the prompt in a `w`x`h` window, with these events.
    fn frame(
        ctx: &egui::Context,
        p: &mut DirPrompt,
        size: (f32, f32),
        events: Vec<egui::Event>,
    ) -> Outcome {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(size.0, size.1),
            )),
            events,
            ..Default::default()
        };
        let mut out = Outcome::Pending;
        let _ = ctx.run_ui(input, |ui| out = p.show(ui));
        out
    }

    fn key(k: egui::Key) -> Vec<egui::Event> {
        vec![egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]
    }

    fn field_focused(ctx: &egui::Context) -> bool {
        ctx.memory(|m| m.has_focus(egui::Id::new("remote_dir_prompt_field")))
    }

    const WIDE: (f32, f32) = (900.0, 700.0);

    fn cut_listing() -> State {
        let many: Vec<String> = (0..40).map(|i| format!("folder{i}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let mut cut = listing("/home/andy", &many);
        if let State::Listed(l) = &mut cut {
            l.truncated = true;
        }
        cut
    }

    /// Loading (fresh and still trying), a listing, an empty directory, a
    /// kept failure, an unreachable Remote with its retry row, and a cut
    /// listing, from the field and from the list: the list area, and the
    /// buttons under it, never move.
    #[test]
    fn the_list_area_is_one_height_in_every_state() {
        let gone = || {
            crate::remote_browse::unreachable(
                "ssh: connect to host desk port 22: Connection refused",
            )
        };
        let states: [(&str, Option<State>); 7] = [
            ("loading", None),
            ("still trying", None),
            ("listed", Some(listing("/home/andy", &["apps", "src"]))),
            ("empty", Some(listing("/home/andy", &[]))),
            (
                "failed",
                Some(State::Failed(Failure::new(
                    "Permission denied: this directory can't be listed. Type the path instead.",
                ))),
            ),
            ("retry", Some(gone())),
            ("cut", Some(cut_listing())),
        ];
        let mut seen = Vec::new();
        for (name, state) in states {
            for enter in [false, true] {
                let ctx = egui::Context::default();
                let mut p = DirPrompt::new(ask(None));
                if let Some(st) = state.clone() {
                    p.browse.put("andy@desk", RemoteShell::Posix, "", st);
                }
                for _ in 0..2 {
                    frame(&ctx, &mut p, WIDE, vec![]);
                }
                if name == "still trying" {
                    p.browse
                        .age("andy@desk", RemoteShell::Posix, "", Duration::from_secs(4));
                }
                if enter {
                    frame(&ctx, &mut p, WIDE, key(egui::Key::ArrowDown));
                    // Down retried; the Remote still refuses.
                    if name == "retry" {
                        p.browse.put("andy@desk", RemoteShell::Posix, "", gone());
                    }
                }
                frame(&ctx, &mut p, WIDE, vec![]);
                let b = &p.browse.laid;
                assert!(b.area.height() > 0.0);
                let words = b.message_text.clone().unwrap_or_default();
                match name {
                    "loading" => assert_eq!(words, "Listing folders…"),
                    "still trying" => assert_eq!(words, "Still trying to reach desk…"),
                    "listed" | "cut" => assert_eq!(b.message, None, "{name}: {words}"),
                    "retry" => {
                        assert!(words.contains("refused"), "{words}");
                        assert_eq!(b.rows.len(), 1, "the retry row");
                    }
                    _ => assert!(b.message.is_some(), "{name}"),
                }
                seen.push((b.area, p.laid.buttons.clone(), format!("{name} {enter}")));
            }
        }
        for (area, buttons, state) in &seen[1..] {
            assert_eq!(*area, seen[0].0, "list area moved: {state}");
            assert_eq!(*buttons, seen[0].1, "buttons moved: {state}");
        }
    }

    /// A cut listing says so on the breadcrumb line, outside the scrolled
    /// rows, where it is always seen; never as words after the 500th row.
    #[test]
    fn a_cut_listing_says_so_on_the_breadcrumb() {
        let ctx = egui::Context::default();
        let mut p = DirPrompt::new(ask(Some("~/")));
        p.browse
            .put("andy@desk", RemoteShell::Posix, "", cut_listing());
        for _ in 0..2 {
            frame(&ctx, &mut p, WIDE, vec![]);
        }
        let b = &p.browse.laid;
        assert_eq!(
            b.crumb_text,
            format!("Folders in ~ · first {}", crate::remote_browse::CAP)
        );
        assert_eq!(b.message, None);
        p.browse.put(
            "andy@desk",
            RemoteShell::Posix,
            "",
            listing("/home/andy", &["a"]),
        );
        frame(&ctx, &mut p, WIDE, vec![]);
        assert_eq!(p.browse.laid.crumb_text, "Folders in ~");
    }

    /// Typing a new directory waits out the debounce before it lists, and
    /// says nothing about loading meanwhile.
    #[test]
    fn nothing_says_loading_before_anything_runs() {
        let ctx = egui::Context::default();
        let mut p = DirPrompt::new(ask(None));
        p.browse.put(
            "andy@desk",
            RemoteShell::Posix,
            "",
            listing("/home/andy", &["a"]),
        );
        frame(&ctx, &mut p, WIDE, vec![]);
        frame(&ctx, &mut p, WIDE, vec![]);
        frame(&ctx, &mut p, WIDE, vec![egui::Event::Text("/srv/".into())]);
        frame(&ctx, &mut p, WIDE, vec![]);
        assert_eq!(p.text, "/srv/");
        assert_eq!(
            p.browse.peek("andy@desk", RemoteShell::Posix, "/srv/"),
            None
        );
        assert_eq!(p.browse.laid.message_text, None, "debouncing: silent");
    }

    /// The first Esc in the list goes back to the field; the second
    /// cancels. Typing in the list lands in the field.
    #[test]
    fn esc_in_the_list_returns_to_the_field_and_typing_lands_there() {
        let ctx = egui::Context::default();
        let mut p = DirPrompt::new(ask(Some("~/")));
        p.browse.put(
            "andy@desk",
            RemoteShell::Posix,
            "",
            listing("/home/andy", &["apps", "src"]),
        );
        frame(&ctx, &mut p, WIDE, vec![]);
        frame(&ctx, &mut p, WIDE, key(egui::Key::ArrowDown));
        frame(&ctx, &mut p, WIDE, vec![]);
        assert!(p.browse.list_focused());
        assert_eq!(
            p.hint(),
            "↑/↓ pick · Enter/→ go in · ← up · Esc, then Enter open"
        );
        assert!(p.laid.hint_fits);
        assert_eq!(
            frame(&ctx, &mut p, WIDE, key(egui::Key::Escape)),
            Outcome::Pending
        );
        assert!(!p.browse.list_focused());
        frame(&ctx, &mut p, WIDE, vec![]);
        assert!(field_focused(&ctx));
        assert_eq!(
            frame(&ctx, &mut p, WIDE, key(egui::Key::Escape)),
            Outcome::Cancelled
        );

        let mut p = DirPrompt::new(ask(Some("~/")));
        p.browse.put(
            "andy@desk",
            RemoteShell::Posix,
            "",
            listing("/home/andy", &["apps", "src"]),
        );
        frame(&ctx, &mut p, WIDE, vec![]);
        frame(&ctx, &mut p, WIDE, key(egui::Key::ArrowDown));
        frame(&ctx, &mut p, WIDE, vec![]);
        assert!(p.browse.list_focused());
        frame(&ctx, &mut p, WIDE, vec![egui::Event::Text("sr".into())]);
        assert_eq!(p.text, "~/sr");
        assert!(!p.browse.list_focused());
        frame(&ctx, &mut p, WIDE, vec![]);
        assert!(field_focused(&ctx));
        frame(&ctx, &mut p, WIDE, vec![egui::Event::Text("c".into())]);
        assert_eq!(p.text, "~/src", "the field types on");
    }

    /// Down while the directory is still listing enters the list (never
    /// moves the caret or opens) and selects the first folder on arrival;
    /// the hint says what is happening meanwhile.
    #[test]
    fn down_while_loading_waits_in_the_list() {
        let ctx = egui::Context::default();
        let mut p = DirPrompt::new(ask(Some("~/")));
        frame(&ctx, &mut p, WIDE, vec![]);
        assert_eq!(p.hint(), "Enter open · ↓ folders · Esc cancel");
        frame(&ctx, &mut p, WIDE, key(egui::Key::ArrowDown));
        assert!(p.browse.list_focused());
        frame(&ctx, &mut p, WIDE, vec![]);
        assert_eq!(p.hint(), "Listing folders · Esc, then Enter open");
        assert!(p.laid.hint_fits);
        p.browse.put(
            "andy@desk",
            RemoteShell::Posix,
            "",
            listing("/home/andy", &["apps", "src"]),
        );
        frame(&ctx, &mut p, WIDE, vec![]);
        frame(&ctx, &mut p, WIDE, key(egui::Key::Enter));
        assert_eq!(p.text, "~/apps/", "the first folder was selected");
    }

    /// Down enters the list, Right goes in, Left comes back up, Enter in
    /// Down enters the list, Right goes in, Left comes back up, Enter in
    /// the list goes in and never opens, Tab returns, Enter in the field
    /// opens what is typed. The field and the list never both hold focus.
    #[test]
    fn the_list_keys_navigate_and_enter_in_the_field_opens() {
        use egui::Key as K;
        let ctx = egui::Context::default();
        let mut p = DirPrompt::new(ask(None));
        p.browse.put(
            "andy@desk",
            RemoteShell::Posix,
            "",
            listing("/home/andy", &["apps", "src"]),
        );
        p.browse.put(
            "andy@desk",
            RemoteShell::Posix,
            "~/src/",
            listing("/home/andy/src", &["foreman"]),
        );
        frame(&ctx, &mut p, WIDE, vec![]);
        frame(&ctx, &mut p, WIDE, vec![]);
        assert!(field_focused(&ctx));
        assert_eq!(p.hint(), "Enter open · ↓ folders · Esc cancel");
        frame(&ctx, &mut p, WIDE, key(K::ArrowDown));
        assert!(p.browse.list_focused());
        frame(&ctx, &mut p, WIDE, vec![]);
        assert!(!field_focused(&ctx), "one focus at a time");
        frame(&ctx, &mut p, WIDE, key(K::ArrowDown)); // apps -> src
        assert_eq!(
            frame(&ctx, &mut p, WIDE, key(K::ArrowRight)),
            Outcome::Pending
        );
        assert_eq!(p.text, "~/src/");
        assert_eq!(
            frame(&ctx, &mut p, WIDE, key(K::ArrowLeft)),
            Outcome::Pending
        );
        assert_eq!(p.text, "~/");
        // Enter on "src" (selected again after going up) goes in.
        assert_eq!(frame(&ctx, &mut p, WIDE, key(K::Enter)), Outcome::Pending);
        assert_eq!(p.text, "~/src/");
        assert!(p.browse.list_focused());
        frame(&ctx, &mut p, WIDE, key(K::Tab));
        assert!(!p.browse.list_focused());
        frame(&ctx, &mut p, WIDE, vec![]);
        assert!(field_focused(&ctx));
        assert_eq!(
            frame(&ctx, &mut p, WIDE, key(K::Enter)),
            Outcome::Accepted("~/src/".into())
        );
    }

    /// The open button opens what is typed even while the list has the
    /// keyboard, where Enter would go into a folder instead.
    #[test]
    fn the_open_button_opens_from_the_list_too() {
        let ctx = egui::Context::default();
        let mut p = DirPrompt::new(ask(Some("~/src/")));
        p.browse.put(
            "andy@desk",
            RemoteShell::Posix,
            "~/src/",
            listing("/home/andy/src", &["foreman"]),
        );
        frame(&ctx, &mut p, WIDE, vec![]);
        frame(&ctx, &mut p, WIDE, key(egui::Key::ArrowDown));
        frame(&ctx, &mut p, WIDE, vec![]);
        assert!(p.browse.list_focused());
        let at = p.laid.buttons[0].center();
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(&ctx, &mut p, WIDE, vec![egui::Event::PointerMoved(at)]);
        frame(&ctx, &mut p, WIDE, vec![button(true)]);
        assert_eq!(
            frame(&ctx, &mut p, WIDE, vec![button(false)]),
            Outcome::Accepted("~/src/".into())
        );
    }

    /// A directory that cannot be listed: the list says so, Down stays
    /// the field's, typing works, and Enter opens what was typed, as before.
    #[test]
    fn a_directory_that_cannot_be_listed_types_and_opens_as_before() {
        let ctx = egui::Context::default();
        let mut p = DirPrompt::new(ask(None));
        for dir in ["", "/srv/"] {
            p.browse.put(
                "andy@desk",
                RemoteShell::Posix,
                dir,
                State::Failed(Failure::new("Permission denied. Type the path instead.")),
            );
        }
        frame(&ctx, &mut p, WIDE, vec![]);
        frame(&ctx, &mut p, WIDE, vec![]);
        assert_eq!(p.hint(), "Enter open · Esc cancel");
        assert!(p.browse.laid.message.is_some());
        frame(
            &ctx,
            &mut p,
            WIDE,
            vec![egui::Event::Text("/srv/app".into())],
        );
        assert_eq!(p.text, "/srv/app");
        frame(&ctx, &mut p, WIDE, key(egui::Key::ArrowDown));
        assert!(!p.browse.list_focused());
        assert_eq!(
            frame(&ctx, &mut p, WIDE, key(egui::Key::Enter)),
            Outcome::Accepted("/srv/app".into())
        );
    }

    /// A Remote that needs a password (or refused, or timed out) is asked
    /// again: Down retries and waits in the list, a retry row is there to
    /// press, and Enter in the field still opens what is typed.
    #[test]
    fn an_unreachable_remote_offers_a_retry() {
        let ctx = egui::Context::default();
        let mut p = DirPrompt::new(ask(None));
        let login = || {
            crate::remote_browse::outcome(
                b"",
                "andy@desk: Permission denied (publickey,password).",
                crate::remote_browse::Style::Posix,
            )
        };
        p.browse.put("andy@desk", RemoteShell::Posix, "", login());
        frame(&ctx, &mut p, WIDE, vec![]);
        frame(&ctx, &mut p, WIDE, vec![]);
        assert_eq!(p.hint(), "Enter open · ↓ retry the folders · Esc cancel");
        assert!(p.laid.hint_fits);
        assert_eq!(
            p.browse.laid.message_text.as_deref(),
            Some(crate::remote_browse::NEEDS_LOGIN)
        );
        frame(&ctx, &mut p, WIDE, key(egui::Key::ArrowDown));
        assert!(p.browse.list_focused());
        frame(&ctx, &mut p, WIDE, vec![]);
        assert!(
            matches!(
                p.browse
                    .peek("andy@desk", RemoteShell::Posix, "")
                    .as_deref(),
                Some(State::Loading)
            ),
            "asked again"
        );
        // Still refused: the retry row is selected and Enter presses it.
        p.browse.put("andy@desk", RemoteShell::Posix, "", login());
        frame(&ctx, &mut p, WIDE, vec![]);
        assert_eq!(p.hint(), "Enter retry · Esc, then Enter open");
        assert!(p.laid.hint_fits);
        assert_eq!(
            frame(&ctx, &mut p, WIDE, key(egui::Key::Enter)),
            Outcome::Pending
        );
        frame(&ctx, &mut p, WIDE, vec![]);
        assert!(matches!(
            p.browse
                .peek("andy@desk", RemoteShell::Posix, "")
                .as_deref(),
            Some(State::Loading)
        ));
        // Back in the field, Enter opens home as typed.
        frame(&ctx, &mut p, WIDE, key(egui::Key::Escape));
        assert_eq!(
            frame(&ctx, &mut p, WIDE, key(egui::Key::Enter)),
            Outcome::Accepted(String::new())
        );
    }

    /// At the prompt's full width each hint fits its one line, and every
    /// At the prompt's full width each hint fits its one line, and every
    /// row stays inside the list; in a small window the prompt, buttons
    /// included, stays on screen and the list gives up rows instead.
    #[test]
    fn hints_and_rows_fit_and_a_small_window_keeps_the_buttons() {
        let long = "a-folder-name-far-too-long-to-fit-the-list-on-one-line-at-all-really";
        for size in [WIDE, (472.0, 330.0)] {
            let ctx = egui::Context::default();
            let mut p = DirPrompt::new(ask(None));
            p.browse.put(
                "andy@desk",
                RemoteShell::Posix,
                "",
                listing(
                    "/home/andy",
                    &["apps", long, "src", "x1", "x2", "x3", "x4", "x5"],
                ),
            );
            for _ in 0..3 {
                frame(&ctx, &mut p, size, vec![]);
            }
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(size.0, size.1));
            let check = |p: &DirPrompt, state: &str| {
                assert!(p.laid.hint_fits, "{state} {size:?}: hint cut");
                assert!(p.laid.prompt.contains_rect(p.laid.hint), "{state} {size:?}");
                assert!(
                    screen.contains_rect(p.laid.prompt),
                    "{state} {size:?}: {:?}",
                    p.laid.prompt
                );
                for b in &p.laid.buttons {
                    assert!(screen.contains_rect(*b), "{state} {size:?}: button {b:?}");
                }
                let area = p.browse.laid.area;
                assert!(p.browse.laid.rows.len() >= 2);
                for r in &p.browse.laid.rows {
                    assert!(
                        r.min.x >= area.min.x && r.max.x <= area.max.x,
                        "{state}: {r:?}"
                    );
                }
                assert!(area.contains_rect(p.browse.laid.crumb));
            };
            check(&p, "field");
            frame(&ctx, &mut p, size, key(egui::Key::ArrowDown));
            frame(&ctx, &mut p, size, vec![]);
            assert!(p.browse.list_focused());
            check(&p, "list");
        }
    }
}
