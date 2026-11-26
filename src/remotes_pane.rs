//! The Remotes pane of the settings menu: list, add, edit and delete Remotes,
//! with reachability and Test per row. A custom body like the Keybindings
//! pane. The model half (`Draft`, `commit`, `RemotesView::key`) is pure and
//! tested; the egui half below only draws it and feeds it keys.

use std::collections::HashMap;

use eframe::egui;

use crate::remote::{self, Remote, RemoteShell};
use crate::remote_browse_ui::{Browser, Enters, Nav, Shows};
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
    /// The Remote's Projects folder (`Remote::base`).
    Base,
    /// The Remote directory of the draft's `dirs[i]`.
    Dir(usize),
}

impl Field {
    /// The next field in Tab order, over a form with `dirs` directory rows.
    fn step(self, forward: bool, dirs: usize) -> Field {
        let n = 4 + dirs;
        let i = match self {
            Field::Name => 0,
            Field::Target => 1,
            Field::Shell => 2,
            Field::Base => 3,
            Field::Dir(k) => 4 + k.min(dirs.saturating_sub(1)),
        };
        match if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        } {
            0 => Field::Name,
            1 => Field::Target,
            2 => Field::Shell,
            3 => Field::Base,
            k => Field::Dir(k - 4),
        }
    }
}

