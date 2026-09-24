//! Git Changes: the working tree's uncommitted files in a read-only window.
//! One `git status` read per refresh, on a cancellable worker; clicking a file
//! opens it in the Project's Diff window.
use super::file_tree::{self, ChangedFile, FileTree};
use super::{DiffTarget, HistoryAct, Stage, git};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

/// Becoming active re-reads the status unless the last read started this recently.
const REFOCUS_DEBOUNCE: Duration = Duration::from_secs(1);

/// Section headings, in display order: what needs attention first, then what
/// the next commit would contain, then everything else.
const SECTIONS: [(&str, Stage); 4] = [
    ("Conflicts", Stage::Unstaged),
    ("Staged", Stage::Staged),
    ("Changes", Stage::Unstaged),
    ("Unversioned Files", Stage::Untracked),
];

#[derive(Debug, PartialEq)]
struct Entry {
    section: usize,
    file: ChangedFile,
}

struct Status {
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
                section: 3,
                file: ChangedFile {
                    status: '?',
                    path: rest.to_owned(),
                    previous: None,
                },
            }),
            "u" => {
                let path = parts.get(9).ok_or_else(incomplete)?;
                entries.push(Entry {
                    section: 0,
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
                        section: 1,
                        file: ChangedFile {
                            status: status_char(xy[0])?,
                            path: (*path).to_owned(),
                            previous,
                        },
                    });
                }
                if xy[1] != b'.' {
                    entries.push(Entry {
                        section: 2,
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

/// Worker-only: read the status and build the sectioned tree.
fn load(cwd: &Path, cancel: &Arc<AtomicBool>) -> Result<Status, String> {
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
    Ok(Status {
        branch,
        tree,
        targets,
    })
}

struct Request {
    cancel: Arc<AtomicBool>,
    receiver: mpsc::Receiver<Result<Status, String>>,
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
            #[cfg(test)]
            tree_top: 0.0,
        }
    }
    /// Start a fresh read, cancelling any in flight. The shown status stays
    /// until the new one arrives, so a refresh never blanks the window.
    fn refresh(&mut self, ctx: &egui::Context) {
        let Some(cwd) = self.cwd.clone() else {
            self.status = Some(Err("This project has no directory".into()));
            return;
        };
        let (tx, receiver) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = load(&cwd, &stop);
            if !stop.load(Ordering::Relaxed) {
                let _ = tx.send(result);
                ctx.request_repaint();
            }
        });
        // Replacing the request drops (and so cancels) the previous one.
        self.request = Some(Request { cancel, receiver });
        self.started = Some(Instant::now());
    }
    fn poll(&mut self) {
        let Some(request) = &self.request else { return };
        let mut next = match request.receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("Git status worker stopped".into()),
        };
        self.request = None;
        // Carry collapse state and the selected file across the re-read.
        if let (Some(Ok(old)), Ok(new)) = (&self.status, &mut next) {
            new.tree.collapse(&old.tree.collapsed_keys());
            let selected = old.tree.selected.map(|i| &old.targets[i]);
            new.tree.selected = selected.and_then(|t| new.targets.iter().position(|n| n == t));
        }
        let old = std::mem::replace(&mut self.status, Some(next));
        std::thread::spawn(move || drop(old));
    }
    pub fn show(&mut self, ui: &mut egui::Ui, rect: egui::Rect, active: bool, base: egui::Id) {
        // Read on first show, and again whenever the window becomes active.
        let refocused = active
            && !self.was_active
            && self.started.is_none_or(|t| t.elapsed() >= REFOCUS_DEBOUNCE);
        self.was_active = active;
        if self.started.is_none() || (refocused && self.request.is_none()) {
            self.refresh(ui.ctx());
        }
        self.poll();
        let th = crate::theme::live(ui.ctx());
        let zoom = crate::view_scale::ViewScale::from_ctx(ui.ctx());
        let scale = zoom.factor();
        ui.painter().rect_filled(rect, 0.0, th.bg);
        let mut ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(base)
                .max_rect(rect.shrink(zoom.px(8.0)))
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        ui.set_clip_rect(rect.intersect(ui.clip_rect()));
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
            if self.request.is_some() {
                ui.spinner();
            }
            refresh = ui
                .button("Refresh")
                .on_hover_text(
                    "Re-read the working tree (also happens when this window is focused)",
                )
                .clicked();
        });
        if refresh {
            self.refresh(ui.ctx());
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
                if let Some(file) = file_tree::show(&mut ui, &mut status.tree, scale) {
                    self.acts
                        .push(HistoryAct::OpenDiff(status.targets[file].clone()));
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
        let empty = load(dir, &flag()).unwrap();
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
        let status = load(dir, &flag()).unwrap();
        assert_eq!(git(dir, &["status", "--porcelain=v1"]), before);
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
        assert!(load(not_repo.path(), &flag()).is_err());
        assert!(load(dir, &Arc::new(AtomicBool::new(true))).is_err());
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

    #[test]
    fn refocus_rereads_keeps_selection_and_clicks_open_the_diff() {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path();
        git(dir, &["init", "-b", "main"]);
        std::fs::write(dir.join("b.txt"), "b\n").unwrap();
        let ctx = egui::Context::default();
        let mut view = ChangesView::new(Some(dir.to_path_buf()));
        frame(&ctx, &mut view, true, vec![]);
        settle(&ctx, &mut view, true);
        let files = |view: &ChangesView| match &view.status {
            Some(Ok(s)) => s
                .tree
                .files
                .iter()
                .map(|f| f.path.clone())
                .collect::<Vec<_>>(),
            _ => panic!("no status"),
        };
        assert_eq!(files(&view), ["b.txt"]);
        let row = view_row_y(&view, "b.txt");
        let pos = egui::pos2(200.0, row);
        frame(&ctx, &mut view, true, vec![egui::Event::PointerMoved(pos)]);
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
        assert_eq!(status.tree.selected, Some(1));
        // Staying active never re-reads; a quick refocus inside the debounce doesn't either.
        frame(&ctx, &mut view, true, vec![]);
        frame(&ctx, &mut view, false, vec![]);
        frame(&ctx, &mut view, true, vec![]);
        assert!(view.request.is_none());
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
