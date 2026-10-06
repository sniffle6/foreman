//! Git Changes: the JetBrains Commit tool window (non-modal, no staging
//! area). One `git status` read per refresh, on a cancellable worker; files
//! sit under Conflicts, Changes and Unversioned Files. Checkboxes are this
//! window's own selection, never Git's index: in Changes they pick what
//! Commit takes (none checked: all of it), in Unversioned Files what Add to
//! VCS takes. While shown, the repository watch (`watch.rs`) triggers the
//! refreshes; without one, becoming active does. The tree's context menu
//! (`menu`) acts on the selection; the writes and the commit panel are
//! `commit.rs`, the push dialog `push.rs`.
use super::commit::{CommitPanel, Scope, Write};
use super::file_tree::{self, ChangedFile, FileTree, MenuItem, TreeEvent};
use super::push::{self, PushDialog};
use super::toolbar::{self, Glyph, Tool};
use super::{DiffTarget, HistoryAct, Stage, git, watch};
use eframe::egui;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

/// Without a live watch, becoming active re-reads the status unless the last
/// read started this recently.
const REFOCUS_DEBOUNCE: Duration = Duration::from_secs(1);

/// What a toolbar button does.
#[derive(Clone, Copy, PartialEq)]
enum Bar {
    Refresh,
    Add,
    Push,
    Directories,
    Expand,
    Collapse,
}

/// Section headings in display order, each file's diff, and whether its rows
/// carry checkboxes. Conflicts only appear while a merge has them.
const SECTIONS: [(&str, Stage, bool); 3] = [
    ("Conflicts", Stage::Unstaged, false),
    ("Changes", Stage::Local, true),
    ("Unversioned Files", Stage::Untracked, true),
];
// Indices into `SECTIONS`.
const CONFLICTS: usize = 0;
const CHANGES: usize = 1;
const UNVERSIONED: usize = 2;

#[derive(Debug, PartialEq)]
struct Entry {
    section: usize,
    file: ChangedFile,
}

#[derive(Debug, Default, PartialEq)]
struct Parsed {
    branch: Option<String>,
    /// No commit yet (`branch.oid (initial)`).
    unborn: bool,
    entries: Vec<Entry>,
}

struct Status {
    /// Of the raw `git status` bytes (and the merge state): an identical
    /// re-read keeps this Status.
    hash: u64,
    branch: Option<String>,
    unborn: bool,
    merging: bool,
    tree: FileTree,
    /// Parallel to `tree.files`.
    targets: Vec<DiffTarget>,
}

fn status_char(b: u8) -> Result<char, String> {
    match b {
        b'A' | b'M' | b'D' | b'R' | b'C' | b'T' => Ok(b as char),
        _ => Err("Git returned an invalid file status".into()),
    }
}

/// One letter for a file's whole change since HEAD, from its index (`x`)
/// and working-copy (`y`) letters: gone from disk is a deletion; otherwise
/// a new, renamed or copied file stays that; otherwise the working copy
/// has the last word.
fn combined(x: u8, y: u8) -> Result<char, String> {
    let letter = match (x, y) {
        (_, b'D') => b'D',
        (b'A' | b'R' | b'C', _) => x,
        (_, b'.') => x,
        _ => y,
    };
    status_char(letter)
}

/// Parse `git status --porcelain=v2 -z --branch`. A file that is staged and
/// edited again is one entry: Changes compares the working copy with HEAD.
fn parse_status(bytes: &[u8]) -> Result<Parsed, String> {
    let mut parsed = Parsed::default();
    let mut records = bytes.split(|b| *b == 0).filter(|r| !r.is_empty());
    let incomplete = || "Git returned an incomplete status entry".to_owned();
    while let Some(record) = records.next() {
        let text = String::from_utf8_lossy(record);
        let (kind, rest) = text.split_at(text.find(' ').ok_or_else(incomplete)?);
        let rest = &rest[1..];
        // Ordinary and rename entries end with the path, which may hold spaces.
        let fields = match kind {
            "1" => 8,
            "2" => 9,
            "u" => 10,
            _ => 0,
        };
        let parts: Vec<&str> = rest.splitn(fields.max(1), ' ').collect();
        let file = |status, path: &str, previous| ChangedFile {
            status,
            path: path.to_owned(),
            previous,
        };
        match kind {
            "#" => {
                if let Some(head) = rest.strip_prefix("branch.head ") {
                    parsed.branch = Some(head.to_owned());
                }
                if rest == "branch.oid (initial)" {
                    parsed.unborn = true;
                }
            }
            "?" => parsed.entries.push(Entry {
                section: UNVERSIONED,
                file: file('?', rest, None),
            }),
            "u" => {
                let path = parts.get(9).ok_or_else(incomplete)?;
                parsed.entries.push(Entry {
                    section: CONFLICTS,
                    file: file('U', path, None),
                });
            }
            "1" | "2" => {
                let xy = parts[0].as_bytes();
                let path = parts.get(fields - 1).ok_or_else(incomplete)?;
                let previous = if kind == "2" {
                    let orig = records.next().ok_or_else(incomplete)?;
                    Some(String::from_utf8_lossy(orig).into_owned())
                } else {
                    None
                };
                if xy.len() != 2 {
                    return Err(incomplete());
                }
                parsed.entries.push(Entry {
                    section: CHANGES,
                    file: file(combined(xy[0], xy[1])?, path, previous),
                });
            }
            "!" => {}
            _ => return Err("Git returned an unknown status entry".into()),
        }
    }
    Ok(parsed)
}