/// One Project directory mapping in the edit form.
#[derive(Clone, Debug, PartialEq)]
pub struct DirRow {
    /// The `Remote::dirs` key as stored: the seat-PC Project directory.
    pub project: String,
    /// The directory on the Remote. Blank is no mapping: the Project opens
    /// in its folder under the Projects folder.
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
    /// The Projects folder (`Remote::base`); blank is the home.
    pub base: String,
    pub field: Field,
    /// The highlighted ssh-config suggestion, while the Target field is active.
    pub pick: Option<usize>,
    /// Why the last save was refused, drawn in the form. Dropped with the
    /// draft, so a cancel leaves nothing behind.
    pub error: Option<String>,
    /// The entry's Project directory mappings, editable.
    pub dirs: Vec<DirRow>,
    /// Project keys cleared in this draft; removed on save. Kept apart from
    /// `dirs` so a mapping a first launch adds while the form is open
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
            base: String::new(),
            field: Field::Name,
            pick: None,
            error: None,
            dirs: Vec::new(),
            cleared: Vec::new(),
        }
    }

    /// `open` is the directories of the Projects open now: each gets a row,
    /// blank unless it has a mapping, so one can be given a directory of its
    /// own without having been launched first.
    pub fn edit(index: usize, r: &Remote, open: &[String]) -> Self {
        let mut dirs: Vec<DirRow> = r
            .dirs
            .iter()
            .map(|(project, remote)| DirRow {
                project: project.clone(),
                remote: remote.clone(),
            })
            .collect();
        for project in open {
            if !dirs.iter().any(|d| remote::same_dir(&d.project, project)) {
                dirs.push(DirRow {
                    project: remote::plain_dir(project),
                    remote: String::new(),
                });
            }
        }
        Self {
            index: Some(index),
            name: r.name.clone(),
            target: r.target.clone(),
            shell: r.shell,
            base: r.base.clone(),
            field: Field::Name,
            pick: None,
            error: None,
            dirs,
            cleared: Vec::new(),
        }
    }

    /// The directory the folder list is listing for: the focused Projects
    /// folder or Project row.
    fn browsed(&self) -> Option<&str> {
        match self.field {
            Field::Base => Some(&self.base),
            Field::Dir(k) => self.dirs.get(k).map(|r| r.remote.as_str()),
            _ => None,
        }
    }

    /// Drop the mapping at `k`: that Project goes back to its folder under
    /// the Projects folder. The active field stays on a row that still exists.
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
            base: remote::checked_dir(&self.base).map_err(|e| invalid(Field::Base, e))?,
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
            let dir = remote::checked_dir(&row.remote).map_err(|e| {
                invalid(
                    Field::Dir(k),
                    format!("{}: {e}", remote::plain_dir(&row.project)),
                )
            })?;
            out.retain(|seat, _| !remote::same_dir(seat, &row.project));
            // Blank is no mapping; the home is spelled `~`.
            if !dir.is_empty() {
                out.insert(remote::plain_dir(&row.project), dir);
            }
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
    /// Ask to copy this PC's git name and email to the selected Remote.
    Git,
    /// Forget the focused Project directory row (Ctrl+Backspace, or Delete
    /// in an empty one).
    Clear,
    /// Up a level, while the folder list has the keyboard.
    Backspace,
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
    /// Write the confirmed git name and email on the Remote at this index.
    SetGit(usize),
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
    /// Showing the git name and email to write on row `row`, waiting for
    /// Enter.
    ConfirmGit {
        row: usize,
        fix: crate::remote_git::Fix,
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
    /// The directories of the Projects open now; each gets a row in the
    /// edit form.
    projects: Vec<String>,
    /// Per target: its agents line, built once per Test answer and marks
    /// change instead of every frame.
    agents: HashMap<String, std::sync::Arc<AgentsCache>>,
    /// The folder list under the focused Directory row. Fresh per form,
    /// so its listings live as long as the form.
    browse: Browser,
    /// The confirmed git name and email, between Enter and its ssh starting.
    git_fix: Option<crate::remote_git::Fix>,
    /// How the last git write went, left by its worker for the footer line.
    git_done: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// Test hook: this PC's git name and email. Under test `git` is never
    /// run for them.
    #[cfg(test)]
    local_git: crate::remote_git::Identity,
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
    /// The scrolling list's viewport: what is on screen of the rows.
    pub list: egui::Rect,
    /// Per row: its status line.
    pub status: Vec<egui::Rect>,
    /// The form's error line, while it shows one.
    pub error: Option<egui::Rect>,
    /// The form's "no Project directories yet" line fit without its cut.
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
    /// Per row: its git line.
    pub git: Vec<egui::Rect>,
    /// Every "use this PC's" button drawn.
    pub git_fixes: Vec<egui::Rect>,
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
                git: Vec::new(),
                git_fixes: Vec::new(),
            },
            install_project: None,
            projects: Vec::new(),
            agents: HashMap::new(),
            browse: Browser::new(),
            git_fix: None,
            git_done: Default::default(),
            #[cfg(test)]
            local_git: Default::default(),
            #[cfg(test)]
            forced: HashMap::new(),
        }
    }

    /// True while the edit form is open.
    pub fn is_editing(&self) -> bool {
        matches!(self.mode, Mode::Edit(_))
    }

    #[cfg(test)]
    pub fn laid(&self) -> &Laid {
        &self.laid
    }

    /// Test hook: the folder list, to seed listings and read its rects.
    #[cfg(test)]
    pub fn browse(&mut self) -> &mut Browser {
        &mut self.browse
    }

    /// Test hook: the open form's draft.
    #[cfg(test)]
    pub fn draft_mut(&mut self) -> Option<&mut Draft> {
        match &mut self.mode {
            Mode::Edit(d) => Some(d),
            _ => None,
        }
    }

    /// Test hook: the form's Directory `k` as typed.
    #[cfg(test)]
    pub fn draft_dir(&self, k: usize) -> Option<&str> {
        match &self.mode {
            Mode::Edit(d) => d.dirs.get(k).map(|r| r.remote.as_str()),
            _ => None,
        }
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

    /// The directories of the Projects open now.
    pub fn set_projects(&mut self, projects: &[String]) {
        if self.projects != projects {
            self.projects = projects.to_vec();
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

    /// What row `r`'s last finished Test read of its git: `None` before a
    /// Test that worked, `Some(None)` when git is not installed.
    fn git_of(&self, r: &Remote) -> Option<Option<&remote_probe::GitInfo>> {
        match self.tests.get(&r.target)?.last.as_ref()? {
            TestOutcome::Ok(report) => Some(report.git.as_ref()),
            _ => None,
        }
    }

    /// Row `i` has git but lacks a name or an email.
    fn git_lacks(&self, i: usize, remotes: &[Remote]) -> bool {
        remotes
            .get(i)
            .and_then(|r| self.git_of(r))
            .flatten()
            .is_some_and(|g| g.name.is_empty() || g.email.is_empty())
    }

    /// This PC's git name and email, read now.
    fn local_identity(&self) -> crate::remote_git::Identity {
        #[cfg(test)]
        return self.local_git.clone();
        #[cfg(not(test))]
        crate::remote_git::local_identity()
    }

    /// Open the confirm that copies this PC's git name and email to row
    /// `row`, or say why there is nothing to copy.
    fn ask_git(&mut self, row: usize, remotes: &[Remote]) {
        let r = &remotes[row];
        let Some(git) = self.git_of(r).map(|g| g.cloned()) else {
            self.message = Some(format!(
                "Test {} first: T reads its git name and email",
                r.name
            ));
            return;
        };
        match crate::remote_git::fix(&r.name, git.as_ref(), &self.local_identity()) {
            Ok(fix) => {
                self.mode = Mode::ConfirmGit { row, fix };
                self.message = None;
            }
            Err(why) => self.message = Some(why),
        }
    }

    /// Write the confirmed name and email on `r`, off the UI thread, then
    /// Test it again so its git line shows them.
    fn start_git(&mut self, ctx: &egui::Context, r: &Remote) {
        let Some(fix) = self.git_fix.take() else {
            return;
        };
        let (ctx, target, shell, name) = (ctx.clone(), r.target.clone(), r.shell, r.name.clone());
        let done = self.git_done.clone();
        remote_probe::spawn_network(move || {
            let msg = match crate::remote_git::run_set(&target, shell, &fix) {
                Ok(()) => {
                    remote_probe::retest(&ctx, &target);
                    format!("Set the git name and email on {name}")
                }
                Err(e) => format!("Could not set the git name and email on {name}: {e}"),
            };
            *done.lock().unwrap_or_else(|e| e.into_inner()) = Some(msg);
            ctx.request_repaint();
        });
    }

    /// Open the add form below the list: select it and scroll it into view.
    fn begin_add(&mut self, len: usize) {
        self.selected = len;
        self.begin_edit(Draft::add());
        self.hosts = None; // re-read: the user may have just edited it
    }

    fn begin_edit(&mut self, d: Draft) {
        self.browse = Browser::new();
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
                    self.begin_edit(Draft::edit(
                        self.selected,
                        &remotes[self.selected],
                        &self.projects,
                    ));
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
                Key::Git if self.selected < len => self.ask_git(self.selected, remotes),
                Key::Esc => return Effect::Close,
                _ => {}
            },
            // The list shrank under the confirm (settings.json reloaded).
            Mode::ConfirmGit { row, .. } if row >= len => {}
            Mode::ConfirmGit { row, fix } => match k {
                Key::Enter => {
                    self.message = Some(format!(
                        "Setting the git name and email on {}…",
                        remotes[row].name
                    ));
                    self.git_fix = Some(fix);
                    return Effect::SetGit(row);
                }
                Key::Esc => {}
                _ => self.mode = Mode::ConfirmGit { row, fix },
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
                if let Some(effect) = self.browse_key(k, &mut d) {
                    self.mode = Mode::Edit(d);
                    return effect;
                }
                let before = d.field;
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
                            self.browse.focus_field();
                            self.scroll_to = true;
                        }
                    }
                    _ => {}
                }
                if d.field != before {
                    self.browse.focus_field();
                }
                self.mode = Mode::Edit(d);
            }
        }
        Effect::None
    }

    /// A key for the folder list under the focused directory field (the
    /// Projects folder or a Project's row): all its
    /// keys while it has the keyboard (Esc steps back one, name field to
    /// list to field, as Esc first drops a Target suggestion), and Down
    /// from the field when there is a list to enter. `None` = the form's
    /// own key.
    fn browse_key(&mut self, k: Key, d: &mut Draft) -> Option<Effect> {
        let text = match d.field {
            Field::Base => &mut d.base,
            Field::Dir(j) => &mut d.dirs.get_mut(j)?.remote,
            _ => return None,
        };
        let target = d.target.trim();
        if !remote::valid_target(target) {
            return None; // nothing to list: the area says why
        }
        self.browse.set_source(target, d.shell, &d.name);
        let list = self.browse.list_focused();
        let nav = match k {
            Key::Esc if list => {
                self.browse.back();
                return Some(Effect::None);
            }
            // Never saves while the Enter that made a folder is held.
            Key::Enter if !list && self.browse.held_enter() => return Some(Effect::None),
            Key::Down => Nav::Down,
            _ if !list => return None,
            Key::Up => Nav::Up,
            Key::Left => Nav::Left,
            Key::Right => Nav::Right,
            Key::Enter => Nav::Enter,
            Key::Backspace => Nav::Backspace,
            Key::Tab | Key::BackTab => Nav::Tab,
            // The name field's: nothing of the form's.
            _ if self.browse.naming() => return Some(Effect::None),
            // Ctrl+Backspace still forgets the row.
            _ => return None,
        };
        let old = text.clone();
        if !self.browse.key(nav, text) {
            return None;
        }
        if *text != old {
            d.error = None;
        }
        self.scroll_to = true;
        Some(Effect::None)
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

    /// The settings footer's line for this frame: the keys that work where
    /// the keyboard is, one line at the window's minimum width. Browsing
    /// offers only what the selection can do: the Add row cannot be edited,
    /// tested or deleted.
    pub fn footer(&self, remotes: &[Remote]) -> &'static str {
        let len = remotes.len();
        match &self.mode {
            Mode::Browse if self.selected < len => {
                let install = !self.installable(self.selected, remotes).is_empty();
                match (install, self.git_lacks(self.selected, remotes)) {
                    (false, false) => {
                        "↑/↓ select · Enter edit · A add · T test · Del delete · Tab/Esc rail"
                    }
                    (true, false) => {
                        "↑/↓ select · Enter edit · A add · T test · I install · Del delete · Tab/Esc rail"
                    }
                    (false, true) => {
                        "↑/↓ select · Enter edit · A add · T test · G git name · Del delete · Tab/Esc rail"
                    }
                    (true, true) => {
                        "↑/↓ select · Enter edit · T test · I install · G git name · Del delete · Tab/Esc rail"
                    }
                }
            }
            Mode::Browse if len == 0 => "Enter or A add · Tab/Esc rail",
            Mode::Browse => "↑/↓ select · Enter add · Tab/Esc rail",
            Mode::Edit(d) => self.edit_footer(d, remotes),
            Mode::ConfirmDelete(_) => "Enter delete · Esc keep",
            Mode::ConfirmInstall { row, .. } if self.installable(*row, remotes).len() > 1 => {
                "Enter install · ←/→ other agent · Esc cancel"
            }
            Mode::ConfirmInstall { .. } => "Enter install · Esc cancel",
            Mode::ConfirmGit { .. } => "Enter set them · Esc cancel",
        }
    }

    /// [`Self::footer`] while a form is open: the focused field's keys, the
    /// folder list's, or the new-folder name field's.
    fn edit_footer(&self, d: &Draft, remotes: &[Remote]) -> &'static str {
        let listable = remote::valid_target(d.target.trim());
        match d.field {
            Field::Base | Field::Dir(_) if listable && self.browse.list_focused() => {
                match d.browsed().map(|t| self.browse.shows(t)) {
                    Some(Shows::Waiting) => "Listing folders · Tab/Esc back to the field",
                    Some(Shows::Retry) => "Enter retry · Tab/Esc back to the field",
                    Some(Shows::Naming) => "Enter create the folder · Esc back to the list",
                    Some(Shows::Creating) => "Creating the folder… · Esc back to the list",
                    Some(Shows::Busy) => {
                        "↑/↓ pick · Enter/→ in · ← up · making a folder… · Tab/Esc back"
                    }
                    _ => "↑/↓ pick · Enter/→ in · ← up · Ctrl+N new folder · Tab/Esc back",
                }
            }
            Field::Dir(k) if listable => match d.dirs.get(k).map(|r| self.browse.enters(&r.remote))
            {
                Some(Enters::Folders) => {
                    "Tab next field · ↓ folders · Ctrl+Backspace clear · Enter save · Esc cancel"
                }
                Some(Enters::Retry) => {
                    "Tab next field · ↓ retry · Ctrl+Backspace clear · Enter save · Esc cancel"
                }
                _ => "Tab next field · Ctrl+Backspace clear · Enter save · Esc cancel",
            },
            Field::Dir(_) => "Tab next field · Ctrl+Backspace clear · Enter save · Esc cancel",
            Field::Target if d.pick.is_some() => {
                "↑/↓ pick a Host · Enter use it · Esc back to the field"
            }
            Field::Target
                if !suggestions(self.hosts.as_deref().unwrap_or_default(), remotes, d)
                    .is_empty() =>
            {
                "Tab next field · ↓ ssh-config Hosts · Enter save · Esc cancel"
            }
            Field::Shell => "←/→ change shell · Tab next field · Enter save · Esc cancel",
            Field::Base if listable => match self.browse.enters(&d.base) {
                Enters::Folders => "Tab next field · ↓ folders · Enter save · Esc cancel",
                Enters::Retry => "Tab next field · ↓ retry · Enter save · Esc cancel",
                Enters::None => "Tab next field · Enter save · Esc cancel",
            },
            Field::Name | Field::Target | Field::Base => "Tab next field · Enter save · Esc cancel",
        }
    }

    /// True while Tab is the pane's own key (moving fields, or choosing the
    /// agent in an install confirm), so the menu leaves it alone.
    pub fn owns_tab(&self) -> bool {
        matches!(self.mode, Mode::Edit(_) | Mode::ConfirmInstall { .. })
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
        // The form is gone: drop its listings, which kills their ssh.
        if !matches!(self.mode, Mode::Edit(_)) && self.browse.used() {
            self.browse = Browser::new();
        }
        let mut changed = self.poll_tests(remotes);
        let landed = self
            .git_done
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(msg) = landed {
            self.message = Some(msg);
        }
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
            Effect::SetGit(i) => view.start_git(&ctx, &remotes[i]),
            Effect::None => {}
        };

        if active {
            let (list, naming) = (self.browse.list_focused(), self.browse.naming());
            for k in read_keys(ui, &self.mode, list, naming) {
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
        self.laid.git.clear();
        self.laid.git_fixes.clear();
        self.laid.error = None;

        let th = crate::theme::live(ui.ctx());
        ui.painter_at(rect).rect_filled(rect, 0.0, th.win_bg);
        // No title and no hint here: the settings title band names the
        // pane and the footer carries the keys (`footer`).
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        let ui = &mut child;
        ui.set_clip_rect(rect);
        ui.visuals_mut().override_text_color = Some(th.text);
        // Rows are allocated edge to edge, as in the other panes; clusters
        // set their own spacing.
        ui.spacing_mut().item_spacing = egui::Vec2::ZERO;

        let mut acts: Vec<Act> = Vec::new();
        self.laid.list = egui::ScrollArea::vertical()
            .id_salt("remotes_pane")
            .auto_shrink([false, false])
            .max_height((rect.height() - FOOT_H).max(60.0))
            .show(ui, |ui| self.render_rows(ui, remotes, active, &mut acts))
            .inner_rect;

        // The separator and the message line, pinned under the list. One
        // line, cut with "…" and whole on hover, so nothing moves.
        let sep_y = self.laid.list.max.y + 6.0;
        let (l, r) = (rect.min.x + PAD, rect.max.x - PAD);
        ui.painter().hline(
            egui::Rangef::new(l, r),
            sep_y,
            egui::Stroke::new(1.0, th.border),
        );
        if let Some(msg) = self
            .confirm_footer(remotes)
            .or_else(|| self.message.clone())
        {
            let line = egui::Rect::from_min_max(
                egui::pos2(l, sep_y + 4.0),
                egui::pos2(r, sep_y + 4.0 + LINE_H),
            );
            let mut c = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(line)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            c.add(egui::Label::new(egui::RichText::new(msg).color(th.dim).size(11.5)).truncate());
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
                    self.begin_edit(Draft::edit(i, &remotes[i], &self.projects));
                }
                Act::Add => self.begin_add(remotes.len()),
                Act::AskDelete(i) => {
                    self.selected = i;
                    self.mode = Mode::ConfirmDelete(i);
                }
                Act::Key(k) => {
                    // A button means what it says: save and cancel must not
                    // go into a folder or only leave the list.
                    self.browse.focus_field();
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
                Act::AskGit(i) => {
                    self.selected = i;
                    // Never over an open form or another confirm.
                    if matches!(self.mode, Mode::Browse | Mode::ConfirmGit { .. }) {
                        self.mode = Mode::Browse;
                        self.ask_git(i, remotes);
                    }
                }
                Act::Field(f) => {
                    if let Mode::Edit(d) = &mut self.mode {
                        d.field = f;
                        d.pick = None;
                        self.browse.focus_field();
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
                        self.browse.focus_field();
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
        let w = ui.available_width();
        for (i, r) in remotes.iter().enumerate() {
            if editing == Some(Some(i)) {
                self.render_form(ui, remotes, active, scroll_to, acts);
                ui.add_space(ITEM_GAP);
                continue;
            }
            let selected = i == self.selected && editing.is_none();
            // The whole item at once, its height fixed in every state, so
            // the wash covers all three lines and nothing below ever moves.
            let (item, resp) = ui.allocate_exact_size(egui::vec2(w, ITEM_H), egui::Sense::click());
            if resp.clicked() {
                acts.push(Act::Select(i));
            }
            if selected {
                ui.painter()
                    .rect_filled(wash(item), egui::CornerRadius::same(4), th.sel_bg);
                if scroll_to {
                    ui.scroll_to_rect(item, None);
                }
            }
            let row = egui::Rect::from_min_size(item.min, egui::vec2(w, ROW_H));
            let status_rect = egui::Rect::from_min_max(
                egui::pos2(item.min.x + PAD, row.max.y),
                egui::pos2(item.max.x - PAD, row.max.y + LINE_H),
            );
            let agents_rect = status_rect.translate(egui::vec2(0.0, LINE_H));
            let git_rect = agents_rect.translate(egui::vec2(0.0, LINE_H));
            let reach = remote_probe::reach(ui.ctx(), &r.target);
            let (dot, reach_text, why) = match &reach {
                Reach::Checking => (th.dim, "checking…", None),
                Reach::Reachable => (th.border_focus, "reachable", None),
                Reach::Offline => (th.dim, "offline", Some(OFFLINE_WHY.to_string())),
                Reach::Unknown(why) => (th.dim, "unknown", Some(why.clone())),
            };

            // Status and buttons first, right to left; the text gets the rest.
            let mut row_ui = cluster(ui, row);
            let row_ui = &mut row_ui;
            if let Mode::ConfirmDelete(d) = self.mode
                && d == i
            {
                if row_ui.small_button("keep").clicked() {
                    acts.push(Act::Key(Key::Esc));
                }
                if row_ui
                    .small_button(egui::RichText::new("delete").color(th.danger))
                    .clicked()
                {
                    acts.push(Act::Key(Key::Enter));
                }
                row_ui.label(egui::RichText::new(format!("Delete {}?", r.name)).color(th.danger));
            } else if let Mode::ConfirmInstall { row: c, provider } = self.mode
                && c == i
            {
                // The delete confirm's shape: answer buttons, then the
                // question naming the agent and the Remote.
                if row_ui.small_button("cancel").clicked() {
                    acts.push(Act::Key(Key::Esc));
                }
                if row_ui
                    .small_button(egui::RichText::new("install").color(th.border_focus))
                    .clicked()
                {
                    acts.push(Act::Key(Key::Enter));
                }
                // Shortened to fit, never cut: the Remote's name stays.
                let font = egui::TextStyle::Body.resolve(row_ui.style());
                let room = row_ui.available_width();
                let fits = |q: &str| {
                    row_ui
                        .painter()
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
                let galley = row_ui.painter().layout_job(job);
                self.laid.question = Some((q, !galley.elided));
                row_ui.add(egui::Label::new(galley));
            } else if let Mode::ConfirmGit { row: c, .. } = &self.mode
                && *c == i
            {
                if row_ui.small_button("cancel").clicked() {
                    acts.push(Act::Key(Key::Esc));
                }
                if row_ui
                    .small_button(egui::RichText::new("set").color(th.border_focus))
                    .clicked()
                {
                    acts.push(Act::Key(Key::Enter));
                }
                row_ui.add(
                    egui::Label::new(format!("Set this PC's git name on {}?", r.name)).truncate(),
                );
            } else {
                if row_ui.small_button("delete").clicked() {
                    acts.push(Act::AskDelete(i));
                }
                if row_ui.small_button("edit").clicked() {
                    acts.push(Act::Edit(i));
                }
                let running = self.tests.get(&r.target).is_some_and(|e| e.running);
                if row_ui
                    .add_enabled(!running, egui::Button::new("test").small())
                    .clicked()
                {
                    acts.push(Act::Test(i));
                }
                row_ui.add_space(4.0);
                let label = row_ui.label(egui::RichText::new(reach_text).color(dot).size(11.5));
                if let Some(why) = &why {
                    label.on_hover_text(why);
                }
            }
            let controls = row_ui.min_rect();

            let name_color = if reach == Reach::Offline {
                th.dim
            } else {
                th.text
            };
            let sub_text = format!("{} · {}", r.target, shell_label(r.shell));
            let left = row.min.x + PAD;
            let room = (controls.min.x - 8.0 - left).max(0.0);
            let name = one_line(ui, &r.name, egui::FontId::proportional(LABEL_PT), room);
            let sub = one_line(ui, &sub_text, egui::FontId::monospace(DESC_PT), room);
            let name_at = egui::pos2(left, row.min.y + LABEL_Y - name.size().y / 2.0);
            let sub_at = egui::pos2(left, row.min.y + DESC_Y - sub.size().y / 2.0);
            // Elided to nothing when the controls take the row (a wrapped
            // question): even a lone "…" would run under them.
            let mut text = egui::Rect::from_min_size(egui::pos2(left, row.min.y), egui::Vec2::ZERO);
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

            if let Mode::ConfirmGit { row: c, fix } = &self.mode
                && *c == i
            {
                // The status and agents lines' room, given to the exact
                // values, so opening the confirm moves nothing.
                let shown = fix.shown();
                let mut job = egui::text::LayoutJob::simple(
                    shown.clone(),
                    egui::FontId::monospace(DESC_PT),
                    th.text,
                    status_rect.width().max(40.0),
                );
                job.wrap.max_rows = 2;
                let galley = ui.painter().layout_job(job);
                let mut c = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(status_rect.union(agents_rect))
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                c.add(egui::Label::new(galley).selectable(false))
                    .on_hover_text(format!(
                        "{shown}\n\nWritten to the global git config on {}. A value it already has is left alone.",
                        r.name
                    ));
                ui.add_space(ITEM_GAP);
                continue;
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
                let (rect, elided) =
                    confirm_block(ui, status_rect.union(agents_rect), offer.as_ref());
                self.laid.confirm = Some(rect);
                self.laid.confirm_elided = elided;
                ui.add_space(ITEM_GAP);
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
            self.laid.status.push(status_line(ui, status_rect, status));
            // Reserved on every row, tested or not, so a Test landing never
            // moves the rows below. A re-run keeps the last answer here.
            let pieces = agents.as_ref().map_or(&[][..], |a| &a.pieces[..]);
            let line = agents_line_ui(ui, agents_rect, pieces, |p| {
                acts.push(Act::AskInstall(i, p))
            });
            self.laid.agents.push(line.rect);
            self.laid.installs.extend(line.installs);
            self.laid.agents_fit &= line.fits;
            // Reserved on every row too. A re-run keeps the last answer.
            let git = self.git_of(r).map(crate::remote_git::line);
            let (line, fix) = git_line_ui(ui, git_rect, git.as_ref());
            self.laid.git.push(line);
            if let Some((rect, clicked)) = fix {
                self.laid.git_fixes.push(rect);
                if clicked {
                    acts.push(Act::AskGit(i));
                }
            }
            ui.add_space(ITEM_GAP);
        }

        if editing == Some(None) {
            self.render_form(ui, remotes, active, scroll_to, acts);
        } else {
            let selected = self.selected >= remotes.len() && self.mode == Mode::Browse;
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), egui::Sense::click());
            if selected {
                ui.painter()
                    .rect_filled(wash(rect), egui::CornerRadius::same(4), th.sel_bg);
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
                egui::pos2(rect.min.x + PAD, rect.center().y),
                egui::Align2::LEFT_CENTER,
                "+ Add Remote",
                egui::FontId::proportional(LABEL_PT),
                color,
            );
            if resp.clicked() {
                acts.push(Act::Add);
            }
        }
    }

    /// The add/edit form, in place of its row (or after the list): the
    /// Remote expanded into ordinary settings rows on the pane background,
    /// marked by a focus bar down its left edge.
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
        let browse = &mut self.browse;
        let hosts = self.hosts.as_deref().unwrap_or_default();
        let name_id = egui::Id::new("remotes_edit_name");
        let target_id = egui::Id::new("remotes_edit_target");
        let base_id = egui::Id::new("remotes_edit_base");
        let dir_id = |k: usize| egui::Id::new(("remotes_edit_dir", k));
        // The model owns which field is active and egui focus follows it,
        // but only while this Win reads the keyboard: typing in a terminal
        // must never land in a field.
        // The folder list holding the keyboard takes it from the field, so
        // the two never both look focused.
        let dirs = d.dirs.len();
        // Typing while the list has the keyboard lands in the field, which
        // takes the keyboard back before it draws.
        let typed_into = match d.field {
            Field::Base => Some(&mut d.base),
            Field::Dir(k) => d.dirs.get_mut(k).map(|r| &mut r.remote),
            _ => None,
        };
        if active
            && let Some(text) = typed_into
            && browse.route_typing(&ctx, text)
        {
            d.error = None;
        }
        let list = browse.list_focused();
        ui.memory_mut(|m| match (active, d.field) {
            (true, Field::Name) => m.request_focus(name_id),
            (true, Field::Target) => m.request_focus(target_id),
            (true, Field::Base) if !list => m.request_focus(base_id),
            (true, Field::Dir(k)) if !list => m.request_focus(dir_id(k)),
            _ => {
                m.surrender_focus(name_id);
                m.surrender_focus(target_id);
                m.surrender_focus(base_id);
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
        let w = ui.available_width();
        let fw = field_w(w);
        let top = ui.cursor().min;

        // The header: the Remote's name, and save and cancel where its
        // delete and keep sit when collapsed.
        let (title, sub) = match d.index.and_then(|i| remotes.get(i)) {
            Some(r) => (
                r.name.clone(),
                Some(format!("{} · {}", r.target, shell_label(r.shell))),
            ),
            None => ("New Remote".to_string(), None),
        };
        let (_, (save, cancel)) = settings_row(ui, &title, sub.as_deref().map(Desc::Mono), |ui| {
            let cancel = ui.small_button("cancel");
            if cancel.clicked() {
                acts.push(Act::Key(Key::Esc));
            }
            let save = ui.small_button("save");
            if save.clicked() {
                acts.push(Act::Key(Key::Enter));
            }
            (save.rect, cancel.rect)
        });
        let buttons = save.union(cancel);
        // A refused save, in the danger colour like a Keybindings conflict,
        // right under the buttons; focus is already on the bad field. The
        // line is reserved, so an error appearing moves nothing. Cut from
        // the front: the reason is at the end.
        let (err_line, _) = ui.allocate_exact_size(egui::vec2(w, LINE_H), egui::Sense::hover());
        if let Some(e) = &d.error {
            let r = err_line.shrink2(egui::vec2(PAD, 0.0));
            let font = egui::FontId::proportional(11.5);
            let (shown, cut) = elide_front(ui, e, &font, r.width());
            let g = ui.painter().layout_no_wrap(shown, font, th.danger);
            let at = egui::pos2(r.min.x, r.center().y - g.size().y / 2.0);
            error_rect = Some(buttons.union(egui::Rect::from_min_size(at, g.size())));
            ui.painter().galley(at, g, th.danger);
            if cut {
                ui.interact(r, egui::Id::new("remotes_edit_error"), egui::Sense::hover())
                    .on_hover_text(e);
            }
        }

        let (_, name) = settings_row(
            ui,
            "Name",
            Some(Desc::Words("What the launcher calls it")),
            |ui| {
                ui.add_sized(
                    egui::vec2(fw, FIELD_H),
                    egui::TextEdit::singleline(&mut d.name)
                        .id(name_id)
                        .lock_focus(true) // Tab is ours: it moves fields
                        .hint_text("desk"),
                )
            },
        );
        laid.push(name.rect);
        if d.field == Field::Name {
            active_rect = Some(name.rect);
        }
        if name.clicked() {
            acts.push(Act::Field(Field::Name));
        }
        if name.changed() {
            d.error = None;
        }

        let (_, target) = settings_row(
            ui,
            "Target",
            Some(Desc::Words("user@pc or an ssh-config Host")),
            |ui| {
                ui.add_sized(
                    egui::vec2(fw, FIELD_H),
                    egui::TextEdit::singleline(&mut d.target)
                        .id(target_id)
                        .lock_focus(true)
                        .font(egui::TextStyle::Monospace)
                        .hint_text("user@pc"),
                )
            },
        );
        laid.push(target.rect);
        if d.field == Field::Target {
            active_rect = Some(target.rect);
        }
        if target.clicked() {
            acts.push(Act::Field(Field::Target));
        }
        if target.changed() {
            d.pick = None;
            d.error = None;
        }
        if !hosts.is_empty() {
            // One fixed line whether or not Target is active, so the form
            // does not grow when focus moves.
            let offered = if d.field == Field::Target {
                suggestions(hosts, remotes, d)
            } else {
                Vec::new()
            };
            let (line, _) = ui.allocate_exact_size(egui::vec2(w, LINE_H), egui::Sense::hover());
            let mut c = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(line.shrink2(egui::vec2(PAD, 0.0)))
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            egui::ScrollArea::horizontal()
                .id_salt("remotes_hosts")
                .show(&mut c, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let small = |t: &str| egui::RichText::new(t).color(th.dim).size(DESC_PT);
                        if offered.is_empty() {
                            ui.label(small(if d.field == Field::Target {
                                "No ~/.ssh/config Host starts with that"
                            } else {
                                "~/.ssh/config Hosts are offered in Target"
                            }));
                        } else {
                            ui.label(small("~/.ssh/config:"));
                        }
                        for (i, host) in offered.iter().enumerate() {
                            let picked = d.pick == Some(i);
                            let (r, resp) =
                                chip(ui, host, egui::FontId::monospace(DESC_PT), 14.0, picked);
                            if picked && scroll {
                                ui.scroll_to_rect(r, None);
                            }
                            if resp.clicked() {
                                acts.push(Act::Accept(host.to_string()));
                            }
                        }
                    });
                });
        }

        let shell_on = active && d.field == Field::Shell;
        let (_, (shell_rect, shell_clicked)) =
            settings_row(ui, "Shell", Some(Desc::Words("Test fills this in")), |ui| {
                let (r, resp) = chip(
                    ui,
                    shell_label(d.shell),
                    egui::FontId::proportional(CHIP_PT),
                    22.0,
                    shell_on,
                );
                (r, resp.clicked())
            });
        laid.push(shell_rect);
        if d.field == Field::Shell {
            active_rect = Some(shell_rect);
        }
        if shell_clicked {
            acts.push(Act::Field(Field::Shell));
            acts.push(Act::Key(Key::Right));
        }

        // Where Projects live on this Remote: one opens in a folder of its
        // own name here, made on the first launch if it is missing.
        if d.field == Field::Base {
            browse.pin_caret(&ctx, base_id, &d.base);
        }
        let (_, base) = settings_row(
            ui,
            "Projects folder",
            Some(Desc::Words("A Project opens in its own folder here")),
            |ui| {
                ui.add_sized(
                    egui::vec2(fw, FIELD_H),
                    egui::TextEdit::singleline(&mut d.base)
                        .id(base_id)
                        .lock_focus(true)
                        .font(egui::TextStyle::Monospace)
                        .hint_text("(home)"),
                )
            },
        );
        laid.push(base.rect);
        if d.field == Field::Base {
            let (rect, changed) = folder_list(
                ui,
                browse,
                w,
                (d.target.trim(), d.shell, d.name.trim()),
                &mut d.base,
                None,
                active,
                base.rect,
            );
            active_rect = Some(rect);
            if changed {
                d.error = None;
            }
        }
        if base.clicked() {
            acts.push(Act::Field(Field::Base));
        }
        if base.changed() {
            d.error = None;
        }

        // A Project that lives somewhere else on this Remote gets its own
        // directory here; a blank row follows the Projects folder.
        if d.index.is_some() {
            section_header(ui, w, "Project directories");
            if d.dirs.is_empty() {
                let text = "None: open a Project to give it its own";
                let (line, _) =
                    ui.allocate_exact_size(egui::vec2(w, LINE_H + 6.0), egui::Sense::hover());
                let font = egui::FontId::proportional(DESC_PT);
                let room = line.width() - 2.0 * PAD;
                note_fits = ui
                    .painter()
                    .layout_no_wrap(text.into(), font.clone(), th.dim)
                    .size()
                    .x
                    <= room;
                let g = one_line(ui, text, font, room);
                ui.painter().galley(
                    egui::pos2(line.min.x + PAD, line.center().y - g.size().y / 2.0),
                    g,
                    th.dim,
                );
            }
        }
        for k in 0..d.dirs.len() {
            if d.field == Field::Dir(k) {
                browse.pin_caret(&ctx, dir_id(k), &d.dirs[k].remote);
            }
            let full = remote::plain_dir(&d.dirs[k].project);
            let label = project_name(&d.dirs[k].project);
            // What a blank row means, spelled out.
            let follows = remote::derived_dir(&d.base, &d.dirs[k].project);
            let row = &mut d.dirs[k];
            let (_, (edit, clear)) = settings_row(ui, &label, Some(Desc::Path(&full)), |ui| {
                let clear = ui.small_button("clear").on_hover_text(
                    "Forget it: this Project goes back to its folder in the Projects folder",
                );
                if clear.clicked() {
                    acts.push(Act::ClearDir(k));
                }
                // The field column's left edge, shared with Name and Target.
                let width = (fw - clear.rect.width() - CLUSTER_GAP).max(60.0);
                let edit = ui.add_sized(
                    egui::vec2(width, FIELD_H),
                    egui::TextEdit::singleline(&mut row.remote)
                        .id(dir_id(k))
                        .lock_focus(true)
                        .font(egui::TextStyle::Monospace)
                        .hint_text(follows.as_str()),
                );
                (edit, clear.rect)
            });
            laid.push(edit.rect);
            laid.push(clear);
            if edit.clicked() {
                acts.push(Act::Field(Field::Dir(k)));
            }
            if edit.changed() {
                d.error = None;
            }
            if d.field != Field::Dir(k) {
                continue;
            }
            let project = project_name(&d.dirs[k].project);
            let (rect, changed) = folder_list(
                ui,
                browse,
                w,
                (d.target.trim(), d.shell, d.name.trim()),
                &mut d.dirs[k].remote,
                Some(&project),
                active,
                edit.rect,
            );
            active_rect = Some(rect);
            if changed {
                d.error = None;
            }
        }
        ui.add_space(4.0);
        laid.push(save);
        laid.push(cancel);

        // The rail's idiom: a focus bar down the left edge marks the item
        // being edited; no fill.
        let form = egui::Rect::from_min_max(top, egui::pos2(top.x + w, ui.cursor().min.y));
        ui.painter().rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(form.min.x + WASH_INSET, form.min.y),
                egui::vec2(2.0, form.height()),
            ),
            0.0,
            th.border_focus,
        );
        self.laid.error = error_rect;
        self.laid.dirs_note_fits = note_fits;
        // After the hosts strip's own scroll, so this one wins the list's.
        // A refused save shows its error with the buttons; otherwise the
        // active field, which on opening is Name near the top of the form.
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
            // its buttons and fields show together.
            let on_name = matches!(&self.mode, Mode::Edit(d) if d.field == Field::Name);
            match active_rect {
                Some(r) if !on_name || form.height() > ui.clip_rect().height() => {
                    ui.scroll_to_rect(r, None)
                }
                _ => ui.scroll_to_rect_animation(form, None, egui::style::ScrollAnimation::none()),
            }
        }
    }
}

