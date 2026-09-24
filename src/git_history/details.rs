//! Selected-commit details: cancellable read-only Git queries and a virtualized file tree.
use super::*;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

#[derive(Debug, PartialEq)]
struct ChangedFile {
    status: char,
    path: String,
    previous: Option<String>,
}

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

struct TreeRow {
    label: String,
    depth: usize,
    file: Option<usize>,
    end: usize,
    collapsed: bool,
}
#[derive(Default)]
struct Directory {
    dirs: BTreeMap<String, Directory>,
    files: BTreeMap<String, usize>,
}
impl Directory {
    fn flatten(self, depth: usize, rows: &mut Vec<TreeRow>) {
        for (label, dir) in self.dirs {
            let index = rows.len();
            rows.push(TreeRow {
                label,
                depth,
                file: None,
                end: 0,
                collapsed: false,
            });
            dir.flatten(depth + 1, rows);
            rows[index].end = rows.len();
        }
        for (label, file) in self.files {
            rows.push(TreeRow {
                label,
                depth,
                file: Some(file),
                end: rows.len() + 1,
                collapsed: false,
            });
        }
    }
}
fn file_tree(files: &[ChangedFile]) -> Vec<TreeRow> {
    let mut root = Directory::default();
    for (index, file) in files.iter().enumerate() {
        let mut parts = file.path.split('/').peekable();
        let mut dir = &mut root;
        while let Some(part) = parts.next() {
            if parts.peek().is_some() {
                dir = dir.dirs.entry(part.into()).or_default();
            } else {
                dir.files.insert(part.into(), index);
            }
        }
    }
    let mut rows = Vec::new();
    root.flatten(0, &mut rows);
    rows
}

struct Details {
    hash: String,
    author: String,
    date: String,
    message: String,
    branches: String,
    merge: bool,
    files: Vec<ChangedFile>,
    tree: Vec<TreeRow>,
    visible: Vec<usize>,
}
impl Details {
    fn rebuild_visible(&mut self) {
        self.visible.clear();
        let mut i = 0;
        while i < self.tree.len() {
            self.visible.push(i);
            i = if self.tree[i].collapsed {
                self.tree[i].end
            } else {
                i + 1
            };
        }
    }
}

// This function runs only on a worker. Pipe readers drain concurrently, while
// the child owner checks cancellation/timeout and always reaps the process.
fn git_output(cwd: &Path, args: &[&str], cancel: &AtomicBool) -> Result<Vec<u8>, String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled".into());
    }
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd)
        .arg("--no-pager")
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(|e| format!("Cannot start Git: {e}"))?;
    fn drain(mut pipe: impl Read, limit: usize) -> std::io::Result<(Vec<u8>, bool)> {
        let mut bytes = Vec::new();
        let mut overflow = false;
        let mut buf = [0; 8192];
        loop {
            let n = pipe.read(&mut buf)?;
            if n == 0 {
                break;
            }
            let keep = n.min(limit.saturating_sub(bytes.len()));
            overflow |= keep < n;
            bytes.extend_from_slice(&buf[..keep]);
        }
        Ok((bytes, overflow))
    }
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || drain(stdout, 32 * 1024 * 1024));
    let err = std::thread::spawn(move || drain(stderr, 16384));
    let start = Instant::now();
    let status = loop {
        if cancel.load(Ordering::Relaxed) || start.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            let _ = child.wait();
            break Err("Git details cancelled or timed out".to_owned());
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(e.to_string());
            }
        }
    };
    let output = out
        .join()
        .map_err(|_| "Git output reader stopped")?
        .map_err(|e| e.to_string())?;
    let error = err
        .join()
        .map_err(|_| "Git error reader stopped")?
        .map_err(|e| e.to_string())?;
    if !status?.success() {
        return Err(format!(
            "Cannot read commit details: {}",
            String::from_utf8_lossy(&error.0).trim()
        ));
    }
    if output.1 {
        return Err("Commit details exceed the 32 MiB display limit".into());
    }
    Ok(output.0)
}

