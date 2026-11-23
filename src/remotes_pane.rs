//! The Remotes pane of the settings menu: list, add, edit and delete Remotes,
//! with reachability and Test per row. A custom body like the Keybindings
//! pane. The model half (`Draft`, `commit`, `RemotesView::key`) is pure and
//! tested; the egui half below only draws it and feeds it keys.

use std::collections::HashMap;

use eframe::egui;

use crate::remote::{self, Remote, RemoteShell};
use crate::remote_install::{AgentState, Offer, Piece};
use crate::remote_probe::{self, Reach, TestEntry, TestOutcome};

/// What the pane wants the settings menu to do after a frame.
pub enum Outcome {
    Pending,
    /// `Settings::remotes` changed: the menu publishes it and the debounced
    /// write saves it.
    Changed,
    /// Esc while browsing: back to the rail.
    Close,
    /// The user confirmed installing `provider` on the Remote named
    /// `remote`: the desktop opens the install Session in the focused
    /// Project and says how it went with [`RemotesView::note`].
    Install {
        remote: String,
        provider: &'static str,
    },
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

/// The edit form's fields, in Tab order: then one per Project directory.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    Name,
    Target,
    Shell,
    /// The Remote directory of the draft's `dirs[i]`.
    Dir(usize),
}

impl Field {
    /// The next field in Tab order, over a form with `dirs` directory rows.
    fn step(self, forward: bool, dirs: usize) -> Field {
        let n = 3 + dirs;
        let i = match self {
            Field::Name => 0,
            Field::Target => 1,
            Field::Shell => 2,
            Field::Dir(k) => 3 + k.min(dirs.saturating_sub(1)),
        };
        match if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        } {
            0 => Field::Name,
            1 => Field::Target,
            2 => Field::Shell,
            k => Field::Dir(k - 3),
        }
    }
}