// The settings rows' grammar (settings_menu `draw_pane`, appearance.rs).
/// A row's height.
const ROW_H: f32 = 46.0;
/// Left and right pad of every row's content.
const PAD: f32 = 18.0;
/// Label and description: size and centre line from the row's top.
const LABEL_PT: f32 = 13.0;
const LABEL_Y: f32 = 16.0;
const DESC_PT: f32 = 11.0;
const DESC_Y: f32 = 32.0;
/// A chip control's text size (Kind::Choice).
const CHIP_PT: f32 = 12.5;
/// A text field's height.
const FIELD_H: f32 = 24.0;
/// The selection wash's inset from the pane's sides (draw_pane's).
const WASH_INSET: f32 = 6.0;
/// Space between a cluster's controls.
const CLUSTER_GAP: f32 = 6.0;
/// The line reserved under each row, and for the hosts strip.
pub(crate) const LINE_H: f32 = 18.0;
/// A collapsed Remote: its row and its status, agents and git lines.
const ITEM_H: f32 = ROW_H + 3.0 * LINE_H;
/// Between one item and the next, so a wash never touches its neighbour.
const ITEM_GAP: f32 = 4.0;
/// A section header (appearance.rs `section_header`).
const HEADER_H: f32 = 30.0;
/// Under the folder list, before the next row.
const PICK_GAP: f32 = 6.0;
/// The separator and message line under the list.
const FOOT_H: f32 = 6.0 + 4.0 + LINE_H + 4.0;

