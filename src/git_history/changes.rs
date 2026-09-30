//! Git Changes: the working tree's uncommitted files. One `git status` read
//! per refresh, on a cancellable worker; double-clicking a file opens it in
//! the Project's Diff window. While shown, the repository watch (`watch.rs`)
//! triggers the refreshes; without one, becoming active does. The tree's
//! context menu (`menu`) acts on the selection; the writes it starts and the
//! commit panel are `commit.rs`.
use super::commit::{CommitPanel, Write};
use super::file_tree::{self, ChangedFile, FileTree, MenuItem, TreeEvent};
use super::{DiffTarget, HistoryAct, Stage, git, watch};
use eframe::egui;
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

/// Section headings, in display order: what needs attention first, then what
/// the next commit would contain, then everything else.
const SECTIONS: [(&str, Stage); 4] = [
    ("Conflicts", Stage::Unstaged),
    ("Staged", Stage::Staged),
    ("Changes", Stage::Unstaged),
    ("Unversioned Files", Stage::Untracked),
];
// Indices into `SECTIONS`.
const CONFLICTS: usize = 0;
const STAGED: usize = 1;
const CHANGES: usize = 2;
const UNVERSIONED: usize = 3;

#[derive(Debug, PartialEq)]
struct Entry {
    section: usize,
    file: ChangedFile,
}