/// One Project directory mapping in the edit form.
#[derive(Clone, Debug, PartialEq)]
pub struct DirRow {
    /// The `Remote::dirs` key as stored: the seat-PC Project directory.
    pub project: String,
    /// The directory on the Remote; blank is its home.
    pub remote: String,
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
    /// The entry's Project directory mappings, editable.
    pub dirs: Vec<DirRow>,
    /// Project keys cleared in this draft; removed on save. Kept apart from
    /// `dirs` so a mapping the directory prompt adds while the form is open
    /// survives the save.
    pub cleared: Vec<String>,
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
            dirs: Vec::new(),
            cleared: Vec::new(),
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
            dirs: r
                .dirs
                .iter()
                .map(|(project, remote)| DirRow {
                    project: project.clone(),
                    remote: remote.clone(),
                })
                .collect(),
            cleared: Vec::new(),
        }
    }

    /// Drop the mapping at `k`: the next launch from that Project asks
    /// again. The active field stays on a row that still exists.
    pub fn clear_dir(&mut self, k: usize) {
        if k >= self.dirs.len() {
            return;
        }
        let gone = self.dirs.remove(k);
        self.cleared.push(gone.project);
        self.error = None;
        if let Field::Dir(j) = self.field {
            self.field = match self.dirs.len() {
                0 => Field::Shell,
                n => Field::Dir(if j > k { j - 1 } else { j.min(n - 1) }),
            };
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
                Some(r) => self.merge_dirs(&r.dirs)?,
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

    /// The saved mappings with this draft's edits and clears applied. Starts
    /// from `live`, not from what the form opened with, so a mapping added
    /// meanwhile is kept. Keys are written without a verbatim prefix.
    fn merge_dirs(
        &self,
        live: &std::collections::BTreeMap<String, String>,
    ) -> Result<std::collections::BTreeMap<String, String>, Invalid> {
        let mut out = live.clone();
        for key in &self.cleared {
            out.retain(|seat, _| !remote::same_dir(seat, key));
        }
        for (k, row) in self.dirs.iter().enumerate() {
            let dir = crate::remote_dir_prompt::validate(&row.remote).map_err(|e| {
                invalid(
                    Field::Dir(k),
                    format!("{}: {e}", remote::plain_dir(&row.project)),
                )
            })?;
            out.retain(|seat, _| !remote::same_dir(seat, &row.project));
            out.insert(remote::plain_dir(&row.project), dir);
        }
        Ok(out)
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
    /// Ask to install the selected Remote's first installable missing agent.
    Install,
    /// Forget the focused Project directory row (Ctrl+Backspace, or Delete
    /// in an empty one).
    Clear,
}

/// What a key asks of the caller beyond the model's own state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effect {
    None,
    Changed,
    Close,
    /// Start Test on the Remote at this index.
    Test(usize),
    /// Install this agent on the Remote at this index (confirmed).
    Install(usize, &'static str),
}

#[derive(Clone, Debug, PartialEq)]
enum Mode {
    Browse,
    Edit(Draft),
    ConfirmDelete(usize),
    /// Showing the command for `provider` on row `row`, waiting for Enter.
    ConfirmInstall {
        row: usize,
        provider: &'static str,
    },
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
    /// This pane's copy of the shared Test cache, by target: what it last
    /// drew and applied.
    tests: HashMap<String, TestEntry>,
    /// The keyboard moved the selection, or a form opened; scroll it into
    /// view once.
    scroll_to: bool,
    /// A save was just refused: frames left to bring the error and buttons
    /// into view. More than one, because the frame the error first shows
    /// has grown the form past what that frame's scroll can reach.
    show_error: u8,
    /// Last frame's widget rects, for the layout test.
    laid: Laid,
    /// The Project an install from here opens in (the desktop says, each
    /// frame); `None` = no Project is open, so an install is refused before
    /// its confirm.
    install_project: Option<String>,
    /// Per target: its agents line, built once per Test answer and marks
    /// change instead of every frame.
    agents: HashMap<String, std::sync::Arc<AgentsCache>>,
    /// Test hook: per target, agent states that stand in for the Test
    /// answer and the install marks (both shared with parallel tests).
    #[cfg(test)]
    forced: HashMap<String, Vec<(&'static str, AgentState)>>,
}

/// One row's agents states and line, valid while its key holds.
#[derive(Debug)]
struct AgentsCache {
    /// Test entry generation, marks generation, the Remote's shell and name.
    generation: u64,
    marks: u64,
    shell: RemoteShell,
    name: String,
    states: Option<Vec<(&'static str, AgentState)>>,
    pieces: Vec<Piece>,
}

/// Where the last frame put things, in screen points.
#[derive(Debug)]
pub struct Laid {
    pub pane: egui::Rect,
    /// The edit form's fields and buttons.
    pub form: Vec<egui::Rect>,
    /// Per row: its painted text, and its status and buttons.
    pub rows: Vec<(egui::Rect, egui::Rect)>,
    /// The key hint line, and whether it fit without its "…" cut.
    pub hint: egui::Rect,
    pub hint_fits: bool,
    /// The scrolling list's viewport: what is on screen of the rows.
    pub list: egui::Rect,
    /// Per row: its status line.
    pub status: Vec<egui::Rect>,
    /// The form's error line, while it shows one.
    pub error: Option<egui::Rect>,
    /// The form's Project directories heading fit without its "…" cut.
    pub dirs_note_fits: bool,
    /// Per row: its agents line.
    pub agents: Vec<egui::Rect>,
    /// Every install button drawn.
    pub installs: Vec<egui::Rect>,
    /// The install confirm's command block, while one is open.
    pub confirm: Option<egui::Rect>,
    /// The confirm's command was cut to fit its two lines.
    pub confirm_elided: bool,
    /// The confirm's question as drawn, and whether it fit uncut.
    pub question: Option<(String, bool)>,
    /// Every agents line fits: no words cut, no button past its edge.
    pub agents_fit: bool,
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
            show_error: 0,
            laid: Laid {
                pane: egui::Rect::NOTHING,
                form: Vec::new(),
                rows: Vec::new(),
                hint: egui::Rect::NOTHING,
                hint_fits: true,
                list: egui::Rect::NOTHING,
                status: Vec::new(),
                error: None,
                dirs_note_fits: true,
                agents: Vec::new(),
                installs: Vec::new(),
                confirm: None,
                confirm_elided: false,
                question: None,
                agents_fit: true,
            },
            install_project: None,
            agents: HashMap::new(),
            #[cfg(test)]
            forced: HashMap::new(),
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

    /// Test hook: a finished Test for `target`, as if its probe answered.
    #[cfg(test)]
    pub fn set_test_result(&mut self, target: &str, outcome: TestOutcome) {
        let generation = self.tests.get(target).map_or(1, |e| e.generation + 1);
        self.tests.insert(
            target.to_string(),
            TestEntry {
                generation,
                running: false,
                last: Some(outcome),
            },
        );
    }

    /// Test hook: draw `target`'s agents line from `states`, whatever its
    /// Test said.
    #[cfg(test)]
    pub fn force_agent_states(&mut self, target: &str, states: Vec<(&'static str, AgentState)>) {
        self.forced.insert(target.to_string(), states);
    }

    /// The footer line, set from outside: how an install request went.
    pub fn note(&mut self, text: String) {
        self.message = Some(text);
    }

    /// The Project an install from here would open in, by name.
    pub fn set_install_project(&mut self, project: Option<&str>) {
        if self.install_project.as_deref() != project {
            self.install_project = project.map(str::to_string);
        }
    }

    /// Row `r`'s cached agents line, rebuilt only when its Test answer, the
    /// install marks, or the Remote's name or shell changed. `None` before
    /// any Test.
    fn agents_for(&mut self, r: &Remote) -> Option<std::sync::Arc<AgentsCache>> {
        #[cfg(test)]
        if let Some(states) = self.forced.get(&r.target) {
            return Some(std::sync::Arc::new(AgentsCache {
                generation: 0,
                marks: 0,
                shell: r.shell,
                name: r.name.clone(),
                pieces: crate::remote_install::agents_line(&r.name, r.shell, states),
                states: Some(states.clone()),
            }));
        }
        let entry = self.tests.get(&r.target)?;
        let marks = crate::remote_install::marks_generation();
        if let Some(c) = self.agents.get(&r.target)
            && c.generation == entry.generation
            && c.marks == marks
            && c.shell == r.shell
            && c.name == r.name
        {
            return Some(c.clone());
        }
        let states = crate::remote_install::states_from(entry, &r.target, r.shell);
        let pieces = states
            .as_ref()
            .map(|s| crate::remote_install::agents_line(&r.name, r.shell, s))
            .unwrap_or_default();
        let c = std::sync::Arc::new(AgentsCache {
            generation: entry.generation,
            marks,
            shell: r.shell,
            name: r.name.clone(),
            states,
            pieces,
        });
        self.agents.insert(r.target.clone(), c.clone());
        Some(c)
    }

    /// Open the install confirm for `provider` on row `row`, unless no
    /// Project is open to run it in: then say so now, not after Enter.
    fn ask_install(&mut self, row: usize, provider: &'static str, remotes: &[Remote]) {
        if self.install_project.is_none() {
            self.message = Some(format!(
                "Open a Project first: the {provider} install on {} runs as a Session inside one",
                remotes[row].name
            ));
            return;
        }
        self.mode = Mode::ConfirmInstall { row, provider };
        self.message = None;
    }

    /// The footer while a confirm is open: the Project it opens in and the
    /// login after, on screen rather than on hover.
    fn confirm_footer(&self, remotes: &[Remote]) -> Option<String> {
        let Mode::ConfirmInstall { row, provider } = self.mode else {
            return None;
        };
        let r = remotes.get(row)?;
        let project = self.install_project.as_deref()?;
        Some(crate::remote_install::confirm_footer(
            project, provider, r.shell,
        ))
    }

    /// Each agent's state on `r` from this pane's last Test; `None` before
    /// a successful one.
    fn states(&self, r: &Remote) -> Option<Vec<(&'static str, AgentState)>> {
        crate::remote_install::states_from(self.tests.get(&r.target)?, &r.target, r.shell)
    }

    /// Row `i`'s installable missing agents.
    fn installable(&self, i: usize, remotes: &[Remote]) -> Vec<&'static str> {
        remotes
            .get(i)
            .and_then(|r| self.states(r))
            .map(|s| crate::remote_install::installable(&s))
            .unwrap_or_default()
    }

    /// Why `I` on row `i` has nothing to confirm, in words naming the Remote.
    fn nothing_to_install(&self, r: &Remote) -> String {
        let name = &r.name;
        let Some(states) = self.states(r) else {
            return format!("Test {name} first: T finds which agent CLIs it lacks");
        };
        states
            .iter()
            .find(|(_, s)| *s != AgentState::Found)
            .map(
                |(p, s)| match crate::remote_install::offer(name, r.shell, p, s) {
                    Offer::Blocked(why) => why,
                    Offer::Install { title, .. } => title,
                },
            )
            .unwrap_or_else(|| format!("Every agent CLI Test looks for is on {name}"))
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
                Key::Install if self.selected < len => {
                    match self.installable(self.selected, remotes).first() {
                        Some(&provider) => self.ask_install(self.selected, provider, remotes),
                        None => {
                            self.message = Some(self.nothing_to_install(&remotes[self.selected]))
                        }
                    }
                }
                Key::Esc => return Effect::Close,
                _ => {}
            },
            Mode::ConfirmDelete(i) => match k {
                Key::Enter => return self.delete(i, remotes),
                Key::Esc => {}
                _ => self.mode = Mode::ConfirmDelete(i),
            },
            // The list shrank under the confirm (settings.json reloaded).
            Mode::ConfirmInstall { row, .. } if row >= len => {}
            Mode::ConfirmInstall { row, provider } => {
                let choices = self.installable(row, remotes);
                match k {
                    // A Test that landed meanwhile may have found it.
                    Key::Enter if choices.contains(&provider) => {
                        self.message = Some(format!(
                            "Opening the {provider} install on {}…",
                            remotes[row].name
                        ));
                        return Effect::Install(row, provider);
                    }
                    Key::Enter => self.message = Some(self.nothing_to_install(&remotes[row])),
                    Key::Esc => {}
                    Key::Left | Key::Right | Key::Tab | Key::BackTab | Key::Install
                        if !choices.is_empty() =>
                    {
                        let at = choices.iter().position(|p| *p == provider).unwrap_or(0);
                        let n = choices.len();
                        let next = if matches!(k, Key::Left | Key::BackTab) {
                            (at + n - 1) % n
                        } else {
                            (at + 1) % n
                        };
                        self.mode = Mode::ConfirmInstall {
                            row,
                            provider: choices[next],
                        };
                    }
                    _ => self.mode = Mode::ConfirmInstall { row, provider },
                }
            }
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
                                    // The error and the buttons, on screen.
                                    self.scroll_to = true;
                                    self.show_error = 3;
                                }
                            }
                        }
                    }
                    Key::Tab => {
                        d.field = d.field.step(true, d.dirs.len());
                        d.pick = None;
                        self.scroll_to = true;
                    }
                    Key::BackTab => {
                        d.field = d.field.step(false, d.dirs.len());
                        d.pick = None;
                        self.scroll_to = true;
                    }
                    Key::Down if !offered.is_empty() => {
                        d.pick = Some(d.pick.map_or(0, |p| (p + 1).min(offered.len() - 1)));
                        self.scroll_to = true;
                    }
                    Key::Up if d.pick.is_some() => {
                        d.pick = d.pick.and_then(|p| p.checked_sub(1));
                        self.scroll_to = true;
                    }
                    Key::Down => {
                        d.field = d.field.step(true, d.dirs.len());
                        self.scroll_to = true;
                    }
                    Key::Up => {
                        d.field = d.field.step(false, d.dirs.len());
                        self.scroll_to = true;
                    }
                    Key::Left | Key::Right | Key::Space if d.field == Field::Shell => {
                        d.shell = cycle_shell(d.shell, k != Key::Left);
                    }
                    Key::Clear => {
                        if let Field::Dir(j) = d.field {
                            d.clear_dir(j);
                            self.scroll_to = true;
                        }
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

    /// Pick up the shared Test cache for every listed target (a Test this
    /// pane started, or the re-run after an install Session ended).
    /// Returns whether settings changed.
    fn poll_tests(&mut self, remotes: &mut [Remote]) -> bool {
        // The generation alone first: most frames nothing is new, and then
        // nothing is cloned.
        let fresh: Vec<(String, TestEntry)> = remotes
            .iter()
            .filter(|r| {
                remote_probe::test_generation(&r.target)
                    .is_some_and(|g| self.tests.get(&r.target).is_none_or(|e| e.generation != g))
            })
            .filter_map(|r| Some((r.target.clone(), remote_probe::test_entry(&r.target)?)))
            .collect();
        let mut changed = false;
        for (target, entry) in fresh {
            changed |= self.absorb(&target, entry, remotes);
        }
        changed
    }

    /// Take one shared entry. A newly finished answer copies its shell into
    /// the settings and into an open draft for the same target. Returns
    /// whether settings changed.
    fn absorb(&mut self, target: &str, entry: TestEntry, remotes: &mut [Remote]) -> bool {
        if self
            .tests
            .get(target)
            .is_some_and(|e| e.generation == entry.generation)
        {
            return false;
        }
        let mut changed = false;
        if !entry.running
            && let Some(outcome) = &entry.last
        {
            changed = apply_test(remotes, target, outcome);
            if let (Mode::Edit(d), TestOutcome::Ok(report)) = (&mut self.mode, outcome)
                && d.target.trim() == target
            {
                d.shell = report.shell;
            }
        }
        self.tests.insert(target.to_string(), entry);
        changed
    }

    fn start_test(&mut self, ctx: &egui::Context, target: &str) {
        remote_probe::start_test(ctx, target);
        if let Some(entry) = remote_probe::test_entry(target) {
            self.tests.insert(target.to_string(), entry);
        }
    }

    /// This frame's key hint, one line at the pane's minimum width. Browsing
    /// offers only what the selection can do: the Add row cannot be edited,
    /// tested or deleted.
    fn hint(&self, len: usize) -> &'static str {
        match self.mode {
            Mode::Browse if self.selected < len => {
                "↑/↓ select · Enter edit · A add · T test · Del delete · Esc back"
            }
            Mode::Browse if len == 0 => "Enter or A add · Esc back",
            Mode::Browse => "↑/↓ select · Enter add · Esc back",
            Mode::Edit(Draft {
                field: Field::Dir(_),
                ..
            }) => "Tab next field · Ctrl+Backspace clear · Enter save · Esc cancel",
            Mode::Edit(_) => "Tab next field · ←/→ shell · Enter save · Esc cancel",
            Mode::ConfirmDelete(_) => "Enter delete · Esc keep",
            Mode::ConfirmInstall { .. } => "Enter install · Esc cancel",
        }
    }

    /// [`Self::hint`], offering `I` when the selected row has something to
    /// install, and ←/→ in a confirm with more than one choice.
    fn hint_for(&self, remotes: &[Remote]) -> &'static str {
        match self.mode {
            Mode::Browse if !self.installable(self.selected, remotes).is_empty() => {
                "↑/↓ · Enter edit · A add · T test · I install · Del delete · Esc back"
            }
            Mode::ConfirmInstall { row, .. } if self.installable(row, remotes).len() > 1 => {
                "Enter install · ←/→ other agent · Esc cancel"
            }
            _ => self.hint(remotes.len()),
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
        let mut install = None;
        let mut effect = |view: &mut Self, e: Effect, remotes: &mut Vec<Remote>| match e {
            Effect::Changed => changed = true,
            Effect::Close => close = true,
            Effect::Test(i) => {
                let target = remotes[i].target.clone();
                view.start_test(&ctx, &target);
            }
            Effect::Install(i, provider) => {
                install = Some((remotes[i].name.clone(), provider));
            }
            Effect::None => {}
        };

        if active {
            for k in read_keys(ui, &self.mode) {
                let e = self.key(k, remotes);
                effect(self, e, remotes);
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
        self.laid.status.clear();
        self.laid.agents.clear();
        self.laid.installs.clear();
        self.laid.confirm = None;
        self.laid.confirm_elided = false;
        self.laid.question = None;
        self.laid.agents_fit = true;
        self.laid.error = None;

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
        // Truncated, never wrapped: a second line would push the list down
        // whenever the hint changes. The copy is short enough not to need
        // the cut at the pane's minimum width; `hint_fits` proves it.
        let hint = self.hint_for(remotes);
        self.laid.hint_fits = ui
            .painter()
            .layout_no_wrap(hint.into(), egui::FontId::proportional(11.5), th.dim)
            .size()
            .x
            <= ui.available_width();
        self.laid.hint = ui
            .add(egui::Label::new(egui::RichText::new(hint).color(th.dim).size(11.5)).truncate())
            .rect;
        ui.add_space(8.0);

        let mut acts: Vec<Act> = Vec::new();
        self.laid.list = egui::ScrollArea::vertical()
            .id_salt("remotes_pane")
            .auto_shrink([false, false])
            .max_height((rect.height() - 110.0).max(60.0))
            .show(ui, |ui| self.render_rows(ui, remotes, active, &mut acts))
            .inner_rect;

        ui.add_space(6.0);
        ui.separator();
        if let Some(msg) = self
            .confirm_footer(remotes)
            .or_else(|| self.message.clone())
        {
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
                    let e = self.key(k, remotes);
                    effect(self, e, remotes);
                }
                Act::Test(i) => effect(self, Effect::Test(i), remotes),
                Act::AskInstall(i, provider) => {
                    self.selected = i;
                    // Never over an open form or delete confirm.
                    if matches!(self.mode, Mode::Browse | Mode::ConfirmInstall { .. }) {
                        self.mode = Mode::Browse;
                        self.ask_install(i, provider, remotes);
                    }
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
                Act::ClearDir(k) => {
                    if let Mode::Edit(d) = &mut self.mode {
                        d.clear_dir(k);
                    }
                }
            }
        }

        if let Some((remote, provider)) = install {
            // The settings edit (a shell a Test found) is saved by the
            // debounce on a later change; the install cannot wait.
            Outcome::Install { remote, provider }
        } else if close {
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
            } else if let Mode::ConfirmInstall { row: c, provider } = self.mode
                && c == i
            {
                // The delete confirm's shape: answer buttons, then the
                // question naming the agent and the Remote.
                if row.small_button("cancel").clicked() {
                    acts.push(Act::Key(Key::Esc));
                }
                if row
                    .small_button(egui::RichText::new("install").color(th.border_focus))
                    .clicked()
                {
                    acts.push(Act::Key(Key::Enter));
                }
                // Shortened to fit, never cut: the Remote's name stays.
                let font = egui::TextStyle::Body.resolve(row.style());
                let room = row.available_width();
                let fits = |q: &str| {
                    row.painter()
                        .layout_no_wrap(q.to_string(), font.clone(), th.text)
                        .size()
                        .x
                        <= room
                };
                let q = crate::remote_install::question(provider, &r.name, fits);
                // No candidate fits one line (a longest name of wide
                // glyphs): wrap onto the row's second line, over the row's
                // own text, rather than cut the name.
                let mut job = egui::text::LayoutJob::simple(q.clone(), font.clone(), th.text, room);
                job.wrap.max_rows = 2;
                job.wrap.break_anywhere = true;
                let galley = row.painter().layout_job(job);
                self.laid.question = Some((q, !galley.elided));
                row.add(egui::Label::new(galley));
            } else {
                if row.small_button("delete").clicked() {
                    acts.push(Act::AskDelete(i));
                }
                if row.small_button("edit").clicked() {
                    acts.push(Act::Edit(i));
                }
                let running = self.tests.get(&r.target).is_some_and(|e| e.running);
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
            // Elided to nothing when the controls take the row (a wrapped
            // question): even a lone "…" would run under them.
            let mut text =
                egui::Rect::from_min_size(egui::pos2(left, rect.min.y), egui::Vec2::ZERO);
            let elided = name.elided || sub.elided;
            for (at, g, color) in [(name_at, name, name_color), (sub_at, sub, th.dim)] {
                if g.size().x <= room {
                    text = text.union(egui::Rect::from_min_size(at, g.size()));
                    ui.painter().galley(at, g, color);
                }
            }
            self.laid.rows.push((text, controls));
            if elided
                && ui
                    .ctx()
                    .pointer_hover_pos()
                    .is_some_and(|p| text.contains(p))
            {
                resp.on_hover_text(format!("{}\n{sub_text}", r.name));
            }

            let agents = self.agents_for(r);
            if let Mode::ConfirmInstall { row: c, provider } = self.mode
                && c == i
            {
                // The status and agents lines' room, given to the exact
                // command, so opening the confirm moves nothing.
                let offer = agents
                    .as_ref()
                    .and_then(|a| a.states.as_ref())
                    .and_then(|s| s.iter().find(|(p, _)| *p == provider))
                    .map(|(p, st)| crate::remote_install::offer(&r.name, r.shell, p, st));
                let (rect, elided) = confirm_block(ui, offer.as_ref());
                self.laid.confirm = Some(rect);
                self.laid.confirm_elided = elided;
                ui.add_space(2.0);
                continue;
            }
            let status = match self.tests.get(&r.target) {
                Some(e) if e.running => Some(("Testing…".to_string(), th.dim)),
                Some(TestEntry { last: Some(o), .. }) => {
                    let color = if matches!(o, TestOutcome::Ok(_)) {
                        th.dim
                    } else {
                        th.danger
                    };
                    Some((o.summary(), color))
                }
                // The reach reason is a tooltip; a keyboard user reads it here.
                _ if selected => why.map(|w| (w, th.dim)),
                _ => None,
            };
            self.laid.status.push(status_line(ui, status));
            // Reserved on every row, tested or not, so a Test landing never
            // moves the rows below. A re-run keeps the last answer here.
            let pieces = agents.as_ref().map_or(&[][..], |a| &a.pieces[..]);
            let line = agents_line_ui(ui, pieces, |p| acts.push(Act::AskInstall(i, p)));
            self.laid.agents.push(line.rect);
            self.laid.installs.extend(line.installs);
            self.laid.agents_fit &= line.fits;
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
        let show_error = std::mem::take(&mut self.show_error);
        let ctx = ui.ctx().clone();
        let Mode::Edit(d) = &mut self.mode else {
            return;
        };
        let laid = &mut self.laid.form;
        let hosts = self.hosts.as_deref().unwrap_or_default();
        let name_id = egui::Id::new("remotes_edit_name");
        let target_id = egui::Id::new("remotes_edit_target");
        let dir_id = |k: usize| egui::Id::new(("remotes_edit_dir", k));
        // The model owns which field is active and egui focus follows it,
        // but only while this Win reads the keyboard: typing in a terminal
        // must never land in a field.
        let dirs = d.dirs.len();
        ui.memory_mut(|m| match (active, d.field) {
            (true, Field::Name) => m.request_focus(name_id),
            (true, Field::Target) => m.request_focus(target_id),
            (true, Field::Dir(k)) => m.request_focus(dir_id(k)),
            _ => {
                m.surrender_focus(name_id);
                m.surrender_focus(target_id);
                for k in 0..dirs {
                    m.surrender_focus(dir_id(k));
                }
            }
        });
        // Where this frame's scroll request goes: the active field, or the
        // error and buttons after a refused save.
        let mut active_rect = None;
        let mut error_rect = None;
        let mut note_fits = true;
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
            if d.field == Field::Name {
                active_rect = Some(r.rect);
            }
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
            if d.field == Field::Target {
                active_rect = Some(r.rect);
            }
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
            if d.field == Field::Shell {
                active_rect = Some(r.rect);
            }
            if r.clicked() {
                acts.push(Act::Field(Field::Shell));
                acts.push(Act::Key(Key::Right));
            }
            // The Project directories this Remote has answered for: the one
            // place a wrong directory-prompt answer can be fixed.
            if d.index.is_some() {
                let text = if d.dirs.is_empty() {
                    "No Project directories yet: a Project's first launch asks"
                } else {
                    "Project directories (blank is the home directory)"
                };
                ui.horizontal(|ui| {
                    ui.add_space(LABEL_W + FORM_GAP);
                    note_fits = ui
                        .painter()
                        .layout_no_wrap(text.into(), egui::FontId::proportional(11.0), th.dim)
                        .size()
                        .x
                        <= ui.available_width();
                    ui.add(
                        egui::Label::new(egui::RichText::new(text).color(th.dim).size(11.0))
                            .truncate(),
                    );
                });
            }
            for k in 0..d.dirs.len() {
                let row = &mut d.dirs[k];
                // Each Project starts a pair; the gap ties a Directory to
                // the Project above it, not the one below.
                ui.add_space(PAIR_GAP);
                form_row(ui, "Project", th.dim, |ui| {
                    // Cut from the front: Projects under one long parent
                    // differ at the end.
                    let full = remote::plain_dir(&row.project);
                    let font = egui::FontId::monospace(11.0);
                    let (shown, cut) = elide_front(ui, &full, &font, ui.available_width());
                    let label = ui.add(
                        egui::Label::new(egui::RichText::new(shown).font(font).color(th.text))
                            .extend(),
                    );
                    if cut {
                        label.on_hover_text(full);
                    }
                });
                let r = form_row(ui, "Directory", th.dim, |ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let clear = ui.small_button("clear").on_hover_text(
                            "Forget it: the next launch from this Project asks again",
                        );
                        if clear.clicked() {
                            acts.push(Act::ClearDir(k));
                        }
                        let edit = ui.add(
                            egui::TextEdit::singleline(&mut row.remote)
                                .id(dir_id(k))
                                .lock_focus(true)
                                .hint_text("home directory")
                                .desired_width(f32::INFINITY),
                        );
                        (edit, clear.rect)
                    })
                    .inner
                });
                let (edit, clear) = r;
                laid.push(edit.rect);
                laid.push(clear);
                if d.field == Field::Dir(k) {
                    active_rect = Some(edit.rect);
                }
                if edit.clicked() {
                    acts.push(Act::Field(Field::Dir(k)));
                }
                if edit.changed() {
                    d.error = None;
                }
            }
            // A refused save, in the danger colour like a Keybindings
            // conflict, under the fields; focus is already on the bad one.
            // The line is reserved, so an error appearing does not push the
            // buttons down.
            ui.horizontal_top(|ui| {
                ui.set_min_height(LINE_H);
                ui.add_space(LABEL_W + FORM_GAP);
                if let Some(e) = &d.error {
                    let r = ui.add(
                        egui::Label::new(egui::RichText::new(e).color(th.danger).size(11.5)).wrap(),
                    );
                    error_rect = Some(r.rect);
                }
            });
            ui.horizontal(|ui| {
                ui.add_space(LABEL_W + FORM_GAP);
                let save = if d.index.is_some() { "save" } else { "add" };
                let r = ui.small_button(save);
                laid.push(r.rect);
                let buttons = r.rect;
                if r.clicked() {
                    acts.push(Act::Key(Key::Enter));
                }
                let r = ui.small_button("cancel");
                laid.push(r.rect);
                if r.clicked() {
                    acts.push(Act::Key(Key::Esc));
                }
                error_rect = error_rect.map(|e| e.union(buttons.union(r.rect)));
            });
        });
        self.laid.error = error_rect;
        self.laid.dirs_note_fits = note_fits;
        // After the hosts strip's own scroll, so this one wins the list's.
        // A refused save shows its error with the buttons; otherwise the
        // active field, which on opening is Name at the top of the form.
        let error_to = error_rect.filter(|_| show_error > 0);
        if let Some(to) = error_to {
            // At once, not animated: the buttons must be there for the next
            // click, not on their way. Again next frame until they are.
            ui.scroll_to_rect_animation(to, None, egui::style::ScrollAnimation::none());
            if !ui.clip_rect().contains_rect(to) {
                self.show_error = show_error - 1;
                ctx.request_repaint();
            }
        } else if scroll {
            // On Name (where the form opens) the whole form when it fits, so
            // its buttons show too, below rows that carry two lines each.
            let form = shown.response.rect;
            let on_name = matches!(&self.mode, Mode::Edit(d) if d.field == Field::Name);
            match active_rect {
                Some(r) if !on_name || form.height() > ui.clip_rect().height() => {
                    ui.scroll_to_rect(r, None)
                }
                _ => ui.scroll_to_rect_animation(form, None, egui::style::ScrollAnimation::none()),
            }
        }
        ui.add_space(4.0);
    }
}

