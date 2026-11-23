//! Installing a missing agent CLI on a Remote. The command table is
//! `remote::INSTALL`; this holds what the Remotes pane and the launcher both
//! show: each agent's state from the last Test, the prerequisite gate, the
//! confirm's words, and the launcher's confirm prompt. The install itself is
//! an ordinary Remote Session the user watches (`WindowManager::open_install`);
//! nothing here runs anything. Spec: UI > Installing an agent CLI in
//! `docs/superpowers/specs/2026-11-20-remote-sessions-design.md`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use eframe::egui;

use crate::remote::{Install, Prereq, RemoteShell};
use crate::remote_probe::TestReport;

/// An install opened from foreman this app run, by (target, provider).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// Its Session is still connected.
    Installing,
    /// Its Session ended; Test runs again to see what it left.
    Ended,
}

fn marks() -> &'static Mutex<HashMap<(String, String), Mark>> {
    static MARKS: OnceLock<Mutex<HashMap<(String, String), Mark>>> = OnceLock::new();
    MARKS.get_or_init(Mutex::default)
}

pub fn set_mark(target: &str, provider: &str, mark: Mark) {
    marks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert((target.to_string(), provider.to_string()), mark);
}

pub fn mark_of(target: &str, provider: &str) -> Option<Mark> {
    marks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(target.to_string(), provider.to_string()))
        .copied()
}

/// Whether a missing agent can be installed from here.
#[derive(Clone, Debug, PartialEq)]
pub enum Gate {
    Ready(&'static Install),
    /// The command needs these on the Remote first, and Test did not find
    /// them. Foreman never installs OS packages.
    Needs(Vec<Prereq>),
    /// No vendor command for this shell kind.
    ByHand,
}

/// One agent CLI on one Remote, from the last Test and this run's installs.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentState {
    Found,
    /// Found by a Test after an install from here ended: it still needs a
    /// login.
    Installed,
    Installing,
    /// An install ended and Test is running again.
    Checking,
    Missing {
        gate: Gate,
        /// An install from here ended and Test still does not find it.
        after_install: bool,
    },
}

fn has(report: &TestReport, p: Prereq) -> bool {
    match p {
        Prereq::Curl => report.curl,
        Prereq::Bash => report.bash,
    }
}

/// The gate for an install command against what Test found. `None` (no
/// table entry) is install-by-hand.
pub fn gate(install: Option<&'static Install>, report: &TestReport) -> Gate {
    let Some(i) = install else {
        return Gate::ByHand;
    };
    let missing: Vec<Prereq> = i
        .needs
        .iter()
        .copied()
        .filter(|p| !has(report, *p))
        .collect();
    if missing.is_empty() {
        Gate::Ready(i)
    } else {
        Gate::Needs(missing)
    }
}

/// `provider`'s state. `install` is its table entry for the Remote's shell,
/// `testing` whether a Test is running now, `mark` this run's install.
pub fn agent_state(
    provider: &str,
    install: Option<&'static Install>,
    report: &TestReport,
    testing: bool,
    mark: Option<Mark>,
) -> AgentState {
    let found = report.agents.iter().any(|a| a == provider);
    match (found, mark) {
        (_, Some(Mark::Installing)) => AgentState::Installing,
        (_, Some(Mark::Ended)) if testing => AgentState::Checking,
        (true, Some(Mark::Ended)) => AgentState::Installed,
        (true, None) => AgentState::Found,
        (false, mark) => AgentState::Missing {
            gate: gate(install, report),
            after_install: mark == Some(Mark::Ended),
        },
    }
}

/// Every Test agent's state on a Remote, in report order, from the shared
/// caches. `None` while the Remote has no successful Test this run: then
/// nothing is marked and the launcher behaves as it always has.
pub fn states_on(target: &str, shell: RemoteShell) -> Option<Vec<(&'static str, AgentState)>> {
    states_from(&crate::remote_probe::test_entry(target)?, target, shell)
}

/// [`states_on`] from a Test entry the caller already holds.
pub fn states_from(
    entry: &crate::remote_probe::TestEntry,
    target: &str,
    shell: RemoteShell,
) -> Option<Vec<(&'static str, AgentState)>> {
    let Some(crate::remote_probe::TestOutcome::Ok(report)) = &entry.last else {
        return None;
    };
    Some(
        crate::remote_probe::AGENTS
            .iter()
            .map(|p| {
                let install = crate::remote::install_for(p, shell);
                (
                    *p,
                    agent_state(p, install, report, entry.running, mark_of(target, p)),
                )
            })
            .collect(),
    )
}