/// Worker-only: read the status and build the sectioned tree. `Ok(None)` when
/// nothing changed since `unchanged`, so the shown tree is kept as is.
/// `git_dir` is found on the first read and reused, so the merge check is
/// one stat, not another Git spawn.
fn load(
    cwd: &Path,
    cancel: &Arc<AtomicBool>,
    unchanged: Option<u64>,
    flat: bool,
    git_dir: &mut Option<PathBuf>,
) -> Result<Option<Status>, String> {
    let read = |args: &[&str], cap| {
        git::output(cwd, args, cancel, cap, Duration::from_secs(30)).map_err(|e| match e {
            git::GitError::TooLarge => "Git status exceeds the 32 MiB display limit".to_owned(),
            git::GitError::Failed(stderr) => format!("Cannot read the working tree: {stderr}"),
            other => other.to_string(),
        })
    };
    if git_dir.is_none() {
        let dir = read(&["rev-parse", "--absolute-git-dir"], 64 * 1024)?;
        *git_dir = Some(PathBuf::from(String::from_utf8_lossy(&dir).trim()));
    }
    let bytes = read(
        &[
            "status",
            "--porcelain=v2",
            "-z",
            "--branch",
            "--untracked-files=all",
            "--find-renames",
        ],
        32 << 20,
    )?;
    let merging = git_dir
        .as_ref()
        .is_some_and(|d| d.join("MERGE_HEAD").exists());
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut h);
        merging.hash(&mut h);
        h.finish()
    };
    if unchanged == Some(hash) {
        return Ok(None);
    }
    let parsed = parse_status(&bytes)?;
    let mut sections: Vec<Vec<ChangedFile>> = SECTIONS.iter().map(|_| Vec::new()).collect();
    for entry in parsed.entries {
        sections[entry.section].push(entry.file);
    }
    // Targets follow the tree's file order: section by section, as listed.
    let targets = sections
        .iter()
        .enumerate()
        .flat_map(|(i, files)| {
            files.iter().map(move |f| DiffTarget {
                stage: SECTIONS[i].1,
                commit: String::new(),
                parent: None,
                status: f.status,
                old_path: f.previous.clone(),
                path: f.path.clone(),
                merge: false,
            })
        })
        .collect();
    let tree = FileTree::grouped(
        SECTIONS
            .iter()
            .zip(sections)
            .map(|((name, _, check), files)| (*name, files, *check))
            .collect(),
        flat,
    );
    Ok(Some(Status {
        hash,
        branch: parsed.branch,
        unborn: parsed.unborn,
        merging,
        tree,
        targets,
    }))
}

/// The tree's context-menu actions. Labels (JetBrains' names) and menu
/// order are in `menu`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Action {
    ShowDiff,
    Add,
    Resolve,
    Rollback,
    Delete,
    Ignore,
    CopyPath,
    Reveal,
}

fn changed(t: &DiffTarget) -> bool {
    t.stage == Stage::Local
}
fn unversioned(t: &DiffTarget) -> bool {
    t.stage == Stage::Untracked
}
fn conflict(t: &DiffTarget) -> bool {
    t.stage == Stage::Unstaged && t.status == 'U'
}
/// Still on disk to delete.
fn deletable(t: &DiffTarget) -> bool {
    unversioned(t) || (changed(t) && t.status != 'D')
}

/// The tree's context menu for the selected `files`. Every item is always
/// listed; each is enabled only if it applies to something selected.
fn menu(targets: &[DiffTarget], files: &[usize], busy: bool) -> Vec<MenuItem<Action>> {
    let any = |kind: fn(&DiffTarget) -> bool| files.iter().any(|&f| kind(&targets[f]));
    let item = |action, label, enabled| MenuItem {
        action,
        label,
        enabled,
    };
    vec![
        item(Action::ShowDiff, "Show Diff", files.len() == 1),
        item(Action::Add, "Add to VCS", !busy && any(unversioned)),
        item(Action::Resolve, "Mark Resolved", !busy && any(conflict)),
        item(Action::Rollback, "Rollback…", !busy && any(changed)),
        item(Action::Delete, "Delete…", !busy && any(deletable)),
        item(
            Action::Ignore,
            "Add to .gitignore",
            !busy && any(unversioned),
        ),
        item(Action::CopyPath, "Copy Path", !files.is_empty()),
        item(Action::Reveal, "Show in Explorer", files.len() == 1),
    ]
}

/// The paths of the `files` that are `kind`, sorted.
fn paths_of(targets: &[DiffTarget], files: &[usize], kind: fn(&DiffTarget) -> bool) -> Vec<String> {
    let mut paths: Vec<String> = files
        .iter()
        .map(|&f| &targets[f])
        .filter(|t| kind(t))
        .map(|t| t.path.clone())
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

/// The write `action` starts, over just the selected files it applies
/// to. Rollback and Delete are confirmed first (`Confirm`).
fn write(targets: &[DiffTarget], action: Action, files: &[usize]) -> Option<Write> {
    let paths = |kind| paths_of(targets, files, kind);
    let write = match action {
        Action::Add => Write::Track(paths(unversioned)),
        Action::Resolve => Write::Resolve(paths(conflict)),
        Action::Delete => Write::Delete(paths(deletable)),
        Action::Ignore => Write::Ignore(paths(unversioned)),
        Action::Rollback => {
            // A new file leaves the index (kept on disk); a rename's new
            // side does too, while its old side comes back from HEAD.
            let (mut untrack, mut restore) = (Vec::new(), Vec::new());
            for t in files.iter().map(|&f| &targets[f]).filter(|t| changed(t)) {
                if matches!(t.status, 'A' | 'C' | 'R') {
                    untrack.push(t.path.clone());
                } else {
                    restore.push(t.path.clone());
                }
                restore.extend(t.old_path.clone().filter(|_| t.status == 'R'));
            }
            for list in [&mut untrack, &mut restore] {
                list.sort();
                list.dedup();
            }
            Write::Rollback { untrack, restore }
        }
        _ => return None,
    };
    (!write.is_empty()).then_some(write)
}

/// What the next commit takes: the checked Changes files, or all of them
/// when none is (always all of them while merging).
fn scope(status: &Status, checked: &HashSet<String>) -> Scope {
    let changes: Vec<&DiffTarget> = status.targets.iter().filter(|t| changed(t)).collect();
    let picked: Vec<&DiffTarget> = changes
        .iter()
        .copied()
        .filter(|t| checked.contains(&t.path))
        .collect();
    let any = !picked.is_empty() && !status.merging;
    let taken = if any { picked } else { changes };
    let mut paths: Vec<String> = taken
        .iter()
        .flat_map(|t| std::iter::once(t.path.clone()).chain(t.old_path.clone()))
        .collect();
    paths.sort();
    paths.dedup();
    Scope {
        paths,
        files: taken.len(),
        checked: any,
        conflicts: status.targets.iter().any(conflict),
        merging: status.merging,
        unborn: status.unborn,
    }
}

/// Show `path` selected in Explorer; a file that's gone opens its folder.
fn reveal(path: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = std::process::Command::new("explorer");
        if path.exists() {
            let shown = path.to_string_lossy().replace('/', "\\");
            // Explorer wants the quotes around the path only, after the comma.
            cmd.raw_arg(format!("/select,\"{shown}\""));
        } else if let Some(dir) = path.parent() {
            cmd.arg(dir);
        }
        let _ = cmd.spawn();
    }
    #[cfg(not(windows))]
    let _ = path;
}