/// A row's height: name over target · shell.
const ROW_H: f32 = 42.0;
/// Row text inset, the Keybindings pane's.
const ROW_PAD: f32 = 6.0;
/// The line reserved under each row, and for the hosts strip.
pub(crate) const LINE_H: f32 = 18.0;
/// The form's label column.
pub(crate) const LABEL_W: f32 = 52.0;
pub(crate) const FORM_GAP: f32 = 8.0;

/// The offline tooltip, and the selected row's reason line.
pub(crate) const OFFLINE_WHY: &str = "No TCP answer on its SSH port. This check cannot see past a \
     ProxyJump, so Test it or open a Remote Session to be sure.";

/// Space before each Project/Directory pair in the form.
const PAIR_GAP: f32 = 6.0;

/// `text` on one line, its front cut with "…" so the rest fits `width`;
/// and whether it was cut.
fn elide_front(ui: &egui::Ui, text: &str, font: &egui::FontId, width: f32) -> (String, bool) {
    let w = |t: &str| {
        ui.painter()
            .layout_no_wrap(t.to_owned(), font.clone(), egui::Color32::PLACEHOLDER)
            .size()
            .x
    };
    if w(text) <= width {
        return (text.to_owned(), false);
    }
    // The longest tail that fits: tails only shrink as the start moves right.
    let starts: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    let (mut lo, mut hi) = (0, starts.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        if w(&format!("…{}", &text[starts[mid]..])) <= width {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    let tail = starts.get(lo).map_or("", |&i| &text[i..]);
    (format!("…{tail}"), true)
}

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

/// The form's label font: every label must fit [`LABEL_W`] in it.
pub(crate) fn label_font() -> egui::FontId {
    egui::FontId::proportional(11.5)
}

/// A form row: a dim label in a fixed column, then the field.
pub(crate) fn form_row<R>(
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
            label_font(),
            color,
        );
        field(ui)
    })
    .inner
}

