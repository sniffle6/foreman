//! Selected-commit details: cancellable read-only Git queries and a virtualized file tree.
use super::file_tree::{self, ChangedFile, FileTree};
use super::*;
use std::path::Path;

// -z keeps tabs, newlines, quoting, and rename pairs unambiguous. Decode only
// after splitting; paths need not be UTF-8 to preserve the record boundaries.
fn parse_files(bytes: &[u8]) -> Result<Vec<ChangedFile>, String> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let bytes = bytes
        .strip_suffix(&[0])
        .ok_or("Git returned an unterminated file change")?;
    let mut fields = bytes.split(|b| *b == 0);
    let mut files = Vec::new();
    while let Some(status) = fields.next() {
        let status = match status.first().copied() {
            Some(b'A' | b'M' | b'D' | b'R' | b'C' | b'T' | b'U' | b'X' | b'B') => status[0] as char,
            _ => return Err("Git returned an invalid file status".into()),
        };
        let mut path = fields
            .next()
            .ok_or("Git returned an incomplete file change")?;
        let previous = if matches!(status, 'R' | 'C') {
            let old = String::from_utf8_lossy(path).into_owned();
            path = fields.next().ok_or("Git returned an incomplete rename")?;
            Some(old)
        } else {
            None
        };
        files.push(ChangedFile {
            status,
            path: String::from_utf8_lossy(path).into_owned(),
            previous,
        });
    }
    Ok(files)
}

struct Details {
    hash: String,
    author: String,
    date: String,
    message: String,
    branches: String,
    merge: bool,
    /// First parent (the side a merge is diffed against); `None` for a root.
    parent: Option<String>,
    tree: FileTree,
}
impl Details {
    /// The Diff window target for `files[file]`, against the first parent.
    pub(super) fn target(&self, file: usize) -> super::DiffTarget {
        let f = &self.tree.files[file];
        super::DiffTarget {
            commit: self.hash.clone(),
            parent: self.parent.clone(),
            status: f.status,
            old_path: f.previous.clone(),
            path: f.path.clone(),
            merge: self.merge,
        }
    }
}

// Runs only on a worker. The shared helper drains the pipes and kills/reaps on
// cancel or timeout; this maps its errors to the pane's existing messages.
fn git_output(cwd: &Path, args: &[&str], cancel: &Arc<AtomicBool>) -> Result<Vec<u8>, String> {
    git::output(cwd, args, cancel, 32 * 1024 * 1024, Duration::from_secs(30)).map_err(|e| match e {
        git::GitError::Cancelled | git::GitError::TimedOut => {
            "Git details cancelled or timed out".to_owned()
        }
        git::GitError::TooLarge => "Commit details exceed the 32 MiB display limit".into(),
        git::GitError::Failed(stderr) => format!("Cannot read commit details: {stderr}"),
        other => other.to_string(),
    })
}

fn load(cwd: &Path, hash: &str, cancel: &Arc<AtomicBool>) -> Result<Details, String> {
    // Only object ids obtained from the timeline may be used as revision args.
    if !matches!(hash.len(), 40 | 64) || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Invalid commit id".into());
    }
    let metadata = git_output(
        cwd,
        &[
            "show",
            "--no-patch",
            "--no-color",
            "--no-show-signature",
            "--encoding=UTF-8",
            "--format=%H%x00%an <%ae>%x00%aI%x00%P%x00%B",
            hash,
            "--",
        ],
        cancel,
    )?;
    let metadata = String::from_utf8_lossy(&metadata);
    let fields: Vec<_> = metadata.splitn(5, '\0').collect();
    if fields.len() != 5 {
        return Err("Git returned incomplete commit metadata".into());
    }
    let parents: Vec<_> = fields[3].split_whitespace().collect();
    let mut args = vec![
        "diff-tree",
        "--no-commit-id",
        "--name-status",
        "-r",
        "-z",
        "--root",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "-M",
    ];
    if let Some(parent) = parents.first() {
        args.push(parent);
    }
    args.extend([hash, "--"]);
    let files = parse_files(&git_output(cwd, &args, cancel)?)?;
    let branches = git_output(
        cwd,
        &[
            "for-each-ref",
            "--format=%(refname:short)",
            "--contains",
            hash,
            "refs/heads/",
            "refs/remotes/",
        ],
        cancel,
    )?;
    Ok(Details {
        hash: fields[0].into(),
        author: fields[1].into(),
        date: fields[2].into(),
        message: fields[4].trim_end_matches('\n').into(),
        branches: String::from_utf8_lossy(&branches).trim().into(),
        merge: parents.len() > 1,
        parent: parents.first().map(|p| p.to_string()),
        tree: FileTree::new(files),
    })
}