/// A Rollback or Delete waiting on the user's OK.
struct Confirm {
    title: &'static str,
    lead: String,
    button: &'static str,
    paths: Vec<String>,
    write: Write,
}
impl Confirm {
    fn new(write: Write) -> Option<Self> {
        let (title, button, verb, paths) = match &write {
            Write::Rollback { untrack, restore } => {
                let mut paths: Vec<String> = untrack.iter().chain(restore).cloned().collect();
                paths.sort();
                paths.dedup();
                (
                    "Rollback Changes",
                    "Rollback",
                    "Discard the local changes to",
                    paths,
                )
            }
            Write::Delete(paths) => (
                "Delete Files",
                "Delete",
                "Move to the Recycle Bin",
                paths.clone(),
            ),
            _ => return None,
        };
        let lead = match paths.len() {
            1 => format!("{verb} this file?"),
            n => format!("{verb} these {n} files?"),
        };
        Some(Self {
            title,
            lead,
            button,
            paths,
            write,
        })
    }
    /// One frame of the modal: `Some(true)` confirmed, `Some(false)` cancelled.
    fn show(&self, ctx: &egui::Context, id: egui::Id) -> Option<bool> {
        let th = crate::theme::live(ctx);
        let mut choice = None;
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
            choice = Some(true);
        }
        let modal = egui::Modal::new(id).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.strong(self.title);
            ui.add_space(4.0);
            ui.label(egui::RichText::new(&self.lead).color(th.dim));
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for path in &self.paths {
                        ui.label(
                            egui::RichText::new(file_tree::display_path(path))
                                .color(th.text)
                                .monospace(),
                        );
                    }
                });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui
                    .button(egui::RichText::new(self.button).color(th.danger))
                    .clicked()
                {
                    choice = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(false);
                }
            });
        });
        if choice.is_none() && modal.should_close() {
            choice = Some(false);
        }
        choice
    }
}