/// The line under a row for its Test or reach reason. Always there and
/// always one line, so a result arriving or the selection passing never
/// moves the rows below; a long one is cut with "…", whole on hover.
fn status_line(ui: &mut egui::Ui, status: Option<(String, egui::Color32)>) -> egui::Rect {
    ui.horizontal(|ui| {
        ui.set_height(LINE_H);
        ui.add_space(ROW_PAD);
        if let Some((text, color)) = status {
            ui.add(egui::Label::new(egui::RichText::new(text).color(color).size(11.0)).truncate());
        }
    })
    .response
    .rect
}

/// What [`agents_line_ui`] drew.
struct AgentsLine {
    rect: egui::Rect,
    installs: Vec<egui::Rect>,
    /// Every piece's words fit uncut and every button is inside the line.
    fits: bool,
}

/// A row's agents line: words for each state, and an "install <agent>"
/// row button beside each agent that can be installed. One line, always.
fn agents_line_ui(
    ui: &mut egui::Ui,
    pieces: &[Piece],
    mut ask: impl FnMut(&'static str),
) -> AgentsLine {
    let th = crate::theme::live(ui.ctx());
    let mut installs = Vec::new();
    let mut fits = true;
    let rect = ui
        .horizontal(|ui| {
            // Buttons never shrink: one past this edge is clipped away.
            let right = ui.max_rect().right();
            ui.set_height(LINE_H);
            ui.add_space(ROW_PAD);
            ui.spacing_mut().item_spacing.x = 4.0;
            for (k, piece) in pieces.iter().enumerate() {
                if k > 0 && !matches!(piece, Piece::Install { retry: true, .. }) {
                    ui.label(egui::RichText::new("·").color(th.dim).size(11.0));
                }
                match piece {
                    Piece::Text { text, hover } => {
                        fits &= ui
                            .painter()
                            .layout_no_wrap(text.clone(), egui::FontId::proportional(11.0), th.dim)
                            .size()
                            .x
                            <= ui.available_width();
                        let l = ui.add(
                            egui::Label::new(egui::RichText::new(text).color(th.dim).size(11.0))
                                .truncate(),
                        );
                        if let Some(h) = hover {
                            l.on_hover_text(h);
                        }
                    }
                    Piece::Install {
                        provider,
                        label,
                        hover,
                        ..
                    } => {
                        let b = ui.small_button(label);
                        installs.push(b.rect);
                        if b.on_hover_text(hover).clicked() {
                            ask(provider);
                        }
                    }
                }
            }
            fits &= ui.min_rect().right() <= right + 0.5;
        })
        .response
        .rect;
    AgentsLine {
        rect,
        installs,
        fits,
    }
}

/// The install confirm's body, in the room of a row's two lines: the exact
/// command, wrapped (two lines hold every command in the table at the
/// narrowest pane; anything longer is cut, whole on hover), and what will
/// happen on hover.
fn confirm_block(ui: &mut egui::Ui, offer: Option<&Offer>) -> (egui::Rect, bool) {
    let th = crate::theme::live(ui.ctx());
    let shown = ui.horizontal_top(|ui| {
        ui.set_height(LINE_H * 2.0);
        ui.add_space(ROW_PAD);
        let (text, hover, mono) = match offer {
            Some(Offer::Install { command, note, .. }) => {
                ((*command).to_string(), format!("{command}\n\n{note}"), true)
            }
            Some(Offer::Blocked(why)) => (why.clone(), why.clone(), false),
            None => {
                let t = "Test this Remote again: its last answer is gone".to_string();
                (t.clone(), t, false)
            }
        };
        let font = if mono {
            egui::FontId::monospace(11.0)
        } else {
            egui::FontId::proportional(11.0)
        };
        let mut job = egui::text::LayoutJob::simple(
            text,
            font,
            th.text,
            (ui.available_width() - ROW_PAD).max(40.0),
        );
        job.wrap.max_rows = 2;
        let galley = ui.painter().layout_job(job);
        let elided = galley.elided;
        ui.add(egui::Label::new(galley).selectable(false))
            .on_hover_text(hover);
        elided
    });
    (shown.response.rect, shown.inner)
}

/// A mouse action, applied after the rows are drawn so the draw never
/// mutates the list it iterates.
enum Act {
    Select(usize),
    Edit(usize),
    Add,
    AskDelete(usize),
    Test(usize),
    /// An install button: open the confirm for that agent.
    AskInstall(usize, &'static str),
    Field(Field),
    Accept(String),
    ClearDir(usize),
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
        egui::Key::I => Key::Install,
        _ => return None,
    })
}