/// The offline tooltip, and the selected row's reason line.
pub(crate) const OFFLINE_WHY: &str = "No TCP answer on its SSH port. This check cannot see past a \
     ProxyJump, so Test it or open a Remote Session to be sure.";

/// A Project directory's last component, the name its row and the folder
/// list give it.
fn project_name(project: &str) -> String {
    let full = remote::plain_dir(project);
    let trimmed = full.trim_end_matches(['\\', '/']);
    match trimmed.rsplit(['\\', '/']).next() {
        Some(last) if !last.is_empty() => last.to_string(),
        _ => full,
    }
}

/// Folder rows the form's list shows before it scrolls: few enough that a
/// Directory row and the list fit the pane's view at its minimum height.
const BROWSE_ROWS: f32 = 4.0;

/// A text field's width in a row `w` wide: the settings menu's.
fn field_w(w: f32) -> f32 {
    (w - 220.0).clamp(160.0, 300.0)
}

/// The selection wash for an item or row: its whole height, inset from the
/// pane's sides.
fn wash(r: egui::Rect) -> egui::Rect {
    r.shrink2(egui::vec2(WASH_INSET, 0.0))
}

/// A right-to-left child over `row`'s padded width, for its controls.
fn cluster(ui: &mut egui::Ui, row: egui::Rect) -> egui::Ui {
    let mut c = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(row.shrink2(egui::vec2(PAD, 0.0)))
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    c.spacing_mut().item_spacing = egui::vec2(CLUSTER_GAP, 0.0);
    c
}

