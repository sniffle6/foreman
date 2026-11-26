//! Installing a missing agent CLI on a Remote. The command table is
//! `remote::INSTALL`; this holds what the Remotes pane and the launcher both
//! show: each agent's state from the last Test, the prerequisite gate, the
//! confirm's words, and the launcher's confirm prompt. The install itself is
//! an ordinary Remote Session the user watches (`WindowManager::open_install`);
//! nothing here runs anything. Spec: UI > Installing an agent CLI in
//! `docs/superpowers/specs/2026-11-20-remote-sessions-design.md`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
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

/// Bumped by every mark change, so a cached agents line can tell it is
/// stale without taking the marks lock.
static MARKS_GENERATION: AtomicU64 = AtomicU64::new(0);

pub fn marks_generation() -> u64 {
    MARKS_GENERATION.load(Ordering::Relaxed)
}

pub fn set_mark(target: &str, provider: &str, mark: Mark) {
    marks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert((target.to_string(), provider.to_string()), mark);
    MARKS_GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// An agent Session for `provider` opened on `target`: an ended install's
/// "needs login" is over (the user is logging in, or has). A
/// running install keeps its mark.
pub fn clear_ended(target: &str, provider: &str) {
    let mut m = marks().lock().unwrap_or_else(|e| e.into_inner());
    let key = (target.to_string(), provider.to_string());
    if m.get(&key) == Some(&Mark::Ended) {
        m.remove(&key);
        MARKS_GENERATION.fetch_add(1, Ordering::Relaxed);
    }
}

/// A Remote Session launched with `argv` opened on `target`: when its
/// program is an agent ([`crate::remote::program_agent`], an accepted
/// `npx` form included), that provider's "needs login" is over. A
/// command that only names one (`vi codex.md`) clears nothing, and neither
/// does an agent run through `wsl`: it runs inside a distro, with that
/// distro's home and login, while the mark is for the agent the install put
/// on the Windows host itself.
pub fn agent_opened(target: &str, argv: &[String]) {
    if let Some(p) = crate::remote::program_agent(argv)
        && p.via != Some(crate::remote::Wrapper::Wsl)
    {
        clear_ended(target, p.agent);
    }
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

/// The last check before an install Session opens, against the live Test
/// answer and marks for the live Remote: both confirms (the pane's row and
/// the launcher's prompt) showed a snapshot, and a Test may have landed or
/// another install started since. Only a missing agent whose installer is
/// ready runs; anything else is the plain message why not. `states` is
/// [`states_on`] for the Remote named `name`.
pub fn ready_to_run(
    name: &str,
    shell: RemoteShell,
    provider: &'static str,
    states: Option<&[(&'static str, AgentState)]>,
) -> Result<&'static Install, String> {
    let Some(state) = states.and_then(|s| s.iter().find(|(p, _)| *p == provider)) else {
        return Err(format!(
            "Test {name} again in Settings > Remotes: foreman installs {provider} only \
             where the last Test did not find it"
        ));
    };
    match (&state.1, offer(name, shell, provider, &state.1)) {
        (
            AgentState::Missing {
                gate: Gate::Ready(i),
                ..
            },
            _,
        ) => Ok(i),
        (_, Offer::Blocked(why)) => Err(why),
        (_, Offer::Install { title, .. }) => Err(title),
    }
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

/// Every PowerShell install command is untested: no Windows Remote has
/// existed to run one against.
fn untested(shell: RemoteShell) -> bool {
    crate::remote::InstallShell::from(shell) == crate::remote::InstallShell::PowerShell
}

/// The pane confirm's footer: what hover used to hide. Where the Session
/// opens, that a login follows, and, for PowerShell, that the command is
/// untested.
pub fn confirm_footer(project: &str, provider: &str, shell: RemoteShell) -> String {
    let mut s = format!("Opens in {project}; {provider} needs a login after.");
    if untested(shell) {
        s.push_str(" Untested on Windows Remotes.");
    }
    s
}

/// The pane confirm's question, in the row's text room. Shortened, never
/// cut, so the Remote's name always shows (the command block and footer
/// name the agent too): the first candidate that `fits` one line, else the
/// shortest, which the pane wraps onto the row's second line.
pub fn question(provider: &str, name: &str, fits: impl Fn(&str) -> bool) -> String {
    let candidates = [
        format!("Install {provider} on {name}?"),
        format!("{provider} on {name}?"),
        format!("On {name}?"),
    ];
    let last = candidates.len() - 1;
    candidates
        .into_iter()
        .enumerate()
        .find(|(k, q)| *k == last || fits(q))
        .map(|(_, q)| q)
        .unwrap_or_default()
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
                 Ctrl+C. Then {p} needs a login: run {p} in a Session on {name}.{}",
                if untested(shell) {
                    " This PowerShell command is untested on Windows Remotes."
                } else {
                    ""
                }
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
    /// An install row button for `provider`, drawn in the line's
    /// right-hand cluster; its hover shows the command.
    Install {
        provider: &'static str,
        /// "install grok"; after installs that did not finish, one "retry"
        /// (the words "did not install" say what it answers): the line must
        /// fit the narrowest pane.
        label: String,
        hover: String,
    },
}

/// The agents line under a tested Remote's row: found agents together, then
/// agents in the same state grouped ("claude, codex need login")
/// so the line fits the narrowest pane, then an install button for each
/// installable one. An install that ended without the agent says so before
/// its button, so a failed or cancelled install never looks untried. Every
/// state is a word, never only a colour.
pub fn agents_line(
    name: &str,
    shell: RemoteShell,
    states: &[(&'static str, AgentState)],
) -> Vec<Piece> {
    let hover_of = |p: &'static str, s: &AgentState| match offer(name, shell, p, s) {
        Offer::Install { command, note, .. } => format!("{command}\n\n{note}"),
        Offer::Blocked(why) => why,
    };
    let of = |want: fn(&AgentState) -> bool| -> Vec<&'static str> {
        states
            .iter()
            .filter(|(_, s)| want(s))
            .map(|(p, _)| *p)
            .collect()
    };
    // One text piece for agents sharing a state: singular and plural words.
    let group = |ps: &[&'static str], one: &str, many: &str| -> Option<Piece> {
        if ps.is_empty() {
            return None;
        }
        let hover = states
            .iter()
            .filter(|(p, _)| ps.contains(p))
            .map(|(p, s)| hover_of(p, s))
            .collect::<Vec<_>>()
            .join("\n\n");
        let words = if ps.len() == 1 { one } else { many };
        Some(Piece::Text {
            text: format!("{} {words}", ps.join(", ")),
            hover: Some(hover),
        })
    };
    let mut out = Vec::new();
    let found = of(|s| *s == AgentState::Found);
    if !found.is_empty() {
        out.push(Piece::Text {
            text: format!("has {}", found.join(", ")),
            hover: None,
        });
    }
    out.extend(group(
        &of(|s| *s == AgentState::Installed),
        "needs login",
        "need login",
    ));
    out.extend(group(
        &of(|s| *s == AgentState::Installing),
        "installing…",
        "installing…",
    ));
    out.extend(group(
        &of(|s| *s == AgentState::Checking),
        "checking…",
        "checking…",
    ));
    let failed = of(|s| {
        matches!(
            s,
            AgentState::Missing {
                gate: Gate::Ready(_),
                after_install: true
            }
        )
    });
    if let Some(first) = failed.first() {
        out.push(Piece::Text {
            text: format!("{} did not install", failed.join(", ")),
            hover: Some(format!(
                "The last install from here ended, but Test does not find {} on {name}'s PATH. Read that Session's output, then install again.",
                and_list(&failed)
            )),
        });
        // One retry, the first button: it opens the confirm on the first,
        // and left/right there reaches the rest.
        let hover = states
            .iter()
            .filter(|(p, _)| failed.contains(p))
            .map(|(p, s)| hover_of(p, s))
            .collect::<Vec<_>>()
            .join(
                "

",
            );
        out.push(Piece::Install {
            provider: first,
            label: "retry".to_string(),
            hover,
        });
    }
    for (p, s) in states {
        if let AgentState::Missing {
            gate: Gate::Ready(_),
            after_install: false,
        } = s
        {
            out.push(Piece::Install {
                provider: p,
                label: format!("install {p}"),
                hover: hover_of(p, s),
            });
        }
    }
    let mut needs: Vec<&str> = Vec::new();
    let mut needed: Vec<&str> = Vec::new();
    for (p, s) in states {
        if let AgentState::Missing {
            gate: Gate::Needs(missing),
            ..
        } = s
        {
            needs.push(p);
            for m in missing {
                if !needed.contains(&m.name()) {
                    needed.push(m.name());
                }
            }
        }
    }
    if !needs.is_empty() {
        let verb = if needs.len() == 1 { "needs" } else { "need" };
        out.push(Piece::Text {
            text: format!("{} {verb} {}", needs.join(", "), needed.join(", ")),
            hover: Some(format!(
                "Not on {name}, and the installer needs {} there first. Install it with \
                 {name}'s package manager, then Test again.",
                and_list(&needed)
            )),
        });
    }
    let by_hand = of(|s| {
        matches!(
            s,
            AgentState::Missing {
                gate: Gate::ByHand,
                ..
            }
        )
    });
    if !by_hand.is_empty() {
        out.push(Piece::Text {
            text: format!("{}: by hand", by_hand.join(", ")),
            hover: Some(format!(
                "No install command for {} on a {} Remote. Install by hand on {name}, then \
                 Test again.",
                by_hand.join(", "),
                crate::remotes_pane::shell_label(shell)
            )),
        });
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
    /// The last Test said missing. That answer can be stale (installed by
    /// hand since), so the prompt also offers launching the agent anyway.
    pub anyway: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Pending,
    Cancelled,
    /// Run the install. Only an [`Offer::Install`] confirms.
    Confirmed,
    /// Launch the agent as if the last Test had found it.
    LaunchAnyway,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Enter,
    Esc,
    /// The "launch anyway" button. No key reaches it: Enter stays on the
    /// safe choice.
    Anyway,
}

/// The desktop modal the launcher opens for a missing agent: the same words
/// and command as the Remotes pane's inline confirm.
pub struct Prompt {
    pub ask: Ask,
    /// The first button has had its opening focus.
    focused: bool,
}

impl Prompt {
    pub fn new(ask: Ask) -> Self {
        Self {
            ask,
            focused: false,
        }
    }

    pub fn key(&self, k: Key) -> Outcome {
        match (k, &self.ask.offer) {
            (Key::Enter, Offer::Install { .. }) => Outcome::Confirmed,
            (Key::Anyway, _) if self.ask.anyway => Outcome::LaunchAnyway,
            (Key::Anyway, _) => Outcome::Pending,
            // A message has nothing to run: Enter only closes it.
            (Key::Enter, Offer::Blocked(_)) | (Key::Esc, _) => Outcome::Cancelled,
        }
    }

    /// The buttons, in order, and the key each one is. The first is what
    /// Enter does.
    pub fn buttons(&self) -> Vec<(&'static str, Key)> {
        let mut b = match self.ask.offer {
            Offer::Install { .. } => vec![("install", Key::Enter), ("cancel", Key::Esc)],
            Offer::Blocked(_) => vec![("close", Key::Esc)],
        };
        if self.ask.anyway {
            b.push(("launch anyway", Key::Anyway));
        }
        b
    }

    /// Why the agent is marked missing and where to check again: the answer
    /// may be older than an install by hand.
    pub fn stale_line(&self) -> Option<String> {
        let Ask {
            remote, provider, ..
        } = &self.ask;
        self.ask.anyway.then(|| {
            format!(
                "The last Test of {remote} did not find {provider}. Installed it since? \
                 Launch anyway, or Test again in Settings > Remotes."
            )
        })
    }

    /// The key line, by what Enter does here.
    pub fn hint(&self) -> &'static str {
        match self.ask.offer {
            Offer::Install { .. } => "Enter install · Esc cancel",
            Offer::Blocked(_) => "Enter or Esc close",
        }
    }

    /// Draw the prompt as a desktop modal; it owns the keyboard while open.
    /// It opens with focus on the first button (what Enter does), so Tab
    /// walks the buttons and Enter presses the focused one, never the
    /// install behind it.
    pub fn show(&mut self, ui: &mut egui::Ui) -> Outcome {
        let ctx = ui.ctx().clone();
        let th = crate::theme::live(&ctx);
        let (esc, enter) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::Escape),
                i.key_pressed(egui::Key::Enter),
            )
        });
        let mut button_focused = false;
        let buttons = self.buttons();
        let take_focus = !std::mem::replace(&mut self.focused, true);
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
                    if let Some(stale) = self.stale_line() {
                        ui.add_space(4.0);
                        ui.add(
                            egui::Label::new(egui::RichText::new(stale).color(th.dim).size(11.5))
                                .wrap(),
                        );
                    }
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
                            }
                            ui.horizontal(|ui| {
                                for (n, (label, k)) in buttons.iter().copied().enumerate() {
                                    let b = ui.small_button(label);
                                    // Focused before this frame: egui turns
                                    // Enter on it into its click.
                                    button_focused |= b.has_focus();
                                    if n == 0 && take_focus {
                                        b.request_focus();
                                    }
                                    if b.clicked() {
                                        acts.push(k);
                                    }
                                }
                            });
                        });
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(self.hint()).color(th.dim).size(11.5));
                });
            });
        if esc {
            acts.insert(0, Key::Esc);
        } else if enter && !button_focused {
            acts.push(Key::Enter);
        }
        for k in acts {
            match self.key(k) {
                Outcome::Pending => {}
                done => return done,
            }
        }
        Outcome::Pending
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
            git: None,
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
            matches!(&line[1], Piece::Install { provider: "codex", label, hover, .. } if label == "install codex" && hover.starts_with("curl -fsSL https://chatgpt.com/codex/install.sh | sh"))
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
            matches!(&line[..], [Piece::Text { text, .. }] if text == "claude, codex, grok need curl, bash"),
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
        assert_eq!(words(AgentState::Installed), "grok needs login");
        assert_eq!(
            words(AgentState::Missing {
                gate: Gate::ByHand,
                after_install: false
            }),
            "grok: by hand"
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
                base: String::new(),
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
        let ask = |offer, anyway| Ask {
            remote: "desk".into(),
            provider: "grok",
            project: "p1".into(),
            offer,
            anyway,
        };
        let install_offer = || Offer::Install {
            provider: "grok",
            title: "Install grok on desk?".into(),
            command: "curl x | bash",
            note: String::new(),
        };
        let install = Prompt::new(ask(install_offer(), false));
        assert_eq!(install.key(Key::Enter), Outcome::Confirmed);
        assert_eq!(install.key(Key::Esc), Outcome::Cancelled);
        assert_eq!(install.hint(), "Enter install · Esc cancel");
        assert_eq!(
            install.buttons(),
            [("install", Key::Enter), ("cancel", Key::Esc)]
        );
        assert_eq!(install.stale_line(), None);
        assert_eq!(install.key(Key::Anyway), Outcome::Pending);
        let blocked = Prompt::new(ask(Offer::Blocked("needs curl".into()), false));
        assert_eq!(blocked.key(Key::Enter), Outcome::Cancelled);
        assert_eq!(blocked.hint(), "Enter or Esc close");
    }

    /// A stale "missing" must not lock the agent out: the prompt offers
    /// "launch anyway", last and never on Enter, and says the last Test did
    /// not find it and where to Test again.
    #[test]
    fn the_prompt_offers_launch_anyway_but_never_on_enter() {
        let ask = |offer| Ask {
            remote: "desk".into(),
            provider: "grok",
            project: "p1".into(),
            offer,
            anyway: true,
        };
        let install = Prompt::new(ask(Offer::Install {
            provider: "grok",
            title: "Install grok on desk?".into(),
            command: "curl x | bash",
            note: String::new(),
        }));
        assert_eq!(
            install.key(Key::Enter),
            Outcome::Confirmed,
            "Enter installs"
        );
        assert_eq!(install.key(Key::Anyway), Outcome::LaunchAnyway);
        assert_eq!(
            install.buttons(),
            [
                ("install", Key::Enter),
                ("cancel", Key::Esc),
                ("launch anyway", Key::Anyway)
            ]
        );
        assert_eq!(install.hint(), "Enter install · Esc cancel");
        let line = install.stale_line().unwrap();
        assert!(
            line.contains("last Test of desk did not find grok")
                && line.contains("Settings > Remotes"),
            "{line}"
        );

        // Nothing to install (needs curl): Enter still only closes.
        let blocked = Prompt::new(ask(Offer::Blocked("needs curl".into())));
        assert_eq!(blocked.key(Key::Enter), Outcome::Cancelled);
        assert_eq!(blocked.key(Key::Anyway), Outcome::LaunchAnyway);
        assert_eq!(
            blocked.buttons(),
            [("close", Key::Esc), ("launch anyway", Key::Anyway)]
        );
    }

    fn line_of(states: &[(&'static str, AgentState)]) -> Vec<Piece> {
        agents_line("desk", RemoteShell::Posix, states)
    }

    fn words(line: &[Piece]) -> Vec<String> {
        line.iter()
            .map(|p| match p {
                Piece::Text { text, .. } => text.clone(),
                Piece::Install { label, .. } => format!("[{label}]"),
            })
            .collect()
    }

    fn ready(after_install: bool) -> AgentState {
        AgentState::Missing {
            gate: Gate::Ready(posix("grok").unwrap()),
            after_install,
        }
    }

    /// Like states are one piece, as needs and by-hand already were, so the
    /// main sequence (install two, a third still to go) fits one line.
    #[test]
    fn the_agents_line_groups_like_states() {
        use AgentState::*;
        assert_eq!(
            words(&line_of(&[
                ("claude", Installed),
                ("codex", Installed),
                ("grok", ready(false))
            ])),
            ["claude, codex need login", "[install grok]"]
        );
        assert_eq!(
            words(&line_of(&[
                ("claude", Installed),
                ("codex", Installed),
                ("grok", Installed)
            ])),
            ["claude, codex, grok need login"]
        );
        assert_eq!(
            words(&line_of(&[
                ("claude", Found),
                ("codex", Installing),
                ("grok", Installing)
            ])),
            ["has claude", "codex, grok installing…"]
        );
        assert_eq!(
            words(&line_of(&[("codex", Checking), ("grok", Checking)])),
            ["codex, grok checking…"]
        );
    }

    /// A failed or cancelled install says so, and still offers the install.
    #[test]
    fn an_install_that_did_not_finish_says_so_and_offers_it_again() {
        let line = line_of(&[("claude", AgentState::Found), ("grok", ready(true))]);
        assert_eq!(
            words(&line),
            ["has claude", "grok did not install", "[retry]"]
        );
        let Piece::Text { hover: Some(h), .. } = &line[1] else {
            panic!("{line:?}")
        };
        assert!(h.contains("Read that Session's output"), "{h}");
        // Never tried: just the button.
        assert_eq!(
            words(&line_of(&[("grok", ready(false))])),
            ["[install grok]"]
        );
        // Several: grouped, with one retry (left/right in the confirm
        // reaches the rest), right after its words and before any untried
        // agent's button.
        let line = line_of(&[
            ("claude", ready(false)),
            ("codex", ready(true)),
            ("grok", ready(true)),
        ]);
        assert_eq!(
            words(&line),
            ["codex, grok did not install", "[retry]", "[install claude]"]
        );
        assert!(matches!(
            &line[1],
            Piece::Install {
                provider: "codex",
                ..
            }
        ));
    }

    /// One frame of the launcher's prompt with `keys` pressed.
    fn prompt_frame(ctx: &egui::Context, p: &mut Prompt, keys: &[egui::Key]) -> Outcome {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            events: keys
                .iter()
                .map(|&key| egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                })
                .collect(),
            ..Default::default()
        };
        let mut out = Outcome::Pending;
        let _ = ctx.run_ui(input, |ui| out = p.show(ui));
        out
    }

    fn install_prompt(anyway: bool) -> Prompt {
        Prompt::new(Ask {
            remote: "desk".into(),
            provider: "grok",
            project: "p1".into(),
            offer: Offer::Install {
                provider: "grok",
                title: "Install grok on desk?".into(),
                command: "curl x | bash",
                note: String::new(),
            },
            anyway,
        })
    }

    /// Enter presses the focused button: Tab off "install" and Enter never
    /// runs the installer.
    #[test]
    fn enter_presses_the_focused_button_not_the_install() {
        use egui::Key::{Enter, Tab};
        let ctx = egui::Context::default();
        let mut p = install_prompt(true);
        assert_eq!(prompt_frame(&ctx, &mut p, &[]), Outcome::Pending);
        assert_eq!(prompt_frame(&ctx, &mut p, &[Tab]), Outcome::Pending);
        assert_eq!(prompt_frame(&ctx, &mut p, &[Enter]), Outcome::Cancelled);

        let ctx = egui::Context::default();
        let mut p = install_prompt(true);
        for keys in [&[][..], &[Tab], &[Tab]] {
            assert_eq!(prompt_frame(&ctx, &mut p, keys), Outcome::Pending);
        }
        assert_eq!(prompt_frame(&ctx, &mut p, &[Enter]), Outcome::LaunchAnyway);

        // Untouched, Enter is still the install, on the first frame too.
        let ctx = egui::Context::default();
        let mut p = install_prompt(false);
        assert_eq!(prompt_frame(&ctx, &mut p, &[Enter]), Outcome::Confirmed);
        let ctx = egui::Context::default();
        let mut p = install_prompt(false);
        assert_eq!(prompt_frame(&ctx, &mut p, &[]), Outcome::Pending);
        assert_eq!(prompt_frame(&ctx, &mut p, &[Enter]), Outcome::Confirmed);
    }

    /// Opening an agent Session for that provider there ends "installed,
    /// needs login"; a running install keeps its mark; every change moves
    /// the generation a cached line keys on.
    #[test]
    fn an_agent_session_clears_only_an_ended_install() {
        let t = "clear-ended@desk";
        set_mark(t, "claude", Mark::Ended);
        let g = marks_generation();
        clear_ended(t, "claude");
        assert_eq!(mark_of(t, "claude"), None);
        assert!(marks_generation() > g);
        set_mark(t, "codex", Mark::Installing);
        clear_ended(t, "codex");
        assert_eq!(mark_of(t, "codex"), Some(Mark::Installing));
        let g = marks_generation();
        clear_ended(t, "grok");
        assert_eq!(marks_generation(), g, "nothing to clear, nothing moved");
    }

    /// An agent opened by itself or through npx ends "needs login"; one run
    /// inside WSL, and a command that only names the agent, do not.
    #[test]
    fn an_agent_opened_through_an_accepted_wrapper_clears_needs_login() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let t = "agent-opened@desk";
        for (argv, provider, clears) in [
            (
                v(&["npx", "-y", "@anthropic-ai/claude-code", "brief x"]),
                "claude",
                true,
            ),
            (
                v(&["npx", "@openai/codex@latest", "brief x"]),
                "codex",
                true,
            ),
            // Inside a distro: not the host's install, so its mark stays.
            (
                v(&["wsl", "-d", "Ubuntu", "--", "claude", "brief x"]),
                "claude",
                false,
            ),
            (v(&["wsl", "codex", "brief x"]), "codex", false),
            (v(&["claude", "brief x"]), "claude", true),
            (v(&["vi", "codex.md"]), "codex", false),
            (v(&["env", "X=1", "claude", "brief x"]), "claude", false),
            (v(&["npx", "claude", "brief x"]), "claude", false),
        ] {
            set_mark(t, provider, Mark::Ended);
            agent_opened(t, &argv);
            let want = if clears { None } else { Some(Mark::Ended) };
            assert_eq!(mark_of(t, provider), want, "{argv:?}");
        }
    }

    /// The last check before a spawn: only a missing agent whose installer
    /// is ready runs; anything else is the offer's own words.
    #[test]
    fn ready_to_run_takes_only_a_ready_missing_agent() {
        use AgentState::*;
        let run = |s: AgentState| {
            ready_to_run("desk", RemoteShell::Posix, "grok", Some(&[("grok", s)][..]))
        };
        assert_eq!(run(ready(false)), Ok(posix("grok").unwrap()));
        assert_eq!(run(ready(true)), Ok(posix("grok").unwrap()), "retry");
        assert_eq!(run(Found).unwrap_err(), "grok is already on desk.");
        assert!(run(Installing).unwrap_err().contains("already installing"));
        assert!(run(Checking).unwrap_err().contains("Test is checking"));
        assert!(run(Installed).unwrap_err().contains("needs a login"));
        let e = run(Missing {
            gate: Gate::Needs(vec![Prereq::Bash]),
            after_install: false,
        })
        .unwrap_err();
        assert!(e.contains("needs bash"), "{e}");
        let e = ready_to_run("desk", RemoteShell::Posix, "grok", None).unwrap_err();
        assert!(e.contains("Test desk again"), "{e}");
    }

    /// The pane's question loses words before the Remote's name.
    #[test]
    fn the_question_keeps_the_remote_name() {
        let all = |_: &str| true;
        assert_eq!(question("grok", "desk", all), "Install grok on desk?");
        let short = |q: &str| q.len() <= 16;
        assert_eq!(question("grok", "desk", short), "grok on desk?");
        let tiny = |q: &str| q.len() <= 9;
        assert_eq!(question("grok", "desk", tiny), "On desk?");
        assert_eq!(question("grok", "desk", |_| false), "On desk?");
    }

    /// Every PowerShell command says, in the confirm's own note and footer,
    /// that it has never been run against a Windows Remote.
    #[test]
    fn powershell_installs_say_they_are_untested() {
        let r = report(true, true, &[]);
        for (shell, untested) in [
            (RemoteShell::Posix, false),
            (RemoteShell::Pwsh, true),
            (RemoteShell::WindowsPowerShell, true),
        ] {
            let i = crate::remote::install_for("grok", shell);
            let s = agent_state("grok", i, &r, false, None);
            let Offer::Install { note, .. } = offer("desk", shell, "grok", &s) else {
                panic!("ready")
            };
            assert_eq!(
                note.contains("untested on Windows Remotes"),
                untested,
                "{note}"
            );
            assert_eq!(
                confirm_footer("proj", "grok", shell).contains("Untested on Windows"),
                untested
            );
        }
        assert_eq!(
            confirm_footer("proj", "grok", RemoteShell::Posix),
            "Opens in proj; grok needs a login after."
        );
    }
}