/// This frame's keys for the current mode. Letters only count while
/// browsing: in the form they are typing.
fn read_keys(ui: &egui::Ui, mode: &Mode) -> Vec<Key> {
    use egui::Key as K;
    // Consumed, not peeked: the row goes before the form draws, and the
    // TextEdit that takes its place (and its Id) would delete a word or a
    // character of the next directory.
    let clear = ui.input_mut(|i| match mode {
        Mode::Edit(d) => match d.field {
            Field::Dir(k) => {
                i.consume_key(egui::Modifiers::CTRL, K::Backspace)
                    || (d.dirs.get(k).is_some_and(|r| r.remote.is_empty())
                        && i.consume_key(egui::Modifiers::NONE, K::Delete))
            }
            _ => false,
        },
        _ => false,
    });
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
                for k in [K::K, K::J, K::A, K::D, K::T, K::I] {
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
            Mode::ConfirmInstall { .. } => {
                if p(K::ArrowLeft) {
                    out.push(Key::Left);
                }
                if p(K::ArrowRight) {
                    out.push(Key::Right);
                }
                if p(K::Tab) {
                    out.push(if shift { Key::BackTab } else { Key::Tab });
                }
            }
        }
        if clear {
            out.push(Key::Clear);
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
                Field::Dir(k) => d.dirs[k].remote = text.into(),
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

    fn mapped() -> Remote {
        let mut desk = r("desk", "andy@desk");
        desk.dirs.insert(r"H:\src\app".into(), "/srv/app".into());
        desk.dirs
            .insert(r"\\?\H:\src\tool".into(), "/srv/tool".into());
        desk
    }

    #[test]
    fn a_project_directory_is_edited_in_the_form_and_saved() {
        let mut v = RemotesView::new();
        let mut list = vec![mapped()];
        v.key(Key::Enter, &mut list);
        assert_eq!(draft(&v).dirs.len(), 2);
        // Tab walks Name, Target, Shell, then each directory.
        for _ in 0..3 {
            v.key(Key::Tab, &mut list);
        }
        assert_eq!(draft(&v).field, Field::Dir(0));
        typed(&mut v, Field::Dir(0), "  /home/andy/app  ");
        assert_eq!(v.key(Key::Enter, &mut list), Effect::Changed);
        assert_eq!(list[0].dirs[r"H:\src\app"], "/home/andy/app");
        // Saving rewrites an old verbatim key in the plain spelling.
        assert_eq!(list[0].dirs[r"H:\src\tool"], "/srv/tool");
        assert!(!list[0].dirs.contains_key(r"\\?\H:\src\tool"));
        assert_eq!(list[0].dirs.len(), 2);
    }

    #[test]
    fn a_cleared_project_directory_is_forgotten_so_the_prompt_asks_again() {
        let mut v = RemotesView::new();
        let mut list = vec![mapped()];
        v.key(Key::Enter, &mut list);
        let Mode::Edit(d) = &mut v.mode else { panic!() };
        d.field = Field::Dir(1);
        d.clear_dir(1);
        assert_eq!(d.field, Field::Dir(0), "focus stays on a row that exists");
        d.clear_dir(0);
        assert_eq!(d.field, Field::Shell);
        assert_eq!(v.key(Key::Enter, &mut list), Effect::Changed);
        assert!(list[0].dirs.is_empty(), "{:?}", list[0].dirs);
        let e = crate::remote::launch_dir(&list[0], None, Some(r"H:\src\app")).unwrap_err();
        assert!(e.contains("prompt"), "{e}");
    }

    #[test]
    fn cancel_keeps_the_mappings_and_a_bad_directory_is_refused() {
        let mut v = RemotesView::new();
        let mut list = vec![mapped()];
        v.key(Key::Enter, &mut list);
        let Mode::Edit(d) = &mut v.mode else { panic!() };
        d.clear_dir(0);
        v.key(Key::Esc, &mut list);
        assert_eq!(list[0], mapped(), "cancel saves nothing");
        v.key(Key::Enter, &mut list);
        typed(&mut v, Field::Dir(1), "/a\tb");
        assert_eq!(v.key(Key::Enter, &mut list), Effect::None);
        let d = draft(&v);
        assert_eq!(d.field, Field::Dir(1), "focus moves to the bad directory");
        assert!(
            d.error
                .as_deref()
                .is_some_and(|e| e.contains(r"H:\src\tool")),
            "{:?}",
            d.error
        );
        assert_eq!(list[0], mapped());
    }

    #[test]
    fn a_mapping_added_while_the_form_is_open_survives_its_save() {
        let mut v = RemotesView::new();
        let mut list = vec![mapped()];
        v.key(Key::Enter, &mut list);
        // The directory prompt answers for another Project meanwhile.
        list[0].dirs.insert(r"H:\src\new".into(), "/srv/new".into());
        let Mode::Edit(d) = &mut v.mode else { panic!() };
        d.clear_dir(0);
        assert_eq!(v.key(Key::Enter, &mut list), Effect::Changed);
        assert_eq!(list[0].dirs[r"H:\src\new"], "/srv/new");
        assert!(!list[0].dirs.contains_key(r"H:\src\app"));
    }

    #[test]
    fn the_clear_key_forgets_the_focused_directory_row_only() {
        let mut v = RemotesView::new();
        let mut list = vec![mapped()];
        v.key(Key::Enter, &mut list);
        // On a field that is not a directory, nothing to clear.
        v.key(Key::Clear, &mut list);
        assert_eq!(draft(&v).dirs.len(), 2);
        for _ in 0..4 {
            v.key(Key::Tab, &mut list);
        }
        assert_eq!(draft(&v).field, Field::Dir(1));
        assert!(v.hint(1).contains("Ctrl+Backspace clear"), "{}", v.hint(1));
        v.key(Key::Clear, &mut list);
        let d = draft(&v);
        assert_eq!(d.dirs.len(), 1);
        assert_eq!(d.dirs[0].project, r"H:\src\app");
        assert_eq!(d.field, Field::Dir(0));
        assert_eq!(v.key(Key::Enter, &mut list), Effect::Changed);
        assert_eq!(list[0].dirs.len(), 1, "{:?}", list[0].dirs);
        assert!(!list[0].dirs.contains_key(r"\\?\H:\src\tool"));
    }

    #[test]
    fn a_long_project_path_is_cut_from_the_front() {
        let ctx = egui::Context::default();
        let mut out = Vec::new();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let font = egui::FontId::monospace(11.0);
            let w = |t: &str| {
                ui.painter()
                    .layout_no_wrap(t.into(), font.clone(), egui::Color32::WHITE)
                    .size()
                    .x
            };
            let parent = r"H:\clients\a-long-parent-directory\projects\";
            for leaf in ["alpha", "beta"] {
                let full = format!("{parent}{leaf}");
                out.push(elide_front(ui, &full, &font, 120.0));
                assert!(w(&out.last().unwrap().0) <= 120.0);
            }
            out.push(elide_front(ui, "H:\\src", &font, 120.0));
            out.push(elide_front(ui, "H:\\src", &font, 1.0));
        });
        assert!(out[0].1 && out[0].0.starts_with('…') && out[0].0.ends_with("alpha"));
        assert!(out[1].0.ends_with("beta"));
        assert_eq!(out[2], ("H:\\src".to_string(), false));
        assert_eq!(out[3], ("…".to_string(), true));
    }

    #[test]
    fn tab_order_wraps_through_the_directory_fields() {
        assert_eq!(Field::Shell.step(true, 0), Field::Name);
        assert_eq!(Field::Shell.step(true, 2), Field::Dir(0));
        assert_eq!(Field::Dir(1).step(true, 2), Field::Name);
        assert_eq!(Field::Name.step(false, 2), Field::Dir(1));
        assert_eq!(Field::Dir(0).step(false, 2), Field::Shell);
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
            bash: false,
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
        let running = TestEntry {
            generation: 1,
            running: true,
            last: None,
        };
        assert!(!v.absorb("andy@desk", running, &mut list));
        let done = TestEntry {
            generation: 2,
            running: false,
            last: Some(TestOutcome::Ok(TestReport {
                shell: RemoteShell::WindowsPowerShell,
                login_shell: None,
                curl: false,
                bash: false,
                agents: vec![],
            })),
        };
        assert!(v.absorb("andy@desk", done.clone(), &mut list));
        assert_eq!(list[0].shell, RemoteShell::WindowsPowerShell);
        // The same answer again (every frame) applies nothing.
        list[0].shell = RemoteShell::Posix;
        assert!(!v.absorb("andy@desk", done, &mut list));
        assert_eq!(list[0].shell, RemoteShell::Posix);
    }

    fn tested(v: &mut RemotesView, target: &str, curl: bool, bash: bool, agents: &[&str]) {
        v.set_test_result(
            target,
            TestOutcome::Ok(TestReport {
                shell: RemoteShell::Posix,
                login_shell: None,
                curl,
                bash,
                agents: agents.iter().map(|a| a.to_string()).collect(),
            }),
        );
    }

    /// I on a row with missing agents shows the confirm for the first one,
    /// ←/→ moves between them, Esc keeps everything, and Enter asks the
    /// caller to open exactly that install on exactly that Remote.
    #[test]
    fn install_asks_first_with_the_agent_and_remote_named() {
        let mut v = RemotesView::new();
        v.set_install_project(Some("proj"));
        let mut list = vec![r("desk", "pane-install-desk")];
        tested(&mut v, "pane-install-desk", true, true, &["claude"]);
        assert_eq!(v.installable(0, &list), ["codex", "grok"]);
        assert!(
            v.hint_for(&list).contains("I install"),
            "{}",
            v.hint_for(&list)
        );

        assert_eq!(v.key(Key::Install, &mut list), Effect::None);
        assert_eq!(
            v.mode,
            Mode::ConfirmInstall {
                row: 0,
                provider: "codex"
            }
        );
        assert!(v.hint_for(&list).contains("←/→ other agent"));
        v.key(Key::Right, &mut list);
        assert_eq!(
            v.mode,
            Mode::ConfirmInstall {
                row: 0,
                provider: "grok"
            }
        );
        v.key(Key::Right, &mut list); // wraps
        assert!(matches!(
            v.mode,
            Mode::ConfirmInstall {
                provider: "codex",
                ..
            }
        ));
        v.key(Key::Left, &mut list);
        assert_eq!(v.key(Key::Esc, &mut list), Effect::None);
        assert_eq!(v.mode, Mode::Browse);

        v.key(Key::Install, &mut list);
        v.key(Key::Right, &mut list);
        assert_eq!(v.key(Key::Enter, &mut list), Effect::Install(0, "grok"));
        assert_eq!(v.mode, Mode::Browse);
        assert_eq!(list.len(), 1, "nothing else changed");
    }

    /// No Project open: the install is refused at I, before a confirm that
    /// could only fail after Enter. With one, the footer names it.
    #[test]
    fn install_with_no_project_open_refuses_before_the_confirm() {
        let mut v = RemotesView::new();
        let mut list = vec![r("desk", "pane-install-noproj")];
        tested(&mut v, "pane-install-noproj", true, true, &[]);
        assert_eq!(v.key(Key::Install, &mut list), Effect::None);
        assert_eq!(v.mode, Mode::Browse, "no confirm to say no to later");
        let m = v.message.clone().unwrap();
        assert!(
            m.contains("Open a Project first") && m.contains("desk"),
            "{m}"
        );
        assert_eq!(v.confirm_footer(&list), None);

        // With one open, the confirm's footer names it and the login after.
        v.set_install_project(Some("proj"));
        v.key(Key::Install, &mut list);
        assert!(matches!(v.mode, Mode::ConfirmInstall { .. }));
        assert_eq!(
            v.confirm_footer(&list).as_deref(),
            Some("Opens in proj; claude needs a login after.")
        );
    }

    /// The row's agents line is built once per Test answer and marks
    /// change, not every frame.
    #[test]
    fn the_agents_line_is_cached_until_its_inputs_change() {
        let mut v = RemotesView::new();
        let mut list = vec![r("desk", "pane-agents-cache")];
        assert!(v.agents_for(&list[0]).is_none(), "untested");
        tested(&mut v, "pane-agents-cache", true, true, &["claude"]);
        let a = v.agents_for(&list[0]).unwrap();
        let b = v.agents_for(&list[0]).unwrap();
        assert!(std::sync::Arc::ptr_eq(&a, &b), "rebuilt with nothing new");
        crate::remote_install::set_mark(
            "pane-agents-cache",
            "grok",
            crate::remote_install::Mark::Installing,
        );
        let c = v.agents_for(&list[0]).unwrap();
        assert!(!std::sync::Arc::ptr_eq(&b, &c), "a mark changed");
        assert!(
            c.pieces
                .iter()
                .any(|p| matches!(p, Piece::Text { text, .. } if text == "grok installing…"))
        );
        list[0].name = "den".into();
        let d = v.agents_for(&list[0]).unwrap();
        assert!(!std::sync::Arc::ptr_eq(&c, &d), "renamed");
        tested(&mut v, "pane-agents-cache", true, true, &[]);
        let e = v.agents_for(&list[0]).unwrap();
        assert!(!std::sync::Arc::ptr_eq(&d, &e), "a new Test answer");
    }

    /// No install to confirm: I says why in words naming the Remote, and
    /// never opens a confirm.
    #[test]
    fn install_with_nothing_to_install_says_why() {
        let mut v = RemotesView::new();
        let mut list = vec![r("desk", "pane-install-why")];
        v.key(Key::Install, &mut list);
        assert_eq!(v.mode, Mode::Browse);
        assert!(v.message.as_deref().unwrap().contains("Test desk first"));
        assert!(!v.hint_for(&list).contains("install"));

        tested(&mut v, "pane-install-why", false, false, &[]);
        v.key(Key::Install, &mut list);
        assert_eq!(v.mode, Mode::Browse);
        let m = v.message.clone().unwrap();
        assert!(
            m.contains("needs curl and bash there first") && m.contains("desk"),
            "{m}"
        );

        tested(
            &mut v,
            "pane-install-why",
            true,
            true,
            &["claude", "codex", "grok"],
        );
        v.key(Key::Install, &mut list);
        assert!(v.message.as_deref().unwrap().contains("Every agent CLI"));
        // The Add row has nothing to install.
        v.selected = 1;
        assert_eq!(v.key(Key::Install, &mut list), Effect::None);
    }

    /// A Test that lands while the confirm is open and finds the agent turns
    /// Enter into a message, not an install of something already there.
    #[test]
    fn a_confirm_overtaken_by_a_test_installs_nothing() {
        let mut v = RemotesView::new();
        v.set_install_project(Some("proj"));
        let mut list = vec![r("desk", "pane-install-late")];
        tested(&mut v, "pane-install-late", true, true, &[]);
        v.key(Key::Install, &mut list);
        tested(
            &mut v,
            "pane-install-late",
            true,
            true,
            &["claude", "codex", "grok"],
        );
        assert_eq!(v.key(Key::Enter, &mut list), Effect::None);
        assert_eq!(v.mode, Mode::Browse);
    }
}
