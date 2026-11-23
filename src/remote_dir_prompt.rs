//! The one-time "which directory on this Remote?" prompt. The first launch on
//! a Remote from a Project with no mapping asks here, and the answer is saved
//! to `Remote::dirs` so it never asks again. Spec: Launch > Directory in
//! `docs/superpowers/specs/2026-11-20-remote-sessions-design.md`.

use eframe::egui;

use crate::landing::SessionKind;
use crate::remote::RemoteShell;
use crate::remote_browse_ui::{Browser, Nav};
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
        browse.set_source(&ask.target, ask.shell);
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
        let rows = !list && self.browse.has_rows(&self.text);
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
            } else if rows && i.consume_key(none, egui::Key::ArrowDown) {
                keys.push(Key::Nav(Nav::Down));
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
        let row_h = ui.spacing().interact_size.y;
        // What the window has left under the rest of the prompt, in whole
        // rows: the list scrolls, the buttons stay on screen.
        let room = screen.height() - 32.0 - self.other_h - Browser::height(ui, 0.0);
        let body_h = (room / row_h).floor().clamp(MIN_ROWS, MAX_ROWS) * row_h;
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
                                if browse.show(ui, text, body_h, true) {
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
            } else {
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

/// The key hint, true for who has the keyboard: the list's keys, or the
/// field's (offering the list only when there is one to enter).
fn hint(browse: &Browser, text: &str) -> &'static str {
    if browse.list_focused() {
        "↑/↓ pick · Enter/→ go in · ← up · Tab type · Esc cancel"
    } else if browse.has_rows(text) {
        "Enter open · ↓ folders · Esc cancel"
    } else {
        "Enter open · Esc cancel"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    use crate::remote_browse::{Entry, Listed, State};

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

    /// Loading, a listing, an empty directory, a long failure and a cut
    /// listing: the list area, and the buttons under it, never move.
    #[test]
    fn the_list_area_is_one_height_in_every_state() {
        let many: Vec<String> = (0..40).map(|i| format!("folder{i}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let mut cut = listing("/home/andy", &many);
        if let State::Listed(l) = &mut cut {
            l.truncated = true;
        }
        let states = [
            None,
            Some(listing("/home/andy", &["apps", "src"])),
            Some(listing("/home/andy", &[])),
            Some(State::Failed(crate::remote_browse::NEEDS_LOGIN.into())),
            Some(cut),
        ];
        let mut seen = Vec::new();
        for state in states {
            let ctx = egui::Context::default();
            let mut p = DirPrompt::new(ask(None));
            if let Some(st) = state.clone() {
                p.browse.put("andy@desk", RemoteShell::Posix, "", st);
            }
            for _ in 0..3 {
                frame(&ctx, &mut p, WIDE, vec![]);
            }
            let area = p.browse.laid.area;
            assert!(area.height() > 0.0);
            // Every state but a plain listing says itself in words (the cut
            // line sits under the rows, scrolled to).
            let plain =
                matches!(&state, Some(State::Listed(l)) if !l.truncated && !l.entries.is_empty());
            assert_eq!(p.browse.laid.message.is_some(), !plain, "{state:?}");
            seen.push((area, p.laid.buttons.clone(), format!("{state:?}")));
        }
        for (area, buttons, state) in &seen[1..] {
            assert_eq!(*area, seen[0].0, "list area moved: {state}");
            assert_eq!(*buttons, seen[0].1, "buttons moved: {state}");
        }
    }

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

    /// A Remote that needs a password: the list says so, Down stays the
    /// field's, typing works, and Enter opens what was typed, as before.
    #[test]
    fn a_remote_that_cannot_be_listed_types_and_opens_as_before() {
        let ctx = egui::Context::default();
        let mut p = DirPrompt::new(ask(None));
        for dir in ["", "/srv/"] {
            p.browse.put(
                "andy@desk",
                RemoteShell::Posix,
                dir,
                State::Failed(crate::remote_browse::NEEDS_LOGIN.into()),
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