struct Status {
    /// Of the raw `git status` bytes: an identical re-read keeps this Status.
    hash: u64,
    branch: Option<String>,
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

/// Parse `git status --porcelain=v2 -z --branch`. A file staged and then
/// edited again is two entries: one Staged, one in Changes.
fn parse_status(bytes: &[u8]) -> Result<(Option<String>, Vec<Entry>), String> {
    let mut branch = None;
    let mut entries = Vec::new();
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
        match kind {
            "#" => {
                if let Some(head) = rest.strip_prefix("branch.head ") {
                    branch = Some(head.to_owned());
                }
            }
            "?" => entries.push(Entry {
                section: UNVERSIONED,
                file: ChangedFile {
                    status: '?',
                    path: rest.to_owned(),
                    previous: None,
                },
            }),
            "u" => {
                let path = parts.get(9).ok_or_else(incomplete)?;
                entries.push(Entry {
                    section: CONFLICTS,
                    file: ChangedFile {
                        status: 'U',
                        path: (*path).to_owned(),
                        previous: None,
                    },
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
                if xy[0] != b'.' {
                    entries.push(Entry {
                        section: STAGED,
                        file: ChangedFile {
                            status: status_char(xy[0])?,
                            path: (*path).to_owned(),
                            previous,
                        },
                    });
                }
                if xy[1] != b'.' {
                    entries.push(Entry {
                        section: CHANGES,
                        file: ChangedFile {
                            status: status_char(xy[1])?,
                            path: (*path).to_owned(),
                            previous: None,
                        },
                    });
                }
            }
            "!" => {}
            _ => return Err("Git returned an unknown status entry".into()),
        }
    }
    Ok((branch, entries))
}

/// Worker-only: read the status and build the sectioned tree. `Ok(None)` when
/// the output hashes to `unchanged`, so the shown tree is kept as is.
fn load(
    cwd: &Path,
    cancel: &Arc<AtomicBool>,
    unchanged: Option<u64>,
) -> Result<Option<Status>, String> {
    let bytes = git::output(
        cwd,
        &[
            "status",
            "--porcelain=v2",
            "-z",
            "--branch",
            "--untracked-files=all",
            "--find-renames",
        ],
        cancel,
        32 << 20,
        Duration::from_secs(30),
    )
    .map_err(|e| match e {
        git::GitError::TooLarge => "Git status exceeds the 32 MiB display limit".to_owned(),
        git::GitError::Failed(stderr) => format!("Cannot read the working tree: {stderr}"),
        other => other.to_string(),
    })?;
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut h);
        h.finish()
    };
    if unchanged == Some(hash) {
        return Ok(None);
    }
    let (branch, entries) = parse_status(&bytes)?;
    let mut sections: Vec<Vec<ChangedFile>> = SECTIONS.iter().map(|_| Vec::new()).collect();
    for entry in entries {
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
    let tree = FileTree::sections(
        SECTIONS
            .iter()
            .zip(sections)
            .map(|((name, _), files)| (*name, files))
            .collect(),
    );
    Ok(Some(Status {
        hash,
        branch,
        tree,
        targets,
    }))
}

// Context-menu items, by index into `menu`.
const OPEN: usize = 0;
const STAGE: usize = 1;
const RESOLVE: usize = 2;
const UNSTAGE: usize = 3;
const COPY: usize = 4;

fn staged(t: &DiffTarget) -> bool {
    t.stage == Stage::Staged
}
// Conflicts sit in the Unstaged stage; adding marks them resolved.
fn conflict(t: &DiffTarget) -> bool {
    !staged(t) && t.status == 'U'
}
fn stageable(t: &DiffTarget) -> bool {
    !staged(t) && t.status != 'U'
}

/// The tree's context menu for the selected `files`. Every item is always
/// listed; each is enabled only if it applies to something selected. The
/// three writes double as a row's hover button.
fn menu(targets: &[DiffTarget], files: &[usize], busy: bool) -> Vec<MenuItem> {
    let any = |kind: fn(&DiffTarget) -> bool| files.iter().any(|&f| kind(&targets[f]));
    let item = |label, enabled, quick| MenuItem {
        label,
        enabled,
        quick,
    };
    vec![
        item("Open Diff", files.len() == 1, false),
        item("Stage", !busy && any(stageable), true),
        item("Mark Resolved", !busy && any(conflict), true),
        item("Unstage", !busy && any(staged), true),
        item("Copy Path", !files.is_empty(), false),
    ]
}

/// The write menu `item` starts: over just the selected files it applies to.
fn write(targets: &[DiffTarget], item: usize, files: &[usize]) -> Option<Write> {
    let paths = |kind: fn(&DiffTarget) -> bool| {
        let mut paths: Vec<String> = files
            .iter()
            .map(|&f| &targets[f])
            .filter(|t| kind(t))
            // A staged rename unstages both of its sides.
            .flat_map(|t| std::iter::once(t.path.clone()).chain(t.old_path.clone()))
            .collect();
        paths.sort();
        paths.dedup();
        paths
    };
    match item {
        STAGE => Some(Write::Stage(paths(stageable))),
        RESOLVE => Some(Write::Stage(paths(conflict))),
        UNSTAGE => Some(Write::Unstage(paths(staged))),
        _ => None,
    }
    .filter(|w| !matches!(w, Write::Stage(p) | Write::Unstage(p) if p.is_empty()))
}

struct Request {
    cancel: Arc<AtomicBool>,
    receiver: mpsc::Receiver<Result<Option<Status>, String>>,
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
            request: None,
            status: None,
            started: None,
            was_active: false,
            follow: watch::Follow::default(),
            seen: None,
            again: false,
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
        std::thread::spawn(move || {
            let result = load(&cwd, &stop, unchanged);
            if !stop.load(Ordering::Relaxed) {
                let _ = tx.send(result);
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
            Ok(Ok(Some(status))) => Ok(status),
            // Same bytes as shown: keep the tree, rows and all.
            Ok(Ok(None)) => {
                self.request = None;
                return self.follow_up(ctx);
            }
            Ok(Err(e)) => Err(e),
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("Git status worker stopped".into()),
        };
        self.request = None;
        // Carry collapse state and the selection across the re-read.
        if let (Some(Ok(old)), Ok(new)) = (&self.status, &mut next) {
            new.tree.collapse(&old.tree.collapsed_keys());
            new.tree.keep_selection(&old.tree);
        }
        let old = std::mem::replace(&mut self.status, Some(next));
        std::thread::spawn(move || drop(old));
        self.follow_up(ctx);
    }
    fn follow_up(&mut self, ctx: &egui::Context) {
        if self.again {
            self.refresh(ctx, true);
        }
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
        let staged = match &self.status {
            Some(Ok(status)) => status
                .targets
                .iter()
                .filter(|t| t.stage == Stage::Staged)
                .count(),
            _ => 0,
        };
        let panel_used = {
            let mut panel = ui.new_child(egui::UiBuilder::new().max_rect(inner));
            panel.set_clip_rect(inner.intersect(ui.clip_rect()));
            zoom.apply(&mut panel);
            let (used, outcome) =
                self.commit
                    .show(&mut panel, inner, self.cwd.as_deref(), staged, base);
            // The watch sees the index change; without one, re-read now.
            if outcome.wrote && generation.is_none() {
                if self.request.is_none() {
                    self.refresh(panel.ctx(), true);
                } else {
                    self.again = true;
                }
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
        let mut refresh = false;
        ui.horizontal(|ui| {
            match &self.status {
                Some(Ok(status)) => {
                    let branch = match status.branch.as_deref() {
                        Some("(detached)") | None => "Detached HEAD".to_owned(),
                        Some(name) => format!("On {name}"),
                    };
                    ui.label(egui::RichText::new(branch).color(th.text).strong());
                    let n = status.tree.files.len();
                    let count = if n == 1 {
                        "1 change".into()
                    } else {
                        format!("{n} changes")
                    };
                    ui.label(egui::RichText::new(count).color(th.dim));
                }
                _ => {
                    ui.label(egui::RichText::new("Working tree").color(th.text).strong());
                }
            }
            if self.request.as_ref().is_some_and(|r| !r.quiet) {
                ui.spinner();
            }
            // A live watch re-reads on every change; Refresh is only for
            // when it can't.
            if self.follow.down() || matches!(self.status, Some(Err(_))) {
                refresh = ui
                    .button("Refresh")
                    .on_hover_text(super::refresh_hint(self.follow.down()))
                    .clicked();
            }
        });
        if refresh {
            self.refresh(ui.ctx(), false);
        }
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
                let busy = self.commit.busy();
                let menu = |files: &[usize]| menu(targets, files, busy);
                match file_tree::show(&mut ui, &mut status.tree, scale, active, &menu) {
                    Some(TreeEvent::Open(file)) => {
                        self.acts.push(HistoryAct::OpenDiff(targets[file].clone()));
                    }
                    Some(TreeEvent::Act { item: OPEN, files }) => {
                        if let [file] = files[..] {
                            self.acts.push(HistoryAct::OpenDiff(targets[file].clone()));
                        }
                    }
                    Some(TreeEvent::Act { item: COPY, files }) => {
                        let paths: Vec<_> =
                            files.iter().map(|&f| targets[f].path.as_str()).collect();
                        ui.ctx().copy_text(paths.join(
                            "
",
                        ));
                    }
                    Some(TreeEvent::Act { item, files }) => {
                        if let (Some(write), Some(cwd)) = (write(targets, item, &files), &self.cwd)
                        {
                            self.commit.start(ui.ctx(), cwd, write);
                        }
                    }
                    None => {}
                }
            }
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
    fn porcelain_v2_splits_staged_unstaged_untracked_and_conflicts() {
        let z = "# branch.oid abc\0# branch.head main\0\
            1 MM N... 100644 100644 100644 h1 h2 src/both sides.rs\0\
            1 .D N... 100644 100644 000000 h1 h2 gone.rs\0\
            2 R. N... 100644 100644 100644 h1 h2 R100 new name.rs\0old name.rs\0\
            u UU N... 100644 100644 100644 100644 h1 h2 h3 clash.rs\0\
            ? dir/untracked file.txt\0";
        let (branch, entries) = parse_status(z.as_bytes()).unwrap();
        assert_eq!(branch.as_deref(), Some("main"));
        let summary: Vec<_> = entries
            .iter()
            .map(|e| {
                let f = &e.file;
                (e.section, f.status, f.path.as_str(), f.previous.as_deref())
            })
            .collect();
        assert_eq!(
            summary,
            [
                (1, 'M', "src/both sides.rs", None),
                (2, 'M', "src/both sides.rs", None),
                (2, 'D', "gone.rs", None),
                (1, 'R', "new name.rs", Some("old name.rs")),
                (0, 'U', "clash.rs", None),
                (3, '?', "dir/untracked file.txt", None),
            ]
        );
        assert!(parse_status(b"2 R. N... 1 1 1 h h R100 new\0").is_err());
        assert!(parse_status(b"1 M\0").is_err());
        assert!(parse_status(b"z what\0").is_err());
        assert_eq!(parse_status(b"").unwrap(), (None, vec![]));
    }

    #[test]
    fn real_working_tree_reads_sections_and_targets_without_writes() {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path();
        for args in [
            &["init", "-b", "main"][..],
            &["config", "user.name", "Changes Test"],
            &["config", "user.email", "changes@example.test"],
            &["config", "commit.gpgsign", "false"],
        ] {
            git(dir, args);
        }
        let empty = load(dir, &flag(), None).unwrap().unwrap();
        assert!(empty.tree.files.is_empty());
        assert_eq!(empty.branch.as_deref(), Some("main"));
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
        let status = load(dir, &flag(), None).unwrap().unwrap();
        assert_eq!(git(dir, &["status", "--porcelain=v1"]), before);
        // An identical re-read keeps the shown tree.
        assert!(load(dir, &flag(), Some(status.hash)).unwrap().is_none());
        assert!(load(dir, &flag(), Some(empty.hash)).unwrap().is_some());
        let sections: Vec<_> = status
            .tree
            .rows
            .iter()
            .filter(|r| r.section)
            .map(|r| (r.label.as_str(), r.file_count))
            .collect();
        assert_eq!(
            sections,
            [("Staged", 2), ("Changes", 1), ("Unversioned Files", 1)]
        );
        let targets: Vec<_> = status
            .targets
            .iter()
            .map(|t| (t.stage, t.status, t.path.as_str(), t.old_path.as_deref()))
            .collect();
        assert_eq!(
            targets,
            [
                (Stage::Staged, 'R', "new.rs", Some("old.rs")),
                (Stage::Staged, 'M', "src/a.rs", None),
                (Stage::Unstaged, 'M', "src/a.rs", None),
                (Stage::Untracked, '?', "notes.txt", None),
            ]
        );
        // Every tree file maps to the target at the same index.
        for (file, target) in status.tree.files.iter().zip(&status.targets) {
            assert_eq!((file.status, &file.path), (target.status, &target.path));
        }
        let not_repo = tempfile::tempdir().unwrap();
        assert!(load(not_repo.path(), &flag(), None).is_err());
        assert!(load(dir, &Arc::new(AtomicBool::new(true)), None).is_err());
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
            target(Stage::Staged, 'R', "new.rs", Some("old.rs")),
            target(Stage::Unstaged, 'M', "a.rs", None),
            target(Stage::Untracked, '?', "notes.txt", None),
        ];
        let enabled = |files: &[usize], busy| -> Vec<bool> {
            menu(&targets, files, busy)
                .iter()
                .map(|m| m.enabled)
                .collect()
        };
        // Open Diff, Stage, Mark Resolved, Unstage, Copy Path.
        assert_eq!(enabled(&[1], false), [true, false, false, true, true]);
        assert_eq!(enabled(&[2, 3], false), [false, true, false, false, true]);
        assert_eq!(enabled(&[0, 1, 2], false), [false, true, true, true, true]);
        assert_eq!(
            enabled(&[0, 1, 2], true),
            [false, false, false, false, true]
        );
        assert_eq!(enabled(&[], false), [false; 5]);
        // A mixed selection: each write takes just the files it applies to.
        let all = [0, 1, 2, 3];
        let paths = |item| match write(&targets, item, &all) {
            Some(Write::Stage(p)) => ("stage", p),
            Some(Write::Unstage(p)) => ("unstage", p),
            _ => panic!("no write for {item}"),
        };
        assert_eq!(
            paths(STAGE),
            ("stage", vec!["a.rs".into(), "notes.txt".into()])
        );
        assert_eq!(paths(RESOLVE), ("stage", vec!["clash.rs".into()]));
        assert_eq!(
            paths(UNSTAGE),
            ("unstage", vec!["new.rs".into(), "old.rs".into()])
        );
        assert!(write(&targets, UNSTAGE, &[2]).is_none());
        assert!(write(&targets, OPEN, &all).is_none());
    }

    fn frame(ctx: &egui::Context, view: &mut ChangesView, active: bool, events: Vec<egui::Event>) {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(500.0, 300.0));
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
        let row = view_row_y(&view, "b.txt");
        let pos = egui::pos2(200.0, row);
        frame(&ctx, &mut view, true, vec![egui::Event::PointerMoved(pos)]);
        // A single click only selects; the second click of a double opens.
        for click in 0..2 {
            assert!(view.acts.is_empty(), "click {click} opened early");
            for pressed in [true, false] {
                frame(
                    &ctx,
                    &mut view,
                    true,
                    vec![egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    }],
                );
            }
        }
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

    fn click(ctx: &egui::Context, view: &mut ChangesView, pos: egui::Pos2) {
        frame(ctx, view, true, vec![egui::Event::PointerMoved(pos)]);
        // One frame so the hover button exists before it is pressed.
        frame(ctx, view, true, vec![]);
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

    #[test]
    fn row_button_stages_and_ctrl_enter_commits_only_the_staged_file() {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path();
        for args in [
            &["init", "-b", "main"][..],
            &["config", "user.name", "Changes Test"],
            &["config", "user.email", "changes@example.test"],
            &["config", "commit.gpgsign", "false"],
        ] {
            git(dir, args);
        }
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        std::fs::write(dir.join("b.txt"), "b\n").unwrap();
        let ctx = egui::Context::default();
        let mut view = ChangesView::new(Some(dir.to_path_buf()));
        // No watch: the write itself must trigger the re-read.
        view.follow = watch::Follow::off();
        frame(&ctx, &mut view, true, vec![]);
        settle(&ctx, &mut view, true);
        // The hover button sits at the row's right edge.
        let row = view_row_y(&view, "a.txt");
        click(&ctx, &mut view, egui::pos2(480.0, row));
        assert!(view.acts.is_empty(), "the button must not open the diff");
        settle_write(&ctx, &mut view);
        assert_eq!(git(dir, &["diff", "--cached", "--name-only"]), "a.txt");
        let Some(Ok(status)) = &view.status else {
            panic!()
        };
        assert_eq!(status.targets[0].stage, Stage::Staged);
        // Type a message, focus it, Ctrl+Enter.
        view.commit.message = "feat: add a".into();
        let id = egui::Id::new("changes-test").with("commit-message");
        ctx.memory_mut(|m| m.request_focus(id));
        frame(&ctx, &mut view, true, vec![]);
        frame(
            &ctx,
            &mut view,
            true,
            vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::COMMAND,
            }],
        );
        assert!(view.commit.busy(), "Ctrl+Enter must start the commit");
        settle_write(&ctx, &mut view);
        assert_eq!(git(dir, &["log", "-1", "--format=%s"]), "feat: add a");
        assert_eq!(git(dir, &["show", "--name-only", "--format="]), "a.txt");
        assert!(
            view.commit.message.is_empty(),
            "a commit clears the message"
        );
        assert_eq!(files(&view), ["b.txt"]);
    }

    /// Screen y of the visible tree row labelled `label` (20px rows at scale 1).
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