struct Request {
    cancel: Arc<AtomicBool>,
    receiver: mpsc::Receiver<(Result<Option<Status>, String>, Option<PathBuf>)>,
    /// Started by the watch: no spinner.
    quiet: bool,
}
impl Drop for Request {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub struct ChangesView {
    /// Recorded during `show`; the owning manager drains them after the draw.
    pub acts: Vec<HistoryAct>,
    cwd: Option<PathBuf>,
    /// Found by the first read.
    git_dir: Option<PathBuf>,
    request: Option<Request>,
    status: Option<Result<Status, String>>,
    started: Option<Instant>,
    was_active: bool,
    follow: watch::Follow,
    /// The watch's worktree generation the last read covers; `None` until
    /// the watch arrives, which triggers one read for anything missed.
    seen: Option<u64>,
    /// The watch fired during a read: read again when it lands.
    again: bool,
    /// The checked files, by path: this window's own selection, never
    /// written to Git. Survives re-reads; a path that leaves the list
    /// leaves the set.
    checked: HashSet<String>,
    /// What Commit takes; recomputed when the status or the checks change.
    scope: Scope,
    /// Files in a flat list per section instead of directories.
    flat: bool,
    confirm: Option<Confirm>,
    push: Option<PushDialog>,
    commit: CommitPanel,
    #[cfg(test)]
    tree_top: f32,
}
impl Drop for ChangesView {
    fn drop(&mut self) {
        let request = self.request.take();
        let status = self.status.take();
        if request.is_some() || status.is_some() {
            std::thread::spawn(move || drop((request, status)));
        }
    }
}
impl ChangesView {
    pub fn new(cwd: Option<PathBuf>) -> Self {
        Self {
            acts: Vec::new(),
            cwd,
            git_dir: None,
            request: None,
            status: None,
            started: None,
            was_active: false,
            follow: watch::Follow::default(),
            seen: None,
            again: false,
            checked: HashSet::new(),
            scope: Scope::default(),
            flat: false,
            confirm: None,
            push: None,
            commit: CommitPanel::new(),
            #[cfg(test)]
            tree_top: 0.0,
        }
    }
    /// Start a fresh read, cancelling any in flight. The shown status stays
    /// until the new one arrives, so a refresh never blanks the window.
    /// `quiet` reads (from the watch) hide the spinner; callers only start
    /// one when no read is in flight, since cancelling on every event would
    /// starve under steady churn.
    fn refresh(&mut self, ctx: &egui::Context, quiet: bool) {
        self.again = false;
        let Some(cwd) = self.cwd.clone() else {
            self.status = Some(Err("This project has no directory".into()));
            return;
        };
        let (tx, receiver) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let ctx = ctx.clone();
        let unchanged = match &self.status {
            Some(Ok(status)) => Some(status.hash),
            _ => None,
        };
        let (flat, mut git_dir) = (self.flat, self.git_dir.clone());
        std::thread::spawn(move || {
            let result = load(&cwd, &stop, unchanged, flat, &mut git_dir);
            if !stop.load(Ordering::Relaxed) {
                let _ = tx.send((result, git_dir));
                ctx.request_repaint();
            }
        });
        // Replacing the request drops (and so cancels) the previous one.
        self.request = Some(Request {
            cancel,
            receiver,
            quiet,
        });
        self.started = Some(Instant::now());
    }
    fn poll(&mut self, ctx: &egui::Context) {
        let Some(request) = &self.request else { return };
        let mut next = match request.receiver.try_recv() {
            Ok((result, git_dir)) => {
                self.git_dir = git_dir;
                match result {
                    Ok(Some(status)) => Ok(status),
                    // Same bytes as shown: keep the tree, rows and all.
                    Ok(None) => {
                        self.request = None;
                        return self.follow_up(ctx);
                    }
                    Err(e) => Err(e),
                }
            }
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("Git status worker stopped".into()),
        };
        self.request = None;
        // Carry collapse state and the selection across the re-read.
        if let (Some(Ok(old)), Ok(new)) = (&self.status, &mut next) {
            new.tree.collapse(&old.tree.collapsed_keys());
            new.tree.keep_selection(&old.tree);
        }
        if let Ok(new) = &mut next {
            // Grouping may have changed while the read ran.
            new.tree.set_flat(self.flat);
        }
        let old = std::mem::replace(&mut self.status, Some(next));
        std::thread::spawn(move || drop(old));
        self.recheck();
        self.follow_up(ctx);
    }
    fn follow_up(&mut self, ctx: &egui::Context) {
        if self.again {
            self.refresh(ctx, true);
        }
    }
    /// After a re-read or a change to `checked`: drop checks on paths no
    /// longer listed, tick the tree to match, and recompute the scope.
    fn recheck(&mut self) {
        let Some(Ok(status)) = &mut self.status else {
            self.scope = Scope::default();
            return;
        };
        let listed: HashSet<&str> = status
            .targets
            .iter()
            .filter(|t| !conflict(t))
            .map(|t| t.path.as_str())
            .collect();
        self.checked.retain(|p| listed.contains(p.as_str()));
        let ticked = status
            .targets
            .iter()
            .enumerate()
            .filter(|(_, t)| !conflict(t) && self.checked.contains(&t.path))
            .map(|(i, _)| i);
        status.tree.set_checked(ticked);
        self.scope = scope(status, &self.checked);
    }
    /// Start `write`. Files it takes out of their section (Add to VCS, a
    /// rolled-back new file) come back unchecked, as in JetBrains.
    fn start(&mut self, ctx: &egui::Context, write: Write) {
        let Some(cwd) = self.cwd.clone() else { return };
        let moved = match &write {
            Write::Track(paths) => paths.clone(),
            Write::Rollback { untrack, .. } => untrack.clone(),
            _ => Vec::new(),
        };
        if self.commit.start(ctx, &cwd, write) {
            for path in &moved {
                self.checked.remove(path);
            }
            self.recheck();
        }
    }
    /// A context-menu choice over `files` (indices into the shown targets).
    fn act(&mut self, ctx: &egui::Context, action: Action, files: &[usize]) {
        let Some(Ok(status)) = &self.status else {
            return;
        };
        let targets = &status.targets;
        match action {
            Action::ShowDiff => {
                if let [file] = files {
                    self.acts.push(HistoryAct::OpenDiff(targets[*file].clone()));
                }
            }
            Action::CopyPath => {
                let paths: Vec<_> = files.iter().map(|&f| targets[f].path.as_str()).collect();
                ctx.copy_text(paths.join("\n"));
            }
            Action::Reveal => {
                if let ([file], Some(cwd)) = (files, &self.cwd) {
                    reveal(&cwd.join(&targets[*file].path));
                }
            }
            Action::Rollback | Action::Delete => {
                self.confirm = write(targets, action, files).and_then(Confirm::new);
            }
            Action::Add | Action::Resolve | Action::Ignore => {
                if let Some(write) = write(targets, action, files) {
                    self.start(ctx, write);
                }
            }
        }
    }
    /// The checked Unversioned files: what the toolbar's Add to VCS takes.
    fn checked_unversioned(&self) -> Vec<String> {
        let Some(Ok(status)) = &self.status else {
            return Vec::new();
        };
        let mut paths: Vec<String> = status
            .targets
            .iter()
            .filter(|t| unversioned(t) && self.checked.contains(&t.path))
            .map(|t| t.path.clone())
            .collect();
        paths.sort();
        paths
    }
    pub fn show(&mut self, ui: &mut egui::Ui, rect: egui::Rect, active: bool, base: egui::Id) {
        // Read on first show; then on every watch change while shown, or,
        // with no live watch, whenever the window becomes active.
        let refocused = active
            && !self.was_active
            && self.started.is_none_or(|t| t.elapsed() >= REFOCUS_DEBOUNCE);
        self.was_active = active;
        if self.started.is_none() {
            self.refresh(ui.ctx(), false);
        }
        let generation = self
            .follow
            .live(self.cwd.as_deref(), ui.ctx())
            .map(|w| w.worktree_gen());
        match generation {
            Some(g) if self.seen != Some(g) => {
                self.seen = Some(g);
                if self.request.is_none() {
                    self.refresh(ui.ctx(), true);
                } else {
                    self.again = true;
                }
            }
            Some(_) => {}
            None if refocused && self.request.is_none() => self.refresh(ui.ctx(), false),
            None => {}
        }
        self.poll(ui.ctx());
        let th = crate::theme::live(ui.ctx());
        let zoom = crate::view_scale::ViewScale::from_ctx(ui.ctx());
        let scale = zoom.factor();
        ui.painter().rect_filled(rect, 0.0, th.bg);
        let inner = rect.shrink(zoom.px(8.0));
        // The commit panel is pinned to the bottom; the tree takes the rest.
        let panel_used = {
            let mut panel = ui.new_child(egui::UiBuilder::new().max_rect(inner));
            panel.set_clip_rect(inner.intersect(ui.clip_rect()));
            zoom.apply(&mut panel);
            let (used, outcome) =
                self.commit
                    .show(&mut panel, inner, self.cwd.as_deref(), &self.scope, base);
            // The watch sees the index change; without one, re-read now.
            if outcome.wrote && generation.is_none() {
                if self.request.is_none() {
                    self.refresh(panel.ctx(), true);
                } else {
                    self.again = true;
                }
            }
            if outcome.push
                && let Some(cwd) = &self.cwd
            {
                self.push = Some(PushDialog::open(panel.ctx(), cwd));
            }
            used
        };
        let tree_rect = egui::Rect::from_min_max(
            inner.min,
            egui::pos2(
                inner.right(),
                (inner.bottom() - panel_used - zoom.px(6.0)).max(inner.top()),
            ),
        );
        let mut ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(base)
                .max_rect(tree_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        ui.set_clip_rect(tree_rect.intersect(ui.clip_rect()));
        zoom.apply(&mut ui);
        self.toolbar(&mut ui, scale);
        let busy = self.commit.busy();
        // A dialog owns Enter and Esc while it's up.
        let keys = active && self.confirm.is_none() && self.push.is_none();
        let mut event = None;
        match &mut self.status {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Reading working tree…");
                });
            }
            Some(Err(error)) => {
                ui.colored_label(th.dim, error.as_str());
            }
            Some(Ok(status)) if status.tree.files.is_empty() => {
                ui.colored_label(th.dim, "Nothing to commit — the working tree is clean.");
            }
            Some(Ok(status)) => {
                #[cfg(test)]
                {
                    self.tree_top = ui.cursor().top();
                }
                let targets = &status.targets;
                let menu = |files: &[usize]| menu(targets, files, busy);
                event = file_tree::show(&mut ui, &mut status.tree, scale, keys, &menu);
            }
        }
        match event {
            Some(TreeEvent::Open(file)) => self.act(ui.ctx(), Action::ShowDiff, &[file]),
            Some(TreeEvent::Act { action, files }) => self.act(ui.ctx(), action, &files),
            Some(TreeEvent::Check { files, on }) => {
                if let Some(Ok(status)) = &self.status {
                    for f in files {
                        let path = status.targets[f].path.clone();
                        if on {
                            self.checked.insert(path);
                        } else {
                            self.checked.remove(&path);
                        }
                    }
                }
                self.recheck();
            }
            None => {}
        }
        if let Some(confirm) = &self.confirm {
            match confirm.show(ui.ctx(), base.with("confirm")) {
                Some(true) => {
                    if let Some(confirm) = self.confirm.take() {
                        self.start(ui.ctx(), confirm.write);
                    }
                }
                Some(false) => self.confirm = None,
                None => {}
            }
        }
        if let Some(dialog) = &mut self.push {
            match dialog.show(ui.ctx(), base.with("push")) {
                push::Choice::Push(target) => {
                    self.push = None;
                    self.start(ui.ctx(), Write::Push(target));
                }
                push::Choice::Cancel => self.push = None,
                push::Choice::Pending => {}
            }
        }
    }
    /// The header: an icon toolbar (Refresh when the watch can't, Add to VCS
    /// for checked unversioned files, Push…, Directories, Expand/Collapse All)
    /// that folds what doesn't fit behind a ">" menu, then the branch and
    /// change count on one truncating line.
    fn toolbar(&mut self, ui: &mut egui::Ui, scale: f32) {
        let th = crate::theme::live(ui.ctx());
        let add = self.checked_unversioned();
        let busy = self.commit.busy();
        let (mut cmds, mut tools) = (Vec::new(), Vec::new());
        let mut tool = |cmd, glyph, label: &str, hint: &str, enabled, on| {
            let (label, hint) = (label.to_owned(), hint.to_owned());
            cmds.push(cmd);
            tools.push(Tool {
                glyph,
                label,
                hint,
                enabled,
                on,
            });
        };
        // A live watch re-reads on every change; Refresh is only for when it
        // can't.
        if self.follow.down() || matches!(self.status, Some(Err(_))) {
            let hint = super::refresh_hint(self.follow.down());
            tool(Bar::Refresh, Glyph::Refresh, "Refresh", hint, true, false);
        }
        if let Some(Ok(status)) = &self.status {
            if !add.is_empty() {
                let label = format!("Add to VCS ({})", add.len());
                let hint = "Put the checked unversioned files under version control";
                tool(Bar::Add, Glyph::Add, &label, hint, !busy, false);
            }
            // One push at a time; and none while a commit is landing, since
            // the dialog would list the outgoing commits without it.
            let can_push = !busy
                && !self.commit.pushing()
                && self.push.is_none()
                && status.branch.as_deref().is_some_and(|b| b != "(detached)");
            let hint = "Review the outgoing commits, then push";
            tool(Bar::Push, Glyph::Push, "Push…", hint, can_push, false);
            let hint = "Group files by directory, or list them flat";
            tool(
                Bar::Directories,
                Glyph::Directories,
                "Directories",
                hint,
                true,
                !self.flat,
            );
            let hint = "Expand every section and folder";
            tool(Bar::Expand, Glyph::Expand, "Expand All", hint, true, false);
            let hint = "Collapse every section and folder";
            tool(
                Bar::Collapse,
                Glyph::Collapse,
                "Collapse All",
                hint,
                true,
                false,
            );
        }
        let clicked = toolbar::show(ui, &tools, scale).map(|i| cmds[i]);
        ui.horizontal(|ui| {
            if self.request.as_ref().is_some_and(|r| !r.quiet) {
                ui.spinner();
            }
            let font = egui::TextStyle::Body.resolve(ui.style());
            let part = |color| egui::TextFormat {
                font_id: font.clone(),
                color,
                ..Default::default()
            };
            let mut job = egui::text::LayoutJob::default();
            let mut merging = false;
            match &self.status {
                Some(Ok(status)) => {
                    let branch = match status.branch.as_deref() {
                        Some("(detached)") | None => "Detached HEAD".to_owned(),
                        Some(name) => format!("On {name}"),
                    };
                    job.append(&branch, 0.0, part(th.text));
                    let n = status.tree.files.len();
                    let count = if n == 1 {
                        "1 change".to_owned()
                    } else {
                        format!("{n} changes")
                    };
                    job.append(&count, 8.0 * scale, part(th.dim));
                    if status.merging {
                        merging = true;
                        job.append("Merging", 8.0 * scale, part(th.caret));
                    }
                }
                _ => job.append("Working tree", 0.0, part(th.text)),
            }
            let label = ui.add(egui::Label::new(job).truncate());
            if merging {
                label.on_hover_text(
                    "A merge is in progress: Commit takes every change, checked or not",
                );
            }
        });
        let (mut refresh, mut add_now) = (false, false);
        match clicked {
            Some(Bar::Refresh) => refresh = true,
            Some(Bar::Add) => add_now = true,
            Some(Bar::Push) => {
                if let Some(cwd) = &self.cwd {
                    self.push = Some(PushDialog::open(ui.ctx(), cwd));
                }
            }
            Some(Bar::Directories) => {
                self.flat = !self.flat;
                if let Some(Ok(status)) = &mut self.status {
                    status.tree.set_flat(self.flat);
                }
            }
            Some(bar @ (Bar::Expand | Bar::Collapse)) => {
                if let Some(Ok(status)) = &mut self.status {
                    status.tree.set_all_collapsed(bar == Bar::Collapse);
                }
            }
            None => {}
        }
        if add_now {
            self.start(ui.ctx(), Write::Track(add));
        }
        if refresh {
            self.refresh(ui.ctx(), false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_history::tests::git;

    fn flag() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    #[test]
    fn porcelain_v2_puts_every_versioned_change_in_one_section() {
        let z = "# branch.oid (initial)\0# branch.head main\0\
            1 MM N... 100644 100644 100644 h1 h2 src/both sides.rs\0\
            1 .D N... 100644 100644 000000 h1 h2 gone.rs\0\
            1 .A N... 000000 000000 100644 h1 h2 tracked.rs\0\
            1 AD N... 000000 100644 000000 h1 h2 added then gone.rs\0\
            2 RM N... 100644 100644 100644 h1 h2 R100 new name.rs\0old name.rs\0\
            u UU N... 100644 100644 100644 100644 h1 h2 h3 clash.rs\0\
            ? dir/untracked file.txt\0";
        let parsed = parse_status(z.as_bytes()).unwrap();
        assert_eq!(parsed.branch.as_deref(), Some("main"));
        assert!(parsed.unborn);
        let summary: Vec<_> = parsed
            .entries
            .iter()
            .map(|e| {
                let f = &e.file;
                (e.section, f.status, f.path.as_str(), f.previous.as_deref())
            })
            .collect();
        assert_eq!(
            summary,
            [
                (CHANGES, 'M', "src/both sides.rs", None),
                (CHANGES, 'D', "gone.rs", None),
                (CHANGES, 'A', "tracked.rs", None),
                (CHANGES, 'D', "added then gone.rs", None),
                (CHANGES, 'R', "new name.rs", Some("old name.rs")),
                (CONFLICTS, 'U', "clash.rs", None),
                (UNVERSIONED, '?', "dir/untracked file.txt", None),
            ]
        );
        assert!(parse_status(b"2 R. N... 1 1 1 h h R100 new\0").is_err());
        assert!(parse_status(b"1 M\0").is_err());
        assert!(parse_status(b"z what\0").is_err());
        assert_eq!(parse_status(b"").unwrap(), Parsed::default());
    }

    fn repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        for args in [
            &["init", "-b", "main"][..],
            &["config", "user.name", "Changes Test"],
            &["config", "user.email", "changes@example.test"],
            &["config", "commit.gpgsign", "false"],
        ] {
            git(repo.path(), args);
        }
        repo
    }

    #[test]
    fn real_working_tree_reads_sections_and_targets_without_writes() {
        let repo = repo();
        let dir = repo.path();
        let mut git_dir = None;
        let empty = load(dir, &flag(), None, false, &mut git_dir)
            .unwrap()
            .unwrap();
        assert!(empty.tree.files.is_empty());
        assert_eq!(empty.branch.as_deref(), Some("main"));
        assert!(empty.unborn && !empty.merging);
        assert!(git_dir.as_ref().is_some_and(|d| d.ends_with(".git")));
        std::fs::create_dir(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "a\n").unwrap();
        std::fs::write(dir.join("old.rs"), "rename me\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "base"]);
        std::fs::write(dir.join("src/a.rs"), "staged\n").unwrap();
        git(dir, &["add", "src/a.rs"]);
        std::fs::write(dir.join("src/a.rs"), "staged then edited\n").unwrap();
        git(dir, &["mv", "old.rs", "new.rs"]);
        std::fs::write(dir.join("notes.txt"), "untracked\n").unwrap();
        let before = git(dir, &["status", "--porcelain=v1"]);
        let status = load(dir, &flag(), None, false, &mut git_dir)
            .unwrap()
            .unwrap();
        assert_eq!(git(dir, &["status", "--porcelain=v1"]), before);
        assert!(!status.unborn);
        // An identical re-read keeps the shown tree.
        let again = load(dir, &flag(), Some(status.hash), false, &mut git_dir);
        assert!(again.unwrap().is_none());
        let sections: Vec<_> = status
            .tree
            .rows
            .iter()
            .filter(|r| r.section)
            .map(|r| (r.label.as_str(), r.file_count))
            .collect();
        assert_eq!(sections, [("Changes", 2), ("Unversioned Files", 1)]);
        let targets: Vec<_> = status
            .targets
            .iter()
            .map(|t| (t.stage, t.status, t.path.as_str(), t.old_path.as_deref()))
            .collect();
        assert_eq!(
            targets,
            [
                (Stage::Local, 'R', "new.rs", Some("old.rs")),
                (Stage::Local, 'M', "src/a.rs", None),
                (Stage::Untracked, '?', "notes.txt", None),
            ]
        );
        // Every tree file maps to the target at the same index.
        for (file, target) in status.tree.files.iter().zip(&status.targets) {
            assert_eq!((file.status, &file.path), (target.status, &target.path));
        }
        // Nothing checked: Commit takes every change, both sides of the rename.
        let all = scope(&status, &HashSet::new());
        assert_eq!(all.paths, ["new.rs", "old.rs", "src/a.rs"]);
        assert_eq!((all.files, all.checked), (2, false));
        // Checked: just those; a checked unversioned file is not committed.
        let picked = HashSet::from(["src/a.rs".to_owned(), "notes.txt".to_owned()]);
        let some = scope(&status, &picked);
        assert_eq!(some.paths, ["src/a.rs"]);
        assert_eq!((some.files, some.checked), (1, true));
        let not_repo = tempfile::tempdir().unwrap();
        assert!(load(not_repo.path(), &flag(), None, false, &mut None).is_err());
        let cancelled = Arc::new(AtomicBool::new(true));
        assert!(load(dir, &cancelled, None, false, &mut git_dir).is_err());
    }

    #[test]
    fn menu_items_follow_the_selection_and_writes_take_only_their_files() {
        let target = |stage, status, path: &str, old: Option<&str>| DiffTarget {
            stage,
            commit: String::new(),
            parent: None,
            status,
            old_path: old.map(Into::into),
            path: path.into(),
            merge: false,
        };
        let targets = [
            target(Stage::Unstaged, 'U', "clash.rs", None),
            target(Stage::Local, 'R', "new.rs", Some("old.rs")),
            target(Stage::Local, 'M', "a.rs", None),
            target(Stage::Untracked, '?', "notes.txt", None),
            target(Stage::Local, 'D', "gone.rs", None),
            target(Stage::Local, 'A', "fresh.rs", None),
        ];
        let enabled = |files: &[usize], busy| -> Vec<bool> {
            menu(&targets, files, busy)
                .iter()
                .map(|m| m.enabled)
                .collect()
        };
        // Show Diff, Add to VCS, Mark Resolved, Rollback, Delete,
        // Add to .gitignore, Copy Path, Show in Explorer.
        let f = false;
        assert_eq!(enabled(&[2], f), [true, f, f, true, true, f, true, true]);
        assert_eq!(enabled(&[3], f), [true, true, f, f, true, true, true, true]);
        assert_eq!(enabled(&[0], f), [true, f, true, f, f, f, true, true]);
        assert_eq!(enabled(&[4], f), [true, f, f, true, f, f, true, true]);
        assert_eq!(
            enabled(&[0, 2, 3], f),
            [f, true, true, true, true, true, true, f]
        );
        assert_eq!(enabled(&[0, 2, 3], true), [f, f, f, f, f, f, true, f]);
        assert_eq!(enabled(&[], f), [f; 8]);
        // A mixed selection: each write takes just the files it applies to.
        let all: Vec<usize> = (0..targets.len()).collect();
        let strings = |p: &[&str]| p.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            write(&targets, Action::Add, &all),
            Some(Write::Track(strings(&["notes.txt"])))
        );
        assert_eq!(
            write(&targets, Action::Resolve, &all),
            Some(Write::Resolve(strings(&["clash.rs"])))
        );
        assert_eq!(
            write(&targets, Action::Rollback, &all),
            Some(Write::Rollback {
                untrack: strings(&["fresh.rs", "new.rs"]),
                restore: strings(&["a.rs", "gone.rs", "old.rs"]),
            })
        );
        assert_eq!(
            write(&targets, Action::Delete, &all),
            Some(Write::Delete(strings(&[
                "a.rs",
                "fresh.rs",
                "new.rs",
                "notes.txt"
            ])))
        );
        assert_eq!(
            write(&targets, Action::Ignore, &all),
            Some(Write::Ignore(strings(&["notes.txt"])))
        );
        assert!(write(&targets, Action::Add, &[2]).is_none());
        assert!(write(&targets, Action::ShowDiff, &all).is_none());
        let confirm = Confirm::new(write(&targets, Action::Rollback, &[1]).unwrap()).unwrap();
        assert_eq!(confirm.paths, ["new.rs", "old.rs"]);
        assert!(Confirm::new(Write::Resolve(Vec::new())).is_none());
    }

    fn frame(ctx: &egui::Context, view: &mut ChangesView, active: bool, events: Vec<egui::Event>) {
        frame_wide(ctx, view, active, events, 700.0);
    }
    fn frame_wide(
        ctx: &egui::Context,
        view: &mut ChangesView,
        active: bool,
        events: Vec<egui::Event>,
        width: f32,
    ) {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 400.0));
        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(rect),
                events,
                ..Default::default()
            },
            |ui| view.show(ui, rect, active, egui::Id::new("changes-test")),
        );
    }
    fn settle(ctx: &egui::Context, view: &mut ChangesView, active: bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while view.request.is_some() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            frame(ctx, view, active, vec![]);
        }
        assert!(view.request.is_none(), "status read did not finish");
    }

    fn files(view: &ChangesView) -> Vec<String> {
        match &view.status {
            Some(Ok(s)) => s.tree.files.iter().map(|f| f.path.clone()).collect(),
            _ => panic!("no status"),
        }
    }

    #[test]
    fn watched_edits_appear_while_shown_without_a_focus_change() {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path();
        git(dir, &["init", "-b", "main"]);
        std::fs::write(dir.join("b.txt"), "b\n").unwrap();
        let ctx = egui::Context::default();
        let mut view = ChangesView::new(Some(dir.to_path_buf()));
        let deadline = Instant::now() + Duration::from_secs(10);
        // Until the watch is live and its catch-up read has landed.
        while (view.seen.is_none() || view.request.is_some()) && Instant::now() < deadline {
            frame(&ctx, &mut view, true, vec![]);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(files(&view), ["b.txt"]);
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        let mut quiet = false;
        while files(&view).len() < 2 && Instant::now() < deadline {
            frame(&ctx, &mut view, true, vec![]);
            quiet |= view.request.as_ref().is_some_and(|r| r.quiet);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(files(&view), ["a.txt", "b.txt"]);
        assert!(quiet, "watch reads show no spinner");
    }

    #[test]
    fn any_width_lays_out_without_panicking_and_keeps_the_tree() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(
            repo.path().join("a.txt"),
            "a
",
        )
        .unwrap();
        git(repo.path(), &["init", "-b", "main"]);
        let ctx = egui::Context::default();
        let mut view = ChangesView::new(Some(repo.path().to_path_buf()));
        frame(&ctx, &mut view, true, vec![]);
        settle(&ctx, &mut view, true);
        for width in [700.0, 300.0, 140.0, 40.0, 0.0] {
            frame_wide(&ctx, &mut view, true, vec![], width);
        }
        assert_eq!(files(&view), ["a.txt"]);
    }

    fn click_at(ctx: &egui::Context, view: &mut ChangesView, pos: egui::Pos2) {
        frame(ctx, view, true, vec![egui::Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            let event = egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            frame(ctx, view, true, vec![event]);
        }
    }

    /// Click visible row `label` at `x`.
    fn click_row(ctx: &egui::Context, view: &mut ChangesView, x: f32, label: &str) {
        let y = view_row_y(view, label);
        click_at(ctx, view, egui::pos2(x, y));
    }

    #[test]
    fn refocus_rereads_keeps_selection_and_double_clicks_open_the_diff() {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path();
        git(dir, &["init", "-b", "main"]);
        std::fs::write(dir.join("b.txt"), "b\n").unwrap();
        let ctx = egui::Context::default();
        let mut view = ChangesView::new(Some(dir.to_path_buf()));
        // Without a watch (a network share), focus drives the re-reads.
        view.follow = watch::Follow::off();
        frame(&ctx, &mut view, true, vec![]);
        settle(&ctx, &mut view, true);
        assert_eq!(files(&view), ["b.txt"]);
        let pos = egui::pos2(200.0, view_row_y(&view, "b.txt"));
        // A single click only selects; the second click of a double opens.
        click_at(&ctx, &mut view, pos);
        assert!(view.acts.is_empty(), "the first click opened early");
        click_at(&ctx, &mut view, pos);
        let [HistoryAct::OpenDiff(target)] = view.acts.as_slice() else {
            panic!("expected one OpenDiff act");
        };
        assert_eq!(
            (target.stage, target.path.as_str()),
            (Stage::Untracked, "b.txt")
        );
        // Focus away and back: a new file appears without pressing Refresh,
        // and the selection follows b.txt to its new index.
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        frame(&ctx, &mut view, false, vec![]);
        view.started = Some(Instant::now() - REFOCUS_DEBOUNCE);
        frame(&ctx, &mut view, true, vec![]);
        assert!(view.request.is_some(), "refocus must start a read");
        settle(&ctx, &mut view, true);
        assert_eq!(files(&view), ["a.txt", "b.txt"]);
        let Some(Ok(status)) = &view.status else {
            panic!()
        };
        assert_eq!(status.tree.selected(), [1]);
        // Staying active never re-reads; a quick refocus inside the debounce doesn't either.
        frame(&ctx, &mut view, true, vec![]);
        frame(&ctx, &mut view, false, vec![]);
        // The Git read in settle() may already have consumed the debounce.
        view.started = Some(Instant::now());
        frame(&ctx, &mut view, true, vec![]);
        assert!(view.request.is_none());
    }

    /// Until the write and the re-read after it have both landed.
    fn settle_write(ctx: &egui::Context, view: &mut ChangesView) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while (view.commit.busy() || view.request.is_some()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            frame(ctx, view, true, vec![]);
        }
        assert!(
            !view.commit.busy() && view.request.is_none(),
            "write did not finish"
        );
    }
    fn key(
        ctx: &egui::Context,
        view: &mut ChangesView,
        key: egui::Key,
        modifiers: egui::Modifiers,
    ) {
        let event = egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        frame(ctx, view, true, vec![event]);
    }
    fn stages(view: &ChangesView) -> Vec<(Stage, char, String)> {
        let Some(Ok(status)) = &view.status else {
            panic!()
        };
        let t = &status.targets;
        t.iter()
            .map(|t| (t.stage, t.status, t.path.clone()))
            .collect()
    }

    #[test]
    fn checks_pick_what_add_to_vcs_and_ctrl_enter_commit_take() {
        let repo = repo();
        let dir = repo.path();
        for f in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(dir.join(f), "x\n").unwrap();
        }
        let ctx = egui::Context::default();
        let mut view = ChangesView::new(Some(dir.to_path_buf()));
        // No watch: each write itself must trigger the re-read.
        view.follow = watch::Follow::off();
        frame(&ctx, &mut view, true, vec![]);
        settle(&ctx, &mut view, true);
        // Ctrl+A then Space ticks every unversioned file; clicking c.txt's
        // box unticks it. Checking writes nothing to Git.
        key(&ctx, &mut view, egui::Key::A, egui::Modifiers::COMMAND);
        key(&ctx, &mut view, egui::Key::Space, egui::Modifiers::NONE);
        click_row(&ctx, &mut view, 40.0, "c.txt");
        assert_eq!(view.checked_unversioned(), ["a.txt", "b.txt"]);
        assert_eq!(git(dir, &["ls-files"]), "");
        view.start(&ctx, Write::Track(view.checked_unversioned()));
        settle_write(&ctx, &mut view);
        // Added: versioned, new (A) in Changes, unchecked; nothing staged.
        let a = |s: &str| (Stage::Local, 'A', s.to_owned());
        assert_eq!(
            stages(&view),
            [
                a("a.txt"),
                a("b.txt"),
                (Stage::Untracked, '?', "c.txt".into())
            ]
        );
        assert!(view.checked.is_empty());
        assert_eq!(git(dir, &["diff", "--cached", "--name-only"]), "");
        // Tick a.txt's box; Ctrl+Enter commits only it.
        click_row(&ctx, &mut view, 40.0, "a.txt");
        assert_eq!(view.scope.paths, ["a.txt"]);
        view.commit.message = "feat: add a".into();
        let id = egui::Id::new("changes-test").with("commit-message");
        ctx.memory_mut(|m| m.request_focus(id));
        frame(&ctx, &mut view, true, vec![]);
        key(&ctx, &mut view, egui::Key::Enter, egui::Modifiers::COMMAND);
        assert!(view.commit.busy(), "Ctrl+Enter must start the commit");
        settle_write(&ctx, &mut view);
        assert_eq!(git(dir, &["log", "-1", "--format=%s"]), "feat: add a");
        assert_eq!(git(dir, &["show", "--name-only", "--format="]), "a.txt");
        assert!(
            view.commit.message.is_empty(),
            "a commit clears the message"
        );
        assert_eq!(files(&view), ["b.txt", "c.txt"]);
        // Nothing checked now: Commit would take all of Changes, never c.txt.
        assert_eq!(view.scope.paths, ["b.txt"]);
        assert!(!view.scope.checked);
    }

    #[test]
    fn flat_grouping_and_collapse_all_keep_selection_and_checks() {
        let repo = repo();
        let dir = repo.path();
        std::fs::create_dir(dir.join("src")).unwrap();
        for f in ["src/a.rs", "src/b.rs", "top.txt"] {
            std::fs::write(dir.join(f), "x\n").unwrap();
        }
        let ctx = egui::Context::default();
        let mut view = ChangesView::new(Some(dir.to_path_buf()));
        view.follow = watch::Follow::off();
        frame(&ctx, &mut view, true, vec![]);
        settle(&ctx, &mut view, true);
        click_row(&ctx, &mut view, 200.0, "b.rs");
        click_row(&ctx, &mut view, 58.0, "b.rs");
        let labels = |view: &ChangesView| -> Vec<String> {
            let Some(Ok(s)) = &view.status else { panic!() };
            s.tree
                .visible
                .iter()
                .map(|&r| s.tree.rows[r].label.clone())
                .collect()
        };
        assert_eq!(
            labels(&view),
            ["Unversioned Files", "src", "a.rs", "b.rs", "top.txt"]
        );
        view.flat = true;
        if let Some(Ok(s)) = &mut view.status {
            s.tree.set_flat(true);
        }
        assert_eq!(
            labels(&view),
            ["Unversioned Files", "a.rs", "b.rs", "top.txt"]
        );
        let Some(Ok(s)) = &mut view.status else {
            panic!()
        };
        let paths = |ix: Vec<usize>| -> Vec<String> {
            ix.into_iter()
                .map(|f| s.tree.files[f].path.clone())
                .collect()
        };
        assert_eq!(paths(s.tree.selected()), ["src/b.rs"]);
        assert_eq!(paths(s.tree.checked()), ["src/b.rs"]);
        s.tree.set_all_collapsed(true);
        assert_eq!(labels(&view), ["Unversioned Files"]);
        // A re-read in flat mode stays flat.
        std::fs::write(dir.join("new.txt"), "x\n").unwrap();
        view.refresh(&ctx, false);
        settle(&ctx, &mut view, true);
        let Some(Ok(s)) = &view.status else { panic!() };
        assert!(s.tree.flat);
        assert_eq!(view.checked, HashSet::from(["src/b.rs".to_owned()]));
    }

    /// Screen y of the visible tree row labelled `label` (20px rows at scale
    /// 1), below the toolbar.
    fn view_row_y(view: &ChangesView, label: &str) -> f32 {
        let Some(Ok(status)) = &view.status else {
            panic!()
        };
        let visible = status
            .tree
            .visible
            .iter()
            .position(|&i| status.tree.rows[i].label == label)
            .unwrap();
        view.tree_top + 20.0 * visible as f32 + 10.0
    }
}
