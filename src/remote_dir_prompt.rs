//! The one-time "which directory on this Remote?" prompt. The first launch on
//! a Remote from a Project with no mapping asks here, and the answer is saved
//! to `Remote::dirs` so it never asks again. Spec: Launch > Directory in
//! `docs/superpowers/specs/2026-11-20-remote-sessions-design.md`.

use eframe::egui;

use crate::landing::SessionKind;
use crate::remote::RemoteShell;
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
    Enter,
    Esc,
}

pub struct DirPrompt {
    pub ask: Ask,
    pub text: String,
    /// Why the last Enter was refused, drawn in the form.
    pub error: Option<String>,
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

impl DirPrompt {
    pub fn new(ask: Ask) -> Self {
        Self {
            text: ask.prefill.clone().unwrap_or_default(),
            ask,
            error: None,
        }
    }

    pub fn key(&mut self, k: Key) -> Outcome {
        match k {
            Key::Esc => Outcome::Cancelled,
            Key::Enter => match validate(&self.text) {
                Ok(dir) => Outcome::Accepted(dir),
                Err(e) => {
                    self.error = Some(e);
                    Outcome::Pending
                }
            },
        }
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
        for k in ui.input(|i| {
            let mut keys = Vec::new();
            if i.key_pressed(egui::Key::Escape) {
                keys.push(Key::Esc);
            }
            if i.key_pressed(egui::Key::Enter) {
                keys.push(Key::Enter);
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
        let mut acts = Vec::new();
        egui::Area::new(egui::Id::new("remote_dir_prompt"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 16.0))
            .show(&ctx, |ui| {
                ui.set_width(width);
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(width);
                    ui.visuals_mut().override_text_color = Some(th.text);
                    ui.label(
                        egui::RichText::new(format!("Directory on {}", self.ask.remote))
                            .color(th.border_focus)
                            .size(14.0)
                            .strong(),
                    );
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(format!(
                                "Where Sessions from {} start on {}. Asked once and saved for \
                                 this Project; blank is the home directory.",
                                self.ask.project_dir, self.ask.remote
                            ))
                            .color(th.dim)
                            .size(11.5),
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
                                    egui::TextEdit::singleline(&mut self.text)
                                        .id(field_id)
                                        .hint_text(placeholder)
                                        .desired_width(f32::INFINITY),
                                )
                            });
                            if r.changed() {
                                self.error = None;
                            }
                            let indent =
                                crate::remotes_pane::LABEL_W + crate::remotes_pane::FORM_GAP;
                            if let Some(e) = &self.error {
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
                                if ui.small_button("open").clicked() {
                                    acts.push(Key::Enter);
                                }
                                if ui.small_button("cancel").clicked() {
                                    acts.push(Key::Esc);
                                }
                            });
                        });
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("Enter open · Esc cancel")
                            .color(th.dim)
                            .size(11.5),
                    );
                });
            });
        // The only field, and the modal owns the keyboard: keep it focused,
        // including after a refused Enter.
        ctx.memory_mut(|m| m.request_focus(field_id));
        for k in acts {
            match self.key(k) {
                Outcome::Pending => {}
                done => outcome = done,
            }
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(prefill: Option<&str>) -> Ask {
        Ask {
            remote: "desk".into(),
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
    fn a_known_directory_is_offered_first() {
        let p = DirPrompt::new(ask(Some("/srv/app")));
        assert_eq!(p.text, "/srv/app");
    }
}