struct Request {
    hash: String,
    cancel: Arc<AtomicBool>,
    receiver: mpsc::Receiver<Result<Details, String>>,
}
impl Drop for Request {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
#[derive(Default)]
pub(super) struct DetailsView {
    request: Option<Request>,
    result: Option<Result<Details, String>>,
    tree_fraction: Option<f32>,
    /// A file row clicked this frame, taken by `HistoryView` after the draw.
    open: Option<super::DiffTarget>,
}
impl Drop for DetailsView {
    fn drop(&mut self) {
        self.clear();
    }
}
impl DetailsView {
    fn clear(&mut self) {
        let request = self.request.take();
        if let Some(request) = &request {
            request.cancel.store(true, Ordering::Relaxed);
        }
        let result = self.result.take();
        if request.is_some() || result.is_some() {
            std::thread::spawn(move || {
                drop(request);
                drop(result);
            });
        }
    }
    /// A file row clicked this frame, for `HistoryView` to forward.
    pub(super) fn take_open(&mut self) -> Option<super::DiffTarget> {
        self.open.take()
    }
    pub(super) fn selected(&self) -> Option<&str> {
        self.request.as_ref().map(|r| r.hash.as_str())
    }
    pub(super) fn select(&mut self, cwd: PathBuf, hash: String, ctx: egui::Context) {
        if self.selected() == Some(&hash) {
            return;
        }
        self.clear();
        let (tx, receiver) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let revision = hash.clone();
        std::thread::spawn(move || {
            let result = load(&cwd, &revision, &stop);
            if !stop.load(Ordering::Relaxed) {
                let _ = tx.send(result);
                ctx.request_repaint();
            }
        });
        self.request = Some(Request {
            hash,
            cancel,
            receiver,
        });
    }
    pub(super) fn show(&mut self, ui: &mut egui::Ui, rect: egui::Rect, base: egui::Id) {
        let th = crate::theme::live(ui.ctx());
        let zoom = crate::view_scale::ViewScale::from_ctx(ui.ctx());
        let scale = zoom.factor();
        let mut ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(base)
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        ui.set_clip_rect(rect.intersect(ui.clip_rect()));
        zoom.apply(&mut ui);
        let Some(request) = &self.request else {
            ui.colored_label(th.dim, "Select a commit to view its details.");
            return;
        };
        if self.result.is_none() {
            match request.receiver.try_recv() {
                Ok(result) => self.result = Some(result),
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.result = Some(Err("Commit details worker stopped".into()))
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        let Some(result) = &mut self.result else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading commit details…");
            });
            return;
        };
        let details = match result {
            Ok(details) => details,
            Err(error) => {
                ui.label(error.as_str());
                return;
            }
        };
        // Keep the split across commit changes, but scope scroll state to the commit.
        let gap = (8.0 * scale).min(rect.height().max(0.0));
        let usable = (rect.height() - gap).max(0.0);
        let minimum = (72.0 * scale).min(usable * 0.5);
        let mut height = (usable * self.tree_fraction.unwrap_or(0.58))
            .clamp(minimum, (usable - minimum).max(minimum));
        let divider = egui::Rect::from_min_size(
            egui::pos2(rect.left(), rect.top() + height),
            egui::vec2(rect.width(), gap),
        );
        let response = ui
            .interact(divider, base.with("divider"), egui::Sense::drag())
            .on_hover_cursor(egui::CursorIcon::ResizeVertical);
        if response.dragged() {
            height = (height + ui.input(|i| i.pointer.delta().y))
                .clamp(minimum, (usable - minimum).max(minimum));
            self.tree_fraction = Some(height / usable.max(1.0));
        }
        let y = rect.top() + height;
        ui.painter().hline(
            rect.x_range(),
            y + gap * 0.5,
            egui::Stroke::new(
                1.0,
                if response.hovered() || response.dragged() {
                    th.dim
                } else {
                    th.border
                },
            ),
        );
        let tree_rect = egui::Rect::from_min_max(rect.min, egui::pos2(rect.right(), y));
        let metadata_rect = egui::Rect::from_min_max(egui::pos2(rect.left(), y + gap), rect.max);
        let mut tree_ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt((&details.hash, "tree"))
                .max_rect(tree_rect),
        );
        tree_ui.set_clip_rect(tree_rect.intersect(ui.clip_rect()));
        tree_ui.horizontal(|ui| {
            ui.strong(format!("Changed files ({})", details.tree.files.len()));
            if details.merge {
                ui.colored_label(th.dim, "· first parent");
            }
        });
        if details.tree.files.is_empty() {
            tree_ui.colored_label(th.dim, "No changed files.");
        } else if let Some(file) = file_tree::show(&mut tree_ui, &mut details.tree, scale) {
            self.open = Some(details.target(file));
        }
        let mut metadata_ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt((&details.hash, "metadata"))
                .max_rect(metadata_rect),
        );
        metadata_ui.set_clip_rect(metadata_rect.intersect(ui.clip_rect()));
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(&mut metadata_ui, |ui| {
                let (subject, body) = details
                    .message
                    .split_once('\n')
                    .unwrap_or((&details.message, ""));
                ui.add(
                    egui::Label::new(egui::RichText::new(subject).strong())
                        .wrap()
                        .selectable(true),
                );
                if !body.trim().is_empty() {
                    ui.add(egui::Label::new(body.trim()).wrap().selectable(true));
                }
                ui.add_space(4.0 * scale);
                ui.horizontal_wrapped(|ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(&details.author).color(th.dim))
                            .selectable(true),
                    );
                    ui.add(
                        egui::Label::new(egui::RichText::new(&details.date).color(th.dim))
                            .selectable(true),
                    );
                    if ui
                        .small_button(
                            egui::RichText::new(&details.hash[..details.hash.len().min(8)])
                                .monospace(),
                        )
                        .on_hover_text(format!("{}\nClick to copy full commit id", details.hash))
                        .clicked()
                    {
                        ui.ctx().copy_text(details.hash.clone());
                    }
                    if details.branches.is_empty() {
                        ui.colored_label(th.dim, "No containing branches");
                    }
                    for branch in details.branches.lines() {
                        egui::Frame::new()
                            .fill(th.tab_bg)
                            .corner_radius((3.0 * scale).round() as u8)
                            .inner_margin(egui::Margin::symmetric(
                                (5.0 * scale).round() as i8,
                                scale.round() as i8,
                            ))
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(egui::RichText::new(branch).small()).wrap(),
                                )
                                .on_hover_text("Branch containing this commit");
                            });
                    }
                });
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_history::tests::git;

    fn fixture() -> Details {
        let files = parse_files(b"A\0src/added.rs\0M\0src/modified.rs\0D\0removed.rs\0").unwrap();
        Details {
            hash: "a".repeat(40),
            author: "Author <author@example.test>".into(),
            date: "2026-09-23T12:00:00-04:00".into(),
            message: "Subject\n\nBody".into(),
            branches: "main\norigin/main".into(),
            merge: false,
            parent: None,
            tree: FileTree::new(files),
        }
    }

    #[test]
    fn file_clicks_select_one_and_directories_preserve_selection() {
        let ctx = egui::Context::default();
        let mut details = fixture();
        assert_eq!(details.tree.rows[0].file_count, 2);
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(500.0, 150.0));
        let frame = |details: &mut Details, events| {
            let mut clicked = None;
            let _ = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(rect),
                    events,
                    ..Default::default()
                },
                |ui| clicked = file_tree::show(ui, &mut details.tree, 1.0),
            );
            clicked
        };
        // Returns the file the click reported (what the Diff window opens).
        let click = |details: &mut Details, pos| {
            let mut clicked = frame(details, vec![egui::Event::PointerMoved(pos)]);
            for pressed in [true, false] {
                clicked = clicked.or(frame(
                    details,
                    vec![egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    }],
                ));
            }
            clicked
        };
        assert_eq!(click(&mut details, egui::pos2(170.0, 30.0)), Some(0));
        assert_eq!(details.tree.selected, Some(0));
        assert_eq!(click(&mut details, egui::pos2(170.0, 50.0)), Some(1));
        assert_eq!(details.tree.selected, Some(1));
        assert_eq!(click(&mut details, egui::pos2(170.0, 11.0)), None);
        assert!(details.tree.rows[0].collapsed);
        assert_eq!(details.tree.selected, Some(1));
        click(&mut details, egui::pos2(170.0, 11.0));
        assert!(!details.tree.rows[0].collapsed);
        assert_eq!(details.tree.selected, Some(1));
        frame(
            &mut details,
            vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                phase: egui::TouchPhase::Move,
                delta: egui::vec2(0.0, -80.0),
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert_eq!(details.tree.selected, Some(1));
    }

    #[test]
    fn divider_drags_and_commit_changes_clear_file_selection() {
        let ctx = egui::Context::default();
        let mut view = DetailsView::default();
        let (_tx, receiver) = mpsc::sync_channel(1);
        view.request = Some(Request {
            hash: "a".repeat(40),
            cancel: Arc::new(AtomicBool::new(false)),
            receiver,
        });
        let mut details = fixture();
        details.tree.selected = Some(1);
        view.result = Some(Ok(details));
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(500.0, 400.0));
        let frame = |view: &mut DetailsView, events| {
            let _ = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(rect),
                    events,
                    ..Default::default()
                },
                |ui| view.show(ui, rect, egui::Id::new("details-test")),
            );
        };
        frame(&mut view, vec![]);
        let start = egui::pos2(200.0, 392.0 * 0.58 + 4.0);
        frame(&mut view, vec![egui::Event::PointerMoved(start)]);
        frame(
            &mut view,
            vec![egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        frame(
            &mut view,
            vec![egui::Event::PointerMoved(start + egui::vec2(0.0, -60.0))],
        );
        assert!(view.tree_fraction.unwrap() < 0.58);
        assert_eq!(
            view.result
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .tree
                .selected,
            Some(1)
        );
        view.select(PathBuf::new(), "different".into(), ctx.clone());
        assert!(view.result.is_none());
        assert!(view.tree_fraction.is_some());
        view.clear();
        assert!(view.selected().is_none());
        assert!(view.result.is_none());
    }

    #[test]
    fn nul_paths_and_rename_pairs_keep_their_boundaries() {
        let files = parse_files(b"A\0dir/tab\tline\n.txt\0R100\0old name\0dir/new name\0C075\0source\0copy\0D\0gone\0T\0link\0").unwrap();
        assert_eq!(files[0].path, "dir/tab\tline\n.txt");
        assert_eq!(files[1].previous.as_deref(), Some("old name"));
        assert_eq!(files[1].path, "dir/new name");
        assert_eq!(files[2].status, 'C');
        assert_eq!(files[4].status, 'T');
        assert!(parse_files(b"R100\0old\0").is_err());
        assert!(parse_files(b"M\0unterminated").is_err());
        assert!(parse_files(b"").unwrap().is_empty());
        assert_eq!(file_tree::display_path("tab\tline\n"), "tab\\tline\\n");
    }

    fn repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-b", "main"]);
        git(repo.path(), &["config", "user.name", "Zoë Tester"]);
        git(
            repo.path(),
            &["config", "user.email", "history@example.test"],
        );
        git(repo.path(), &["config", "commit.gpgsign", "false"]);
        repo
    }
    fn read(dir: &Path, rev: &str) -> Details {
        let hash = git(dir, &["rev-parse", rev]);
        load(dir, &hash, &Arc::new(AtomicBool::new(false))).unwrap()
    }

    #[test]
    fn real_commit_details_cover_roots_renames_messages_branches_and_worktrees() {
        let repo = repo();
        let dir = repo.path();
        std::fs::create_dir(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/old name.txt"), "rename me\n").unwrap();
        std::fs::write(dir.join("src/edit.txt"), "original\n").unwrap();
        std::fs::write(dir.join("gone.txt"), "delete\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "Root"]);
        let root = read(dir, "HEAD");
        assert_eq!(root.tree.files.len(), 3);
        assert!(root.tree.files.iter().all(|f| f.status == 'A'));
        assert_eq!(root.target(0).parent, None);
        git(dir, &["mv", "src/old name.txt", "src/new name.txt"]);
        git(dir, &["rm", "gone.txt"]);
        std::fs::write(dir.join("src/edit.txt"), "modified\n").unwrap();
        std::fs::write(dir.join("日本語.txt"), "added\n").unwrap();
        git(dir, &["add", "."]);
        git(
            dir,
            &[
                "commit",
                "-m",
                "Subject 日本語\n\nFull message body.\n\nTrailer: value",
            ],
        );
        git(dir, &["branch", "topic"]);
        git(dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let work = tempfile::tempdir().unwrap();
        git(
            dir,
            &[
                "worktree",
                "add",
                "--detach",
                work.path().to_str().unwrap(),
                "HEAD",
            ],
        );
        let before = git(work.path(), &["status", "--porcelain=v1"]);
        let details = read(work.path(), "HEAD");
        assert!(details.author.contains("Zoë Tester <history@example.test>"));
        assert!(details.date.contains('T'));
        assert_eq!(
            details.message,
            "Subject 日本語\n\nFull message body.\n\nTrailer: value"
        );
        assert!(details.branches.lines().any(|b| b == "main"));
        assert!(details.branches.lines().any(|b| b == "topic"));
        assert!(details.branches.lines().any(|b| b == "origin/main"));
        assert_eq!(
            details.tree.files.iter().map(|f| f.status).collect::<Vec<_>>(),
            ['D', 'M', 'R', 'A']
        );
        let renamed = &details.tree.files[2];
        assert_eq!(renamed.previous.as_deref(), Some("src/old name.txt"));
        assert_eq!(renamed.path, "src/new name.txt");
        let t = details.target(2);
        assert_eq!(
            (t.status, t.old_path.as_deref(), t.path.as_str()),
            ('R', Some("src/old name.txt"), "src/new name.txt")
        );
        assert!(!t.merge);
        assert_eq!(git(work.path(), &["status", "--porcelain=v1"]), before);
        let mut details = details;
        let tree = &mut details.tree;
        let folder = tree.rows.iter().position(|r| r.label == "src").unwrap();
        tree.rows[folder].collapsed = true;
        tree.rebuild_visible();
        assert!(!tree.visible.contains(&(folder + 1)));
        assert!(tree.visible.contains(&folder));
        tree.rows[folder].collapsed = false;
        tree.rebuild_visible();
        assert_eq!(tree.visible.len(), tree.rows.len());
    }

    #[test]
    fn merge_uses_first_parent_and_empty_detached_commit_has_no_files_or_branches() {
        let repo = repo();
        let dir = repo.path();
        git(dir, &["commit", "--allow-empty", "-m", "Root"]);
        git(dir, &["checkout", "-b", "topic"]);
        std::fs::write(dir.join("topic.txt"), "topic").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "Topic"]);
        git(dir, &["checkout", "main"]);
        std::fs::write(dir.join("main.txt"), "main").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "Main"]);
        git(dir, &["merge", "--no-ff", "topic", "-m", "Merge"]);
        let details = read(dir, "HEAD");
        assert!(details.merge);
        assert_eq!(details.tree.files.len(), 1);
        assert_eq!(details.tree.files[0].path, "topic.txt");
        let target = details.target(0);
        assert_eq!(target.commit, details.hash);
        assert_eq!(
            target.parent.as_deref(),
            Some(git(dir, &["rev-parse", "HEAD^1"]).as_str())
        );
        assert!(target.merge);
        assert_eq!((target.status, target.path.as_str()), ('A', "topic.txt"));
        git(dir, &["checkout", "--detach"]);
        git(dir, &["commit", "--allow-empty", "-m", "Detached"]);
        let details = read(dir, "HEAD");
        assert!(details.tree.files.is_empty());
        assert!(details.branches.is_empty());
        assert!(load(dir, &"0".repeat(40), &Arc::new(AtomicBool::new(false))).is_err());
        assert!(load(dir, "--bad-revision", &Arc::new(AtomicBool::new(false))).is_err());
        assert!(load(dir, &details.hash, &Arc::new(AtomicBool::new(true))).is_err());
    }

    #[test]
    fn changing_selection_cancels_and_disconnects_the_previous_result() {
        let mut view = DetailsView::default();
        let (old_tx, receiver) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        view.request = Some(Request {
            hash: "old".into(),
            cancel: cancel.clone(),
            receiver,
        });
        view.select(PathBuf::new(), "new".into(), egui::Context::default());
        assert!(cancel.load(Ordering::Relaxed));
        assert_eq!(view.selected(), Some("new"));
        // Dropping the obsolete receiver happens off-thread; any queued result
        // remains on that obsolete channel, never the new selection's channel.
        let _ = old_tx.try_send(Err("stale result".into()));
        let result = view
            .request
            .as_ref()
            .unwrap()
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert!(matches!(result, Err(e) if e == "Invalid commit id"));
    }
}