/// The logical argv of an install Session for `provider` on `remote`: the
/// table's command for the Remote's shell, handed to that shell. This is
/// what `WindowManager::open_install` gives the remote spawn path.
pub fn session_argv(remote: &crate::remote::Remote, provider: &str) -> Result<Vec<String>, String> {
    let i = crate::remote::install_for(provider, remote.shell).ok_or_else(|| {
        format!(
            "foreman has no install command for {provider} on {}; install it by hand there",
            remote.name
        )
    })?;
    Ok(crate::remote::install_argv(remote.shell, i.command))
}

/// "curl", "curl and bash".
fn and_list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// What choosing to install `provider` on Remote `name` offers: the confirm,
/// or a plain message naming why there is nothing to run. The Remotes pane
/// and the launcher prompt both say exactly this.
#[derive(Clone, Debug, PartialEq)]
pub enum Offer {
    Install {
        provider: &'static str,
        title: String,
        command: &'static str,
        note: String,
    },
    Blocked(String),
}

pub fn offer(name: &str, shell: RemoteShell, provider: &'static str, state: &AgentState) -> Offer {
    let p = provider;
    match state {
        AgentState::Missing {
            gate: Gate::Ready(i),
            ..
        } => Offer::Install {
            provider,
            title: format!("Install {p} on {name}?"),
            command: i.command,
            note: format!(
                "Runs {p}'s own installer in a new Session on {name}, in its home \
                 directory, so you can read the output, answer any prompt or press \
                 Ctrl+C. Then {p} needs a login: run {p} in a Session on {name}."
            ),
        },
        AgentState::Missing {
            gate: Gate::Needs(missing),
            ..
        } => {
            let names: Vec<&str> = missing.iter().map(|m| m.name()).collect();
            let them = if names.len() == 1 { "it" } else { "them" };
            Offer::Blocked(format!(
                "{p} is not on {name}, and its installer needs {} there first. Install \
                 {them} with {name}'s package manager, then Test again.",
                and_list(&names)
            ))
        }
        AgentState::Missing {
            gate: Gate::ByHand, ..
        } => Offer::Blocked(format!(
            "{p} is not on {name}, and foreman has no install command for it on a {} \
             Remote. Install it by hand there, then Test again.",
            crate::remotes_pane::shell_label(shell)
        )),
        AgentState::Installing => Offer::Blocked(format!(
            "{p} is already installing on {name}, in a Session you can switch to."
        )),
        AgentState::Checking => Offer::Blocked(format!(
            "The {p} install on {name} ended; Test is checking it now."
        )),
        AgentState::Found => Offer::Blocked(format!("{p} is already on {name}.")),
        AgentState::Installed => Offer::Blocked(format!(
            "{p} is installed on {name} and needs a login: run {p} in a Session there."
        )),
    }
}

/// One piece of a Remotes-pane row's agents line.
#[derive(Clone, Debug, PartialEq)]
pub enum Piece {
    Text {
        text: String,
        /// The whole story, on hover.
        hover: Option<String>,
    },
    /// An "install <provider>" row button; its hover shows the command.
    Install {
        provider: &'static str,
        hover: String,
    },
}