fn load(cwd: &Path, hash: &str, cancel: &AtomicBool) -> Result<Details, String> {
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
    let tree = file_tree(&files);
    let visible = (0..tree.len()).collect();
    Ok(Details {
        hash: fields[0].into(),
        author: fields[1].into(),
        date: fields[2].into(),
        message: fields[4].trim_end_matches('\n').into(),
        branches: String::from_utf8_lossy(&branches).trim().into(),
        merge: parents.len() > 1,
        files,
        tree,
        visible,
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
        let scale = crate::terminal::font_size(ui.ctx()) / crate::config::DEFAULT_FONT_SIZE;
        let mut ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(base)
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        ui.set_clip_rect(rect.intersect(ui.clip_rect()));
        ui.spacing_mut().button_padding *= scale;
        ui.spacing_mut().interact_size *= scale;
        for font in ui.style_mut().text_styles.values_mut() {
            font.size *= scale;
        }
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
        ui.push_id(egui::Id::new(&details.hash), |ui| {
            egui::ScrollArea::vertical()
                .id_salt("metadata")
                .max_height((rect.height() * 0.48).max(24.0))
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.strong("Commit details");
                    ui.horizontal(|ui| {
                        ui.label("Hash");
                        if ui.small_button("Copy").clicked() {
                            ui.ctx().copy_text(details.hash.clone());
                        }
                    });
                    ui.add(
                        egui::Label::new(egui::RichText::new(&details.hash).monospace())
                            .wrap()
                            .selectable(true),
                    );
                    ui.add(
                        egui::Label::new(format!(
                            "Author: {}\nDate: {}",
                            details.author, details.date
                        ))
                        .wrap()
                        .selectable(true),
                    );
                    ui.label(egui::RichText::new("Containing branches").color(th.dim));
                    ui.add(
                        egui::Label::new(if details.branches.is_empty() {
                            "None (detached or tag-only history)"
                        } else {
                            &details.branches
                        })
                        .wrap()
                        .selectable(true),
                    );
                    ui.separator();
                    ui.add(egui::Label::new(&details.message).wrap().selectable(true));
                });
            ui.separator();
            ui.strong(format!("Changed files ({})", details.files.len()));
            if details.merge {
                ui.colored_label(th.dim, "Compared with first parent");
            }
            if details.files.is_empty() {
                ui.colored_label(th.dim, "No changed files.");
                return;
            }
            ui.horizontal_wrapped(|ui| {
                for status in ['A', 'M', 'D', 'R', 'C', 'T'] {
                    ui.colored_label(status_color(status), status_name(status));
                }
            });
            let mut toggled = None;
            egui::ScrollArea::both()
                .id_salt("files")
                .auto_shrink([false, false])
                .show_rows(ui, 22.0 * scale, details.visible.len(), |ui, range| {
                    for i in range {
                        let index = details.visible[i];
                        let row = &details.tree[index];
                        ui.horizontal(|ui| {
                            ui.add_space(row.depth as f32 * 14.0 * scale);
                            if let Some(file) = row.file {
                                let file = &details.files[file];
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(format!(
                                            "{}  {}",
                                            file.status,
                                            display_path(&row.label)
                                        ))
                                        .color(status_color(file.status)),
                                    )
                                    .extend(),
                                )
                                .on_hover_text(
                                    match &file.previous {
                                        Some(old) => format!(
                                            "{}\n{} → {}",
                                            status_name(file.status),
                                            display_path(old),
                                            display_path(&file.path)
                                        ),
                                        None => format!(
                                            "{}\n{}",
                                            status_name(file.status),
                                            display_path(&file.path)
                                        ),
                                    },
                                );
                            } else if ui
                                .add(
                                    egui::Button::new(format!(
                                        "{} {}/",
                                        if row.collapsed { "▶" } else { "▼" },
                                        display_path(&row.label)
                                    ))
                                    .frame(false),
                                )
                                .clicked()
                            {
                                toggled = Some(index);
                            }
                        });
                    }
                });
            if let Some(index) = toggled {
                details.tree[index].collapsed = !details.tree[index].collapsed;
                details.rebuild_visible();
            }
        });
    }
}
fn display_path(path: &str) -> String {
    path.chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}
fn status_name(status: char) -> &'static str {
    match status {
        'A' => "Added",
        'D' => "Deleted",
        'R' => "Renamed",
        'C' => "Copied",
        'T' => "Type changed",
        'M' => "Modified",
        _ => "Other",
    }
}
fn status_color(status: char) -> egui::Color32 {
    // JetBrains Darcula FILESTATUS colors, independent of graph lane colors.
    // Copies are additions; Git type changes use the modified-file color.
    match status {
        'A' | 'C' => egui::Color32::from_rgb(0x62, 0x97, 0x55),
        'D' => egui::Color32::from_rgb(0x6c, 0x6c, 0x6c),
        'R' => egui::Color32::from_rgb(0x3a, 0x84, 0x84),
        'M' | 'T' => egui::Color32::from_rgb(0x68, 0x97, 0xbb),
        _ => COLORS[3],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_history::tests::git;

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
        assert_eq!(display_path("tab\tline\n"), "tab\\tline\\n");
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
        load(dir, &hash, &AtomicBool::new(false)).unwrap()
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
        assert_eq!(root.files.len(), 3);
        assert!(root.files.iter().all(|f| f.status == 'A'));
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
            details.files.iter().map(|f| f.status).collect::<Vec<_>>(),
            ['D', 'M', 'R', 'A']
        );
        let renamed = &details.files[2];
        assert_eq!(renamed.previous.as_deref(), Some("src/old name.txt"));
        assert_eq!(renamed.path, "src/new name.txt");
        assert_eq!(git(work.path(), &["status", "--porcelain=v1"]), before);
        let mut details = details;
        let folder = details.tree.iter().position(|r| r.label == "src").unwrap();
        details.tree[folder].collapsed = true;
        details.rebuild_visible();
        assert!(!details.visible.contains(&(folder + 1)));
        assert!(details.visible.contains(&folder));
        details.tree[folder].collapsed = false;
        details.rebuild_visible();
        assert_eq!(details.visible.len(), details.tree.len());
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
        assert_eq!(details.files.len(), 1);
        assert_eq!(details.files[0].path, "topic.txt");
        git(dir, &["checkout", "--detach"]);
        git(dir, &["commit", "--allow-empty", "-m", "Detached"]);
        let details = read(dir, "HEAD");
        assert!(details.files.is_empty());
        assert!(details.branches.is_empty());
        assert!(load(dir, &"0".repeat(40), &AtomicBool::new(false)).is_err());
        assert!(load(dir, "--bad-revision", &AtomicBool::new(false)).is_err());
        assert!(load(dir, &details.hash, &AtomicBool::new(true)).is_err());
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