/// A row's dim description: words cut at the end, or a path in monospace
/// cut at the front (whole on hover), or another machine string in
/// monospace cut at the end.
#[derive(Clone, Copy)]
enum Desc<'a> {
    Words(&'a str),
    Path(&'a str),
    Mono(&'a str),
}

/// One settings row: `label` over its description on the left, cut to stop
/// short of the controls `control` draws right to left. Returns the row and
/// what `control` returned.
fn settings_row<R>(
    ui: &mut egui::Ui,
    label: &str,
    desc: Option<Desc>,
    control: impl FnOnce(&mut egui::Ui) -> R,
) -> (egui::Rect, R) {
    let th = crate::theme::live(ui.ctx());
    let w = ui.available_width();
    let (row, _) = ui.allocate_exact_size(egui::vec2(w, ROW_H), egui::Sense::hover());
    let mut c = cluster(ui, row);
    let out = control(&mut c);
    let left = row.min.x + PAD;
    let room = (c.min_rect().min.x - 8.0 - left).max(0.0);
    let g = one_line(ui, label, egui::FontId::proportional(LABEL_PT), room);
    let at = egui::pos2(left, row.min.y + LABEL_Y - g.size().y / 2.0);
    ui.painter().galley(at, g, th.text);
    if let Some(desc) = desc {
        let (g, whole) = match desc {
            Desc::Words(t) => (
                one_line(ui, t, egui::FontId::proportional(DESC_PT), room),
                None,
            ),
            Desc::Mono(t) => (
                one_line(ui, t, egui::FontId::monospace(DESC_PT), room),
                None,
            ),
            Desc::Path(t) => {
                let font = egui::FontId::monospace(DESC_PT);
                let (shown, cut) = elide_front(ui, t, &font, room);
                let g = ui.painter().layout_no_wrap(shown, font, th.dim);
                (g, cut.then_some(t))
            }
        };
        let at = egui::pos2(left, row.min.y + DESC_Y - g.size().y / 2.0);
        let r = egui::Rect::from_min_size(at, g.size());
        ui.painter().galley(at, g, th.dim);
        if let Some(whole) = whole {
            ui.interact(
                r,
                ui.id().with(("remotes_desc", whole)),
                egui::Sense::hover(),
            )
            .on_hover_text(whole);
        }
    }
    (row, out)
}

/// An outline chip, the settings choice control's look: 1 px border, no
/// fill, `border_focus` when `on` or hovered.
fn chip(
    ui: &mut egui::Ui,
    text: &str,
    font: egui::FontId,
    h: f32,
    on: bool,
) -> (egui::Rect, egui::Response) {
    let th = crate::theme::live(ui.ctx());
    let g = ui.painter().layout_no_wrap(text.to_owned(), font, th.text);
    let pad = if h >= 20.0 { 12.0 } else { 5.0 };
    let (r, resp) =
        ui.allocate_exact_size(egui::vec2(g.size().x + 2.0 * pad, h), egui::Sense::click());
    let border = if on || resp.hovered() {
        th.border_focus
    } else {
        th.border
    };
    ui.painter().rect_stroke(
        r,
        egui::CornerRadius::same(4),
        egui::Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    ui.painter().galley(r.center() - g.size() / 2.0, g, th.text);
    (r, resp)
}

/// A section header: small dim caps over a hairline.
fn section_header(ui: &mut egui::Ui, w: f32, label: &str) {
    let th = crate::theme::live(ui.ctx());
    let (r, _) = ui.allocate_exact_size(egui::vec2(w, HEADER_H), egui::Sense::hover());
    ui.painter().text(
        egui::pos2(r.left() + PAD, r.bottom() - 10.0),
        egui::Align2::LEFT_CENTER,
        label.to_uppercase(),
        egui::FontId::proportional(DESC_PT),
        th.dim,
    );
    ui.painter().hline(
        egui::Rangef::new(r.left() + PAD, r.right() - PAD),
        r.bottom() - 1.0,
        egui::Stroke::new(1.0, th.border),
    );
}

/// `text` on one line, its front cut with "…" so the rest fits `width`;
/// and whether it was cut.
pub(crate) fn elide_front(
    ui: &egui::Ui,
    text: &str,
    font: &egui::FontId,
    width: f32,
) -> (String, bool) {
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

/// The line under a row for its Test or reach reason. Always there and
/// always one line, so a result arriving or the selection passing never
/// moves the rows below; a long one is cut with "…", whole on hover.
fn status_line(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    status: Option<(String, egui::Color32)>,
) -> egui::Rect {
    if let Some((text, color)) = status {
        let mut c = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        c.add(egui::Label::new(egui::RichText::new(text).color(color).size(DESC_PT)).truncate());
    }
    rect
}

/// What [`agents_line_ui`] drew.
struct AgentsLine {
    rect: egui::Rect,
    installs: Vec<egui::Rect>,
    /// Every piece's words fit uncut and every button is inside the line.
    fits: bool,
}

/// A row's agents line: words for each state on the left, and an
/// "install <agent>" (or "retry") button for each agent that can be
/// installed in the line's right-hand cluster. One line, always.
fn agents_line_ui(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    pieces: &[Piece],
    mut ask: impl FnMut(&'static str),
) -> AgentsLine {
    let th = crate::theme::live(ui.ctx());
    let mut installs = Vec::new();
    let mut fits = true;
    // The buttons first, right to left, so the words know their room.
    let mut buttons = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    buttons.spacing_mut().item_spacing.x = 4.0;
    for piece in pieces.iter().rev() {
        if let Piece::Install {
            provider,
            label,
            hover,
        } = piece
        {
            let b = buttons.small_button(label);
            installs.push(b.rect);
            if b.on_hover_text(hover).clicked() {
                ask(provider);
            }
        }
    }
    installs.reverse();
    // Buttons never shrink: one past the line's start is clipped away.
    fits &= installs.first().is_none_or(|b| b.min.x >= rect.min.x - 0.5);
    let words_end = installs
        .first()
        .map_or(rect.max.x, |b| b.min.x - CLUSTER_GAP);
    // One label, groups split by "; " (commas already list the agents in a
    // group): the narrowest such line is what lets a three-state mix fit
    // beside its button at the pane's minimum width.
    let (texts, hovers): (Vec<&str>, Vec<&str>) = pieces
        .iter()
        .filter_map(|p| match p {
            Piece::Text { text, hover } => Some((text.as_str(), hover.as_deref().unwrap_or(text))),
            Piece::Install { .. } => None,
        })
        .unzip();
    if !texts.is_empty() {
        let text = texts.join("; ");
        let font = egui::FontId::proportional(DESC_PT);
        let room = (words_end - rect.min.x).max(0.0);
        let need = ui
            .painter()
            .layout_no_wrap(text.clone(), font.clone(), th.dim)
            .size()
            .x;
        fits &= need <= room;
        let g = one_line(ui, &text, font, room);
        let at = egui::pos2(rect.min.x, rect.center().y - g.size().y / 2.0);
        let r = egui::Rect::from_min_size(at, g.size());
        ui.painter().galley(at, g, th.dim);
        ui.interact(
            r,
            ui.id().with(("remotes_agents", rect.min.y as i32)),
            egui::Sense::hover(),
        )
        .on_hover_text(hovers.join(
            "

",
        ));
    }
    AgentsLine {
        rect,
        installs,
        fits,
    }
}

/// A row's git line: what Test read on the left, and a "use this PC's"
/// button on the right when the Remote lacks a name or an email. Returns the
/// line, and the button's rect and whether it was clicked.
fn git_line_ui(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    line: Option<&crate::remote_git::Line>,
) -> (egui::Rect, Option<(egui::Rect, bool)>) {
    let Some(line) = line else {
        return (rect, None);
    };
    let th = crate::theme::live(ui.ctx());
    let mut c = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    c.spacing_mut().item_spacing.x = CLUSTER_GAP;
    let fix = line.lacks.then(|| {
        let b = c.small_button("use this PC's").on_hover_text(
            "Copy this PC's git name and email to this Remote. Shows them and asks first.",
        );
        (b.rect, b.clicked())
    });
    let room = match &fix {
        Some((b, _)) => (b.min.x - CLUSTER_GAP - rect.min.x).max(0.0),
        None => rect.width(),
    };
    let g = one_line(ui, &line.text, egui::FontId::proportional(DESC_PT), room);
    let at = egui::pos2(rect.min.x, rect.center().y - g.size().y / 2.0);
    let r = egui::Rect::from_min_size(at, g.size());
    let cut = g.elided;
    ui.painter().galley(at, g, th.dim);
    if cut {
        ui.interact(
            r,
            ui.id().with(("remotes_git", rect.min.y as i32)),
            egui::Sense::hover(),
        )
        .on_hover_text(&line.text);
    }
    (rect, fix)
}

/// The install confirm's body, in the room of a row's two lines: the exact
/// command, wrapped (two lines hold every command in the table at the
/// narrowest pane; anything longer is cut, whole on hover), and what will
/// happen on hover.
fn confirm_block(ui: &mut egui::Ui, rect: egui::Rect, offer: Option<&Offer>) -> (egui::Rect, bool) {
    let th = crate::theme::live(ui.ctx());
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
        egui::FontId::monospace(DESC_PT)
    } else {
        egui::FontId::proportional(DESC_PT)
    };
    let mut job = egui::text::LayoutJob::simple(text, font, th.text, rect.width().max(40.0));
    job.wrap.max_rows = 2;
    let galley = ui.painter().layout_job(job);
    let elided = galley.elided;
    let mut c = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    c.add(egui::Label::new(galley).selectable(false))
        .on_hover_text(hover);
    (rect, elided)
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
    /// A "use this PC's" button: open the git confirm.
    AskGit(usize),
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
        egui::Key::G => Key::Git,
        _ => return None,
    })
}

/// The folder list directly under the focused directory field `edit`: the
/// same height whatever it shows, so a listing landing moves nothing.
/// `source` is the draft's (target, shell, name); `whose` names the Project
/// a row is for. Returns what to keep in view (the field and its list
/// together when they fit, else whichever has the keyboard) and whether the
/// list changed `text`.
#[allow(clippy::too_many_arguments)]
fn folder_list(
    ui: &mut egui::Ui,
    browse: &mut Browser,
    w: f32,
    source: (&str, RemoteShell, &str),
    text: &mut String,
    whose: Option<&str>,
    active: bool,
    edit: egui::Rect,
) -> (egui::Rect, bool) {
    let (target, shell, name) = source;
    let body_h = BROWSE_ROWS * crate::remote_browse_ui::ROW_H;
    let h = Browser::height(ui, body_h);
    let (outer, _) = ui.allocate_exact_size(egui::vec2(w, h + PICK_GAP), egui::Sense::hover());
    let area = egui::Rect::from_min_size(
        egui::pos2(outer.min.x + PAD, outer.min.y),
        egui::vec2(w - 2.0 * PAD, h),
    );
    let mut c = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(area)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    if !remote::valid_target(target) {
        crate::remote_browse_ui::idle(
            &mut c,
            body_h,
            "Give a Target to list its folders",
            &mut browse.laid,
        );
        return (edit, false);
    }
    browse.set_source(target, shell, if name.is_empty() { target } else { name });
    let changed = browse.show(&mut c, text, body_h, active, whose);
    let area = browse.laid.area;
    // A margin, so neither sits flush on the edge.
    let both = edit.union(area).expand2(egui::vec2(0.0, 4.0));
    let rect = if both.height() <= ui.clip_rect().height() {
        both
    } else if browse.list_focused() {
        area
    } else {
        edit
    };
    (rect, changed)
}

/// This frame's keys for the current mode. Letters only count while
/// browsing: in the form they are typing. While the folder list's name
/// field is open, the editing keys are its own.
fn read_keys(ui: &egui::Ui, mode: &Mode, list: bool, naming: bool) -> Vec<Key> {
    use egui::Key as K;
    // Consumed, not peeked: the row goes before the form draws, and the
    // TextEdit that takes its place (and its Id) would delete a word or a
    // character of the next directory.
    let clear = ui.input_mut(|i| match mode {
        _ if naming => false,
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
    // The folder list's own keys; the field is not focused meanwhile, so
    // none of them edits it.
    let browse: Vec<Key> = ui.input_mut(|i| {
        let mut out = Vec::new();
        if list
            && !naming
            && matches!(mode, Mode::Edit(d) if matches!(d.field, Field::Base | Field::Dir(_)))
        {
            for (k, key) in [
                (K::ArrowLeft, Key::Left),
                (K::ArrowRight, Key::Right),
                (K::Backspace, Key::Backspace),
            ] {
                if i.consume_key(egui::Modifiers::NONE, k) {
                    out.push(key);
                }
            }
        }
        out
    });
    ui.input(|i| {
        let mut out = browse;
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
                for k in [K::K, K::J, K::A, K::D, K::T, K::I, K::G] {
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
            Mode::ConfirmDelete(_) | Mode::ConfirmGit { .. } => {}
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
            base: String::new(),
            dirs: Default::default(),
        }
    }

    fn typed(view: &mut RemotesView, field: Field, text: &str) {
        if let Mode::Edit(d) = &mut view.mode {
            match field {
                Field::Name => d.name = text.into(),
                Field::Target => d.target = text.into(),
                Field::Base => d.base = text.into(),
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
        // Tab walks Name, Target, Shell, Projects folder, then each directory.
        for _ in 0..4 {
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
    fn a_cleared_project_directory_is_forgotten_so_the_next_launch_derives_it() {
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
        assert_eq!(
            crate::remote::launch_dir(&list[0], None, Some(r"H:\src\app")).as_deref(),
            Ok("~/app")
        );
    }

    /// The Projects folder is saved with the Remote. Every open Project has
    /// a row, blank unless mapped; a blank row saves no mapping and a typed
    /// one saves under the plain key.
    #[test]
    fn the_projects_folder_is_saved_and_open_projects_get_rows() {
        let list = vec![mapped()];
        let open = [r"\\?\H:\src\app".to_string(), r"H:\src\new".to_string()];
        let mut d = Draft::edit(0, &list[0], &open);
        assert_eq!(d.dirs.len(), 3, "the two mapped, and the open unmapped one");
        assert_eq!(d.dirs[2].project, r"H:\src\new");
        assert_eq!(d.dirs[2].remote, "");
        d.base = " ~/src ".into();
        let saved = d.validate(&list).unwrap();
        assert_eq!(saved.base, "~/src");
        assert_eq!(saved.dirs.len(), 2, "a blank row maps nothing");
        assert!(!saved.dirs.contains_key(r"H:\src\new"));
        assert_eq!(
            crate::remote::launch_dir(&saved, None, Some(r"H:\src\new")).as_deref(),
            Ok("~/src/new")
        );
        // Typed: that Project's own directory. Blanked: back to the base.
        d.dirs[2].remote = "/srv/new".into();
        d.dirs[0].remote = " ".into();
        let saved = d.validate(&list).unwrap();
        assert_eq!(saved.dirs[r"H:\src\new"], "/srv/new");
        assert_eq!(saved.dirs.len(), 2, "{:?}", saved.dirs);
        d.base = "a\tb".into();
        assert_eq!(d.validate(&list).unwrap_err().field, Field::Base);
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
        // Another Project's first launch saves its directory meanwhile.
        list[0].dirs.insert(r"H:\src\new".into(), "/srv/new".into());
        let Mode::Edit(d) = &mut v.mode else { panic!() };
        d.clear_dir(0);
        assert_eq!(v.key(Key::Enter, &mut list), Effect::Changed);
        assert_eq!(list[0].dirs[r"H:\src\new"], "/srv/new");
        assert!(!list[0].dirs.contains_key(r"H:\src\app"));
    }

    fn listing(resolved: &str, names: &[&str]) -> crate::remote_browse::State {
        crate::remote_browse::State::Listed(crate::remote_browse::Listed {
            resolved: resolved.into(),
            entries: names
                .iter()
                .map(|n| crate::remote_browse::Entry::dir(n))
                .collect(),
            truncated: false,
        })
    }

    /// The form on Dir(0) ("/srv/app") with "/srv/" and "/srv/data/" listed.
    fn on_first_dir() -> (RemotesView, Vec<Remote>) {
        let mut v = RemotesView::new();
        let mut list = vec![mapped()];
        v.key(Key::Enter, &mut list);
        for _ in 0..4 {
            v.key(Key::Tab, &mut list);
        }
        assert_eq!(draft(&v).field, Field::Dir(0));
        let b = v.browse();
        // What a drawn frame does first.
        b.set_source("andy@desk", RemoteShell::Posix, "desk");
        b.put(
            "andy@desk",
            RemoteShell::Posix,
            "/srv/",
            listing("/srv", &["app", "apps", "data"]),
        );
        b.put(
            "andy@desk",
            RemoteShell::Posix,
            "/srv/data/",
            listing("/srv/data", &["x"]),
        );
        (v, list)
    }

    /// The Projects folder has the same folder list as a Project row: Down
    /// enters it, Enter goes into a folder and writes it to the field, and
    /// Enter from the field saves it as the Remote's base.
    #[test]
    fn the_projects_folder_is_picked_from_the_folder_list() {
        let mut v = RemotesView::new();
        let mut list = vec![mapped()];
        v.key(Key::Enter, &mut list);
        for _ in 0..3 {
            v.key(Key::Tab, &mut list);
        }
        assert_eq!(draft(&v).field, Field::Base);
        let b = v.browse();
        b.set_source("andy@desk", RemoteShell::Posix, "desk");
        b.put(
            "andy@desk",
            RemoteShell::Posix,
            "",
            listing("/home/andy", &["apps", "src"]),
        );
        assert!(v.footer(&list).contains("↓ folders"), "{}", v.footer(&list));
        v.key(Key::Down, &mut list);
        assert!(v.browse.list_focused());
        assert_eq!(draft(&v).field, Field::Base, "Down entered the list");
        assert!(v.footer(&list).contains("→ in"), "{}", v.footer(&list));
        v.key(Key::Down, &mut list); // apps -> src
        assert_eq!(v.key(Key::Enter, &mut list), Effect::None);
        assert_eq!(draft(&v).base, "~/src/");
        assert_eq!(list[0].base, "", "nothing saved");
        // Tab hands the keyboard back to the field; Enter there saves.
        v.key(Key::Tab, &mut list);
        assert!(!v.browse.list_focused());
        assert_eq!(draft(&v).field, Field::Base);
        assert_eq!(v.key(Key::Enter, &mut list), Effect::Changed);
        assert_eq!(list[0].base, "~/src/");
        assert_eq!(
            crate::remote::launch_dir(&list[0], None, Some(r"H:\src\new")).as_deref(),
            Ok("~/src/new")
        );
    }

    /// Down enters the list (not the next field), Enter there goes into a
    /// folder and never saves, Backspace goes back up, Esc hands the
    /// keyboard back without closing the form, and Enter then saves.
    #[test]
    fn the_folder_list_owns_its_keys_and_enter_saves_only_from_the_field() {
        let (mut v, mut list) = on_first_dir();
        assert!(v.footer(&list).contains("↓ folders"), "{}", v.footer(&list));
        v.key(Key::Down, &mut list);
        assert!(v.browse.list_focused());
        assert_eq!(draft(&v).field, Field::Dir(0), "Down entered the list");
        assert!(v.footer(&list).contains("→ in"), "{}", v.footer(&list));
        // "app" and "apps" match the filter; ".." is row 0.
        v.key(Key::Down, &mut list); // apps
        assert_eq!(v.key(Key::Enter, &mut list), Effect::None);
        assert_eq!(draft(&v).dirs[0].remote, "/srv/apps/");
        assert_eq!(list[0].dirs[r"H:\src\app"], "/srv/app", "nothing saved");
        v.key(Key::Backspace, &mut list);
        assert_eq!(draft(&v).dirs[0].remote, "/srv/");
        v.key(Key::Down, &mut list); // apps -> data
        v.key(Key::Right, &mut list);
        assert_eq!(draft(&v).dirs[0].remote, "/srv/data/");
        v.key(Key::Left, &mut list);
        assert_eq!(draft(&v).dirs[0].remote, "/srv/");
        assert_eq!(v.key(Key::Esc, &mut list), Effect::None);
        assert!(v.is_editing(), "Esc left the list, not the form");
        assert!(!v.browse.list_focused());
        assert_eq!(v.key(Key::Enter, &mut list), Effect::Changed);
        assert_eq!(list[0].dirs[r"H:\src\app"], "/srv/");
    }

    /// Tab from the list returns to the field (the next Tab moves on), and
    /// Ctrl+Backspace still forgets the row from the list.
    #[test]
    fn tab_leaves_the_list_and_clear_still_forgets_from_it() {
        let (mut v, mut list) = on_first_dir();
        v.key(Key::Down, &mut list);
        v.key(Key::Tab, &mut list);
        assert!(!v.browse.list_focused());
        assert_eq!(draft(&v).field, Field::Dir(0));
        v.key(Key::Tab, &mut list);
        assert_eq!(draft(&v).field, Field::Dir(1));
        v.key(Key::BackTab, &mut list);
        v.key(Key::Down, &mut list);
        assert!(v.browse.list_focused());
        v.key(Key::Clear, &mut list);
        assert_eq!(draft(&v).dirs.len(), 1);
        assert!(!v.browse.list_focused(), "the row it listed for is gone");
    }

    /// While the focused Directory is still listing, Down waits in the
    /// list instead of moving to the next field, and selects the first
    /// folder once the listing lands.
    #[test]
    fn down_while_loading_stays_on_the_directory_and_selects_on_arrival() {
        let mut v = RemotesView::new();
        let mut list = vec![mapped()];
        v.key(Key::Enter, &mut list);
        for _ in 0..4 {
            v.key(Key::Tab, &mut list);
        }
        // What a drawn frame does first.
        v.browse()
            .set_source("andy@desk", RemoteShell::Posix, "desk");
        assert!(v.footer(&list).contains("↓ folders"), "{}", v.footer(&list));
        v.key(Key::Down, &mut list);
        assert_eq!(draft(&v).field, Field::Dir(0), "did not move on");
        assert!(v.browse.list_focused());
        assert_eq!(
            v.footer(&list),
            "Listing folders · Tab/Esc back to the field"
        );
        v.browse().put(
            "andy@desk",
            RemoteShell::Posix,
            "/srv/",
            listing("/srv", &["app", "apps"]),
        );
        // "app" and "apps" match the filter: Enter goes into the first.
        v.key(Key::Enter, &mut list);
        assert_eq!(draft(&v).dirs[0].remote, "/srv/app/");
    }

    /// An unreachable Remote: Down retries (the cached answer is dropped)
    /// and the hint offers it.
    #[test]
    fn an_unreachable_remote_is_retried_from_the_form() {
        let (mut v, mut list) = on_first_dir();
        let gone = || crate::remote_browse::unreachable("ssh did not finish in 15s");
        v.browse()
            .put("andy@desk", RemoteShell::Posix, "/srv/", gone());
        assert!(v.footer(&list).contains("↓ retry"), "{}", v.footer(&list));
        v.key(Key::Down, &mut list);
        assert!(v.browse.list_focused());
        assert_eq!(
            v.browse().peek("andy@desk", RemoteShell::Posix, "/srv/"),
            None
        );
        assert_eq!(draft(&v).field, Field::Dir(0));
    }

    /// No valid Target: nothing to list, so Down is the form's.
    #[test]
    fn without_a_target_down_moves_fields() {
        let mut v = RemotesView::new();
        let mut list = vec![mapped()];
        v.key(Key::Enter, &mut list);
        if let Mode::Edit(d) = &mut v.mode {
            d.target = "not a target".into();
        }
        for _ in 0..4 {
            v.key(Key::Tab, &mut list);
        }
        v.key(Key::Down, &mut list);
        assert_eq!(draft(&v).field, Field::Dir(1));
        assert!(!v.browse.list_focused());
    }

    #[test]
    fn a_project_is_named_by_its_last_folder() {
        assert_eq!(project_name(r"H:\src\app"), "app");
        assert_eq!(project_name(r"\\?\H:\src\tool\"), "tool");
        assert_eq!(project_name("/home/a/b/"), "b");
        assert_eq!(project_name(r"H:\"), "H:");
    }

    /// A directory that cannot be listed: Down moves fields as before.
    #[test]
    fn with_nothing_listed_down_still_moves_to_the_next_field() {
        let mut v = RemotesView::new();
        let mut list = vec![mapped()];
        v.key(Key::Enter, &mut list);
        for _ in 0..4 {
            v.key(Key::Tab, &mut list);
        }
        v.browse().put(
            "andy@desk",
            RemoteShell::Posix,
            "/srv/",
            crate::remote_browse::State::Failed(crate::remote_browse::Failure::new(
                "No such directory",
            )),
        );
        v.key(Key::Down, &mut list);
        assert_eq!(draft(&v).field, Field::Dir(1));
        assert!(!v.browse.list_focused());
        assert!(
            v.footer(&list).starts_with("Tab next field"),
            "{}",
            v.footer(&list)
        );
    }

    #[test]
    fn the_clear_key_forgets_the_focused_directory_row_only() {
        let mut v = RemotesView::new();
        let mut list = vec![mapped()];
        v.key(Key::Enter, &mut list);
        // On a field that is not a directory, nothing to clear.
        v.key(Key::Clear, &mut list);
        assert_eq!(draft(&v).dirs.len(), 2);
        for _ in 0..5 {
            v.key(Key::Tab, &mut list);
        }
        assert_eq!(draft(&v).field, Field::Dir(1));
        assert!(
            v.footer(&list).contains("Ctrl+Backspace clear"),
            "{}",
            v.footer(&list)
        );
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
        assert_eq!(Field::Shell.step(true, 0), Field::Base);
        assert_eq!(Field::Base.step(true, 0), Field::Name);
        assert_eq!(Field::Name.step(false, 0), Field::Base);
        assert_eq!(Field::Shell.step(true, 2), Field::Base);
        assert_eq!(Field::Base.step(true, 2), Field::Dir(0));
        assert_eq!(Field::Dir(1).step(true, 2), Field::Name);
        assert_eq!(Field::Name.step(false, 2), Field::Dir(1));
        assert_eq!(Field::Dir(0).step(false, 2), Field::Base);
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
    fn the_browse_footer_offers_only_what_the_selection_can_do() {
        let mut v = RemotesView::new();
        let list = vec![r("desk", "desk")];
        assert!(!v.footer(&[]).contains("Del"), "{}", v.footer(&[]));
        assert!(v.footer(&list).contains("T test"));
        v.selected = 1; // the Add row
        for h in [v.footer(&list), v.footer(&[])] {
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
        let mut same = Draft::edit(0, &list[0], &[]);
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
        let d = Draft::edit(0, &list[0], &[]);
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
        v.key(Key::BackTab, &mut list); // wraps to Projects folder
        v.key(Key::BackTab, &mut list); // Shell
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
            git: None,
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
                git: None,
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
                git: None,
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
        assert!(v.footer(&list).contains("I install"), "{}", v.footer(&list));

        assert_eq!(v.key(Key::Install, &mut list), Effect::None);
        assert_eq!(
            v.mode,
            Mode::ConfirmInstall {
                row: 0,
                provider: "codex"
            }
        );
        assert!(v.footer(&list).contains("←/→ other agent"));
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
        assert!(!v.footer(&list).contains("install"));

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

    fn tested_git(v: &mut RemotesView, target: &str, git: Option<(&str, &str)>) {
        v.set_test_result(
            target,
            TestOutcome::Ok(TestReport {
                shell: RemoteShell::Posix,
                login_shell: None,
                curl: true,
                bash: true,
                agents: vec!["claude".into(), "codex".into(), "grok".into()],
                git: git.map(|(name, email)| remote_probe::GitInfo {
                    version: "2.43.0".into(),
                    name: name.into(),
                    email: email.into(),
                }),
            }),
        );
    }

    fn me() -> crate::remote_git::Identity {
        crate::remote_git::Identity {
            name: "Ann".into(),
            email: "ann@example.com".into(),
        }
    }

    /// G on a Remote with no git name shows this PC's values first; Esc
    /// writes nothing, and Enter asks the caller to write exactly them.
    #[test]
    fn the_git_name_is_copied_only_after_a_confirm_that_shows_it() {
        let mut v = RemotesView::new();
        v.local_git = me();
        let mut list = vec![r("desk", "pane-git-desk")];
        tested_git(&mut v, "pane-git-desk", Some(("", "")));
        assert!(
            v.footer(&list).contains("G git name"),
            "{}",
            v.footer(&list)
        );

        assert_eq!(v.key(Key::Git, &mut list), Effect::None);
        let Mode::ConfirmGit { row: 0, fix } = &v.mode else {
            panic!("{:?}", v.mode)
        };
        assert_eq!(fix.shown(), "user.name = Ann\nuser.email = ann@example.com");
        assert_eq!(v.footer(&list), "Enter set them · Esc cancel");
        assert_eq!(v.key(Key::Esc, &mut list), Effect::None);
        assert_eq!(v.mode, Mode::Browse);
        assert_eq!(v.git_fix, None, "Esc kept nothing to write");

        v.key(Key::Git, &mut list);
        assert_eq!(v.key(Key::Enter, &mut list), Effect::SetGit(0));
        assert_eq!(v.mode, Mode::Browse);
        assert_eq!(
            v.git_fix.as_ref().map(|f| f.shown()).as_deref(),
            Some("user.name = Ann\nuser.email = ann@example.com")
        );
        assert!(v.message.as_deref().unwrap().contains("desk"));
    }

    /// Nothing to copy: G says why and opens no confirm.
    #[test]
    fn g_with_nothing_to_copy_says_why() {
        let mut v = RemotesView::new();
        v.local_git = me();
        let mut list = vec![r("desk", "pane-git-why")];
        let mut said = |v: &mut RemotesView| {
            assert_eq!(v.key(Key::Git, &mut list), Effect::None);
            assert_eq!(v.mode, Mode::Browse);
            v.message.clone().unwrap()
        };
        assert!(said(&mut v).contains("Test desk first"));
        tested_git(&mut v, "pane-git-why", None);
        assert_eq!(said(&mut v), "git is not installed on desk");
        tested_git(&mut v, "pane-git-why", Some(("Bo", "bo@x")));
        assert!(said(&mut v).contains("already has"));
        v.local_git = Default::default();
        tested_git(&mut v, "pane-git-why", Some(("", "")));
        assert!(said(&mut v).contains("This PC has no git name"));
        let list = vec![r("desk", "pane-git-why")];
        assert!(!v.footer(&list).contains("G git") || v.git_lacks(0, &list));
        tested_git(&mut v, "pane-git-why", Some(("Bo", "bo@x")));
        assert!(!v.footer(&list).contains("G git"));
    }

    /// A value the Remote already has is never in what is written.
    #[test]
    fn only_the_missing_git_value_is_offered() {
        let mut v = RemotesView::new();
        v.local_git = me();
        let mut list = vec![r("desk", "pane-git-half")];
        tested_git(&mut v, "pane-git-half", Some(("Bo", "")));
        v.key(Key::Git, &mut list);
        let Mode::ConfirmGit { fix, .. } = &v.mode else {
            panic!("{:?}", v.mode)
        };
        assert_eq!(fix.shown(), "user.email = ann@example.com");
    }
}