/// The agents line under a tested Remote's row: found agents together, then
/// each other agent's state in words (or its install button), with agents
/// blocked for the same reason grouped so the line fits the narrowest pane.
/// Every state is a word, never only a colour.
pub fn agents_line(
    name: &str,
    shell: RemoteShell,
    states: &[(&'static str, AgentState)],
) -> Vec<Piece> {
    let text = |text: String, hover: Option<String>| Piece::Text { text, hover };
    let mut out = Vec::new();
    let found: Vec<&str> = states
        .iter()
        .filter(|(_, s)| *s == AgentState::Found)
        .map(|(p, _)| *p)
        .collect();
    if !found.is_empty() {
        out.push(text(format!("has {}", found.join(", ")), None));
    }
    let mut needs: Vec<&str> = Vec::new();
    let mut needed: Vec<&str> = Vec::new();
    let mut by_hand: Vec<&str> = Vec::new();
    for (p, s) in states {
        let hover = match offer(name, shell, p, s) {
            Offer::Install { command, note, .. } => format!("{command}\n\n{note}"),
            Offer::Blocked(why) => why,
        };
        match s {
            AgentState::Found => {}
            AgentState::Installed => {
                out.push(text(format!("{p} installed, needs login"), Some(hover)))
            }
            AgentState::Installing => out.push(text(format!("{p} installing…"), Some(hover))),
            AgentState::Checking => out.push(text(format!("{p} checking…"), Some(hover))),
            AgentState::Missing {
                gate: Gate::Ready(_),
                after_install,
            } => {
                let hover = if *after_install {
                    format!(
                        "The last {p} install ended, but Test does not find {p} on {name}'s \
                         PATH. Read that Session's output.\n\n{hover}"
                    )
                } else {
                    hover
                };
                out.push(Piece::Install { provider: p, hover });
            }
            AgentState::Missing {
                gate: Gate::Needs(missing),
                ..
            } => {
                needs.push(p);
                for m in missing {
                    if !needed.contains(&m.name()) {
                        needed.push(m.name());
                    }
                }
            }
            AgentState::Missing {
                gate: Gate::ByHand, ..
            } => by_hand.push(p),
        }
    }
    if !needs.is_empty() {
        let verb = if needs.len() == 1 { "needs" } else { "need" };
        out.push(text(
            format!("{} {verb} {}", needs.join(", "), and_list(&needed)),
            Some(format!(
                "Not on {name}, and the installer needs {} there first. Install it with \
                 {name}'s package manager, then Test again.",
                and_list(&needed)
            )),
        ));
    }
    if !by_hand.is_empty() {
        out.push(text(
            format!("{}: install by hand", by_hand.join(", ")),
            Some(format!(
                "No install command for {} on a {} Remote. Install by hand on {name}, then \
                 Test again.",
                by_hand.join(", "),
                crate::remotes_pane::shell_label(shell)
            )),
        ));
    }
    out
}

/// The installable missing agents, in report order: the pane's keyboard
/// install path cycles through these.
pub fn installable(states: &[(&'static str, AgentState)]) -> Vec<&'static str> {
    states
        .iter()
        .filter(|(_, s)| {
            matches!(
                s,
                AgentState::Missing {
                    gate: Gate::Ready(_),
                    ..
                }
            )
        })
        .map(|(p, _)| *p)
        .collect()
}

// ---------------------------------------------------------------------------
// The launcher's confirm
// ---------------------------------------------------------------------------

/// A launcher choice of a missing agent, held until the user answers.
#[derive(Clone, Debug, PartialEq)]
pub struct Ask {
    pub remote: String,
    pub provider: &'static str,
    /// The Project's tag (`p7`) whose launcher asked: the install opens there.
    pub project: String,
    pub offer: Offer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Pending,
    Cancelled,
    /// Run the install. Only an [`Offer::Install`] confirms.
    Confirmed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Enter,
    Esc,
}

/// The desktop modal the launcher opens for a missing agent: the same words
/// and command as the Remotes pane's inline confirm.
pub struct Prompt {
    pub ask: Ask,
}

impl Prompt {
    pub fn new(ask: Ask) -> Self {
        Self { ask }
    }

    pub fn key(&self, k: Key) -> Outcome {
        match (k, &self.ask.offer) {
            (Key::Enter, Offer::Install { .. }) => Outcome::Confirmed,
            // A message has nothing to run: Enter only closes it.
            (Key::Enter, Offer::Blocked(_)) | (Key::Esc, _) => Outcome::Cancelled,
        }
    }

    /// The key line, by what Enter does here.
    pub fn hint(&self) -> &'static str {
        match self.ask.offer {
            Offer::Install { .. } => "Enter install · Esc cancel",
            Offer::Blocked(_) => "Enter or Esc close",
        }
    }

    /// Draw the prompt as a desktop modal; it owns the keyboard while open.
    pub fn show(&mut self, ui: &mut egui::Ui) -> Outcome {
        let ctx = ui.ctx().clone();
        let th = crate::theme::live(&ctx);
        let keys = ui.input(|i| {
            let mut keys = Vec::new();
            if i.key_pressed(egui::Key::Escape) {
                keys.push(Key::Esc);
            }
            if i.key_pressed(egui::Key::Enter) {
                keys.push(Key::Enter);
            }
            keys
        });
        for k in keys {
            match self.key(k) {
                Outcome::Pending => {}
                done => return done,
            }
        }

        let screen = ctx.content_rect();
        ui.painter().rect_filled(screen, 0.0, th.scrim);
        let width = (screen.width() - 32.0).clamp(200.0, 440.0);
        let mut acts = Vec::new();
        egui::Area::new(egui::Id::new("remote_install_prompt"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 16.0))
            .show(&ctx, |ui| {
                ui.set_width(width);
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(width);
                    ui.visuals_mut().override_text_color = Some(th.text);
                    let (title, body) = match &self.ask.offer {
                        Offer::Install { title, note, .. } => (title.clone(), note.clone()),
                        Offer::Blocked(why) => (
                            format!("{} is not ready on {}", self.ask.provider, self.ask.remote),
                            why.clone(),
                        ),
                    };
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(title)
                                .color(th.border_focus)
                                .size(14.0)
                                .strong(),
                        )
                        .wrap(),
                    );
                    ui.add(
                        egui::Label::new(egui::RichText::new(body).color(th.dim).size(11.5)).wrap(),
                    );
                    ui.add_space(6.0);
                    egui::Frame::new()
                        .fill(th.sel_bg)
                        .corner_radius(egui::CornerRadius::same(4))
                        .inner_margin(egui::Margin::same(8))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            if let Offer::Install { command, .. } = &self.ask.offer {
                                // The whole command, wrapped: this is what runs.
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(*command)
                                            .monospace()
                                            .size(11.5)
                                            .color(th.text),
                                    )
                                    .wrap(),
                                );
                                ui.add_space(4.0);
                                ui.horizontal(|ui| {
                                    if ui.small_button("install").clicked() {
                                        acts.push(Key::Enter);
                                    }
                                    if ui.small_button("cancel").clicked() {
                                        acts.push(Key::Esc);
                                    }
                                });
                            } else if ui.small_button("close").clicked() {
                                acts.push(Key::Esc);
                            }
                        });
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(self.hint()).color(th.dim).size(11.5));
                });
            });
        let mut outcome = Outcome::Pending;
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

    fn report(curl: bool, bash: bool, agents: &[&str]) -> TestReport {
        TestReport {
            shell: RemoteShell::Posix,
            login_shell: None,
            curl,
            bash,
            agents: agents.iter().map(|a| a.to_string()).collect(),
        }
    }

    fn posix(p: &str) -> Option<&'static Install> {
        crate::remote::install_for(p, RemoteShell::Posix)
    }

    #[test]
    fn the_gate_names_each_missing_prerequisite() {
        let codex = posix("codex");
        let claude = posix("claude");
        assert_eq!(
            gate(codex, &report(true, false, &[])),
            Gate::Ready(codex.unwrap())
        );
        assert_eq!(
            gate(claude, &report(true, false, &[])),
            Gate::Needs(vec![Prereq::Bash])
        );
        assert_eq!(
            gate(claude, &report(false, false, &[])),
            Gate::Needs(vec![Prereq::Curl, Prereq::Bash])
        );
        assert_eq!(gate(None, &report(true, true, &[])), Gate::ByHand);
        // Windows installers need nothing Test can miss.
        let win = crate::remote::install_for("grok", RemoteShell::Pwsh);
        assert!(matches!(
            gate(win, &report(false, false, &[])),
            Gate::Ready(_)
        ));
    }

    #[test]
    fn agent_state_follows_the_test_and_this_run_s_install() {
        let r = report(true, true, &["claude"]);
        let st = |p, testing, mark| agent_state(p, posix(p), &r, testing, mark);
        assert_eq!(st("claude", false, None), AgentState::Found);
        assert!(matches!(
            st("grok", false, None),
            AgentState::Missing {
                gate: Gate::Ready(_),
                after_install: false
            }
        ));
        assert_eq!(
            st("grok", false, Some(Mark::Installing)),
            AgentState::Installing
        );
        assert_eq!(st("grok", true, Some(Mark::Ended)), AgentState::Checking);
        assert!(matches!(
            st("grok", false, Some(Mark::Ended)),
            AgentState::Missing {
                after_install: true,
                ..
            }
        ));
        assert_eq!(
            st("claude", false, Some(Mark::Ended)),
            AgentState::Installed
        );
        // A missing table entry offers no install.
        assert!(matches!(
            agent_state("grok", None, &r, false, None),
            AgentState::Missing {
                gate: Gate::ByHand,
                ..
            }
        ));
    }

    #[test]
    fn the_offer_names_the_remote_and_shows_the_exact_command() {
        let r = report(true, true, &[]);
        let s = agent_state("grok", posix("grok"), &r, false, None);
        let Offer::Install {
            title,
            command,
            note,
            ..
        } = offer("desk", RemoteShell::Posix, "grok", &s)
        else {
            panic!("ready to install")
        };
        assert_eq!(title, "Install grok on desk?");
        assert_eq!(command, posix("grok").unwrap().command);
        assert!(
            note.contains("new Session on desk") && note.contains("login"),
            "{note}"
        );

        let blocked = |r: &TestReport| {
            let s = agent_state("grok", posix("grok"), r, false, None);
            match offer("desk", RemoteShell::Posix, "grok", &s) {
                Offer::Blocked(m) => m,
                o => panic!("{o:?}"),
            }
        };
        let m = blocked(&report(false, false, &[]));
        assert!(m.contains("needs curl and bash there first"), "{m}");
        assert!(m.contains("desk's package manager"), "{m}");
        let s = agent_state("grok", None, &r, false, None);
        let Offer::Blocked(m) = offer("desk", RemoteShell::Posix, "grok", &s) else {
            panic!()
        };
        assert!(m.contains("Install it by hand"), "{m}");
    }

    #[test]
    fn the_agents_line_says_every_state_in_words() {
        let r = report(true, false, &["claude"]);
        let states: Vec<_> = crate::remote_probe::AGENTS
            .iter()
            .map(|p| (*p, agent_state(p, posix(p), &r, false, None)))
            .collect();
        let line = agents_line("desk", RemoteShell::Posix, &states);
        // codex pipes to sh, so only grok waits on bash.
        assert_eq!(line.len(), 3, "{line:?}");
        assert!(matches!(&line[0], Piece::Text { text, .. } if text == "has claude"));
        assert!(
            matches!(&line[1], Piece::Install { provider: "codex", hover } if hover.starts_with("curl -fsSL https://chatgpt.com/codex/install.sh | sh"))
        );
        assert!(matches!(&line[2], Piece::Text { text, .. } if text == "grok needs bash"));
        assert_eq!(installable(&states), ["codex"]);

        // No curl: every Posix installer is blocked, said once.
        let r = report(false, false, &[]);
        let states: Vec<_> = crate::remote_probe::AGENTS
            .iter()
            .map(|p| (*p, agent_state(p, posix(p), &r, false, None)))
            .collect();
        let line = agents_line("desk", RemoteShell::Posix, &states);
        assert!(
            matches!(&line[..], [Piece::Text { text, .. }] if text == "claude, codex, grok need curl and bash"),
            "{line:?}"
        );
        assert!(installable(&states).is_empty());

        let words = |s: AgentState| match &agents_line("desk", RemoteShell::Posix, &[("grok", s)])[..]
        {
            [Piece::Text { text, .. }] => text.clone(),
            l => panic!("{l:?}"),
        };
        assert_eq!(words(AgentState::Installing), "grok installing…");
        assert_eq!(words(AgentState::Checking), "grok checking…");
        assert_eq!(words(AgentState::Installed), "grok installed, needs login");
        assert_eq!(
            words(AgentState::Missing {
                gate: Gate::ByHand,
                after_install: false
            }),
            "grok: install by hand"
        );
    }

    /// What the install Session hands the remote spawn path: the table's
    /// command for that Remote's shell, as the logical argv.
    #[test]
    fn the_session_argv_is_the_table_command_for_the_remote_s_shell() {
        for shell in [
            RemoteShell::Posix,
            RemoteShell::Pwsh,
            RemoteShell::WindowsPowerShell,
        ] {
            let desk = crate::remote::Remote {
                name: "desk".into(),
                target: "desk".into(),
                shell,
                dirs: Default::default(),
            };
            let argv = session_argv(&desk, "codex").unwrap();
            let command = crate::remote::install_for("codex", shell).unwrap().command;
            assert_eq!(argv, crate::remote::install_argv(shell, command));
            assert_eq!(crate::remote::install_of(&argv), Some("codex"));
            let e = session_argv(&desk, "aider").unwrap_err();
            assert!(e.contains("desk") && e.contains("by hand"), "{e}");
        }
    }

    #[test]
    fn the_prompt_confirms_only_an_install() {
        let ask = |offer| Ask {
            remote: "desk".into(),
            provider: "grok",
            project: "p1".into(),
            offer,
        };
        let install = Prompt::new(ask(Offer::Install {
            provider: "grok",
            title: "Install grok on desk?".into(),
            command: "curl x | bash",
            note: String::new(),
        }));
        assert_eq!(install.key(Key::Enter), Outcome::Confirmed);
        assert_eq!(install.key(Key::Esc), Outcome::Cancelled);
        assert_eq!(install.hint(), "Enter install · Esc cancel");
        let blocked = Prompt::new(ask(Offer::Blocked("needs curl".into())));
        assert_eq!(blocked.key(Key::Enter), Outcome::Cancelled);
        assert_eq!(blocked.hint(), "Enter or Esc close");
    }
}
